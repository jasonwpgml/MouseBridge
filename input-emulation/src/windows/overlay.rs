use std::ffi::c_void;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER,
    BLENDFUNCTION, CreateCompatibleDC, CreateDIBSection, CreateFontW, DIB_RGB_COLORS, DeleteDC,
    DeleteObject, FONT_QUALITY, GetDC, GetTextExtentPoint32W, HBITMAP, HDC, HGDIOBJ, ReleaseDC,
    SelectObject, SetBkColor, SetTextColor, TextOutW,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, HWND_TOPMOST,
    MSG, PostMessageW, PostQuitMessage, RegisterClassW, SW_HIDE, SWP_ASYNCWINDOWPOS,
    SWP_NOACTIVATE, SWP_NOSIZE, SWP_SHOWWINDOW, SetWindowPos, ShowWindowAsync, TranslateMessage,
    ULW_ALPHA, UpdateLayeredWindow, WM_CLOSE, WM_DESTROY, WNDCLASSW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{PCWSTR, w};

use crate::cursor_image::{self, TIP, TextMask};

/// A click-through, always-on-top window showing a second cursor with a
/// name tag. It runs its own message loop on a separate thread.
pub(super) struct Overlay {
    /// raw HWND, which is not Send itself
    hwnd: isize,
    thread: Option<JoinHandle<()>>,
}

impl Overlay {
    pub(super) fn new(label: &str, color: (u8, u8, u8)) -> Option<Self> {
        let label: Vec<u16> = label.encode_utf16().collect();
        let (tx, rx) = mpsc::channel();
        let thread = thread::spawn(move || {
            let hwnd = match unsafe { create_window(&label, color) } {
                Ok(hwnd) => hwnd,
                Err(e) => {
                    log::warn!("could not create second cursor window: {e}");
                    let _ = tx.send(None);
                    return;
                }
            };
            let _ = tx.send(Some(hwnd.0 as isize));
            let mut msg = MSG::default();
            unsafe {
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        });
        let hwnd = rx.recv().ok().flatten()?;
        Some(Self {
            hwnd,
            thread: Some(thread),
        })
    }

    fn hwnd(&self) -> HWND {
        HWND(self.hwnd as *mut _)
    }

    /// shows the overlay with the arrow's tip at the given screen position
    pub(super) fn move_to(&self, (x, y): (i32, i32)) {
        let flags = SWP_NOSIZE | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS | SWP_SHOWWINDOW;
        unsafe {
            let _ = SetWindowPos(
                self.hwnd(),
                Some(HWND_TOPMOST),
                x - TIP.0,
                y - TIP.1,
                0,
                0,
                flags,
            );
        }
    }

    pub(super) fn hide(&self) {
        unsafe {
            let _ = ShowWindowAsync(self.hwnd(), SW_HIDE);
        }
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        unsafe {
            let _ = PostMessageW(Some(self.hwnd()), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

unsafe fn create_window(label: &[u16], color: (u8, u8, u8)) -> windows::core::Result<HWND> {
    let instance = unsafe { GetModuleHandleW(None)? };
    let class = w!("lan-mouse-second-cursor");
    let wc = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        hInstance: instance.into(),
        lpszClassName: class,
        ..Default::default()
    };
    // fails harmlessly when another overlay registered the class already
    unsafe { RegisterClassW(&wc) };
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class,
            PCWSTR::null(),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(instance.into()),
            None,
        )?
    };
    let image = cursor_image::render(color, &unsafe { text_mask(label) });
    unsafe { show_image(hwnd, &image)? };
    Ok(hwnd)
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CLOSE => {
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// a 32 bit top-down bitmap and its pixels, selected into a memory DC
struct Canvas {
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    pixels: *mut [u8; 4],
    width: usize,
    height: usize,
}

impl Canvas {
    unsafe fn new(width: usize, height: usize) -> windows::core::Result<Self> {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let bitmap = unsafe { CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0)? };
        let dc = unsafe { CreateCompatibleDC(None) };
        let old = unsafe { SelectObject(dc, HGDIOBJ(bitmap.0)) };
        Ok(Self {
            dc,
            bitmap,
            old,
            pixels: bits as *mut [u8; 4],
            width,
            height,
        })
    }

    /// pixels in blue, green, red, alpha order
    fn pixels(&mut self) -> &mut [[u8; 4]] {
        unsafe { std::slice::from_raw_parts_mut(self.pixels, self.width * self.height) }
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
            let _ = DeleteDC(self.dc);
        }
    }
}

/// renders the label with GDI, white on black, and keeps the brightness
unsafe fn text_mask(label: &[u16]) -> TextMask {
    let font = unsafe {
        CreateFontW(
            -12,
            0,
            0,
            0,
            600,
            0,
            0,
            0,
            Default::default(),
            Default::default(),
            Default::default(),
            FONT_QUALITY(ANTIALIASED_QUALITY.0),
            0,
            w!("Segoe UI"),
        )
    };
    let mut size = SIZE::default();
    let measure = unsafe { CreateCompatibleDC(None) };
    unsafe {
        let old = SelectObject(measure, HGDIOBJ(font.0));
        let _ = GetTextExtentPoint32W(measure, label, &mut size);
        SelectObject(measure, old);
        let _ = DeleteDC(measure);
    }
    let (width, height) = (size.cx.max(1) as usize, size.cy.max(1) as usize);
    let coverage = match unsafe { Canvas::new(width, height) } {
        Ok(mut canvas) => unsafe {
            let old = SelectObject(canvas.dc, HGDIOBJ(font.0));
            SetTextColor(canvas.dc, COLORREF(0x00ff_ffff));
            SetBkColor(canvas.dc, COLORREF(0));
            let _ = TextOutW(canvas.dc, 0, 0, label);
            SelectObject(canvas.dc, old);
            canvas.pixels().iter().map(|p| p[1]).collect()
        },
        Err(e) => {
            log::warn!("could not render second cursor label: {e}");
            vec![0; width * height]
        }
    };
    unsafe {
        let _ = DeleteObject(HGDIOBJ(font.0));
    }
    TextMask {
        width,
        height,
        coverage,
    }
}

/// sizes the window to the image and shows it with per-pixel alpha
unsafe fn show_image(hwnd: HWND, image: &cursor_image::Image) -> windows::core::Result<()> {
    let mut canvas = unsafe { Canvas::new(image.width, image.height)? };
    for (dst, &[r, g, b, a]) in canvas.pixels().iter_mut().zip(&image.pixels) {
        *dst = [b, g, r, a];
    }
    let size = SIZE {
        cx: image.width as i32,
        cy: image.height as i32,
    };
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    let screen = unsafe { GetDC(None) };
    let result = unsafe {
        UpdateLayeredWindow(
            hwnd,
            Some(screen),
            None,
            Some(&size),
            Some(canvas.dc),
            Some(&POINT { x: 0, y: 0 }),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        )
    };
    unsafe { ReleaseDC(None, screen) };
    result
}

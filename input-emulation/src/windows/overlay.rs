use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER,
    BLENDFUNCTION, CreateCompatibleDC, CreateDIBSection, CreateFontW, DIB_RGB_COLORS, DeleteDC,
    DeleteObject, FONT_QUALITY, GetDC, GetTextExtentPoint32W, HBITMAP, HDC, HGDIOBJ, ReleaseDC,
    SelectObject, SetBkColor, SetTextColor, TextOutW,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, HWND_TOPMOST,
    KillTimer, MSG, PostMessageW, PostQuitMessage, RegisterClassW, SW_HIDE, SWP_NOACTIVATE,
    SWP_NOSIZE, SWP_SHOWWINDOW, SetTimer, SetWindowPos, ShowWindow, TranslateMessage, ULW_ALPHA,
    UpdateLayeredWindow, WM_APP, WM_CLOSE, WM_DESTROY, WM_DPICHANGED, WM_TIMER, WNDCLASSW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{PCWSTR, w};

use crate::cursor_image::{self, Picture, TextMask};

/// moves the window to the latest position in [`Target`]
const WM_MOVE_TO_TARGET: u32 = WM_APP;
const WM_HIDE: u32 = WM_APP + 1;
/// shows the name again, which then fades out
const WM_SHOW_LABEL: u32 = WM_APP + 2;

const FADE_TIMER: usize = 1;
const FADE_FRAME_MS: u32 = 30;
/// text is drawn this many times larger and shrunk, which comes out smoother
/// than GDI's own antialiasing at small sizes
const TEXT_OVERSAMPLING: usize = 4;
/// height of the name at 96 dpi
const TEXT_SIZE: f32 = 13.0;

/// Where the window should go. Pointer motion arrives faster than windows
/// can be moved, so only the latest position is kept and at most one move
/// is queued: queuing every move made the cursor trail far behind.
#[derive(Default)]
struct Target {
    /// x in the upper, y in the lower 32 bits
    position: AtomicU64,
    queued: AtomicBool,
}

thread_local! {
    /// target of the overlay owned by this thread
    static TARGET: RefCell<Option<Arc<Target>>> = const { RefCell::new(None) };
    /// what the overlay owned by this thread shows
    static FACE: RefCell<Option<Face>> = const { RefCell::new(None) };
}

struct Face {
    label: Vec<u16>,
    color: (u8, u8, u8),
    picture: Picture,
    /// when the name last came up, `None` while nothing was shown yet
    label_since: Option<Instant>,
}

impl Face {
    fn label_opacity(&self) -> f32 {
        self.label_since
            .map_or(1.0, |since| cursor_image::label_opacity(since.elapsed()))
    }
}

/// draws the current frame; never holds [`FACE`] while calling into the
/// window system, which can call back into [`window_proc`]
unsafe fn redraw(hwnd: HWND) {
    let frame = FACE.with_borrow(|face| {
        let face = face.as_ref()?;
        Some(face.picture.frame(face.label_opacity()))
    });
    if let Some(frame) = frame {
        if let Err(e) = unsafe { show_image(hwnd, &frame) } {
            log::warn!("could not draw second cursor: {e}");
        }
    }
}

unsafe fn show_label(hwnd: HWND) {
    FACE.with_borrow_mut(|face| {
        if let Some(face) = face {
            face.label_since = Some(Instant::now());
        }
    });
    unsafe {
        redraw(hwnd);
        SetTimer(Some(hwnd), FADE_TIMER, FADE_FRAME_MS, None);
    }
}

/// A click-through, always-on-top window showing a second cursor with a
/// name tag. It runs its own message loop on a separate thread.
pub(super) struct Overlay {
    /// raw HWND, which is not Send itself
    hwnd: isize,
    target: Arc<Target>,
    thread: Option<JoinHandle<()>>,
}

impl Overlay {
    pub(super) fn new(label: &str, color: (u8, u8, u8)) -> Option<Self> {
        let label: Vec<u16> = label.encode_utf16().collect();
        let target = Arc::new(Target::default());
        let thread_target = target.clone();
        let (tx, rx) = mpsc::channel();
        let thread = thread::spawn(move || {
            TARGET.set(Some(thread_target));
            // draws at the display's real resolution instead of being stretched
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
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
            target,
            thread: Some(thread),
        })
    }

    fn hwnd(&self) -> HWND {
        HWND(self.hwnd as *mut _)
    }

    /// shows the overlay with the arrow's tip at the given screen position
    pub(super) fn move_to(&self, (x, y): (i32, i32)) {
        let position = ((x as u32 as u64) << 32) | y as u32 as u64;
        self.target.position.store(position, Ordering::Release);
        if !self.target.queued.swap(true, Ordering::AcqRel) {
            self.post(WM_MOVE_TO_TARGET);
        }
    }

    pub(super) fn hide(&self) {
        self.post(WM_HIDE);
    }

    /// shows the name for a moment, for when the pointer comes in
    pub(super) fn show_label(&self) {
        self.post(WM_SHOW_LABEL);
    }

    fn post(&self, msg: u32) {
        unsafe {
            let _ = PostMessageW(Some(self.hwnd()), msg, WPARAM(0), LPARAM(0));
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
    let scale = unsafe { GetDpiForWindow(hwnd) } as f32 / 96.0;
    let picture = cursor_image::render(color, &unsafe { text_mask(label, scale) }, scale);
    FACE.set(Some(Face {
        label: label.to_vec(),
        color,
        picture,
        label_since: None,
    }));
    unsafe { redraw(hwnd) };
    Ok(hwnd)
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_MOVE_TO_TARGET => {
            let position = TARGET.with_borrow(|target| {
                let target = target.as_ref()?;
                // a move requested from here on needs a new message
                target.queued.store(false, Ordering::Release);
                Some(target.position.load(Ordering::Acquire))
            });
            let tip = FACE.with_borrow(|face| face.as_ref().map(|f| f.picture.tip));
            if let (Some(position), Some(tip)) = (position, tip) {
                let (x, y) = ((position >> 32) as u32 as i32, position as u32 as i32);
                let flags = SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW;
                unsafe {
                    let _ =
                        SetWindowPos(hwnd, Some(HWND_TOPMOST), x - tip.0, y - tip.1, 0, 0, flags);
                }
            }
            // the first time it appears the name comes up by itself
            if FACE.with_borrow(|face| face.as_ref().is_some_and(|f| f.label_since.is_none())) {
                unsafe { show_label(hwnd) };
            }
            LRESULT(0)
        }
        WM_HIDE => {
            let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
            LRESULT(0)
        }
        WM_SHOW_LABEL => {
            unsafe { show_label(hwnd) };
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == FADE_TIMER => {
            unsafe { redraw(hwnd) };
            if FACE.with_borrow(|face| face.as_ref().is_none_or(|f| f.label_opacity() == 0.0)) {
                let _ = unsafe { KillTimer(Some(hwnd), FADE_TIMER) };
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            // moved to a display with another scale; the window takes the
            // new picture's size when it is drawn
            let scale = (wparam.0 & 0xffff) as f32 / 96.0;
            let source = FACE.with_borrow(|face| face.as_ref().map(|f| (f.label.clone(), f.color)));
            if let Some((label, color)) = source {
                let picture =
                    cursor_image::render(color, &unsafe { text_mask(&label, scale) }, scale);
                FACE.with_borrow_mut(|face| {
                    if let Some(face) = face {
                        face.picture = picture;
                    }
                });
                unsafe { redraw(hwnd) };
            }
            LRESULT(0)
        }
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
unsafe fn text_mask(label: &[u16], scale: f32) -> TextMask {
    let height = (TEXT_SIZE * scale).round() as i32 * TEXT_OVERSAMPLING as i32;
    let font = unsafe {
        CreateFontW(
            -height,
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
    let n = TEXT_OVERSAMPLING;
    let (width, height) = (
        (size.cx.max(1) as usize).div_ceil(n),
        (size.cy.max(1) as usize).div_ceil(n),
    );
    let coverage = match unsafe { Canvas::new(width * n, height * n) } {
        Ok(mut canvas) => {
            unsafe {
                let old = SelectObject(canvas.dc, HGDIOBJ(font.0));
                SetTextColor(canvas.dc, COLORREF(0x00ff_ffff));
                SetBkColor(canvas.dc, COLORREF(0));
                let _ = TextOutW(canvas.dc, 0, 0, label);
                SelectObject(canvas.dc, old);
            }
            let large = canvas.pixels();
            let mut coverage = Vec::with_capacity(width * height);
            for y in 0..height {
                for x in 0..width {
                    let mut sum = 0usize;
                    for row in large[y * n * width * n..].chunks(width * n).take(n) {
                        sum += row[x * n..x * n + n]
                            .iter()
                            .map(|p| p[1] as usize)
                            .sum::<usize>();
                    }
                    coverage.push((sum / (n * n)) as u8);
                }
            }
            coverage
        }
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

use std::cell::RefCell;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreatePen, CreateSolidBrush, DeleteObject, EndPaint, FillRect,
    GetTextExtentPoint32W, HGDIOBJ, PAINTSTRUCT, PS_SOLID, Polygon, RoundRect, SelectObject,
    SetBkMode, SetTextColor, TRANSPARENT, TextOutW,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, HWND_TOPMOST,
    LWA_COLORKEY, MSG, PostMessageW, PostQuitMessage, RegisterClassW, SW_HIDE, SWP_ASYNCWINDOWPOS,
    SWP_NOACTIVATE, SWP_NOSIZE, SWP_SHOWWINDOW, SetLayeredWindowAttributes, SetWindowPos,
    ShowWindowAsync, TranslateMessage, WM_CLOSE, WM_DESTROY, WM_PAINT, WNDCLASSW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{PCWSTR, w};

const WIDTH: i32 = 280;
const HEIGHT: i32 = 48;
/// painted where the window should be see-through
const TRANSPARENT_KEY: COLORREF = rgb(255, 0, 255);
/// classic arrow cursor, tip at (1, 1)
const ARROW: [POINT; 7] = [
    POINT { x: 1, y: 1 },
    POINT { x: 1, y: 23 },
    POINT { x: 7, y: 18 },
    POINT { x: 11, y: 27 },
    POINT { x: 15, y: 25 },
    POINT { x: 11, y: 17 },
    POINT { x: 18, y: 17 },
];

pub(super) const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

thread_local! {
    /// label and color of the overlay owned by this thread
    static STYLE: RefCell<(Vec<u16>, COLORREF)> = const { RefCell::new((Vec::new(), COLORREF(0))) };
}

/// A click-through, always-on-top window drawing an arrow cursor with a
/// label. It runs its own message loop on a separate thread.
pub(super) struct Overlay {
    /// raw HWND, which is not Send itself
    hwnd: isize,
    thread: Option<JoinHandle<()>>,
}

impl Overlay {
    pub(super) fn new(label: &str, color: COLORREF) -> Option<Self> {
        let label: Vec<u16> = label.encode_utf16().collect();
        let (tx, rx) = mpsc::channel();
        let thread = thread::spawn(move || {
            STYLE.set((label, color));
            let hwnd = match unsafe { create_window() } {
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
            let _ = SetWindowPos(self.hwnd(), Some(HWND_TOPMOST), x - 1, y - 1, 0, 0, flags);
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

unsafe fn create_window() -> windows::core::Result<HWND> {
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
            WIDTH,
            HEIGHT,
            None,
            None,
            Some(instance.into()),
            None,
        )?
    };
    unsafe { SetLayeredWindowAttributes(hwnd, TRANSPARENT_KEY, 0, LWA_COLORKEY)? };
    Ok(hwnd)
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            STYLE.with_borrow(|(label, color)| unsafe { paint(hwnd, label, *color) });
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

unsafe fn paint(hwnd: HWND, label: &[u16], color: COLORREF) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
    let background = unsafe { CreateSolidBrush(TRANSPARENT_KEY) };
    let fill = unsafe { CreateSolidBrush(color) };
    let outline = unsafe { CreatePen(PS_SOLID, 1, rgb(255, 255, 255)) };
    let font = unsafe {
        CreateFontW(
            -14,
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
            Default::default(),
            0,
            w!("Segoe UI"),
        )
    };
    let full = RECT {
        left: 0,
        top: 0,
        right: WIDTH,
        bottom: HEIGHT,
    };
    unsafe {
        FillRect(hdc, &full, background);
        let old_pen = SelectObject(hdc, HGDIOBJ(outline.0));
        let old_brush = SelectObject(hdc, HGDIOBJ(fill.0));
        let old_font = SelectObject(hdc, HGDIOBJ(font.0));
        let _ = Polygon(hdc, &ARROW);

        // label in a pill below-right of the tip
        let mut size = SIZE::default();
        let _ = GetTextExtentPoint32W(hdc, label, &mut size);
        let (left, top) = (18, 22);
        let right = (left + size.cx + 14).min(WIDTH - 1);
        let _ = RoundRect(hdc, left, top, right, top + size.cy + 6, 10, 10);
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, rgb(255, 255, 255));
        let _ = TextOutW(hdc, left + 7, top + 3, label);

        SelectObject(hdc, old_pen);
        SelectObject(hdc, old_brush);
        SelectObject(hdc, old_font);
        let _ = DeleteObject(HGDIOBJ(background.0));
        let _ = DeleteObject(HGDIOBJ(fill.0));
        let _ = DeleteObject(HGDIOBJ(outline.0));
        let _ = DeleteObject(HGDIOBJ(font.0));
        let _ = EndPaint(hwnd, &ps);
    }
}

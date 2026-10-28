/// A display in device-independent units: points on macOS, physical pixels
/// divided by the scaling on Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// scaling in percent
    pub scale: u16,
}

/// this device's displays, empty where that is unsupported
#[cfg(target_os = "macos")]
pub fn displays() -> Vec<DisplayRect> {
    use core_graphics::display::CGDisplay;

    let Ok(ids) = CGDisplay::active_displays() else {
        return Vec::new();
    };
    ids.into_iter()
        .map(|id| {
            let display = CGDisplay::new(id);
            let bounds = display.bounds();
            let scale = display
                .display_mode()
                .filter(|mode| mode.width() > 0)
                .map_or(100, |mode| (mode.pixel_width() * 100 / mode.width()) as u16);
            DisplayRect {
                x: bounds.origin.x as i32,
                y: bounds.origin.y as i32,
                width: bounds.size.width as i32,
                height: bounds.size.height as i32,
                scale,
            }
        })
        .collect()
}

/// this device's displays, empty where that is unsupported
#[cfg(windows)]
pub fn displays() -> Vec<DisplayRect> {
    use windows::Win32::Foundation::{LPARAM, RECT};
    use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, HDC, HMONITOR};
    use windows::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, MDT_EFFECTIVE_DPI,
        SetThreadDpiAwarenessContext,
    };
    use windows::core::BOOL;

    unsafe extern "system" fn add(
        monitor: HMONITOR,
        _: HDC,
        rect: *mut RECT,
        data: LPARAM,
    ) -> BOOL {
        let displays = unsafe { &mut *(data.0 as *mut Vec<DisplayRect>) };
        let rect = unsafe { *rect };
        let (mut dpi_x, mut dpi_y) = (96, 96);
        let _ = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) };
        let scale = (dpi_x * 100 / 96).max(1) as i32;
        displays.push(DisplayRect {
            x: rect.left * 100 / scale,
            y: rect.top * 100 / scale,
            width: (rect.right - rect.left) * 100 / scale,
            height: (rect.bottom - rect.top) * 100 / scale,
            scale: scale as u16,
        });
        true.into()
    }

    let mut displays = Vec::new();
    unsafe {
        // physical pixels and the real scaling, whatever the process'
        // DPI awareness
        let previous = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(add),
            LPARAM(&mut displays as *mut Vec<DisplayRect> as isize),
        );
        SetThreadDpiAwarenessContext(previous);
    }
    displays
}

/// this device's displays, empty where that is unsupported
#[cfg(not(any(target_os = "macos", windows)))]
pub fn displays() -> Vec<DisplayRect> {
    Vec::new()
}

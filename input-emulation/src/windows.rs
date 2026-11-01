use super::error::{EmulationError, WindowsEmulationCreationError};
use input_event::{
    BTN_BACK, BTN_FORWARD, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, Event, KeyboardEvent, PointerEvent,
    WINDOWS_INJECTED_TAG, scancode,
};

use async_trait::async_trait;
use std::collections::{HashMap, HashSet};
use std::ops::BitOrAssign;
use std::time::Duration;
use tokio::task::AbortHandle;
use windows::Win32::Foundation::{LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, HDC, HMONITOR};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, MDT_EFFECTIVE_DPI,
    SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, MAPVK_VK_TO_VSC, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_VIRTUALDESK,
    MapVirtualKeyW, VIRTUAL_KEY, VK_CONTROL, VK_HANGUL, VK_HANJA, VK_SHIFT,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE,
    MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
    MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSEEVENTF_WHEEL, MOUSEINPUT,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT_0, KEYEVENTF_EXTENDEDKEY, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, SendInput,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetSystemMetrics, PostMessageW, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SetCursorPos, WM_MOUSEHWHEEL, WM_MOUSEWHEEL,
    WindowFromPoint, XBUTTON1, XBUTTON2,
};
use windows::core::BOOL;

use super::{Edge, Emulation, EmulationHandle};

mod overlay;
mod pointer;

use crate::cursor_image::color_for;
use overlay::Overlay;
use pointer::{Pointer, Rect};

const DEFAULT_REPEAT_DELAY: Duration = Duration::from_millis(500);
const DEFAULT_REPEAT_INTERVAL: Duration = Duration::from_millis(32);

pub(crate) struct WindowsEmulation {
    repeat_task: Option<AbortHandle>,
    second_cursors: HashMap<EmulationHandle, SecondCursor>,
    /// fraction of a pixel not moved yet, as small motions must add up
    motion_remainder: (f64, f64),
    /// trackpad scrolling not sent yet, per axis
    pending_scroll: [i32; 2],
    /// where the last second cursor was, to continue from there
    last_position: Option<(i32, i32)>,
}

impl WindowsEmulation {
    pub(crate) fn new() -> Result<Self, WindowsEmulationCreationError> {
        Ok(Self {
            repeat_task: None,
            second_cursors: HashMap::new(),
            motion_remainder: (0.0, 0.0),
            pending_scroll: [0; 2],
            last_position: None,
        })
    }
}

#[async_trait]
impl Emulation for WindowsEmulation {
    async fn consume(
        &mut self,
        event: Event,
        handle: EmulationHandle,
    ) -> Result<(), EmulationError> {
        let event = match event {
            Event::Pointer(PointerEvent::Axis { axis, value, .. }) => {
                let Some(pending) = self.pending_scroll.get_mut(axis as usize) else {
                    return Ok(());
                };
                let Some(value) = wheel_steps(value, pending) else {
                    return Ok(());
                };
                Event::Pointer(PointerEvent::AxisDiscrete120 { axis, value })
            }
            event => event,
        };
        if let Event::Pointer(pointer_event) = event {
            if let Some(cursor) = self.second_cursors.get_mut(&handle) {
                cursor.consume(pointer_event);
                return Ok(());
            }
        }
        match event {
            Event::Pointer(pointer_event) => match pointer_event {
                PointerEvent::Motion { time: _, dx, dy } => {
                    let dx = whole_pixels(dx, &mut self.motion_remainder.0);
                    let dy = whole_pixels(dy, &mut self.motion_remainder.1);
                    rel_mouse(dx, dy);
                }
                PointerEvent::Button {
                    time: _,
                    button,
                    state,
                } => mouse_button(button, state),
                PointerEvent::Axis {
                    time: _,
                    axis,
                    value,
                } => scroll(axis, value as i32),
                PointerEvent::AxisDiscrete120 { axis, value } => scroll(axis, value),
            },
            Event::Keyboard(keyboard_event) => match keyboard_event {
                KeyboardEvent::Key {
                    time: _,
                    key,
                    state,
                } => {
                    match state {
                        // pressed
                        0 => self.kill_repeat_task(),
                        // repeating an IME toggle would flip the input mode back and forth
                        1 if ime_virtual_key(key).is_none() => self.spawn_repeat_task(key).await,
                        _ => {}
                    }
                    key_event(key, state)
                }
                KeyboardEvent::Modifiers { .. } => {}
            },
        }
        // FIXME
        Ok(())
    }

    async fn create(&mut self, _handle: EmulationHandle) {}

    async fn destroy(&mut self, handle: EmulationHandle) {
        if let Some(mut cursor) = self.second_cursors.remove(&handle) {
            cursor.release_buttons();
            self.last_position = Some(cursor.pointer.position());
        }
    }

    async fn terminate(&mut self) {
        for (_, mut cursor) in self.second_cursors.drain() {
            cursor.release_buttons();
        }
    }

    async fn enter_at(&mut self, handle: EmulationHandle, point: (i32, i32)) {
        let at = to_physical(point);
        log::debug!("entering at {point:?}, {at:?} in pixels");
        match self.second_cursors.get_mut(&handle) {
            Some(cursor) => {
                cursor.pointer = Pointer::new(at);
                if let Some(overlay) = &cursor.overlay {
                    overlay.move_to(at);
                    overlay.show_label();
                }
            }
            None => set_cursor_position(at),
        }
    }

    async fn set_second_cursor(&mut self, handle: EmulationHandle, label: String) {
        let displays = displays();
        let start = self
            .last_position
            .unwrap_or_else(|| default_position(&displays));
        let cursor = SecondCursor {
            pointer: Pointer::new(start),
            overlay: Overlay::new(&label, color_for(&label)),
            displays,
            held: HashSet::new(),
            restore_to: None,
            crossed: None,
        };
        log::info!("showing {label} as a second cursor");
        self.second_cursors.insert(handle, cursor);
    }

    fn take_crossed_edge(&mut self, handle: EmulationHandle) -> Option<Edge> {
        self.second_cursors.get_mut(&handle)?.crossed.take()
    }
}

/// A remote device's pointer drawn as its own cursor. The system cursor
/// only visits it for clicks and drags, then returns to where the local
/// mouse left it. Scrolling goes straight to the window under it.
struct SecondCursor {
    pointer: Pointer,
    overlay: Option<Overlay>,
    displays: Vec<Rect>,
    /// mouse buttons held through this cursor
    held: HashSet<u32>,
    /// system cursor position to return to once all buttons are released
    restore_to: Option<(i32, i32)>,
    /// edge pushed against, not yet reported
    crossed: Option<Edge>,
}

impl SecondCursor {
    fn consume(&mut self, event: PointerEvent) {
        let at = self.pointer.position();
        match event {
            PointerEvent::Motion { dx, dy, .. } => {
                if let Some(edge) = self.pointer.move_by(dx, dy, &self.displays) {
                    self.crossed = Some(edge);
                }
                let at = self.pointer.position();
                if let Some(overlay) = &self.overlay {
                    overlay.move_to(at);
                }
                // a drag needs the system cursor to follow
                if !self.held.is_empty() {
                    send_mouse_inputs(&[absolute_move(at)]);
                }
            }
            PointerEvent::Button { button, state, .. } => {
                let Some(mi) = button_input(button, state) else {
                    return;
                };
                if state == 1 {
                    if self.held.is_empty() {
                        self.restore_to = cursor_position();
                    }
                    self.held.insert(button);
                    send_mouse_inputs(&[absolute_move(at), mi]);
                } else {
                    self.held.remove(&button);
                    let mut inputs = vec![absolute_move(at), mi];
                    if self.held.is_empty() {
                        inputs.extend(self.restore_to.take().map(absolute_move));
                    }
                    send_mouse_inputs(&inputs);
                }
            }
            PointerEvent::Axis { axis, value, .. } => self.scroll(at, axis, value as i32),
            PointerEvent::AxisDiscrete120 { axis, value } => self.scroll(at, axis, value),
        }
    }

    fn scroll(&self, at: (i32, i32), axis: u8, value: i32) {
        let Some(mi) = scroll_input(axis, value) else {
            return;
        };
        // the system cursor is already here, and a drag must not lose it
        if !self.held.is_empty() {
            send_mouse_inputs(&[absolute_move(at), mi]);
        } else if post_scroll(at, axis, value) {
            // the system cursor stayed with the local mouse
        } else if let Some(back) = cursor_position() {
            // the wheel input goes to the window under the system cursor
            send_mouse_inputs(&[absolute_move(at), mi, absolute_move(back)]);
        }
    }

    fn release_buttons(&mut self) {
        let at = self.pointer.position();
        let mut inputs = vec![absolute_move(at)];
        inputs.extend(self.held.drain().filter_map(|b| button_input(b, 0)));
        inputs.extend(self.restore_to.take().map(absolute_move));
        if inputs.len() > 1 {
            send_mouse_inputs(&inputs);
        }
        if let Some(overlay) = &self.overlay {
            overlay.hide();
        }
    }
}

/// bounds of all displays in virtual screen coordinates
fn displays() -> Vec<Rect> {
    unsafe extern "system" fn add(_: HMONITOR, _: HDC, rect: *mut RECT, data: LPARAM) -> BOOL {
        let displays = unsafe { &mut *(data.0 as *mut Vec<Rect>) };
        let rect = unsafe { *rect };
        displays.push(Rect {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        });
        true.into()
    }
    let mut displays = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(add),
            LPARAM(&mut displays as *mut Vec<Rect> as isize),
        );
    }
    displays
}

/// bottom centre of the primary display, which contains the origin
fn default_position(displays: &[Rect]) -> (i32, i32) {
    let primary = displays
        .iter()
        .find(|d| d.left <= 0 && 0 < d.right && d.top <= 0 && 0 < d.bottom)
        .or(displays.first());
    primary.map_or((0, 0), |d| ((d.left + d.right) / 2, d.bottom - 1))
}

/// A point in device-independent coordinates (physical pixels divided by the
/// display's scaling, as the display was reported to other devices) in
/// physical pixels.
fn to_physical((x, y): (i32, i32)) -> (i32, i32) {
    unsafe extern "system" fn add(
        monitor: HMONITOR,
        _: HDC,
        rect: *mut RECT,
        data: LPARAM,
    ) -> BOOL {
        let displays = unsafe { &mut *(data.0 as *mut Vec<(RECT, i32)>) };
        let (mut dpi_x, mut dpi_y) = (96, 96);
        let _ = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) };
        displays.push((unsafe { *rect }, (dpi_x * 100 / 96).max(1) as i32));
        true.into()
    }
    let mut displays: Vec<(RECT, i32)> = Vec::new();
    unsafe {
        let previous = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(add),
            LPARAM(&mut displays as *mut Vec<(RECT, i32)> as isize),
        );
        SetThreadDpiAwarenessContext(previous);
    }
    let dip = |r: &RECT, scale: i32| {
        let (left, top) = (r.left * 100 / scale, r.top * 100 / scale);
        let (right, bottom) = (r.right * 100 / scale, r.bottom * 100 / scale);
        left <= x && x < right && top <= y && y < bottom
    };
    let scale = displays
        .iter()
        .find(|(r, scale)| dip(r, *scale))
        .map_or(100, |&(_, scale)| scale);
    (x * scale / 100, y * scale / 100)
}

/// moves the system cursor to a point in physical pixels
fn set_cursor_position((x, y): (i32, i32)) {
    unsafe {
        let previous = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        if let Err(e) = SetCursorPos(x, y) {
            log::warn!("could not move the cursor to {x},{y}: {e}");
        }
        SetThreadDpiAwarenessContext(previous);
    }
}

fn cursor_position() -> Option<(i32, i32)> {
    let mut point = POINT::default();
    unsafe { GetCursorPos(&mut point) }.ok()?;
    Some((point.x, point.y))
}

/// moves the system cursor to the given virtual screen position
fn absolute_move((x, y): (i32, i32)) -> MOUSEINPUT {
    let (left, top, width, height) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1),
            GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1),
        )
    };
    // absolute coordinates are normalized to 0..=65535 across the virtual screen
    let normalize = |v: i32, origin: i32, size: i32| {
        ((v - origin) as i64 * 65536 / size as i64 + 65536 / (2 * size as i64)) as i32
    };
    MOUSEINPUT {
        dx: normalize(x, left, width),
        dy: normalize(y, top, height),
        mouseData: 0,
        dwFlags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
        time: 0,
        dwExtraInfo: WINDOWS_INJECTED_TAG,
    }
}

/// sends the inputs as one batch, so nothing gets in between
fn send_mouse_inputs(inputs: &[MOUSEINPUT]) {
    let inputs: Vec<INPUT> = inputs
        .iter()
        .map(|&mi| INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 { mi },
        })
        .collect();
    unsafe {
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

impl WindowsEmulation {
    async fn spawn_repeat_task(&mut self, key: u32) {
        // there can only be one repeating key and it's
        // always the last to be pressed
        self.kill_repeat_task();
        let repeat_task = tokio::task::spawn_local(async move {
            tokio::time::sleep(DEFAULT_REPEAT_DELAY).await;
            loop {
                key_event(key, 1);
                tokio::time::sleep(DEFAULT_REPEAT_INTERVAL).await;
            }
        });
        self.repeat_task = Some(repeat_task.abort_handle());
    }
    fn kill_repeat_task(&mut self) {
        if let Some(task) = self.repeat_task.take() {
            task.abort();
        }
    }
}

fn send_input_safe(input: INPUT) {
    unsafe {
        loop {
            /* retval = number of successfully submitted events */
            if SendInput(&[input], std::mem::size_of::<INPUT>() as i32) > 0 {
                break;
            }
        }
    }
}

fn send_mouse_input(mi: MOUSEINPUT) {
    send_input_safe(INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 { mi },
    });
}

fn send_keyboard_input(ki: KEYBDINPUT) {
    send_input_safe(INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki },
    });
}
/// the whole pixels of a motion, keeping the rest for the next one
fn whole_pixels(delta: f64, remainder: &mut f64) -> i32 {
    // a NaN would stay in the remainder and stop all further motion
    if !delta.is_finite() {
        return 0;
    }
    let total = delta + *remainder;
    let whole = total.trunc();
    *remainder = total - whole;
    whole as i32
}

fn rel_mouse(dx: i32, dy: i32) {
    let mi = MOUSEINPUT {
        dx,
        dy,
        mouseData: 0,
        dwFlags: MOUSEEVENTF_MOVE,
        time: 0,
        dwExtraInfo: WINDOWS_INJECTED_TAG,
    };
    send_mouse_input(mi);
}

fn mouse_button(button: u32, state: u32) {
    if let Some(mi) = button_input(button, state) {
        send_mouse_input(mi);
    }
}

fn button_input(button: u32, state: u32) -> Option<MOUSEINPUT> {
    let dw_flags = match state {
        0 => match button {
            BTN_LEFT => MOUSEEVENTF_LEFTUP,
            BTN_RIGHT => MOUSEEVENTF_RIGHTUP,
            BTN_MIDDLE => MOUSEEVENTF_MIDDLEUP,
            BTN_BACK => MOUSEEVENTF_XUP,
            BTN_FORWARD => MOUSEEVENTF_XUP,
            _ => return None,
        },
        1 => match button {
            BTN_LEFT => MOUSEEVENTF_LEFTDOWN,
            BTN_RIGHT => MOUSEEVENTF_RIGHTDOWN,
            BTN_MIDDLE => MOUSEEVENTF_MIDDLEDOWN,
            BTN_BACK => MOUSEEVENTF_XDOWN,
            BTN_FORWARD => MOUSEEVENTF_XDOWN,
            _ => return None,
        },
        _ => return None,
    };
    let mouse_data = match button {
        BTN_BACK => XBUTTON1 as u32,
        BTN_FORWARD => XBUTTON2 as u32,
        _ => 0,
    };
    Some(MOUSEINPUT {
        dx: 0,
        dy: 0, // no movement
        mouseData: mouse_data,
        dwFlags: dw_flags,
        time: 0,
        dwExtraInfo: WINDOWS_INJECTED_TAG,
    })
}

fn scroll(axis: u8, value: i32) {
    if let Some(mi) = scroll_input(axis, value) {
        send_mouse_input(mi);
    }
}

/// Turns a trackpad's scroll in points into wheel units, sent once there is
/// enough of it. A trackpad reports a few points at a time, which many
/// windows ignore as too small a fraction of a wheel notch of 120.
fn wheel_steps(points: f64, pending: &mut i32) -> Option<i32> {
    // a browser scrolls about 100 points per notch
    const UNITS_PER_POINT: f64 = 1.2;
    // a third of a notch, which is one line
    const SMALLEST_STEP: i32 = 40;
    let units = (points * UNITS_PER_POINT).round() as i32;
    // the fingers turned around, what was left belongs to the old direction
    if units.signum() == -pending.signum() {
        *pending = 0;
    }
    *pending = pending.saturating_add(units);
    if pending.abs() < SMALLEST_STEP {
        return None;
    }
    Some(std::mem::take(pending))
}

/// sends a scroll to the window at a point without moving the system cursor,
/// false if it could not be delivered, as with an elevated window
fn post_scroll(at: (i32, i32), axis: u8, value: i32) -> bool {
    // WHEEL is positive up but HWHEEL positive right, like scroll_input
    let (msg, delta) = match axis {
        0 => (WM_MOUSEWHEEL, value.saturating_neg()),
        1 => (WM_MOUSEHWHEEL, value),
        _ => return false,
    };
    let window = unsafe {
        let previous = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let window = WindowFromPoint(POINT { x: at.0, y: at.1 });
        SetThreadDpiAwarenessContext(previous);
        window
    };
    if window.is_invalid() {
        return false;
    }
    let (control, shift) = unsafe {
        (
            GetAsyncKeyState(VK_CONTROL.0 as i32) < 0,
            GetAsyncKeyState(VK_SHIFT.0 as i32) < 0,
        )
    };
    let wparam = WPARAM(wheel_wparam(delta, control, shift));
    let lparam = LPARAM(wheel_lparam(at));
    unsafe { PostMessageW(Some(window), msg, wparam, lparam) }.is_ok()
}

/// delta in the high word, held modifiers in the low word
fn wheel_wparam(delta: i32, control: bool, shift: bool) -> usize {
    const MK_SHIFT: usize = 0x4;
    const MK_CONTROL: usize = 0x8;
    let delta = delta.clamp(i16::MIN as i32, i16::MAX as i32) as i16 as u16 as usize;
    let keys = if control { MK_CONTROL } else { 0 } | if shift { MK_SHIFT } else { 0 };
    (delta << 16) | keys
}

/// signed screen coordinates, x in the low word and y in the high word
fn wheel_lparam((x, y): (i32, i32)) -> isize {
    (((y as i16 as u16 as u32) << 16) | x as i16 as u16 as u32) as i32 as isize
}

fn scroll_input(axis: u8, value: i32) -> Option<MOUSEINPUT> {
    // WHEEL is positive up but HWHEEL positive right, like lan-mouse's horizontal axis
    let (event_type, value) = match axis {
        0 => (MOUSEEVENTF_WHEEL, value.saturating_neg()),
        1 => (MOUSEEVENTF_HWHEEL, value),
        _ => return None,
    };
    Some(MOUSEINPUT {
        dx: 0,
        dy: 0,
        mouseData: value as u32,
        dwFlags: event_type,
        time: 0,
        dwExtraInfo: WINDOWS_INJECTED_TAG,
    })
}

fn key_event(key: u32, state: u8) {
    if let Some(vk) = ime_virtual_key(key) {
        let ki = KEYBDINPUT {
            wVk: vk,
            wScan: unsafe { MapVirtualKeyW(vk.0 as u32, MAPVK_VK_TO_VSC) } as u16,
            dwFlags: if state == 0 {
                KEYEVENTF_KEYUP
            } else {
                Default::default()
            },
            time: 0,
            dwExtraInfo: 0,
        };
        send_keyboard_input(ki);
        return;
    }
    let scancode = match linux_keycode_to_windows_scancode(key) {
        Some(code) => code,
        None => return,
    };
    let extended = scancode > 0xff;
    let scancode = scancode & 0xff;
    let mut flags = KEYEVENTF_SCANCODE;
    if extended {
        flags.bitor_assign(KEYEVENTF_EXTENDEDKEY);
    }
    if state == 0 {
        flags.bitor_assign(KEYEVENTF_KEYUP);
    }
    let ki = KEYBDINPUT {
        wVk: Default::default(),
        wScan: scancode,
        dwFlags: flags,
        time: 0,
        dwExtraInfo: 0,
    };
    send_keyboard_input(ki);
}

/// Korean IME toggle keys, sent as virtual keys: their scancodes differ
/// between keyboard types, but the IME always reacts to the virtual key.
fn ime_virtual_key(key: u32) -> Option<VIRTUAL_KEY> {
    match scancode::Linux::try_from(key) {
        Ok(scancode::Linux::KeyHanguel) => Some(VK_HANGUL),
        Ok(scancode::Linux::KeyHanja) => Some(VK_HANJA),
        _ => None,
    }
}

fn linux_keycode_to_windows_scancode(linux_keycode: u32) -> Option<u16> {
    let linux_scancode = match scancode::Linux::try_from(linux_keycode) {
        Ok(s) => s,
        Err(_) => {
            log::warn!("unknown keycode: {linux_keycode}");
            return None;
        }
    };
    log::trace!("linux code: {linux_scancode:?}");
    let windows_scancode = match scancode::Windows::try_from(linux_scancode) {
        Ok(s) => s,
        Err(_) => {
            log::warn!("failed to translate linux code into windows scancode: {linux_scancode:?}");
            return None;
        }
    };
    log::trace!("windows code: {windows_scancode:?}");
    Some(windows_scancode as u16)
}

#[cfg(test)]
mod tests {
    use super::{wheel_lparam, wheel_steps, wheel_wparam, whole_pixels};

    #[test]
    fn small_scrolls_wait_until_there_is_a_step() {
        let mut pending = 0;
        assert_eq!(wheel_steps(10.0, &mut pending), None);
        assert_eq!(wheel_steps(10.0, &mut pending), None);
        assert_eq!(wheel_steps(20.0, &mut pending), Some(48));
        assert_eq!(pending, 0);
    }

    #[test]
    fn large_scrolls_go_out_whole() {
        let mut pending = 0;
        assert_eq!(wheel_steps(-100.0, &mut pending), Some(-120));
    }

    #[test]
    fn turning_around_drops_what_was_left() {
        let mut pending = 0;
        assert_eq!(wheel_steps(30.0, &mut pending), None);
        assert_eq!(wheel_steps(-5.0, &mut pending), None);
        assert_eq!(pending, -6);
    }

    #[test]
    fn invalid_motion_leaves_the_remainder_alone() {
        let mut remainder = 0.5;
        assert_eq!(whole_pixels(f64::NAN, &mut remainder), 0);
        assert_eq!(whole_pixels(f64::INFINITY, &mut remainder), 0);
        assert_eq!(whole_pixels(0.5, &mut remainder), 1);
    }

    #[test]
    fn huge_scrolls_do_not_overflow() {
        let mut pending = 30;
        assert_eq!(wheel_steps(f64::MAX, &mut pending), Some(i32::MAX));
    }

    #[test]
    fn small_motions_add_up() {
        let mut remainder = 0.0;
        let moved: i32 = (0..8).map(|_| whole_pixels(0.5, &mut remainder)).sum();
        assert_eq!(moved, 4);
        let moved: i32 = (0..8).map(|_| whole_pixels(-0.5, &mut remainder)).sum();
        assert_eq!(moved, -4);
    }

    #[test]
    fn wheel_delta_is_in_the_high_word() {
        assert_eq!(wheel_wparam(120, false, false), 120 << 16);
        assert_eq!(wheel_wparam(-120, false, false) >> 16, 0xff88);
        assert_eq!(wheel_wparam(70000, false, false) >> 16, 0x7fff);
    }

    #[test]
    fn wheel_keys_are_in_the_low_word() {
        assert_eq!(wheel_wparam(1, true, false) & 0xffff, 0x8);
        assert_eq!(wheel_wparam(1, false, true) & 0xffff, 0x4);
    }

    #[test]
    fn wheel_point_keeps_negative_coordinates() {
        let lparam = wheel_lparam((-1920, 300)) as u32;
        assert_eq!((lparam & 0xffff) as u16 as i16, -1920);
        assert_eq!((lparam >> 16) as u16 as i16, 300);
    }
}

use std::{
    collections::{HashMap, HashSet, VecDeque},
    fmt::Display,
    sync::RwLock,
    task::{Poll, ready},
};

use async_trait::async_trait;
use futures::StreamExt;
use futures_core::Stream;

use input_event::{Event, KeyboardEvent, scancode};

pub use displays::{DisplayRect, displays};
pub use error::{CaptureCreationError, CaptureError, InputCaptureError};

mod displays;
pub mod error;

#[cfg(libei)]
mod libei;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(layer_shell)]
mod layer_shell;

#[cfg(windows)]
mod windows;

#[cfg(x11)]
mod x11;

/// fallback input capture (does not produce events)
mod dummy;

pub type CaptureHandle = u64;

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum CaptureEvent {
    /// capture on this capture handle is now active
    Begin,
    /// capture is now active, the pointer left the screen at this point in
    /// pointer coordinates
    BeginAt((i32, i32)),
    /// input event coming from capture handle
    Input(Event),
}

impl Display for CaptureEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CaptureEvent::Begin => write!(f, "begin capture"),
            CaptureEvent::BeginAt((x, y)) => write!(f, "begin capture at {x},{y}"),
            CaptureEvent::Input(e) => write!(f, "{e}"),
        }
    }
}

/// Where keyboard input goes, independently of the pointer.
/// Only the Windows and macOS backends honor it.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub enum KeyboardCapture {
    /// keys go wherever the pointer is
    #[default]
    FollowPointer,
    /// keys are never captured and stay on this device
    Local,
    /// keys are always captured for the client at this position, even
    /// while the pointer is on this device
    Always(Position),
}

static KEYBOARD_CAPTURE: RwLock<KeyboardCapture> = RwLock::new(KeyboardCapture::FollowPointer);

pub fn set_keyboard_capture(capture: KeyboardCapture) {
    *KEYBOARD_CAPTURE.write().expect("lock") = capture;
}

#[cfg(any(windows, target_os = "macos"))]
fn keyboard_capture() -> KeyboardCapture {
    *KEYBOARD_CAPTURE.read().expect("lock")
}

#[derive(Debug, Clone, Copy, Eq, Hash, PartialEq)]
pub enum Position {
    Left,
    Right,
    Top,
    Bottom,
}

impl Position {
    pub fn opposite(&self) -> Self {
        match self {
            Position::Left => Self::Right,
            Position::Right => Self::Left,
            Position::Top => Self::Bottom,
            Position::Bottom => Self::Top,
        }
    }
}

impl Display for Position {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let pos = match self {
            Position::Left => "left",
            Position::Right => "right",
            Position::Top => "top",
            Position::Bottom => "bottom",
        };
        write!(f, "{pos}")
    }
}

/// How far a point may be from a gate's edge and still pass through it, as
/// the pointer is held a pixel inside the screen.
const GATE_TOLERANCE: i32 = 2;

/// A stretch of a display edge the pointer may leave through, in pointer
/// coordinates. Only the Windows and macOS backends honor gates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Gate {
    pub side: Position,
    /// coordinate of the edge, x for left and right, y for top and bottom
    pub at: i32,
    /// stretch of the edge, end exclusive: y for left and right, x for top
    /// and bottom
    pub from: i32,
    pub to: i32,
}

impl Gate {
    /// whether leaving through `side` at `(x, y)` passes this gate
    pub fn contains(&self, side: Position, (x, y): (i32, i32)) -> bool {
        let (at, along) = match side {
            Position::Left | Position::Right => (x, y),
            Position::Top | Position::Bottom => (y, x),
        };
        side == self.side
            && (at - self.at).abs() <= GATE_TOLERANCE
            && (self.from..self.to).contains(&along)
    }
}

/// whether leaving through `side` at `point` is allowed by `gates`, where no
/// gates means the whole edge
#[cfg(any(windows, target_os = "macos"))]
fn passes(gates: Option<&Vec<Gate>>, side: Position, point: (i32, i32)) -> bool {
    gates.is_none_or(|gates| gates.iter().any(|g| g.contains(side, point)))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Backend {
    #[cfg(libei)]
    InputCapturePortal,
    #[cfg(layer_shell)]
    LayerShell,
    #[cfg(x11)]
    X11,
    #[cfg(windows)]
    Windows,
    #[cfg(target_os = "macos")]
    MacOs,
    Dummy,
}

impl Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(libei)]
            Backend::InputCapturePortal => write!(f, "input-capture-portal"),
            #[cfg(layer_shell)]
            Backend::LayerShell => write!(f, "layer-shell"),
            #[cfg(x11)]
            Backend::X11 => write!(f, "X11"),
            #[cfg(windows)]
            Backend::Windows => write!(f, "windows"),
            #[cfg(target_os = "macos")]
            Backend::MacOs => write!(f, "MacOS"),
            Backend::Dummy => write!(f, "dummy"),
        }
    }
}

pub struct InputCapture {
    /// capture backend
    capture: Box<dyn Capture>,
    /// keys pressed by active capture
    pressed_keys: HashSet<scancode::Linux>,
    /// map from position to ids
    position_map: HashMap<Position, Vec<CaptureHandle>>,
    /// map from id to position
    id_map: HashMap<CaptureHandle, Position>,
    /// pending events
    pending: VecDeque<(CaptureHandle, CaptureEvent)>,
    /// gates of the handles that only take the pointer through those
    gates: HashMap<CaptureHandle, Vec<Gate>>,
    /// the handles the current capture goes to, chosen by where the pointer
    /// left
    routed: Option<(Position, Vec<CaptureHandle>)>,
}

impl InputCapture {
    /// create a new client with the given id
    pub async fn create(&mut self, id: CaptureHandle, pos: Position) -> Result<(), CaptureError> {
        assert!(!self.id_map.contains_key(&id));

        self.id_map.insert(id, pos);

        if let Some(v) = self.position_map.get_mut(&pos) {
            v.push(id);
        } else {
            self.position_map.insert(pos, vec![id]);
            self.capture.create(pos).await?;
        }
        self.update_gates(pos);
        Ok(())
    }

    /// Lets the pointer reach the handle only through `gates`, or anywhere
    /// along its edge with `None`.
    pub fn set_gates(&mut self, id: CaptureHandle, gates: Option<Vec<Gate>>) {
        match gates {
            Some(gates) => self.gates.insert(id, gates),
            None => self.gates.remove(&id),
        };
        if let Some(&pos) = self.id_map.get(&id) {
            self.update_gates(pos);
        }
    }

    /// tells the backend the gates of all handles at `pos`
    fn update_gates(&mut self, pos: Position) {
        let ids = self.position_map.get(&pos).cloned().unwrap_or_default();
        let gated = ids
            .iter()
            .filter_map(|id| self.gates.get(id))
            .flatten()
            .copied()
            .collect::<Vec<_>>();
        let gates = ids
            .iter()
            .any(|id| self.gates.contains_key(id))
            .then_some(gated);
        self.capture.set_gates(pos, gates);
    }

    /// The handles at `pos` a capture beginning at `point` goes to, all of
    /// them with `None`. Where handles have gates the edge is shared, so
    /// input never goes to more than one of them: the one with a gate at
    /// `point`, else those without gates, else the one with the nearest
    /// gate.
    fn route(&self, pos: Position, point: Option<(i32, i32)>) -> Option<Vec<CaptureHandle>> {
        let ids = self.position_map.get(&pos).cloned().unwrap_or_default();
        let (gated, ungated): (Vec<_>, Vec<_>) =
            ids.into_iter().partition(|id| self.gates.contains_key(id));
        if gated.is_empty() {
            return None;
        }
        let gates = |id: &CaptureHandle| self.gates[id].iter();
        if let Some(point) = point {
            if let Some(&id) = gated
                .iter()
                .find(|id| gates(id).any(|g| g.contains(pos, point)))
            {
                return Some(vec![id]);
            }
        }
        if !ungated.is_empty() {
            return Some(ungated);
        }
        let along = |(x, y): (i32, i32)| match pos {
            Position::Left | Position::Right => y,
            Position::Top | Position::Bottom => x,
        };
        let distance = |id: &CaptureHandle| {
            let Some(point) = point else { return 0 };
            gates(id)
                .map(|g| {
                    (g.from - along(point))
                        .max(along(point) - (g.to - 1))
                        .max(0)
                })
                .min()
                .unwrap_or(i32::MAX)
        };
        gated.into_iter().min_by_key(distance).map(|id| vec![id])
    }

    /// destroy the client with the given id, if it exists
    pub async fn destroy(&mut self, id: CaptureHandle) -> Result<(), CaptureError> {
        let pos = self
            .id_map
            .remove(&id)
            .expect("no position for this handle");

        log::debug!("destroying capture {id} @ {pos}");
        let remaining = self.position_map.get_mut(&pos).expect("id vector");
        remaining.retain(|&i| i != id);

        log::debug!("remaining ids @ {pos}: {remaining:?}");
        self.gates.remove(&id);
        if remaining.is_empty() {
            log::debug!("destroying capture @ {pos} - no remaining ids");
            self.position_map.remove(&pos);
            self.capture.destroy(pos).await?;
        } else {
            self.update_gates(pos);
        }
        Ok(())
    }

    /// release mouse
    pub async fn release(&mut self) -> Result<(), CaptureError> {
        self.pressed_keys.clear();
        self.routed = None;
        self.capture.release().await
    }

    /// Drain and return every key the capture has forwarded as
    /// down-but-not-up. The caller is expected to synthesize key-up
    /// events to the remote peer for each — otherwise the peer
    /// retains phantom-held keys after capture is released. The
    /// canonical case is the release-bind chord
    /// (Ctrl+Shift+Alt+Meta): the down events were sent while
    /// capture was active, but the matching up events arrive after
    /// the local tap has flipped to passthrough and never reach
    /// the peer.
    pub fn take_pressed_keys(&mut self) -> HashSet<scancode::Linux> {
        std::mem::take(&mut self.pressed_keys)
    }

    /// destroy the input capture
    pub async fn terminate(&mut self) -> Result<(), CaptureError> {
        self.capture.terminate().await
    }

    /// creates a new [`InputCapture`]
    pub async fn new(backend: Option<Backend>) -> Result<Self, CaptureCreationError> {
        let capture = create(backend).await?;
        Ok(Self {
            capture,
            id_map: Default::default(),
            pending: Default::default(),
            position_map: Default::default(),
            pressed_keys: HashSet::new(),
            gates: Default::default(),
            routed: None,
        })
    }

    /// check whether the given keys are pressed
    pub fn keys_pressed(&self, keys: &[scancode::Linux]) -> bool {
        keys.iter().all(|k| self.pressed_keys.contains(k))
    }

    fn update_pressed_keys(&mut self, key: u32, state: u8) {
        if let Ok(scancode) = scancode::Linux::try_from(key) {
            log::debug!("key: {key}, state: {state}, scancode: {scancode:?}");
            match state {
                1 => self.pressed_keys.insert(scancode),
                _ => self.pressed_keys.remove(&scancode),
            };
        }
    }
}

impl Stream for InputCapture {
    type Item = Result<(CaptureHandle, CaptureEvent), CaptureError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        if let Some(e) = self.pending.pop_front() {
            return Poll::Ready(Some(Ok(e)));
        }

        // ready
        let event = ready!(self.capture.poll_next_unpin(cx));

        // stream closed
        let event = match event {
            Some(e) => e,
            None => return Poll::Ready(None),
        };

        // error occurred
        let (pos, event) = match event {
            Ok(e) => e,
            Err(e) => return Poll::Ready(Some(Err(e))),
        };

        // handle key presses
        if let CaptureEvent::Input(Event::Keyboard(KeyboardEvent::Key { key, state, .. })) = event {
            self.update_pressed_keys(key, state);
        }

        match event {
            CaptureEvent::BeginAt(point) => {
                self.routed = self.route(pos, Some(point)).map(|ids| (pos, ids))
            }
            CaptureEvent::Begin => self.routed = self.route(pos, None).map(|ids| (pos, ids)),
            CaptureEvent::Input(_) => {}
        }
        let ids = match &self.routed {
            Some((routed_pos, ids)) if *routed_pos == pos => ids.clone(),
            _ => self.position_map.get(&pos).cloned().unwrap_or_default(),
        };

        match ids.as_slice() {
            [] => Poll::Pending,
            [id] => Poll::Ready(Some(Ok((*id, event)))),
            [first, rest @ ..] => {
                self.pending.extend(rest.iter().map(|&id| (id, event)));
                Poll::Ready(Some(Ok((*first, event))))
            }
        }
    }
}

#[async_trait]
trait Capture: Stream<Item = Result<(Position, CaptureEvent), CaptureError>> + Unpin {
    /// create a new client with the given id
    async fn create(&mut self, pos: Position) -> Result<(), CaptureError>;

    /// destroy the client with the given id, if it exists
    async fn destroy(&mut self, pos: Position) -> Result<(), CaptureError>;

    /// release mouse
    async fn release(&mut self) -> Result<(), CaptureError>;

    /// let the pointer leave at `pos` only through `gates`, anywhere with
    /// `None`
    fn set_gates(&mut self, _pos: Position, _gates: Option<Vec<Gate>>) {}

    /// destroy the input capture
    async fn terminate(&mut self) -> Result<(), CaptureError>;
}

async fn create_backend(
    backend: Backend,
) -> Result<
    Box<dyn Capture<Item = Result<(Position, CaptureEvent), CaptureError>>>,
    CaptureCreationError,
> {
    match backend {
        #[cfg(libei)]
        Backend::InputCapturePortal => Ok(Box::new(libei::LibeiInputCapture::new().await?)),
        #[cfg(layer_shell)]
        Backend::LayerShell => Ok(Box::new(layer_shell::LayerShellInputCapture::new()?)),
        #[cfg(x11)]
        Backend::X11 => Ok(Box::new(x11::X11InputCapture::new()?)),
        #[cfg(windows)]
        Backend::Windows => Ok(Box::new(windows::WindowsInputCapture::new())),
        #[cfg(target_os = "macos")]
        Backend::MacOs => Ok(Box::new(macos::MacOSInputCapture::new().await?)),
        Backend::Dummy => Ok(Box::new(dummy::DummyInputCapture::new())),
    }
}

async fn create(
    backend: Option<Backend>,
) -> Result<
    Box<dyn Capture<Item = Result<(Position, CaptureEvent), CaptureError>>>,
    CaptureCreationError,
> {
    if let Some(backend) = backend {
        let b = create_backend(backend).await;
        if b.is_ok() {
            log::info!("using capture backend: {backend}");
        }
        return b;
    }

    for backend in [
        #[cfg(libei)]
        Backend::InputCapturePortal,
        #[cfg(layer_shell)]
        Backend::LayerShell,
        #[cfg(x11)]
        Backend::X11,
        #[cfg(windows)]
        Backend::Windows,
        #[cfg(target_os = "macos")]
        Backend::MacOs,
    ] {
        match create_backend(backend).await {
            Ok(b) => {
                log::info!("using capture backend: {backend}");
                return Ok(b);
            }
            Err(e) if e.cancelled_by_user() => return Err(e),
            Err(e) => log::warn!("{backend} input capture backend unavailable: {e}"),
        }
    }
    Err(CaptureCreationError::NoAvailableBackend)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// two devices sharing the top edge: 1 on x 0..700, 2 on x 700..1400
    fn shared_top() -> InputCapture {
        let gate = |from, to| Gate {
            side: Position::Top,
            at: 0,
            from,
            to,
        };
        InputCapture {
            capture: Box::new(dummy::DummyInputCapture::new()),
            pressed_keys: HashSet::new(),
            position_map: HashMap::from([(Position::Top, vec![1, 2])]),
            id_map: HashMap::from([(1, Position::Top), (2, Position::Top)]),
            pending: VecDeque::new(),
            gates: HashMap::from([(1, vec![gate(0, 700)]), (2, vec![gate(700, 1400)])]),
            routed: None,
        }
    }

    /// the dummy backend needs a runtime to be created in
    fn with_runtime(test: impl FnOnce()) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let _guard = rt.enter();
        test();
    }

    #[test]
    fn capture_goes_to_the_device_whose_gate_it_is() {
        with_runtime(capture_goes_to_the_device_whose_gate_it_is_);
    }

    fn capture_goes_to_the_device_whose_gate_it_is_() {
        let capture = shared_top();
        assert_eq!(capture.route(Position::Top, Some((100, 0))), Some(vec![1]));
        assert_eq!(capture.route(Position::Top, Some((900, 1))), Some(vec![2]));
    }

    #[test]
    fn shared_edge_never_goes_to_both_devices() {
        with_runtime(shared_edge_never_goes_to_both_devices_);
    }

    fn shared_edge_never_goes_to_both_devices_() {
        let mut capture = shared_top();
        // outside both gates: the nearest one only
        assert_eq!(capture.route(Position::Top, Some((1500, 0))), Some(vec![2]));
        // no point from the backend: one of them
        assert_eq!(
            capture.route(Position::Top, None).map(|ids| ids.len()),
            Some(1)
        );
        // a device without gates yet does not get the other's input
        capture.gates.remove(&2);
        assert_eq!(capture.route(Position::Top, Some((100, 0))), Some(vec![1]));
    }

    #[test]
    fn edges_without_gates_go_to_everyone_there() {
        with_runtime(edges_without_gates_go_to_everyone_there_);
    }

    fn edges_without_gates_go_to_everyone_there_() {
        let mut capture = shared_top();
        capture.gates.clear();
        assert_eq!(capture.route(Position::Top, Some((100, 0))), None);
    }
}

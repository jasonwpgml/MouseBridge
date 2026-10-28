use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    time::{Duration, Instant},
};

use futures::StreamExt;
use input_capture::{
    CaptureError, CaptureEvent, CaptureHandle, Gate, InputCapture, InputCaptureError,
    KeyboardCapture, Position,
};
use input_event::{Event, KeyboardEvent, PointerEvent, scancode};
use lan_mouse_proto::ProtoEvent;
use local_channel::mpsc::{Receiver, Sender, channel};
use tokio::task::{JoinHandle, spawn_local};
use tokio_util::sync::CancellationToken;

use crate::{connect::LanMouseConnection, key_remap::KeyRemap, layout::Portal};

pub(crate) struct Capture {
    cancellation_token: CancellationToken,
    request_tx: Sender<CaptureRequest>,
    task: JoinHandle<()>,
    event_rx: Receiver<ICaptureEvent>,
}

pub(crate) enum ICaptureEvent {
    /// a client was entered
    CaptureBegin(CaptureHandle),
    /// capture disabled
    CaptureDisabled,
    /// capture disabled
    CaptureEnabled,
    /// A (new) client was entered.
    /// In contrast to [`ICaptureEvent::CaptureBegin`] this
    /// event is only triggered when the capture was
    /// explicitly released in the meantime by
    /// either the remote client leaving its device region,
    /// a new device entering the screen or the release bind.
    ClientEntered(u64),
    /// The previously active client was left, i.e. capture
    /// was released for the given handle. Mirrors
    /// [`ICaptureEvent::ClientEntered`] for the leave side
    /// and fires on every release path (release-bind chord,
    /// remote `Leave`, explicit `Release` request, send
    /// failure, or destroy of the active capture).
    ClientLeft(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CaptureType {
    /// a normal input capture
    Default,
    /// A capture only interested in [`CaptureEvent::Begin`] events.
    /// The capture is released immediately, if there is no
    /// Default capture at the same position.
    EnterOnly,
}

#[derive(Clone, Debug)]
enum CaptureRequest {
    /// capture must release the mouse
    Release,
    /// add a capture client
    Create(CaptureHandle, Position, CaptureType),
    /// destory a capture client
    Destroy(CaptureHandle),
    /// reenable input capture
    Reenable,
    /// set release bind
    SetReleaseBind(Vec<scancode::Linux>),
    /// set key remapping for events sent to a client
    SetKeyRemap(CaptureHandle, Option<KeyRemap>),
    /// choose where keyboard input goes
    SetKeyboard(KeyboardRoute),
    /// let the pointer reach a client only through these passages, or
    /// anywhere along its edge with `None`
    SetPassages(CaptureHandle, Option<Vec<Passage>>),
    /// a mouse button was pressed on this device
    LocalClick,
    /// a mouse button was pressed on this client
    ClientClicked(CaptureHandle),
}

/// A portal to a client and the gate it is in this device's pointer
/// coordinates.
#[derive(Clone, Debug)]
pub(crate) struct Passage {
    pub gate: Gate,
    pub portal: Portal,
    /// pointer units per device-independent unit on the portal's display, in
    /// percent
    pub pointer_scale: i32,
}

/// where keyboard input goes
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum KeyboardRoute {
    /// to the client the pointer is on
    #[default]
    FollowPointer,
    /// nowhere, keys stay on this device
    Local,
    /// always to this client
    Client(CaptureHandle),
    /// to the device last clicked on: this one with `None`
    LastClicked(Option<CaptureHandle>),
}

impl Capture {
    pub(crate) fn new(
        backend: Option<input_capture::Backend>,
        conn: LanMouseConnection,
        release_bind: Vec<scancode::Linux>,
    ) -> Self {
        let (request_tx, request_rx) = channel();
        let (event_tx, event_rx) = channel();
        let cancellation_token = CancellationToken::new();
        let capture_task = CaptureTask {
            active_client: None,
            backend,
            cancellation_token: cancellation_token.clone(),
            captures: Default::default(),
            conn,
            event_tx,
            key_remaps: Default::default(),
            keyboard: Default::default(),
            passages: Default::default(),
            ack_deadline: None,
            request_rx,
            release_bind: Rc::new(RefCell::new(release_bind)),
            state: Default::default(),
        };
        let task = spawn_local(capture_task.run());
        Self {
            cancellation_token,
            request_tx,
            task,
            event_rx,
        }
    }

    pub(crate) fn reenable(&self) {
        self.request_tx
            .send(CaptureRequest::Reenable)
            .expect("channel closed");
    }

    pub(crate) async fn terminate(&mut self) {
        self.cancellation_token.cancel();
        log::debug!("terminating capture");
        if let Err(e) = (&mut self.task).await {
            log::warn!("{e}");
        }
    }

    pub(crate) fn create(
        &self,
        handle: CaptureHandle,
        pos: lan_mouse_ipc::Position,
        capture_type: CaptureType,
    ) {
        let pos = to_capture_pos(pos);
        self.request_tx
            .send(CaptureRequest::Create(handle, pos, capture_type))
            .expect("channel closed");
    }

    pub(crate) fn destroy(&self, handle: CaptureHandle) {
        self.request_tx
            .send(CaptureRequest::Destroy(handle))
            .expect("channel closed");
    }

    pub(crate) fn release(&self) {
        self.request_tx
            .send(CaptureRequest::Release)
            .expect("channel closed");
    }

    pub(crate) async fn event(&mut self) -> ICaptureEvent {
        self.event_rx.recv().await.expect("channel closed")
    }

    pub(crate) fn set_release_bind(&mut self, bind: Vec<scancode::Linux>) {
        let _ = self.request_tx.send(CaptureRequest::SetReleaseBind(bind));
    }

    pub(crate) fn set_key_remap(&self, handle: CaptureHandle, remap: Option<KeyRemap>) {
        let _ = self
            .request_tx
            .send(CaptureRequest::SetKeyRemap(handle, remap));
    }

    pub(crate) fn set_keyboard(&self, route: KeyboardRoute) {
        let _ = self.request_tx.send(CaptureRequest::SetKeyboard(route));
    }

    pub(crate) fn local_click(&self) {
        let _ = self.request_tx.send(CaptureRequest::LocalClick);
    }

    pub(crate) fn client_clicked(&self, handle: CaptureHandle) {
        let _ = self.request_tx.send(CaptureRequest::ClientClicked(handle));
    }

    pub(crate) fn set_passages(&self, handle: CaptureHandle, passages: Option<Vec<Passage>>) {
        let _ = self
            .request_tx
            .send(CaptureRequest::SetPassages(handle, passages));
    }
}

/// debounce a statement `$st`, i.e. the statement is executed only if the
/// time since the previous execution is at least `$dur`.
/// `$prev` is used to keep track of this timestamp
macro_rules! debounce {
    ($prev:ident, $dur:expr, $st:stmt) => {
        let exec = match $prev.get() {
            None => true,
            Some(instant) if instant.elapsed() > $dur => true,
            _ => false,
        };
        if exec {
            $prev.replace(Some(Instant::now()));
            $st
        }
    };
}

struct CaptureTask {
    active_client: Option<CaptureHandle>,
    backend: Option<input_capture::Backend>,
    cancellation_token: CancellationToken,
    captures: Vec<(CaptureHandle, Position, CaptureType)>,
    conn: LanMouseConnection,
    event_tx: Sender<ICaptureEvent>,
    key_remaps: HashMap<CaptureHandle, KeyRemap>,
    keyboard: KeyboardRoute,
    passages: HashMap<CaptureHandle, Vec<Passage>>,
    /// when to give up on a client that has not acknowledged entering it
    ack_deadline: Option<Instant>,
    release_bind: Rc<RefCell<Vec<scancode::Linux>>>,
    request_rx: Receiver<CaptureRequest>,
    state: State,
}

impl CaptureTask {
    fn add_capture(&mut self, handle: CaptureHandle, pos: Position, capture_type: CaptureType) {
        self.captures.push((handle, pos, capture_type));
        self.apply_keyboard();
    }

    fn remove_capture(&mut self, handle: CaptureHandle) {
        self.captures.retain(|&(h, ..)| handle != h);
        self.key_remaps.remove(&handle);
        self.apply_keyboard();
    }

    fn set_keyboard(&mut self, route: KeyboardRoute) {
        // asking for keys to follow clicks again keeps the last click
        if !matches!(
            (self.keyboard, route),
            (
                KeyboardRoute::LastClicked(_),
                KeyboardRoute::LastClicked(None)
            )
        ) {
            self.keyboard = route;
        }
        self.apply_keyboard();
    }

    /// tells the capture backend which keys to take, which for a client
    /// keys are locked to depends on where its capture is
    fn apply_keyboard(&self) {
        let capture = match self.keyboard {
            KeyboardRoute::FollowPointer => KeyboardCapture::FollowPointer,
            KeyboardRoute::Local => KeyboardCapture::Local,
            KeyboardRoute::Client(target) => self
                .captures
                .iter()
                .find(|&&(h, ..)| h == target)
                .map_or(KeyboardCapture::FollowPointer, |&(_, pos, _)| {
                    KeyboardCapture::Always(pos)
                }),
            KeyboardRoute::LastClicked(None) => KeyboardCapture::Local,
            KeyboardRoute::LastClicked(Some(target)) => self
                .captures
                .iter()
                .find(|&&(h, ..)| h == target)
                .map_or(KeyboardCapture::Local, |&(_, pos, _)| {
                    KeyboardCapture::Always(pos)
                }),
        };
        input_capture::set_keyboard_capture(capture);
    }

    /// keys follow a click to the device it was on, this one with `None`
    fn clicked(&mut self, at: Option<CaptureHandle>) {
        if let KeyboardRoute::LastClicked(last) = self.keyboard {
            if last != at {
                log::info!("keyboard follows the click to {at:?}");
                self.keyboard = KeyboardRoute::LastClicked(at);
                self.apply_keyboard();
            }
        }
    }

    /// tells the clients this device was clicked, for those whose keys
    /// follow clicks
    async fn local_click(&mut self) {
        self.clicked(None);
        let clients = self
            .captures
            .iter()
            .filter(|&&(_, _, t)| t == CaptureType::Default)
            .map(|&(h, ..)| h)
            .collect::<Vec<_>>();
        for handle in clients {
            if let Err(e) = self.conn.send(ProtoEvent::Clicked, handle).await {
                log::debug!("click notification to client {handle} failed: {e}");
            }
        }
    }

    fn set_passages(&mut self, handle: CaptureHandle, passages: Option<Vec<Passage>>) {
        match passages {
            Some(passages) => self.passages.insert(handle, passages),
            None => self.passages.remove(&handle),
        };
    }

    fn gates(&self, handle: CaptureHandle) -> Option<Vec<Gate>> {
        let passages = self.passages.get(&handle)?;
        Some(passages.iter().map(|p| p.gate).collect())
    }

    /// where the pointer enters the client, having left at `point` in
    /// pointer coordinates
    fn entry_point(&self, handle: CaptureHandle, point: (i32, i32)) -> Option<(i32, i32)> {
        let side = self.get_pos(handle);
        let passage = self
            .passages
            .get(&handle)?
            .iter()
            .find(|p| p.gate.contains(side, point))?;
        let scale = passage.pointer_scale.max(1);
        let point = (point.0 * 100 / scale, point.1 * 100 / scale);
        Some(passage.portal.entry_point(point))
    }

    fn set_key_remap(&mut self, handle: CaptureHandle, remap: Option<KeyRemap>) {
        match remap {
            Some(remap) => self.key_remaps.insert(handle, remap),
            None => self.key_remaps.remove(&handle),
        };
    }

    fn remap(&self, handle: CaptureHandle, event: Event) -> Event {
        match self.key_remaps.get(&handle) {
            Some(remap) => remap.apply(event),
            None => event,
        }
    }

    fn is_default_capture_at(&self, pos: Position) -> bool {
        self.captures
            .iter()
            .any(|&(_, p, t)| p == pos && t == CaptureType::Default)
    }

    fn get_pos(&self, handle: CaptureHandle) -> Position {
        self.captures
            .iter()
            .find(|(h, ..)| *h == handle)
            .expect("no such capture")
            .1
    }

    fn get_type(&self, handle: CaptureHandle) -> CaptureType {
        self.captures
            .iter()
            .find(|(h, ..)| *h == handle)
            .expect("no such capture")
            .2
    }

    async fn run(mut self) {
        loop {
            if let Err(e) = self.do_capture().await {
                log::warn!("input capture exited: {e}");
            }
            loop {
                tokio::select! {
                    r = self.request_rx.recv() => match r.expect("channel closed") {
                        CaptureRequest::Reenable => break,
                        CaptureRequest::Create(h, p, t) => self.add_capture(h, p, t),
                        CaptureRequest::Destroy(h) => self.remove_capture(h),
                        CaptureRequest::Release => { /* nothing to do */ }
                        CaptureRequest::SetReleaseBind(bind) => {
                            self.release_bind.borrow_mut().clone_from(&bind);
                        }
                        CaptureRequest::SetKeyRemap(h, r) => self.set_key_remap(h, r),
                        CaptureRequest::SetKeyboard(route) => self.set_keyboard(route),
                        CaptureRequest::SetPassages(h, p) => self.set_passages(h, p),
                        CaptureRequest::LocalClick => self.local_click().await,
                        CaptureRequest::ClientClicked(h) => self.clicked(Some(h)),
                    },
                    _ = self.cancellation_token.cancelled() => return,
                }
            }
        }
    }

    async fn do_capture(&mut self) -> Result<(), InputCaptureError> {
        /* allow cancelling capture request */
        let mut capture = tokio::select! {
            r = InputCapture::new(self.backend) => r?,
            _ = self.cancellation_token.cancelled() => return Ok(()),
        };

        let _capture_guard = DropGuard::new(
            self.event_tx.clone(),
            ICaptureEvent::CaptureEnabled,
            ICaptureEvent::CaptureDisabled,
        );

        /* create barriers for active clients */
        let r = self.create_captures(&mut capture).await;
        if let Err(e) = r {
            capture.terminate().await?;
            return Err(e.into());
        }

        let r = self.do_capture_session(&mut capture).await;

        // FIXME replace with async drop when stabilized
        capture.terminate().await?;

        r
    }

    async fn create_captures(&mut self, capture: &mut InputCapture) -> Result<(), CaptureError> {
        let captures = self.captures.clone();
        for (handle, pos, _type) in captures {
            tokio::select! {
                r = capture.create(handle, pos) => r?,
                _ = self.cancellation_token.cancelled() => return Ok(()),
            }
            capture.set_gates(handle, self.gates(handle));
        }
        Ok(())
    }

    async fn do_capture_session(
        &mut self,
        capture: &mut InputCapture,
    ) -> Result<(), InputCaptureError> {
        loop {
            tokio::select! {
                event = capture.next() => match event {
                    Some(event) => self.handle_capture_event(capture, event?).await?,
                    None => return Ok(()),
                },
                // input stays captured until the client acknowledges; never
                // wait for that forever
                _ = until(self.ack_deadline), if self.ack_deadline.is_some() => {
                    log::warn!("releasing capture: client did not acknowledge within {ACK_TIMEOUT:?}");
                    self.release_capture(capture).await?;
                }
                (handle, event) = self.conn.recv() => {
                    if let Some(active) = self.active_client {
                        if handle != active {
                            // we only care about events coming from the client we are currently connected to
                            // only `Ack` and `Leave` are relevant
                            continue
                        }
                    }

                    match event {
                        // connection acknowlegded => set state to Sending
                        ProtoEvent::Ack(_) => {
                            log::info!("client {handle} acknowledged the connection!");
                            self.state = State::Sending;
                            self.ack_deadline = None;
                        }
                        // client disconnected
                        ProtoEvent::Leave(_) => {
                            log::info!("releasing capture: left remote client device region");
                            self.release_capture(capture).await?;
                        },
                        _ => {}
                    }
                },
                e = self.request_rx.recv() => match e.expect("channel closed") {
                    CaptureRequest::Reenable => { /* already active */ },
                    CaptureRequest::Release => self.release_capture(capture).await?,
                    CaptureRequest::Create(h, p, t) => {
                        self.add_capture(h, p, t);
                        capture.create(h, p).await?;
                        capture.set_gates(h, self.gates(h));
                    }
                    CaptureRequest::Destroy(h) => {
                        // If the capture we're tearing down is the
                        // currently-active one, treat this as a
                        // release for hook purposes. The release_capture
                        // path also clears active_client and flushes
                        // pressed-key state to the peer; without this,
                        // `cli deactivate` (or a hostname change
                        // re-creating the client) would skip leave_hook.
                        if self.active_client == Some(h) {
                            self.release_capture(capture).await?;
                        }
                        self.remove_capture(h);
                        capture.destroy(h).await?;
                    }
                    CaptureRequest::SetReleaseBind(bind) => {
                        self.release_bind.borrow_mut().clone_from(&bind);
                    }
                    CaptureRequest::SetKeyRemap(h, r) => self.set_key_remap(h, r),
                    CaptureRequest::SetKeyboard(route) => self.set_keyboard(route),
                    CaptureRequest::SetPassages(h, p) => {
                        self.set_passages(h, p);
                        capture.set_gates(h, self.gates(h));
                    }
                    CaptureRequest::LocalClick => self.local_click().await,
                    CaptureRequest::ClientClicked(h) => self.clicked(Some(h)),
                },
                _ = self.cancellation_token.cancelled() => break,
            }
        }
        Ok(())
    }

    async fn handle_capture_event(
        &mut self,
        capture: &mut InputCapture,
        event: (CaptureHandle, CaptureEvent),
    ) -> Result<(), CaptureError> {
        let (handle, event) = event;
        log::trace!("({handle}): {event:?}");

        if capture.keys_pressed(&self.release_bind.borrow()) {
            log::info!("releasing capture: release-bind pressed");
            return self.release_capture(capture).await;
        }

        // a click on a client takes the keys there
        if let CaptureEvent::Input(Event::Pointer(PointerEvent::Button { state: 1, .. })) = event {
            if self.get_type(handle) == CaptureType::Default {
                self.clicked(Some(handle));
            }
        }

        // keys locked to one client go there, wherever the pointer is
        let locked = match self.keyboard {
            KeyboardRoute::Client(target) | KeyboardRoute::LastClicked(Some(target)) => {
                Some(target)
            }
            _ => None,
        };
        if let (Some(target), CaptureEvent::Input(e @ Event::Keyboard(_))) = (locked, event) {
            // every capture at a position may get a copy of its events; send
            // once
            if handle == target {
                let event = ProtoEvent::Input(self.remap(target, e));
                if let Err(e) = self.conn.send(event, target).await {
                    const DUR: Duration = Duration::from_millis(500);
                    debounce!(
                        PREV_KEY_LOG,
                        DUR,
                        log::warn!("keys for client {target}: {e}")
                    );
                }
            }
            return Ok(());
        }

        let begin = matches!(event, CaptureEvent::Begin | CaptureEvent::BeginAt(_));
        if begin {
            self.event_tx
                .send(ICaptureEvent::CaptureBegin(handle))
                .expect("channel closed");
        }

        // enter only capture (for incoming connections)
        if self.get_type(handle) == CaptureType::EnterOnly {
            // if there is no active outgoing connection at the current capture,
            // we release the capture
            if !self.is_default_capture_at(self.get_pos(handle)) {
                log::info!("releasing capture: no active client at this position");
                capture.release().await?;
            }
            // we dont care about events from incoming handles except for releasing the capture
            return Ok(());
        }

        // activated a new client
        if begin && Some(handle) != self.active_client {
            self.state = State::WaitingForAck;
            self.ack_deadline = Some(Instant::now() + ACK_TIMEOUT);
            self.active_client.replace(handle);
            self.event_tx
                .send(ICaptureEvent::ClientEntered(handle))
                .expect("channel closed");
        }

        let opposite_pos = to_proto_pos(self.get_pos(handle).opposite());

        let entry = match event {
            CaptureEvent::BeginAt(point) => self.entry_point(handle, point),
            _ => None,
        };
        let event = match event {
            CaptureEvent::Begin | CaptureEvent::BeginAt(_) => ProtoEvent::Enter(opposite_pos),
            CaptureEvent::Input(e) => match self.state {
                // connection not acknowledged, repeat `Enter` event
                State::WaitingForAck => ProtoEvent::Enter(opposite_pos),
                State::Sending => ProtoEvent::Input(self.remap(handle, e)),
            },
        };

        let mut sent = self.conn.send(event, handle).await;
        if let (Ok(()), Some((x, y))) = (&sent, entry) {
            log::debug!("entering client {handle} at {x},{y}");
            sent = self.conn.send(ProtoEvent::EnterAt { x, y }, handle).await;
        }
        if let Err(e) = sent {
            const DUR: Duration = Duration::from_millis(500);
            debounce!(PREV_LOG, DUR, log::warn!("releasing capture: {e}"));
            // Funnel through release_capture so the leave_hook
            // fires and active_client is cleared (without this the
            // active_client field would stay stale until the next
            // Begin from a different handle).
            self.release_capture(capture).await?;
        }
        Ok(())
    }

    async fn release_capture(&mut self, capture: &mut InputCapture) -> Result<(), CaptureError> {
        self.ack_deadline = None;
        // If we have an active client, notify them we're leaving
        if let Some(handle) = self.active_client.take() {
            // Surface the leave to the service layer so it can fire
            // the per-client leave_hook. Sent before the network
            // teardown below so we never race against the peer
            // disappearing.
            self.event_tx
                .send(ICaptureEvent::ClientLeft(handle))
                .expect("channel closed");
            // Synthesize key-up events for every key still held in the
            // capture's pressed_keys set BEFORE sending Leave. Without
            // this, pressing the release-bind chord (typically all four
            // modifiers) leaves the peer with phantom held modifiers:
            // the down events were forwarded while capture was active,
            // but the matching up events arrive after the local tap
            // flips to passthrough and never reach the peer. The peer
            // then runs every subsequent keystroke through those held
            // mods until its watchdog times out (1+ s) or our Leave
            // arrives — and Leave can be lost over UDP/DTLS.
            // Remapped like regular input so the peer releases the key it saw pressed.
            for key in capture.take_pressed_keys() {
                let key_up = Event::Keyboard(KeyboardEvent::Key {
                    time: 0,
                    key: key as u32,
                    state: 0,
                });
                let key_up = ProtoEvent::Input(self.remap(handle, key_up));
                if let Err(e) = self.conn.send(key_up, handle).await {
                    log::warn!("failed to send key-up to client {handle}: {e}");
                }
            }
            // Reset the modifier mask too. The peer's input-emulation
            // layer keeps a separate XKB-style modifier state that's
            // updated by KeyboardEvent::Modifiers, distinct from the
            // pressed_keys set drained above. Without this, an
            // already-locked CapsLock would survive the release.
            let mods_zero = ProtoEvent::Input(Event::Keyboard(KeyboardEvent::Modifiers {
                depressed: 0,
                latched: 0,
                locked: 0,
                group: 0,
            }));
            if let Err(e) = self.conn.send(mods_zero, handle).await {
                log::warn!("failed to reset modifiers on client {handle}: {e}");
            }

            log::info!("sending Leave event to client {handle}");
            if let Err(e) = self.conn.send(ProtoEvent::Leave(0), handle).await {
                log::warn!("failed to send Leave to client {handle}: {e}");
            }
        }
        capture.release().await
    }
}

thread_local! {
    static PREV_LOG: Cell<Option<Instant>> = const { Cell::new(None) };
    static PREV_KEY_LOG: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// how long a client may take to acknowledge that the pointer entered it
const ACK_TIMEOUT: Duration = Duration::from_secs(1);

async fn until(deadline: Option<Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at.into()).await,
        None => std::future::pending().await,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum State {
    #[default]
    WaitingForAck,
    Sending,
}

pub(crate) fn to_capture_pos(pos: lan_mouse_ipc::Position) -> input_capture::Position {
    match pos {
        lan_mouse_ipc::Position::Left => input_capture::Position::Left,
        lan_mouse_ipc::Position::Right => input_capture::Position::Right,
        lan_mouse_ipc::Position::Top => input_capture::Position::Top,
        lan_mouse_ipc::Position::Bottom => input_capture::Position::Bottom,
    }
}

fn to_proto_pos(pos: input_capture::Position) -> lan_mouse_proto::Position {
    match pos {
        input_capture::Position::Left => lan_mouse_proto::Position::Left,
        input_capture::Position::Right => lan_mouse_proto::Position::Right,
        input_capture::Position::Top => lan_mouse_proto::Position::Top,
        input_capture::Position::Bottom => lan_mouse_proto::Position::Bottom,
    }
}

struct DropGuard<T> {
    tx: Sender<T>,
    on_drop: Option<T>,
}

impl<T> DropGuard<T> {
    fn new(tx: Sender<T>, on_new: T, on_drop: T) -> Self {
        tx.send(on_new).expect("channel closed");
        let on_drop = Some(on_drop);
        Self { tx, on_drop }
    }
}

impl<T> Drop for DropGuard<T> {
    fn drop(&mut self) {
        self.tx
            .send(self.on_drop.take().expect("item"))
            .expect("channel closed");
    }
}

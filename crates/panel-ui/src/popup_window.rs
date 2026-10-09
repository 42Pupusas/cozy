//! A `zwlr_layer_shell_v1` popup window.
//!
//! A small, top-anchored overlay surface placed at a caller-chosen x
//! offset — the shared window every statusbar panel (Wi-Fi, Bluetooth,
//! volume) opens below its bar module.
//!
//! Structurally this is `LauncherWindow`'s sibling — same
//! `platform::surface::SurfaceBootstrap` bootstrap and
//! `platform::present::PresentState` swap chain — with two differences:
//!   * **anchored top-left**: the caller passes the bar-relative x (the
//!     clicked module's position) as the left margin, so the popup drops
//!     straight down from that module;
//!   * **no fixed size**: the caller passes width/height, chosen per panel
//!     from its content.

use crate::placement::{Anchored, ModuleAnchor};
use platform::conn::{Sender, WindowSender};
use platform::draw::{Canvas, Point};
use platform::event::RawEvent;
use platform::id::{CompositorId, KeyboardId, LayerShellId, OutputId, RegistryId, SeatId, ShmId};
use platform::keyboard_decoder::{KeyInput, KeyboardDecoder, KeyboardUpdate};
use platform::layer::{LayerSurfaceConfig, LayerSurfaceHandle};
use platform::present::{PresentAttempt, PresentState};
use platform::protocol::{
    self as proto, Anchor, Format, Key, KeyboardInteractivity, Layer, LayerSurfaceEvent,
    SeatCapability, SeatEvent, SeatRequest,
};
use platform::toplevel::keysym::Key as DecodedKey;
use platform::wire::MessageBuilder;
use platform::{PlatformError, Result};
use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc;
use quetzalcoatl::spsc::Consumer;

use platform::surface::{self, Frame};

const EVENT_RING_CAP: usize = 128;

/// Screen width assumed when the compositor advertises no usable output mode.
const DEFAULT_SCREEN_W: i32 = 1920;
const APP_ID: &str = "statusbar-panel";

/// Top margin: the bar's own thickness, so the popup's top edge lands
/// exactly at the bar's bottom edge. The caller passes the real value;
/// this default matches the bar config's `height = 28` default.
pub const BAR_HEIGHT_DEFAULT: i32 = 28;

/// One resolved keyboard event a panel acts on: text input, navigation,
/// or a request to close.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelKey {
    Char(char),
    Backspace,
    Enter,
    Esc,
    Tab,
    Up,
    Down,
    Left,
    Right,
}

impl PanelKey {
    /// Narrow a decoded [`DecodedKey`] to the subset a panel acts on —
    /// deliberately total over the keys we care about and `None` for the
    /// rest (F-keys, Insert, ...) so a panel ignores them rather than
    /// guessing a meaning. Mirrors `LauncherKey::from_key`.
    #[must_use]
    pub const fn from_key(key: Option<DecodedKey>) -> Option<Self> {
        match key {
            Some(DecodedKey::Char(c)) => Some(Self::Char(c)),
            Some(DecodedKey::Backspace) => Some(Self::Backspace),
            Some(DecodedKey::Enter) => Some(Self::Enter),
            Some(DecodedKey::Escape) => Some(Self::Esc),
            Some(DecodedKey::Tab) => Some(Self::Tab),
            Some(DecodedKey::Up) => Some(Self::Up),
            Some(DecodedKey::Down) => Some(Self::Down),
            Some(DecodedKey::Left) => Some(Self::Left),
            Some(DecodedKey::Right) => Some(Self::Right),
            _ => None,
        }
    }
}

/// Pointer state for a popup.
///
/// Position tracking, click detection, scroll accumulation —
/// `statusbar::pointer_state::PointerState`'s popup twin (public fields
/// rather than accessors, since `panel-ui` owns the whole stack here and
/// has no separate `BarWindow` to keep honest).
#[derive(Default)]
pub struct PopupPointerState {
    pub pos: Point,
    pub id: Option<platform::id::PointerId>,
    pending_scroll: Vec<i32>,
    scroll_fixed_remainder: i32,
    left_pressed: bool,
    pending_click: bool,
}

impl PopupPointerState {
    fn take_scroll(&mut self) -> Vec<i32> {
        std::mem::take(&mut self.pending_scroll)
    }

    fn take_click(&mut self) -> bool {
        std::mem::take(&mut self.pending_click)
    }

    fn on_event(&mut self, ev: &RawEvent) {
        use platform::protocol::{ButtonState, PointerButton, PointerEvent};
        match ev.event::<PointerEvent>() {
            Some(PointerEvent::Enter) => self.pos = ev.point(2, 3),
            Some(PointerEvent::Motion) => self.pos = ev.point(1, 2),
            Some(PointerEvent::Button) => {
                let button = PointerButton::from_code(ev.arg(2));
                let state = ButtonState::from_raw(ev.arg(3));
                match (button, state) {
                    (Some(PointerButton::Left), Some(ButtonState::Pressed)) => {
                        self.left_pressed = true;
                    }
                    (Some(PointerButton::Left), Some(ButtonState::Released)) => {
                        if self.left_pressed {
                            self.pending_click = true;
                        }
                        self.left_pressed = false;
                    }
                    _ => {}
                }
            }
            Some(PointerEvent::Axis) if ev.arg(1) == 0 => {
                let raw = self.scroll_fixed_remainder + ev.fixed(2).0;
                let px = raw / 256;
                self.scroll_fixed_remainder = raw - px * 256;
                if px != 0 {
                    self.pending_scroll.push(px);
                }
            }
            _ => {}
        }
    }
}

/// A top-anchored popup layer-shell window.
pub struct PopupWindow {
    sender: Sender,
    request_rx: mpsc::Consumer<platform::conn::WindowRequest>,
    window_tx: WindowSender,
    events: Consumer<RawEvent>,
    #[allow(dead_code)]
    reader: Option<std::thread::JoinHandle<Result<()>>>,
    reader_health: platform::ReaderHealth,
    wake: waker::Waker,

    #[allow(dead_code)]
    compositor: CompositorId,
    #[allow(dead_code)]
    layer_shell: LayerShellId,
    layer: LayerSurfaceHandle,

    width: i32,
    height: i32,

    seat: SeatId,
    keyboard: Option<KeyboardId>,
    keys: KeyboardDecoder,
    key_repeat: platform::key_repeat::KeyRepeatState,
    held_resolved: Option<PanelKey>,

    pointer: PopupPointerState,

    present: PresentState,
    background: platform::draw::Color,
    bg_alpha: u8,

    pending_keys: Vec<PanelKey>,
    configured: bool,
    closing: bool,
}

struct Globals {
    compositor: CompositorId,
    shm: ShmId,
    layer_shell: LayerShellId,
    seat: SeatId,
    output: OutputId,
}

#[derive(Default)]
struct GlobalsBuilder {
    compositor: Option<CompositorId>,
    shm: Option<ShmId>,
    layer_shell: Option<LayerShellId>,
    seat: Option<SeatId>,
    output: Option<OutputId>,
}

impl surface::GlobalCollector for GlobalsBuilder {
    fn is_complete(&self) -> bool {
        self.compositor.is_some()
            && self.shm.is_some()
            && self.layer_shell.is_some()
            && self.seat.is_some()
    }

    fn bind_global(
        &mut self,
        sender: &mut Sender,
        registry: RegistryId,
        name: u32,
        interface: &str,
        version: u32,
    ) -> Result<()> {
        match interface {
            s if s == proto::WL_COMPOSITOR.name => Self::bind_if_missing(
                &mut self.compositor,
                sender,
                registry,
                name,
                interface,
                version,
                proto::WL_COMPOSITOR.version,
            ),
            s if s == proto::WL_SHM.name => Self::bind_if_missing(
                &mut self.shm,
                sender,
                registry,
                name,
                interface,
                version,
                proto::WL_SHM.version,
            ),
            s if s == proto::ZWLR_LAYER_SHELL_V1.name => Self::bind_if_missing(
                &mut self.layer_shell,
                sender,
                registry,
                name,
                interface,
                version,
                proto::ZWLR_LAYER_SHELL_V1.version,
            ),
            s if s == proto::WL_SEAT.name => Self::bind_if_missing(
                &mut self.seat,
                sender,
                registry,
                name,
                interface,
                version,
                proto::WL_SEAT.version,
            ),
            // The first output is where placement measures the screen;
            // a popup's module anchor is bar-relative, so multi-output
            // bars would need output targeting -- single-display simple
            // for now, same posture as the bar's own `OutputId(0)`.
            s if s == proto::WL_OUTPUT.name => Self::bind_if_missing(
                &mut self.output,
                sender,
                registry,
                name,
                interface,
                version,
                proto::WL_OUTPUT.version,
            ),
            _ => Ok(()),
        }
    }
}

impl GlobalsBuilder {
    fn finish(self) -> Result<Globals> {
        Ok(Globals {
            compositor: self.compositor.ok_or(PlatformError::ReaderStopped)?,
            shm: self.shm.ok_or(PlatformError::ReaderStopped)?,
            layer_shell: self.layer_shell.ok_or(PlatformError::ReaderStopped)?,
            seat: self.seat.ok_or(PlatformError::ReaderStopped)?,
            // Optional in practice: a compositor with no wl_output global
            // still hosts popups; placement then needs an explicit width.
            output: self.output.unwrap_or(OutputId(0)),
        })
    }
}

impl Anchored {
    fn anchor_bits(&self) -> u32 {
        let vertical = match self.position {
            config::BarPosition::Top => Anchor::Top,
            config::BarPosition::Bottom => Anchor::Bottom,
        };
        Anchor::bits(&[vertical, Anchor::Left])
    }
}

impl Drop for PopupWindow {
    fn drop(&mut self) {
        let _ = self.layer.destroy(&mut self.sender);
    }
}

impl PopupWindow {
    /// Open a popup `width`×`height` logical pixels at the placement
    /// [`Anchored::resolve`] picked from `anchor` (the clicked module's
    /// x-span and bar thickness), floating `gap` px below the bar and
    /// from the screen edges. When `screen_w` is `None` the width is
    /// probed from the bound `wl_output`'s current mode (falling back
    /// to 1920, a common desktop width, if the output stays silent).
    /// The surface clears to `theme`'s panel surface color/alpha.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        width: i32,
        height: i32,
        anchor: ModuleAnchor,
        side: config::PanelSide,
        screen_w: Option<i32>,
        gap: i32,
        theme: &crate::widgets::Theme,
        wake: waker::Waker,
    ) -> Result<Self> {
        let (mut sender, repeat_timer, mut events, reader_handle) =
            surface::SurfaceBootstrap::connect(EVENT_RING_CAP, "panel-wayland-reader", &wake)?;

        let Globals {
            compositor,
            shm,
            layer_shell,
            seat,
            output,
        } = Self::discover(&mut sender, &mut events)?;

        // The probe reads past whatever else the compositor is sending --
        // including the once-only `wl_seat.capabilities` that grants this
        // popup its keyboard and pointer -- so its spill is replayed below,
        // after the window exists to dispatch it.
        let (screen_w, deferred) =
            Self::resolve_screen_width(&mut sender, &mut events, output, screen_w);
        let place = Anchored::resolve(anchor, side, width, screen_w, gap);

        // Same assume-1-until-configure posture as `LauncherWindow` — the
        // bar's own output scale flows in via `at_x`/`bar_height` being
        // physical already when the bar runs scaled.
        let scale = 1;
        let px_w = width * scale;
        let px_h = height * scale;

        let (frames, shm_mem, pool) =
            Frame::create_pair(&mut sender, shm, px_w, px_h, Format::Argb8888)?;

        let layer = LayerSurfaceHandle::create(
            &mut sender,
            compositor,
            layer_shell,
            &LayerSurfaceConfig {
                layer: Layer::Overlay,
                namespace: APP_ID,
                output,
                buffer_scale: Some(scale),
                anchor: Some(place.anchor_bits()),
                size: Some((width, height)),
                exclusive_zone: None,
                margin: Some(place.margin()),
                keyboard_interactivity: KeyboardInteractivity::Exclusive,
            },
        )?;

        let (req_tx, req_rx) =
            mpsc::RingBuffer::<platform::conn::WindowRequest>::new(Capacity::exact(64)).split();
        let (id_start, id_end) = sender.alloc_id_block(64);
        let window_tx = WindowSender {
            tx: req_tx,
            next_id: id_start,
            id_end,
            free_ids: Vec::new(),
        };

        let mut window = Self {
            sender,
            request_rx: req_rx,
            window_tx,
            events,
            reader: Some(reader_handle),
            reader_health: platform::ReaderHealth::new(),
            wake,
            compositor,
            layer_shell,
            layer,
            width: px_w,
            height: px_h,
            seat,
            keyboard: None,
            keys: KeyboardDecoder::new(),
            key_repeat: platform::key_repeat::KeyRepeatState::new(repeat_timer),
            held_resolved: None,
            pointer: PopupPointerState::default(),
            present: PresentState::new(frames, shm_mem, pool, Format::Argb8888, 1),
            background: theme.bg,
            bg_alpha: theme.bg_alpha,
            pending_keys: Vec::new(),
            configured: false,
            closing: false,
        };

        for ev in deferred {
            window.handle_event(&ev)?;
        }
        window
            .sender
            .drain_requests(&mut window.request_rx)
            .map_err(PlatformError::Conn)?;
        Ok(window)
    }

    pub fn request_redraw(&mut self) {
        self.present.mark_dirty();
        self.wake.signal();
    }

    pub fn take_keys(&mut self) -> Vec<PanelKey> {
        std::mem::take(&mut self.pending_keys)
    }

    pub fn wait(&self, timeout_ms: i32) {
        let _ = self.wake.wait(timeout_ms);
        self.wake.drain();
    }

    /// Block until something actually happens.
    ///
    /// Every producer signals this waker: the Wayland reader on each batch
    /// of events (and on close), and the panel's worker on each snapshot.
    /// There is nothing a timeout would discover that a signal does not,
    /// so waiting on a deadline only adds latency between the event and
    /// the repaint.
    pub fn wait_for_event(&self) {
        let _ = self.wake.wait(-1);
        self.wake.drain();
    }

    #[must_use]
    pub const fn pointer_pos(&self) -> Point {
        self.pointer.pos
    }

    /// Drain every scroll delta queued since the last call. Positive =
    /// wheel down/away. One entry per `wl_pointer.axis` event.
    pub fn take_scroll(&mut self) -> Vec<i32> {
        self.pointer.take_scroll()
    }

    /// Consume a pending left-click (press + matching release), if any.
    pub fn take_click(&mut self) -> bool {
        self.pointer.take_click()
    }

    /// Process pending events and present if dirty. Returns `false` once
    /// the surface is destroyed / the connection is lost.
    pub fn tick(&mut self, mut paint: impl FnMut(&mut Canvas)) -> Result<bool> {
        if self.closing {
            return Ok(false);
        }
        self.sender
            .drain_requests(&mut self.request_rx)
            .map_err(PlatformError::Conn)?;

        while let Some(ev) = self.events.pop() {
            self.handle_event(&ev)?;
            if self.closing {
                return Ok(false);
            }
        }

        if !self
            .reader_health
            .observe(self.events.is_closed(), self.events.is_empty())
            .is_alive()
        {
            self.closing = true;
            return Ok(false);
        }

        if self.configured
            && let PresentAttempt::Ready(slot) = self.present.begin_present()
        {
            self.do_present(slot, &mut paint);
        }

        // Everything above only *queues* onto the request ring; the socket
        // write happens here. Without this second drain the frame just
        // painted -- and the `ack_configure` / `get_keyboard` that event
        // handling queued -- would wait for the next tick's leading drain,
        // which the loop reaches only after parking in `wait`. That parks a
        // finished frame for the full poll timeout and is felt as input lag.
        self.sender
            .drain_requests(&mut self.request_rx)
            .map_err(PlatformError::Conn)?;
        Ok(true)
    }

    fn handle_event(&mut self, ev: &RawEvent) -> Result<()> {
        if let RawEvent::Fd(fds) = ev {
            self.handle_keymap_fd(fds);
            return Ok(());
        }
        if matches!(ev, RawEvent::Repeat) {
            self.handle_key_repeat_tick();
            return Ok(());
        }

        match self.present.route_common(ev, &mut self.window_tx) {
            surface::Routed::Fatal(e) => return Err(e),
            surface::Routed::Handled => return Ok(()),
            surface::Routed::Passthrough => {}
        }

        if ev.is(self.seat) && ev.event::<SeatEvent>() == Some(SeatEvent::Capabilities) {
            self.handle_seat_capabilities(ev);
            return Ok(());
        }

        if let Some(keyboard) = self.keyboard
            && ev.is(keyboard)
        {
            self.dispatch_keyboard_event(ev);
            return Ok(());
        }

        if let Some(pointer) = self.pointer.id
            && ev.is(pointer)
        {
            self.pointer.on_event(ev);
            return Ok(());
        }

        if ev.is(self.layer.layer_surface) {
            self.handle_layer_surface_event(ev);
        }
        Ok(())
    }

    fn handle_keymap_fd(&mut self, fds: &[std::os::fd::OwnedFd]) {
        self.keys.absorb_keymap_fd(fds);
    }

    fn handle_seat_capabilities(&mut self, ev: &RawEvent) {
        let caps = ev.arg(0);
        if self.keyboard.is_none() && SeatCapability::Keyboard.in_mask(caps) {
            let keyboard: KeyboardId = self.window_tx.alloc_id();
            self.window_tx
                .request(MessageBuilder::new(self.seat, SeatRequest::GetKeyboard).new_id(keyboard));
            self.keyboard = Some(keyboard);
        }
        if self.pointer.id.is_none() && SeatCapability::Pointer.in_mask(caps) {
            let pointer: platform::id::PointerId = self.window_tx.alloc_id();
            self.window_tx
                .request(MessageBuilder::new(self.seat, SeatRequest::GetPointer).new_id(pointer));
            self.pointer.id = Some(pointer);
        }
    }

    fn dispatch_keyboard_event(&mut self, ev: &RawEvent) {
        match self.keys.observe(ev) {
            KeyboardUpdate::Key(input) => self.handle_key(input),
            KeyboardUpdate::Repeat(info) => {
                self.key_repeat.set_repeat_info(info.rate, info.delay_ms);
            }
            KeyboardUpdate::Left => {
                self.held_resolved = None;
                self.key_repeat.on_leave();
            }
            KeyboardUpdate::Ignored | KeyboardUpdate::Entered | KeyboardUpdate::Modifiers(_) => {}
        }
    }

    fn handle_key(&mut self, input: KeyInput) {
        let evdev_code = input.evdev;
        if !input.is_press() {
            if let Some(k) = Key::from_code(evdev_code) {
                if self.key_repeat.held_key() == Some(k) {
                    self.held_resolved = None;
                }
                self.key_repeat.on_key_release(k);
            }
            return;
        }

        let Some(resolved) = PanelKey::from_key(input.key) else {
            return;
        };
        self.pending_keys.push(resolved);
        if let Some(k) = Key::from_code(evdev_code)
            && !k.is_modifier()
        {
            self.held_resolved = Some(resolved);
            self.key_repeat.on_key_press(k);
        }
    }

    fn handle_key_repeat_tick(&mut self) {
        if self.key_repeat.held_key().is_none() {
            return;
        }
        if let Some(resolved) = self.held_resolved {
            self.pending_keys.push(resolved);
        }
    }

    fn handle_layer_surface_event(&mut self, ev: &RawEvent) {
        if ev.event::<LayerSurfaceEvent>() == Some(LayerSurfaceEvent::Configure) {
            self.layer.ack_configure_queued(&self.window_tx, ev.arg(0));
            self.configured = true;
            self.request_redraw();
        } else if ev.event::<LayerSurfaceEvent>() == Some(LayerSurfaceEvent::Closed) {
            self.closing = true;
        }
    }

    fn do_present(&mut self, slot: usize, paint: &mut impl FnMut(&mut Canvas)) {
        let bg = self.pack_bg();
        self.present.repaint_slot(
            slot,
            (self.width, self.height),
            platform::SurfaceFormat::with_bg_alpha(self.bg_alpha),
            bg,
            paint,
        );

        surface::SurfacePresenter::commit_full_frame(
            &mut self.present,
            &mut self.window_tx,
            self.layer.surface,
            slot,
            self.width,
            self.height,
        );
    }

    fn pack_bg(&self) -> u32 {
        surface::SurfaceBootstrap::pack_bg(self.background, self.bg_alpha)
    }

    /// Settle on the screen width placement measures against, returning it
    /// alongside any events the output probe had to read past.
    ///
    /// An explicit `--screen-w` skips the probe entirely, so nothing is read
    /// from the event ring and the spill is empty.
    fn resolve_screen_width(
        sender: &mut Sender,
        events: &mut Consumer<RawEvent>,
        output: OutputId,
        requested: Option<i32>,
    ) -> (i32, Vec<RawEvent>) {
        if let Some(w) = requested {
            return (w, Vec::new());
        }
        let mut probe = surface::SurfaceBootstrap::output_probe(sender, events, output);
        let spill = probe.take_spill();
        (
            probe.logical_size().map_or(DEFAULT_SCREEN_W, |(w, _)| w),
            spill,
        )
    }

    fn discover(sender: &mut Sender, events: &mut Consumer<RawEvent>) -> Result<Globals> {
        let mut globals = GlobalsBuilder::default();
        surface::RegistryDiscovery::collect(sender, events, &mut globals)?;
        globals.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform::toplevel::keysym::sym;

    #[test]
    fn text_and_navigation_narrow_to_panel_keys() {
        assert_eq!(
            PanelKey::from_key(DecodedKey::decode(0x61, u32::from('a'))),
            Some(PanelKey::Char('a'))
        );
        assert_eq!(
            PanelKey::from_key(DecodedKey::decode(sym::UP, 0)),
            Some(PanelKey::Up)
        );
        assert_eq!(
            PanelKey::from_key(DecodedKey::decode(sym::ESCAPE, 0x1b)),
            Some(PanelKey::Esc)
        );
        assert_eq!(
            PanelKey::from_key(DecodedKey::decode(sym::TAB, 0x09)),
            Some(PanelKey::Tab)
        );
    }

    #[test]
    fn keys_carrying_a_control_codepoint_stay_actions_not_text() {
        assert_eq!(
            PanelKey::from_key(DecodedKey::decode(sym::RETURN, 0x0d)),
            Some(PanelKey::Enter)
        );
        assert_eq!(
            PanelKey::from_key(DecodedKey::decode(sym::BACKSPACE, 0x08)),
            Some(PanelKey::Backspace)
        );
    }

    #[test]
    fn irrelevant_keys_are_ignored() {
        assert_eq!(PanelKey::from_key(DecodedKey::decode(sym::F1, 0)), None);
        assert_eq!(PanelKey::from_key(None), None);
    }

    /// `wait_for_event` blocks with no timeout, so a signal that arrives
    /// between draining the rings and parking must not be lost -- that
    /// would hang the panel outright rather than merely delay it. The
    /// eventfd counts rather than edge-triggers, which is what makes the
    /// unsynchronised drain-then-park sequence safe.
    #[test]
    fn a_signal_racing_the_park_is_not_lost() {
        let wake = waker::Waker::new().expect("a waker");
        wake.signal();
        let started = std::time::Instant::now();
        assert!(wake.wait(2_000), "a pending signal must not park");
        assert!(
            started.elapsed() < std::time::Duration::from_millis(500),
            "wait returned late: it parked despite a pending signal"
        );
    }

    /// Several producers signal this waker (the reader thread and the
    /// panel worker), so coalesced signals must still wake exactly one
    /// following wait rather than being swallowed by the drain.
    #[test]
    fn coalesced_signals_still_wake_the_next_wait() {
        let wake = waker::Waker::new().expect("a waker");
        wake.signal();
        wake.signal();
        assert!(wake.wait(2_000));
        wake.drain();
        wake.signal();
        assert!(wake.wait(2_000), "a signal after a drain must still wake");
    }

    #[test]
    fn gentle_scroll_accumulates_before_producing_a_step() {
        let mut p = PopupPointerState::default();
        for _ in 0..10 {
            p.scroll_fixed_remainder += 30;
            let raw = p.scroll_fixed_remainder;
            let px = raw / 256;
            p.scroll_fixed_remainder = raw - px * 256;
            if px != 0 {
                p.pending_scroll.push(px);
            }
        }
        let total: i32 = p.take_scroll().iter().sum();
        assert!(total > 0);
    }
}

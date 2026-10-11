//! A `zwlr_layer_shell_v1` status-bar window: a thin, edge-anchored,
//! exclusive-zone strip drawn with a software framebuffer.
//!
//! This is the layer-shell sibling of `platform::TermWindow` — same
//! connection plumbing (`platform::conn::WaylandConn`, the wire encoder, the raw
//! pixel `Canvas`), but anchored via `zwlr_layer_shell_v1` instead of
//! `xdg_wm_base`, with no PTY/grid/selection/clipboard machinery: a bar just
//! redraws a row of text modules on a timer or content change.
//!
//! The `wl_shm_pool` is created exactly once, at construction, matching
//! `TermWindow`'s pattern: **the pool is a factory object, not a
//! frame-sized resource** — the compositor rejects a second
//! `wl_shm.create_pool` from a client with `invalid arguments` once a layer
//! surface has gone through its first `ack_configure`/commit round-trip (at
//! least on niri/wlroots), so every subsequent resize must reuse the
//! existing pool via `wl_shm_pool.resize` (grow-only, lazy-shrink) and only
//! recreate the two `wl_buffer`s carved out of it.

use crate::bar_keyboard::BarKeyboard;
use crate::bar_popup::BarPopup;
use crate::pointer_state::PointerState;
use crate::pointer_target::PointerTarget;
use crate::popup_key::PanelKey;
use crate::popup_surface::PopupStyle;
use platform::conn::{Sender, WindowSender};
use platform::draw::{Canvas, Color, Point};
use platform::event::RawEvent;
use platform::id::{
    CompositorId, KeyboardId, LayerShellId, OutputId, RegistryId, SeatId, ShmId, WmBaseId,
};
use platform::popup::PopupPositioner;
use platform::layer::{LayerSurfaceConfig, LayerSurfaceHandle};
use platform::present::{PresentAttempt, PresentState};
use platform::protocol::{
    self as proto, Anchor, Format, KeyboardInteractivity, Layer, LayerSurfaceEvent, SeatCapability,
    SeatEvent, SeatRequest,
};
use platform::wire::MessageBuilder;
use platform::{PlatformError, Result};
use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc;
use quetzalcoatl::spsc::Consumer;

const EVENT_RING_CAP: usize = 256;
const APP_ID: &str = "statusbar";
/// Object ids available after startup. Popups allocate their whole chain
/// at runtime, so this is sized well past one popup's worth.
const RUNTIME_ID_BLOCK: u32 = 512;

/// Placeholder width the pool is first sized at, before the compositor's
/// initial `configure` tells us the real (output-width) size. Matches
/// `TermWindow`'s "size known before first commit" ordering closely enough
/// that we never hit the "second `create_pool`" rejection — the *first* frame
/// pair (buffers + pool) is created up front, and every later size change
/// reuses the same pool via `resize`.
const INITIAL_WIDTH: i32 = 1;

/// Which edge of the output the bar docks to.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Edge {
    Top,
    Bottom,
}

impl From<config::BarPosition> for Edge {
    fn from(position: config::BarPosition) -> Self {
        match position {
            config::BarPosition::Top => Self::Top,
            config::BarPosition::Bottom => Self::Bottom,
        }
    }
}

impl Edge {
    fn anchor_bits(self) -> u32 {
        match self {
            Self::Top => Anchor::bits(&[Anchor::Top, Anchor::Left, Anchor::Right]),
            Self::Bottom => Anchor::bits(&[Anchor::Bottom, Anchor::Left, Anchor::Right]),
        }
    }
}

use platform::surface::{self, Frame};

/// A layer-shell status-bar window: anchors to a screen edge, reserves an
/// exclusive zone, and blits whatever [`BarWindow::draw`] paints.
pub struct BarWindow {
    sender: Sender,
    request_rx: mpsc::Consumer<platform::conn::WindowRequest>,
    window_tx: WindowSender,
    events: Consumer<RawEvent>,
    #[allow(dead_code)]
    reader: Option<std::thread::JoinHandle<Result<()>>>,
    /// Tracks whether the reader thread still exists. A bar drains its ring
    /// with the non-blocking `pop()`, so without this an exited reader is
    /// indistinguishable from an idle one and the process would tick on
    /// forever after the compositor went away.
    reader_health: platform::ReaderHealth,
    wake: waker::Waker,

    #[allow(dead_code)]
    compositor: CompositorId,
    #[allow(dead_code)]
    layer_shell: LayerShellId,
    layer: LayerSurfaceHandle,

    #[allow(dead_code)]
    edge: Edge,
    height: i32,
    scale: i32,

    seat: SeatId,
    /// `wl_pointer` position/click/scroll tracking -- see
    /// [`PointerState`]'s module doc for why this is its own type rather
    /// than six loose fields.
    pointer: PointerState,
    keyboard: BarKeyboard,
    /// `None` when the compositor offers no `xdg_wm_base`: panels can't open.
    popup: Option<BarPopup>,

    width: i32,
    /// The `wl_shm` double-buffered swap chain plus the dirty/ready/callback
    /// handshake -- reuses `platform::present::PresentState` verbatim rather
    /// than reimplementing it, since the bar's swap chain is structurally
    /// identical to `platform::TermWindow`'s (see the module doc): a pool
    /// created once, grown/shrunk in place across resizes, two frame slots.
    present: PresentState,

    /// Background the canvas is cleared to before each frame's `paint`
    /// closure runs. Defaults to a dark slate; set via
    /// [`BarWindow::set_background`] (mirrors `[bar] background` / the
    /// terminal's own `[colors] background` -- see `config::Config::bar_background`).
    background: Color,
    /// Alpha applied to background pixels, `0-255` (`255` = opaque). Mirrors
    /// `platform::Renderer`'s `bg_alpha` -- `255` packs plain XRGB8888
    /// (byte-identical to the pre-transparency path), anything less packs
    /// premultiplied ARGB8888 per `wl_shm`'s wire format. Set via
    /// [`BarWindow::set_opacity`]; the buffer format (`Argb8888` vs.
    /// `Xrgb8888`) is fixed at construction from the same value (see
    /// [`BarWindow::new`]) since it determines which `wl_shm` format we
    /// negotiate and that can't change after the pool/buffers exist.
    bg_alpha: u8,
    configured: bool,
    closing: bool,
}

impl Drop for BarWindow {
    fn drop(&mut self) {
        let _ = self.layer.destroy(&mut self.sender);
    }
}

struct Globals {
    compositor: CompositorId,
    shm: ShmId,
    layer_shell: LayerShellId,
    seat: SeatId,
    output: Option<OutputId>,
    wm_base: Option<WmBaseId>,
}

/// Accumulates globals as `wl_registry.global` events arrive. Mirrors
/// `platform::window`'s `GlobalsBuilder`: pulled out of `BarWindow::discover`
/// (which had the same shape -- a `while` loop over 5 Option<Id> locals plus
/// a 5-armed match, each arm repeating "already bound? else bind and stash")
/// so the loop condition and per-event dispatch are each one named,
/// independently readable method.
#[derive(Default)]
struct GlobalsBuilder {
    compositor: Option<CompositorId>,
    shm: Option<ShmId>,
    layer_shell: Option<LayerShellId>,
    seat: Option<SeatId>,
    output: Option<OutputId>,
    wm_base: Option<WmBaseId>,
}

impl surface::GlobalCollector for GlobalsBuilder {
    /// Whether every global `BarWindow::discover`'s caller needs has been
    /// bound. `wl_output` is optional (not every compositor advertises one
    /// at startup), so it's excluded -- the registry burst loop stops as
    /// soon as the required set is complete.
    fn is_complete(&self) -> bool {
        self.compositor.is_some()
            && self.shm.is_some()
            && self.layer_shell.is_some()
            && self.seat.is_some()
    }

    /// Bind `interface` if it's one this bar needs and hasn't already bound,
    /// delegating to `Self::bind_if_missing` so each match arm is a single
    /// call instead of an inline `&&`-guard plus a 2-line body (see
    /// `platform::window::GlobalsBuilder`'s identical split for the same
    /// rationale). A no-op for any interface not in the list below.
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
            s if s == proto::WL_OUTPUT.name => Self::bind_if_missing(
                &mut self.output,
                sender,
                registry,
                name,
                interface,
                version,
                proto::WL_OUTPUT.version,
            ),
            s if s == proto::XDG_WM_BASE.name => Self::bind_if_missing(
                &mut self.wm_base,
                sender,
                registry,
                name,
                interface,
                version,
                proto::XDG_WM_BASE.version,
            ),
            _ => Ok(()),
        }
    }
}

impl GlobalsBuilder {
    /// Consume the builder into the final [`Globals`], failing if any
    /// required global never arrived.
    fn finish(self) -> Result<Globals> {
        Ok(Globals {
            compositor: self.compositor.ok_or(PlatformError::ReaderStopped)?,
            shm: self.shm.ok_or(PlatformError::ReaderStopped)?,
            layer_shell: self.layer_shell.ok_or(PlatformError::ReaderStopped)?,
            seat: self.seat.ok_or(PlatformError::ReaderStopped)?,
            output: self.output,
            wm_base: self.wm_base,
        })
    }
}

impl BarWindow {
    /// Open a layer-shell bar `height` pixels thick, docked at `edge`, and
    /// reserving that much exclusive space, with the given background
    /// `opacity` (`0.0` fully transparent .. `1.0` opaque -- mirrors
    /// `config::Config::bar_opacity`/the terminal's own `[window] opacity`).
    /// The buffer format (`Argb8888` for `opacity < 1.0`, else the cheaper
    /// `Xrgb8888`) is fixed for the window's whole lifetime from this value,
    /// matching `platform::TermWindow`'s `needs_alpha` split.
    pub fn new(edge: Edge, height: i32, opacity: f64, wake: waker::Waker) -> Result<Self> {
        let (mut sender, repeat_timer, mut events, reader_handle) =
            surface::SurfaceBootstrap::connect(EVENT_RING_CAP, "statusbar-wayland-reader", &wake)?;

        let Globals {
            compositor,
            shm,
            layer_shell,
            seat,
            output,
            wm_base,
        } = Self::discover(&mut sender, &mut events)?;
        if wm_base.is_none() {
            eprintln!("statusbar: compositor offers no xdg_wm_base; panels are disabled");
        }

        let scale = output.map_or(1, |o| Self::output_scale(&mut sender, &mut events, o));

        let px_height = height * scale;
        // Float source, no lossless-widening path available (see the font
        // crate's raster.rs doc for the full float<->int cast rationale) --
        // `.clamp(0.0, 1.0)` right before multiplying guarantees the result
        // is in `0.0..=255.0`, so this is a deliberate, reviewed allow.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let bg_alpha = (opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
        // A translucent bar (opacity < 1.0) needs per-pixel alpha (Argb8888);
        // an opaque one keeps the cheaper Xrgb8888 the compositor doesn't
        // have to blend -- same split as `platform::TermWindow::new`.
        let buf_format = if bg_alpha == 255 {
            Format::Xrgb8888
        } else {
            Format::Argb8888
        };

        // Create the shm pool + initial frame pair *before* any layer-shell
        // setup — matching TermWindow's ordering, and the ordering the
        // compositor apparently requires (a `create_pool` issued after the
        // layer surface's first configure/commit round-trip gets rejected
        // with "invalid arguments" on niri/wlroots). The real width arrives
        // via the first `configure`; `resize` grows this same pool in place.
        let (frames, shm_mem, pool) =
            Self::create_frame_pair(&mut sender, shm, INITIAL_WIDTH, px_height, buf_format)?;

        let layer = LayerSurfaceHandle::create(
            &mut sender,
            compositor,
            layer_shell,
            &LayerSurfaceConfig {
                layer: Layer::Top,
                namespace: APP_ID,
                output: OutputId(0), // null: let the compositor pick the output
                buffer_scale: Some(scale),
                anchor: Some(edge.anchor_bits()),
                size: Some((0, height)),
                exclusive_zone: Some(height),
                margin: Some((0, 0, 0, 0)),
                keyboard_interactivity: KeyboardInteractivity::None,
            },
        )?;

        let (req_tx, req_rx) =
            mpsc::RingBuffer::<platform::conn::WindowRequest>::new(Capacity::exact(64)).split();
        let (id_start, id_end) = sender.alloc_id_block(RUNTIME_ID_BLOCK);
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
            edge,
            height: px_height,
            scale,
            seat,
            pointer: PointerState::new(),
            keyboard: BarKeyboard::new(repeat_timer),
            popup: wm_base.map(|wm_base| BarPopup::new(compositor, shm, wm_base, seat)),
            width: INITIAL_WIDTH,
            // A bar always re-copies its whole backbuffer into whichever
            // slot presents next (see `present`) rather than diffing rows --
            // there's no scrollback/selection-style partial repaint here --
            // so the per-row stale bitmap `PresentState` carries is unused;
            // `row_count: 1` just keeps that unused allocation tiny.
            present: PresentState::new(frames, shm_mem, pool, buf_format, 1),
            background: Color::rgb(0x1e, 0x1e, 0x2e),
            bg_alpha,
            configured: false,
            closing: false,
        };
        window.bind_seat_devices(SeatCapability::Pointer.mask() | SeatCapability::Keyboard.mask());
        Ok(window)
    }

    /// Physical cell height in pixels the bar was opened at (already scaled).
    #[must_use]
    pub const fn height(&self) -> i32 {
        self.height
    }

    #[must_use]
    pub const fn scale(&self) -> i32 {
        self.scale
    }

    /// Mark the bar dirty so the next `tick` repaints and presents.
    pub const fn request_redraw(&mut self) {
        self.present.mark_dirty();
    }

    /// Latest known pointer position, physical pixels, surface-local
    /// (`(0,0)` at the bar's top-left).
    #[must_use]
    pub const fn pointer_pos(&self) -> Point {
        self.pointer.pos()
    }

    /// Drain and return every scroll delta queued since the last call.
    /// Positive = wheel down/away, negative = wheel up/toward -- one entry
    /// per `wl_pointer.axis` event, already converted from `wl_fixed` to
    /// pixels (a typical detent is ~15).
    pub fn take_scroll(&mut self) -> Vec<i32> {
        self.pointer.take_scroll()
    }

    /// Consume a pending left-click, reporting which surface it landed on.
    pub fn take_click(&mut self) -> Option<PointerTarget> {
        let surface = self.pointer.take_click()?;
        Some(self.classify(Some(surface)))
    }

    /// The surface the pointer is currently over.
    #[must_use]
    pub fn pointer_target(&self) -> PointerTarget {
        self.classify(self.pointer.focus())
    }

    /// Keys typed into the open panel popup since the last call.
    pub fn take_keys(&mut self) -> Vec<PanelKey> {
        self.keyboard.take_keys()
    }

    /// The bar's width in logical pixels, the space popups are placed in.
    #[must_use]
    pub const fn logical_width(&self) -> i32 {
        self.width / self.scale
    }

    /// Open a panel popup grabbed by the latest press, replacing any open
    /// one. Returns `Ok(false)` when popups are unsupported or no press
    /// has been seen to grab with.
    pub fn open_popup(&mut self, positioner: &PopupPositioner, style: PopupStyle) -> Result<bool> {
        let (Some(popup), Some(serial)) = (&mut self.popup, self.pointer.press_serial()) else {
            return Ok(false);
        };
        self.keyboard.release_all();
        popup.open(&mut self.window_tx, self.layer, serial, positioner, style)?;
        self.wake.signal();
        Ok(true)
    }

    /// Whether [`Self::open_popup`] can succeed: the compositor offers
    /// `xdg_wm_base` and a press has been seen to grab with.
    #[must_use]
    pub const fn can_open_popup(&self) -> bool {
        self.popup.is_some() && self.pointer.press_serial().is_some()
    }

    pub fn close_popup(&mut self) {
        if let Some(popup) = &mut self.popup {
            popup.close(&self.window_tx, self.layer);
        }
        self.keyboard.release_all();
        self.wake.signal();
    }

    #[must_use]
    pub fn popup_open(&self) -> bool {
        self.popup.as_ref().is_some_and(BarPopup::is_open)
    }

    /// The compositor dismissed the popup; the caller should close it.
    #[must_use]
    pub fn popup_dismissed(&self) -> bool {
        self.popup.as_ref().is_some_and(BarPopup::is_dismissed)
    }

    pub fn request_popup_redraw(&mut self) {
        if let Some(popup) = &mut self.popup {
            popup.request_redraw();
        }
        self.wake.signal();
    }

    fn classify(&self, focus: Option<platform::id::SurfaceId>) -> PointerTarget {
        let popup = self.popup.as_ref().and_then(BarPopup::surface);
        PointerTarget::classify(focus, self.layer.surface, popup)
    }

    /// Set the color the canvas is cleared to before each frame's `paint`
    /// closure runs, and request a redraw so the change is visible
    /// immediately.
    pub const fn set_background(&mut self, color: Color) {
        self.background = color;
        self.present.mark_dirty();
    }

    /// A clone of the shared wakeup channel — hand it to any producer (e.g.
    /// a module's poll timer) so [`BarWindow::wait`] unblocks.
    #[must_use]
    pub fn waker(&self) -> waker::Waker {
        self.wake.clone()
    }

    /// Block until a Wayland event arrives or `timeout_ms` elapses (negative
    /// = forever).
    pub fn wait(&self, timeout_ms: i32) {
        let _ = self.wake.wait(timeout_ms);
        self.wake.drain();
    }

    /// Process pending Wayland events (non-blocking) and present the bar
    /// and any open popup if dirty. Returns `false` once the layer surface
    /// is destroyed / connection lost.
    pub fn tick(
        &mut self,
        mut paint: impl FnMut(&mut Canvas),
        mut paint_popup: impl FnMut(&mut Canvas),
    ) -> Result<bool> {
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

        // Drain first, *then* ask whether the reader is gone: a reader that
        // exited may have pushed the `wl_display.error` explaining why, and
        // the loop above must get to consume it before we tear down.
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
        if let Some(popup) = &mut self.popup {
            popup.present(&mut self.window_tx, &mut paint_popup);
        }
        self.sender
            .drain_requests(&mut self.request_rx)
            .map_err(PlatformError::Conn)?;
        Ok(true)
    }

    fn handle_event(&mut self, ev: &RawEvent) -> Result<()> {
        match ev {
            RawEvent::Fd(fds) => {
                self.keyboard.absorb_keymap_fd(fds);
                return Ok(());
            }
            RawEvent::Repeat => {
                self.keyboard.on_repeat_tick();
                return Ok(());
            }
            RawEvent::Bytes(_) | RawEvent::Wire { .. } => {}
        }
        // The `wl_display` error/delete_id routing, `wl_buffer.release`, and
        // frame callback are handled identically to `platform::TermWindow` via
        // the shared `surface::route_common`; only the bar-specific seat /
        // pointer / layer-surface events below differ.
        match self.present.route_common(ev, &mut self.window_tx) {
            surface::Routed::Fatal(e) => return Err(e),
            surface::Routed::Handled => return Ok(()),
            surface::Routed::Passthrough => {}
        }

        if let Some(popup) = &mut self.popup
            && popup.handle_event(ev, &mut self.window_tx)?
        {
            return Ok(());
        }

        if let Some(keyboard) = self.keyboard.id()
            && ev.is(keyboard)
        {
            self.keyboard.on_event(ev);
            return Ok(());
        }

        // wl_seat.capabilities
        if ev.is(self.seat) && ev.event::<SeatEvent>() == Some(SeatEvent::Capabilities) {
            self.handle_seat_capabilities(ev);
            return Ok(());
        }

        // wl_pointer.* -- only Enter/Motion (track position), Button (click
        // detection) and Axis (queue a scroll delta) matter for a bar with
        // no selection/drag of its own; see `PointerState` for all of it.
        if let Some(pointer) = self.pointer.id()
            && ev.is(pointer)
        {
            self.pointer.on_event(ev);
            return Ok(());
        }

        if ev.is(self.layer.layer_surface) {
            return self.handle_layer_surface_event(ev);
        }
        Ok(())
    }

    /// Handle `wl_seat.capabilities`: request a `wl_pointer` the first time
    /// the seat advertises one. Split out of `handle_event` -- this whole
    /// block is one self-contained concern (bind-once pointer discovery)
    /// that doesn't need to share stack frame with the connection-level
    /// dispatch around it.
    fn handle_seat_capabilities(&mut self, ev: &RawEvent) {
        self.bind_seat_devices(ev.arg(0));
    }

    /// Startup discovery reads past the seat's one-shot `capabilities`
    /// event, so `new` binds eagerly; a device that doesn't exist yields an
    /// object that never sends events. Guarded so a repeated capabilities
    /// event never binds a duplicate.
    fn bind_seat_devices(&mut self, caps: u32) {
        if self.pointer.id().is_none() && SeatCapability::Pointer.in_mask(caps) {
            let pointer: platform::id::PointerId = self.window_tx.alloc_id();
            self.window_tx
                .request(MessageBuilder::new(self.seat, SeatRequest::GetPointer).new_id(pointer));
            self.pointer.set_id(pointer);
        }
        if self.popup.is_some()
            && self.keyboard.id().is_none()
            && SeatCapability::Keyboard.in_mask(caps)
        {
            let keyboard: KeyboardId = self.window_tx.alloc_id();
            self.window_tx
                .request(MessageBuilder::new(self.seat, SeatRequest::GetKeyboard).new_id(keyboard));
            self.keyboard.set_id(keyboard);
        }
    }

    /// Handle `zwlr_layer_surface_v1.*`: `Configure` (ack + resize to the
    /// compositor-assigned width) and `Closed` (mark the bar for teardown).
    /// Split out of `handle_event` for the same reason as
    /// [`Self::handle_seat_capabilities`] -- this is the only branch group
    /// in `handle_event` that can itself fail (via `resize`), so giving it
    /// its own `Result`-returning method keeps that fallibility local
    /// instead of threading through an inline match arm.
    fn handle_layer_surface_event(&mut self, ev: &RawEvent) -> Result<()> {
        match ev.event::<LayerSurfaceEvent>() {
            Some(LayerSurfaceEvent::Configure) => {
                let serial = ev.arg(0);
                // Wayland surface dimensions never approach i32::MAX, so
                // this is never actually a sign-changing wrap.
                let w = ev.arg(1).cast_signed() * self.scale;
                self.layer.ack_configure_queued(&self.window_tx, serial);
                if w > 0 {
                    self.resize(w)?;
                }
                self.configured = true;
                self.present.mark_dirty();
                Ok(())
            }
            Some(LayerSurfaceEvent::Closed) => {
                self.closing = true;
                Ok(())
            }
            None => Ok(()),
        }
    }

    /// Resize to `width` (physical pixels; height is fixed for the bar's
    /// lifetime). Reuses the existing `wl_shm_pool` — grows it in place via
    /// `ftruncate`/`mremap`/`wl_shm_pool.resize` when the new size doesn't
    /// fit, lazily keeps it oversized on a shrink — and only tears down and
    /// recreates the two `wl_buffer`s. Never issues a second
    /// `wl_shm.create_pool` (see the module doc for why that gets rejected).
    fn resize(&mut self, width: i32) -> Result<()> {
        if width == self.width {
            return Ok(());
        }
        self.width = width;

        self.present.destroy_old_buffers(&mut self.window_tx);

        let grew = self
            .present
            .resize_shm(width, self.height)
            .map_err(|e| PlatformError::Conn(platform::conn::ConnError::Sys(e)))?;
        if let Some(new_capacity) = grew {
            self.present.grow_pool(&mut self.window_tx, new_capacity);
        }
        self.present
            .recreate_buffers(&mut self.window_tx, width, self.height);
        // A bar repaints its whole backbuffer every present (see
        // `do_present`), so it has no rows to mark stale -- `row_count: 1`
        // just matches the placeholder size `PresentState::new` was built
        // with; only `dirty`/`frame_ready` actually matter here.
        self.present.reset_after_resize(1);
        Ok(())
    }

    /// Build a fresh `wl_shm_pool` plus the two `wl_buffer`s carved out of
    /// it. Called exactly once, from [`BarWindow::new`] — every later resize
    /// goes through [`BarWindow::resize`] instead, which reuses this same
    /// pool.
    /// Delegates to the role-agnostic [`platform::surface::Frame::create_pair`],
    /// shared with `platform::TermWindow`.
    fn create_frame_pair(
        sender: &mut Sender,
        shm: ShmId,
        width: i32,
        height: i32,
        format: Format,
    ) -> Result<([Frame; 2], platform::sys::ShmBuffer, platform::id::PoolId)> {
        Frame::create_pair(sender, shm, width, height, format)
    }

    /// Pack `self.background` for this window's fixed buffer format via the
    /// role-agnostic [`platform::surface::SurfaceBootstrap::pack_bg`] -- the
    /// single source of truth shared with `platform::Renderer`.
    fn pack_bg(&self) -> u32 {
        surface::SurfaceBootstrap::pack_bg(self.background, self.bg_alpha)
    }

    /// Paint into the claimed free slot `slot` (from
    /// `self.present.begin_present()`) and commit it. Named `do_present`
    /// rather than `present` now that the swap-chain state itself lives in
    /// the `present: PresentState` field of the same name.
    fn do_present(&mut self, slot: usize, paint: &mut impl FnMut(&mut Canvas)) {
        // The canvas carries this window's packing, so the painter's drawing
        // calls (and `TextRenderer`'s glyph-edge blending, which reads the
        // alpha back off the canvas) are correct without each one being
        // handed `bg_alpha` separately.
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

    /// `xdg_wm_base` is optional, so the burst is finished with a sync
    /// round-trip to catch it when it is advertised after the required set.
    fn discover(sender: &mut Sender, events: &mut Consumer<RawEvent>) -> Result<Globals> {
        let mut globals = GlobalsBuilder::default();
        let registry = surface::RegistryDiscovery::collect(sender, events, &mut globals)?;
        surface::RegistryDiscovery::collect_through_sync(sender, events, registry, &mut globals)?;
        globals.finish()
    }

    /// Delegates to the role-agnostic
    /// [`platform::surface::SurfaceBootstrap::output_scale`], shared with
    /// `platform::TermWindow`.
    fn output_scale(sender: &mut Sender, events: &mut Consumer<RawEvent>, output: OutputId) -> i32 {
        surface::SurfaceBootstrap::output_scale(sender, events, output)
    }
}

#[cfg(test)]
mod edge_tests {
    use super::Edge;

    #[test]
    fn config_position_maps_to_matching_edge() {
        assert_eq!(Edge::from(config::BarPosition::Top), Edge::Top);
        assert_eq!(Edge::from(config::BarPosition::Bottom), Edge::Bottom);
    }
}

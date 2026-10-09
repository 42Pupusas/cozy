//! [`Toplevel`] -- a generic, content-agnostic application window.
//!
//! The owner supplies a [`Painter`]; this type supplies a correctly mapped,
//! correctly resized surface and calls back whenever pixels are needed. It
//! knows nothing about grids, rows, columns, file listings, or widgets.

use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc;
use quetzalcoatl::spsc::Consumer;

use crate::conn::{Sender, WindowSender};
use crate::draw::{Canvas, Color, Damage};
use crate::error::{PlatformError, Result};
use crate::event::RawEvent;
use crate::id::{KeyboardId, PointerId, SeatId};
use crate::keyboard_decoder::{KeyInput, KeyboardUpdate};
use crate::present::{PresentAttempt, PresentState};
use crate::protocol::{
    Format, KeyState, PointerEvent, SeatCapability, SeatEvent, SeatRequest, ToplevelEvent,
    WmBaseEvent, XdgSurfaceEvent,
};
use crate::surface::{Frame, RegistryDiscovery, Routed, SurfaceBootstrap, SurfacePresenter};
use crate::surface_format::SurfaceFormat;
use crate::toplevel::geometry::{Geometry, LogicalSize, PhysicalSize};
use crate::toplevel::globals::{ToplevelGlobals, ToplevelGlobalsBuilder};
use crate::toplevel::keyboard::{KeyPress, KeyboardState};
use crate::toplevel::lifecycle::Lifecycle;
use crate::toplevel::pointer::{PointerEventKind, PointerState};
use crate::toplevel::shell::ShellSurface;
use crate::wire::MessageBuilder;

/// Depth of the event ring between the reader thread and this window.
///
/// Shallower than the terminal's, which absorbs bursts of PTY output; a
/// GUI window's events are paced by human input and the compositor.
const EVENT_RING_CAP: usize = 256;

/// Object ids pre-allocated for this window to hand out without a
/// round-trip. Frame callbacks and resize buffers churn through these, but
/// `wl_display.delete_id` recycles them.
const ID_BLOCK: u32 = 256;

/// Draws a window's contents.
///
/// A trait rather than a closure so implementors can hold state (scroll
/// offset, selection, a directory listing) and so the paint call has a name
/// in backtraces. The canvas is sized in *physical* pixels.
pub trait Painter {
    /// Paint the whole window. Called only when a present is actually going
    /// to happen, so it is safe to do real work here.
    fn paint(&mut self, canvas: &mut Canvas<'_>, size: PhysicalSize);

    /// Background colour, used to clear the canvas before [`Self::paint`]
    /// and to fill newly exposed area during a resize.
    fn background(&self) -> Color;

    /// The area the last [`Self::paint`] changed, reported to the compositor
    /// so it re-uploads only that. Defaults to the whole surface.
    fn damage(&self) -> Damage {
        Damage::full()
    }

    /// A key was pressed (or auto-repeated -- see [`KeyPress::repeat`]).
    ///
    /// Defaulted to a no-op so existing painters that only draw are
    /// unaffected, and so a painter opts in to exactly the input it wants.
    fn on_key(&mut self, _press: KeyPress) {}

    /// A pointer gesture occurred.
    fn on_pointer(&mut self, _event: PointerEventKind) {}

    /// Keyboard focus was gained or lost. Useful for drawing a focused
    /// selection differently, or hiding a text cursor.
    fn on_focus(&mut self, _focused: bool) {}

    /// Files were dropped onto this window from a `text/uri-list` drag --
    /// either another application's, or this window's own (see
    /// [`crate::toplevel::drag::DragController`] for why both arrive here
    /// identically). `at` is the surface-local logical position the drop
    /// landed at, for resolving which row/target it hit. Defaulted to a
    /// no-op so painters that don't accept drops are unaffected.
    fn on_files_dropped(&mut self, _paths: Vec<std::path::PathBuf>, _at: (i32, i32)) {}
}

/// Something the window wants its owner to know about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowEvent {
    /// The user or compositor asked the window to close.
    CloseRequested,
    /// The window was resized to this physical size.
    Resized(PhysicalSize),
}

/// A generic xdg-shell toplevel window with a software framebuffer.
pub struct Toplevel {
    sender: Sender,
    request_rx: mpsc::Consumer<crate::conn::WindowRequest>,
    window_tx: WindowSender,
    events: Consumer<RawEvent>,
    activations: crate::toplevel::activation_queue::ActivationQueue,
    reader: Option<std::thread::JoinHandle<Result<()>>>,
    reader_health: crate::ReaderHealth,
    wake: waker::Waker,

    seat: SeatId,
    wm_base: crate::id::WmBaseId,
    shell: ShellSurface,

    geometry: Geometry,
    lifecycle: Lifecycle,
    present: PresentState,

    /// How this surface's pixels are packed, decided once at construction
    /// from the requested opacity and fixed for the window's lifetime (the
    /// `wl_shm` buffer format is negotiated at pool creation).
    ///
    /// Handed to every [`Canvas`] this window builds so the painter's drawing
    /// calls pack correctly without knowing whether the window is see-through.
    format: SurfaceFormat,

    /// Events queued for the owner to drain.
    queue: std::collections::VecDeque<WindowEvent>,

    /// Input device ids, once the seat advertises them. `None` until then;
    /// a compositor may advertise capabilities late or never (a headless
    /// session has no pointer at all).
    pointer_id: Option<PointerId>,
    keyboard_id: Option<KeyboardId>,

    pointer: PointerState,
    keyboard: KeyboardState,

    /// Cursor images, built on demand. Empty until a painter asks for a
    /// shape, so a window that never changes its cursor pays nothing.
    cursors: crate::toplevel::cursor_set::CursorSet,

    keys: crate::keyboard_decoder::KeyboardDecoder,

    /// Drag-and-drop over the shared `wl_data_device` -- see
    /// [`crate::toplevel::drag::DragController`] for why this handles both
    /// dragging out of and dropping into this window.
    drag: crate::toplevel::drag::DragController,

    /// Text this window has put on the clipboard, published through the
    /// same data device the drag controller owns.
    selection: crate::toplevel::text_selection::TextSelection,

    /// `xdg_activation_v1`, when the compositor offers it. `None` means a
    /// launched window cannot ask to be raised, which is the pre-protocol
    /// behaviour rather than a failure.
    activation: Option<crate::toplevel::activation::Activation>,
}

impl Toplevel {
    /// Open an opaque window of `size` physical pixels titled `title`.
    ///
    /// `app_id` should be stable for the process -- compositors match window
    /// rules on it, unlike the title, which apps rewrite at runtime.
    ///
    /// For a translucent window use [`Self::new_with_format`]; this stays as
    /// the name for the common case so opaque callers need not mention a
    /// pixel format they do not care about.
    pub fn new(
        title: &str,
        app_id: &str,
        size: LogicalSize,
        wake: waker::Waker,
        background: Color,
    ) -> Result<Self> {
        Self::new_with_format(
            title,
            app_id,
            size,
            wake,
            background,
            SurfaceFormat::opaque(),
        )
    }

    /// Open a window whose surface is packed according to `format`.
    ///
    /// A translucent `format` selects the `Argb8888` buffer the compositor
    /// blends with what is behind the window; an opaque one keeps the cheaper
    /// `Xrgb8888` it can display directly. The format is fixed for the
    /// window's lifetime -- the buffer format is negotiated once here, at pool
    /// creation, and changing it later would mean tearing down and rebuilding
    /// every buffer.
    pub fn new_with_format(
        title: &str,
        app_id: &str,
        size: LogicalSize,
        wake: waker::Waker,
        background: Color,
        format: SurfaceFormat,
    ) -> Result<Self> {
        let (mut sender, _repeat_timer, mut events, reader_handle) =
            SurfaceBootstrap::connect(EVENT_RING_CAP, "toplevel-wayland-reader", &wake)?;

        let mut builder = ToplevelGlobalsBuilder::default();
        let registry = RegistryDiscovery::collect(&mut sender, &mut events, &mut builder)?;
        // A sync round-trip after the required set completes, so optional
        // globals (the decoration manager especially) advertised later in
        // the same unordered burst are not missed -- otherwise whether the
        // window gets a titlebar would depend on registry ordering luck.
        RegistryDiscovery::collect_through_sync(&mut sender, &mut events, registry, &mut builder)?;
        let ToplevelGlobals {
            compositor,
            shm,
            // `shm` is consumed immediately by `Frame::create_pair` below;
            // the window never needs it again, so it is not a field.
            wm_base,
            seat,
            output,
            decoration_manager,
            data_device_manager,
            activation,
        } = builder.finish().ok_or(PlatformError::ReaderStopped)?;

        // No output bound (some nested/headless compositors advertise none)
        // means we cannot ask, so assume 1 rather than fail to start.
        let scale = output.map_or(1, |o| {
            SurfaceBootstrap::output_scale(&mut sender, &mut events, o)
        });
        let geometry = Geometry::new(
            PhysicalSize {
                width: size.width * scale,
                height: size.height * scale,
            },
            scale,
        )
        .with_min(PhysicalSize {
            width: 160 * scale,
            height: 120 * scale,
        });
        let physical = geometry.physical();

        let buf_format = Self::buffer_format(format);
        let (frames, shm_mem, pool) = Frame::create_pair(
            &mut sender,
            shm,
            physical.width,
            physical.height,
            buf_format,
        )?;

        let shell = ShellSurface::create(
            &mut sender,
            compositor,
            wm_base,
            decoration_manager,
            scale,
            title,
            app_id,
        )?;

        let (req_tx, request_rx) =
            mpsc::RingBuffer::<crate::conn::WindowRequest>::new(Capacity::exact(64)).split();
        let (id_start, id_end) = sender.alloc_id_block(ID_BLOCK);
        let mut window_tx = WindowSender {
            tx: req_tx,
            next_id: id_start,
            id_end,
            free_ids: Vec::new(),
        };
        // Bound eagerly, alongside every other global: a file browser (the
        // reason this type exists) needs drag-and-drop from its first
        // frame, and there is exactly one `wl_data_device` per seat to bind.
        let drag =
            crate::toplevel::drag::DragController::bind(data_device_manager, seat, &mut window_tx);

        let mut present = PresentState::new(frames, shm_mem, pool, buf_format, 1);
        present.fill_all(format.pack_glass(background));

        let mut window = Self {
            sender,
            request_rx,
            window_tx,
            events,
            reader: Some(reader_handle),
            reader_health: crate::ReaderHealth::default(),
            wake,
            seat,
            wm_base,
            shell,
            geometry,
            lifecycle: Lifecycle::new(),
            present,
            format,
            queue: std::collections::VecDeque::new(),
            pointer_id: None,
            keyboard_id: None,
            pointer: PointerState::new(),
            keyboard: KeyboardState::new(),
            cursors: crate::toplevel::cursor_set::CursorSet::new(shm, compositor, scale),
            keys: crate::keyboard_decoder::KeyboardDecoder::new(),
            activations: crate::toplevel::activation_queue::ActivationQueue::new(),
            drag,
            selection: crate::toplevel::text_selection::TextSelection::new(),
            activation: activation.map(crate::toplevel::activation::Activation::new),
        };
        window.bind_seat_devices_eagerly();
        Ok(window)
    }

    /// The window's current size in physical pixels.
    #[must_use]
    pub const fn size(&self) -> PhysicalSize {
        self.geometry.physical()
    }

    /// The output buffer scale this window opened at (1 = normal DPI,
    /// 2 = `HiDPI` @2x, ...).
    ///
    /// Pointer events arrive in **logical** surface-local pixels while the
    /// painter's canvas is **physical**; a painter that hit-tests in physical
    /// coordinates needs this to convert (`physical = logical * scale`).
    #[must_use]
    pub const fn scale(&self) -> i32 {
        self.geometry.scale()
    }

    /// A handle the owner can use to wake this window's event loop from
    /// another thread.
    #[must_use]
    pub fn waker(&self) -> waker::Waker {
        self.wake.clone()
    }

    /// Set the pointer image while it is over this window.
    ///
    /// `key` names the shape and must be stable for it (it is the cache
    /// key); `names` are the Xcursor aliases to try, best first, because
    /// themes disagree about which alias they ship.
    ///
    /// Repeating the same `key` is free: the request is skipped. A shape no
    /// installed theme provides leaves the pointer as it was, so a sparse
    /// theme degrades to a plain arrow rather than an invisible cursor.
    ///
    /// Does nothing before the pointer has entered the surface, since the
    /// protocol requires the enter serial.
    pub fn set_cursor(&mut self, key: &'static str, names: &[&str]) {
        let Some(pointer) = self.pointer_id else {
            return;
        };
        // `set_cursor` must quote the enter serial, not a button serial.
        let serial = self.pointer.enter_serial();
        if serial == 0 {
            return;
        }
        self.cursors
            .apply(&mut self.window_tx, pointer, serial, key, names);
    }

    /// Change the window title.
    pub fn set_title(&self, title: &str) {
        self.shell.set_title(&self.window_tx, title);
    }

    /// Ask for an activation token for a program this window is about to
    /// launch, so the launched window may raise itself, and return at once.
    ///
    /// Call this from the handler for the input event that caused the
    /// launch. The token carries that event's serial and this window's
    /// surface, which together are the compositor's own evidence that a
    /// user acted -- see [`crate::toplevel::activation`].
    ///
    /// The answer is collected with [`Self::next_activation`], matched to the
    /// returned ticket. A compositor with no `xdg_activation_v1` answers
    /// immediately with no token, which means "launch without one": the
    /// child still starts, it simply appears where it already lived instead
    /// of on the current workspace.
    ///
    /// `app_id` names the program being launched, not this one; pass `None`
    /// when it is not known.
    pub fn request_activation_token(
        &mut self,
        app_id: Option<&str>,
    ) -> crate::toplevel::activation_queue::ActivationTicket {
        let Some(activation) = self.activation else {
            return self.resolve_activation_unavailable();
        };
        // The id must come from this window's reserved block, like every
        // other object created after startup. `Sender::alloc_id` hands out
        // ids from the range that block was carved out of, so using it here
        // names an object the window already owns -- and the compositor
        // rejects the request as invalid arguments.
        let token: crate::id::ActivationTokenId = self.window_tx.alloc_id();
        let evidence = crate::toplevel::activation::LaunchEvidence {
            seat: self.seat,
            surface: self.shell.surface,
            serial: self.pointer.last_serial(),
            app_id,
        };
        match activation.begin(token, &mut self.sender, evidence) {
            Ok(()) => self.activations.track(token),
            // Reported, not swallowed: a protocol error means the compositor
            // is about to drop the connection, and the window would
            // otherwise die a few frames later with no stated cause.
            Err(e) => {
                eprintln!("platform: activation token unavailable: {e}");
                self.resolve_activation_unavailable()
            }
        }
    }

    /// An answer that needs no compositor still has to wake the loop, or it
    /// would sit uncollected until some unrelated event arrived.
    fn resolve_activation_unavailable(
        &mut self,
    ) -> crate::toplevel::activation_queue::ActivationTicket {
        let ticket = self.activations.resolve_unavailable();
        self.wake.signal();
        ticket
    }

    /// The oldest token answer not yet collected.
    ///
    /// Poll after [`Self::tick`], which is where answers are read off the
    /// connection.
    pub fn next_activation(
        &mut self,
    ) -> Option<crate::toplevel::activation_queue::ActivationOutcome> {
        self.activations.pop()
    }

    /// Whether the compositor can issue activation tokens.
    #[must_use]
    pub const fn supports_activation(&self) -> bool {
        self.activation.is_some()
    }

    /// Start dragging `paths` out of this window -- the file-browser-facing
    /// entry point over [`crate::toplevel::drag::DragController::start_drag`].
    ///
    /// Must be called from a left-button press handler (or shortly after,
    /// while the same press's serial is still live): `wl_data_device.start_drag`
    /// requires an implicit grab matching a recent button-press serial, which
    /// is exactly what [`crate::toplevel::pointer::PointerState::last_serial`]
    /// tracks. No-op if nothing has been pressed yet, there's no data device,
    /// or `paths` is empty -- see [`crate::toplevel::drag::DragController::start_drag`].
    pub fn start_drag(&mut self, paths: &[std::path::PathBuf]) {
        let serial = self.pointer.last_serial();
        self.drag
            .start_drag(&mut self.window_tx, self.shell.surface, serial, paths);
    }

    /// Put `text` on the system clipboard.
    ///
    /// Must follow a pointer press, whose serial the compositor checks. A
    /// compositor without `wl_data_device_manager`, an empty `text`, or a
    /// window that has not yet seen a press makes this a no-op.
    pub fn copy_text(&mut self, text: String) {
        let serial = self.pointer.last_serial();
        let Some(device) = self.drag.data_device() else {
            return;
        };
        if serial == 0 {
            return;
        }
        self.selection
            .publish(&mut self.window_tx, device, serial, text);
    }

    /// Whether a drag started from this window (via [`Self::start_drag`]) is
    /// currently live.
    #[must_use]
    pub const fn is_dragging(&self) -> bool {
        self.drag.is_dragging()
    }

    /// Whether an incoming drag (another window's, or our own) is currently
    /// hovering this window's surface -- for painting a drop-target
    /// highlight.
    #[must_use]
    pub const fn is_drag_over(&self) -> bool {
        self.drag.is_drag_over()
    }

    /// The incoming drag's current surface-local logical position, if one is
    /// hovering this window -- for a drop-target highlight that follows the
    /// pointer. Meaningless (and unused) unless [`Self::is_drag_over`] is
    /// true.
    #[must_use]
    pub const fn drag_position(&self) -> (i32, i32) {
        self.drag.incoming_position()
    }

    /// Whether the window is shutting down.
    #[must_use]
    pub const fn is_closing(&self) -> bool {
        self.lifecycle.is_closing()
    }

    /// Take the next queued [`WindowEvent`], if any.
    pub fn next_event(&mut self) -> Option<WindowEvent> {
        self.queue.pop_front()
    }

    /// Request a repaint on the next tick.
    pub const fn request_redraw(&mut self) {
        self.present.mark_dirty();
    }

    /// Drain pending compositor events, apply any resize, and present a
    /// frame if one is owed.
    ///
    /// Call this in a loop; it does not block. Use [`Self::wait`] to sleep
    /// until something happens.
    pub fn tick(&mut self, painter: &mut impl Painter) -> Result<()> {
        self.sender.drain_requests(&mut self.request_rx)?;
        while let Some(ev) = self.events.pop() {
            self.handle_event(ev, painter)?;
        }
        self.pump_repeat(painter);
        // A drop payload is read on a helper thread, so its completion is
        // not tied to any compositor event -- draining only from the
        // data-device dispatch would strand the last drop of a gesture
        // until some unrelated event happened to arrive.
        self.deliver_drops(painter);
        // Drain first, *then* observe: a reader that hit a protocol error
        // pushes the explaining event before exiting, and checking liveness
        // first would discard it and turn a diagnosable fault into a silent
        // one. See `ReaderHealth::observe`.
        if !self
            .reader_health
            .observe(self.events.is_closed(), true)
            .is_alive()
        {
            return Err(PlatformError::ReaderStopped);
        }
        self.apply_resize()?;
        self.maybe_present(painter);
        self.sender.drain_requests(&mut self.request_rx)?;
        Ok(())
    }

    /// Block until the reader (or another thread) signals the waker, or
    /// `timeout_ms` elapses (negative blocks forever).
    ///
    /// Drains the eventfd afterwards: without that the counter stays
    /// non-zero and every subsequent `wait` returns instantly, turning the
    /// event loop into a busy spin that pins a core.
    pub fn wait(&self, timeout_ms: i32) -> bool {
        let signalled = self.wake.wait(timeout_ms);
        if signalled {
            self.wake.drain();
        }
        signalled
    }

    /// Dispatch one raw event.
    fn handle_event(&mut self, ev: RawEvent, painter: &mut impl Painter) -> Result<()> {
        // The keymap fd and a drag source's `send` fd both arrive as a
        // standalone `Fd` event immediately after the message that
        // triggered them, so both are correlated by arrival order rather
        // than by id -- routed by which one is currently expected.
        if let RawEvent::Fd(fds) = ev {
            self.handle_fd_event(fds.into_iter().next().as_ref());
            return Ok(());
        }

        if let Some(token) = self.activations.absorb(&ev) {
            let _ = crate::toplevel::activation::Activation::release(&mut self.sender, token);
            return Ok(());
        }

        match self.present.route_common(&ev, &mut self.window_tx) {
            Routed::Handled => return Ok(()),
            Routed::Fatal(e) => return Err(e),
            Routed::Passthrough => {}
        }

        if self.pointer_id.is_some_and(|p| ev.is(p)) {
            self.handle_pointer_event(&ev, painter);
            return Ok(());
        }
        if self.keyboard_id.is_some_and(|k| ev.is(k)) {
            self.handle_keyboard_event(&ev, painter);
            return Ok(());
        }
        if self.dispatch_drag_event(&ev, painter) {
            return Ok(());
        }

        // xdg_wm_base.ping -- answer promptly or the compositor kills us.
        if ev.is(self.wm_base) && ev.event::<WmBaseEvent>() == Some(WmBaseEvent::Ping) {
            ShellSurface::pong(&self.window_tx, self.wm_base, ev.serial(0).raw());
            return Ok(());
        }

        // xdg_surface.configure -- ack, which maps the surface.
        if ev.is(self.shell.xdg_surface)
            && ev.event::<XdgSurfaceEvent>() == Some(XdgSurfaceEvent::Configure)
        {
            self.lifecycle.configure_received(ev.serial(0).raw());
            if let Some(serial) = self.lifecycle.take_ack() {
                self.shell.ack_configure(&self.window_tx, serial);
                self.lifecycle.acked();
            }
            self.present.mark_dirty();
            return Ok(());
        }

        if ev.is(self.shell.toplevel) {
            match ev.event::<ToplevelEvent>() {
                Some(ToplevelEvent::Close) => {
                    self.lifecycle.close_requested();
                    self.queue.push_back(WindowEvent::CloseRequested);
                    return Ok(());
                }
                Some(ToplevelEvent::Configure) => {
                    // Logical pixels -- `Geometry` converts and coalesces.
                    self.geometry.propose(LogicalSize {
                        width: ev.arg_i32(0),
                        height: ev.arg_i32(1),
                    });
                    return Ok(());
                }
                None => {}
            }
        }

        if ev.is(self.seat) && ev.event::<SeatEvent>() == Some(SeatEvent::Capabilities) {
            self.bind_seat_devices(ev.arg(0));
            return Ok(());
        }

        Ok(())
    }

    /// Route the three drag-and-drop protocol objects -- `wl_data_device`
    /// (enter/leave/motion/drop on our surface), `wl_data_source` (a peer
    /// wants our dragged payload), and `wl_data_offer` (MIME types a peer
    /// advertises during an incoming drag) -- to [`crate::toplevel::drag::DragController`].
    /// Returns whether one of them matched, so `handle_event` knows to stop.
    fn dispatch_drag_event(&mut self, ev: &RawEvent, painter: &mut impl Painter) -> bool {
        if let Some(dev) = self.drag.device_id()
            && ev.is(dev)
        {
            self.dispatch_data_device_event(ev, painter);
            return true;
        }
        if let Some(src) = self.drag.source_id()
            && ev.is(src)
        {
            if let Some(src_ev) = ev.event::<crate::protocol::DataSourceEvent>() {
                self.drag.on_source_event(src_ev, ev.arg(0));
            }
            return true;
        }
        if let Some(src) = self.selection.id()
            && ev.is(src)
        {
            if let Some(src_ev) = ev.event::<crate::protocol::DataSourceEvent>() {
                self.selection.on_event(&mut self.window_tx, src_ev);
            }
            return true;
        }
        if let Some(offer) = self.drag.incoming_offer_id()
            && ev.is(offer)
        {
            if let Some(offer_ev) = ev.event::<crate::protocol::DataOfferEvent>() {
                // `Offer`'s MIME-type string arg is not one of the fixed
                // inline words `RawEvent::Wire` carries; it must be
                // re-parsed from the full message bytes, same as
                // `wl_registry.global`'s interface name.
                let mime = ev
                    .wire_bytes()
                    .and_then(crate::wire::WireEvent::parse)
                    .map(|(mut wire, _)| wire.string());
                self.drag.on_offer_event(offer_ev, mime, ev.arg(0));
            }
            return true;
        }
        false
    }

    /// `wl_data_device.*`: track an incoming drag's lifecycle and fire the
    /// painter's drop callback once a drop actually lands.
    fn dispatch_data_device_event(&mut self, ev: &RawEvent, painter: &mut impl Painter) {
        use crate::protocol::DataDeviceEvent;
        match ev.event::<DataDeviceEvent>() {
            Some(DataDeviceEvent::DataOffer) => self.drag.on_new_offer(ev.arg(0)),
            Some(DataDeviceEvent::Enter) => {
                // `enter(serial, surface, x, y, id)` -- five words, one more
                // than `RawEvent::Wire` inlines, so the offer id must be
                // read back out of the full message bytes. Taking it from
                // `arg(4)` yields a constant 0, i.e. "this drag has no
                // source", and every incoming drop is silently discarded.
                let at = ev.point(2, 3);
                let (serial, offer) = Self::parse_data_device_enter(ev);
                self.drag
                    .on_enter(&self.window_tx, offer, serial, (at.x, at.y));
                self.present.mark_dirty();
            }
            Some(DataDeviceEvent::Leave) => {
                self.drag.on_leave(&self.window_tx);
                self.present.mark_dirty();
            }
            Some(DataDeviceEvent::Motion) => {
                // `motion(time, x, y)`.
                let at = ev.point(1, 2);
                self.drag.on_motion(&self.window_tx, (at.x, at.y));
                self.present.mark_dirty();
            }
            Some(DataDeviceEvent::Drop) => {
                self.drag.handle_drop(&self.window_tx, &self.wake);
                self.present.mark_dirty();
            }
            _ => {}
        }
        self.deliver_drops(painter);
    }

    /// Read `wl_data_device.enter`'s `(serial, offer_id)` out of the raw
    /// message. Both are plain words, but the event has five of them and
    /// only the first four are inlined -- see [`crate::event::RawEvent`].
    fn parse_data_device_enter(ev: &RawEvent) -> (u32, u32) {
        let Some(bytes) = ev.wire_bytes() else {
            return (ev.serial(0).raw(), 0);
        };
        let Some((mut wire, _)) = crate::wire::WireEvent::parse(bytes) else {
            return (ev.serial(0).raw(), 0);
        };
        let serial = wire.u32();
        // surface, x, y -- skipped to reach the offer id.
        let _ = (wire.u32(), wire.u32(), wire.u32());
        (serial, wire.u32())
    }

    /// Opaque windows use the cheaper XRGB format the compositor never has
    /// to blend; a translucent one needs the per-pixel alpha channel.
    const fn buffer_format(format: SurfaceFormat) -> Format {
        if format.needs_alpha() {
            Format::Argb8888
        } else {
            Format::Xrgb8888
        }
    }

    /// Hand any completed drop payload to the painter, then conclude the
    /// transfer.
    ///
    /// The `finish` is what makes the *source* see `dnd_finished` rather
    /// than `cancelled`, so a peer that deletes moved files knows the move
    /// succeeded. It is sent after the painter has acted, so a drop is never
    /// reported complete before it has been.
    fn deliver_drops(&mut self, painter: &mut impl Painter) {
        let mut dropped = Vec::new();
        self.drag.drain_drops(|d| dropped.push(d));
        if dropped.is_empty() {
            return;
        }
        // An empty payload still completes the transfer, but there is
        // nothing for the painter to act on -- a peer that advertised files
        // and then wrote none must not look like a drop of zero files.
        for d in dropped.into_iter().filter(|d| !d.paths.is_empty()) {
            painter.on_files_dropped(d.paths, d.position);
        }
        self.drag.finish_drop(&self.window_tx);
        self.present.mark_dirty();
    }

    /// Route one `RawEvent::Fd` to whichever wait it satisfies: the XKB
    /// keymap (parsed and consumed here), or a drag source's `send` fd
    /// (handed to the drag controller to write into). Borrowed for the
    /// keymap case (mmap'd and never stored); consumed for the drag case.
    fn handle_fd_event(&mut self, fd: Option<&std::os::fd::OwnedFd>) {
        use std::os::fd::AsFd as _;
        if self.keys.is_awaiting_keymap_fd() {
            self.keys
                .absorb_keymap_fd(fd.map(std::slice::from_ref).unwrap_or_default());
            return;
        }
        if self.drag.is_awaiting_send_fd()
            && let Some(fd) = fd
        {
            // `write_send_fd` takes ownership; `take_keymap_fd` only
            // borrows via `AsFd`, so mint an owned duplicate here rather
            // than restructure the caller's borrow for one branch.
            if let Ok(owned) = fd.as_fd().try_clone_to_owned() {
                self.drag.write_send_fd(owned);
            }
            return;
        }
        if self.selection.is_awaiting_send_fd()
            && let Some(fd) = fd
            && let Ok(owned) = fd.as_fd().try_clone_to_owned()
        {
            self.selection.write_send_fd(owned);
        }
    }

    /// Request the pointer and keyboard without waiting for a capabilities
    /// event.
    ///
    /// # Why not wait for `wl_seat.capabilities`
    ///
    /// Because it has already been missed by the time this runs. The seat is
    /// bound during registry discovery, and the compositor answers with
    /// `capabilities` immediately -- but `RegistryDiscovery::collect_through_sync`
    /// only forwards `wl_registry.global` events to the collector and drops
    /// everything else, so that one event is consumed and discarded before
    /// the window's own event loop ever starts. Waiting for it means waiting
    /// forever, which is exactly the silent no-input bug this replaces.
    ///
    /// Asking unconditionally is safe: `wl_seat.get_pointer` on a seat with
    /// no pointer yields an object that simply never sends events, which is
    /// indistinguishable from not having asked. If a device is hot-plugged
    /// later, [`Self::bind_seat_devices`] still handles the follow-up
    /// capabilities event.
    fn bind_seat_devices_eagerly(&mut self) {
        let all = SeatCapability::Pointer.mask() | SeatCapability::Keyboard.mask();
        self.bind_seat_devices(all);
    }

    /// Bind pointer/keyboard when the seat advertises them.
    ///
    /// Guarded on `is_none` because `wl_seat.capabilities` is re-sent
    /// whenever the set changes -- plugging in a second mouse would
    /// otherwise bind a duplicate pointer and deliver every event twice.
    fn bind_seat_devices(&mut self, caps: u32) {
        if SeatCapability::Pointer.in_mask(caps) && self.pointer_id.is_none() {
            let pointer: PointerId = self.window_tx.alloc_id();
            self.window_tx
                .request(MessageBuilder::new(self.seat, SeatRequest::GetPointer).new_id(pointer));
            self.pointer_id = Some(pointer);
        }
        if SeatCapability::Keyboard.in_mask(caps) && self.keyboard_id.is_none() {
            let keyboard: KeyboardId = self.window_tx.alloc_id();
            self.window_tx
                .request(MessageBuilder::new(self.seat, SeatRequest::GetKeyboard).new_id(keyboard));
            self.keyboard_id = Some(keyboard);
        }
    }

    /// Route one `wl_pointer` event.
    fn handle_pointer_event(&mut self, ev: &RawEvent, painter: &mut impl Painter) {
        let mods = self.keyboard.modifiers();
        let emitted = match ev.event::<PointerEvent>() {
            // Enter carries `serial, surface, x, y` -- coordinates start at
            // arg 2, unlike Motion's `time, x, y`.
            Some(PointerEvent::Enter) => {
                // The compositor resets the cursor on every enter, so the
                // cached "already applied" shape is stale now.
                self.cursors.invalidate();
                Some(
                    self.pointer
                        .enter(ev.serial(0).raw(), ev.fixed(2), ev.fixed(3)),
                )
            }
            Some(PointerEvent::Leave) => Some(self.pointer.leave()),
            Some(PointerEvent::Motion) => Some(self.pointer.motion(ev.fixed(1), ev.fixed(2))),
            Some(PointerEvent::Button) => {
                let pressed = ev.arg(3) == 1;
                self.pointer
                    .button(ev.serial(0).raw(), ev.arg(2), pressed, mods)
            }
            Some(PointerEvent::Axis) => self.pointer.axis(ev.arg(1), ev.fixed(2), mods),
            None => None,
        };
        if let Some(kind) = emitted {
            painter.on_pointer(kind);
            self.present.mark_dirty();
        }
    }

    /// Route one `wl_keyboard` event.
    fn handle_keyboard_event(&mut self, ev: &RawEvent, painter: &mut impl Painter) {
        match self.keys.observe(ev) {
            KeyboardUpdate::Entered => {
                self.keyboard.enter();
                painter.on_focus(true);
                self.present.mark_dirty();
            }
            KeyboardUpdate::Left => {
                self.keyboard.leave();
                painter.on_focus(false);
                self.present.mark_dirty();
            }
            KeyboardUpdate::Modifiers(mods) => self.keyboard.set_modifiers(mods),
            KeyboardUpdate::Repeat(info) => self.keyboard.set_repeat_info(info),
            KeyboardUpdate::Key(input) => self.handle_key(input, painter),
            KeyboardUpdate::Ignored => {}
        }
    }

    /// Dispatch one decoded `wl_keyboard.key`.
    fn handle_key(&mut self, input: KeyInput, painter: &mut impl Painter) {
        let key = input.key;
        match input.state {
            KeyState::Pressed => {
                // Deliberately the local monotonic clock, not the event's
                // `time` argument: `wl_keyboard.key` timestamps come from
                // the compositor's own clock with an unspecified epoch, so
                // mixing them with the clock `pump_repeat` reads would make
                // the first repeat fire immediately or never.
                let now = Self::now_ms();
                if let Some(press) = self.keyboard.press(key, now) {
                    painter.on_key(press);
                    self.present.mark_dirty();
                }
            }
            KeyState::Released => self.keyboard.release(key),
        }
    }

    /// Emit any auto-repeat that has come due.
    fn pump_repeat(&mut self, painter: &mut impl Painter) {
        if let Some(press) = self.keyboard.tick(Self::now_ms()) {
            painter.on_key(press);
            self.present.mark_dirty();
        }
    }

    /// Milliseconds since an arbitrary fixed point, from a *monotonic*
    /// clock.
    ///
    /// Monotonic rather than wall-clock so an NTP step or a DST change
    /// cannot stall a held key for an hour, or fire a burst of repeats.
    fn now_ms() -> u64 {
        use std::sync::OnceLock;
        static START: OnceLock<std::time::Instant> = OnceLock::new();
        let start = START.get_or_init(std::time::Instant::now);
        // Saturating rather than a cast: a u64 of milliseconds covers ~584
        // million years, so the clamp is unreachable in practice, but it
        // keeps the conversion total instead of silently wrapping.
        u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// When the next repeat is due, for a caller pacing its own wait.
    #[must_use]
    pub const fn next_repeat_due_ms(&self) -> Option<u64> {
        self.keyboard.next_repeat_due_ms()
    }

    /// How long until the next key auto-repeat is due, or `-1` (block
    /// forever) when no key is held.
    ///
    /// The only deadline an otherwise event-driven loop has: everything else
    /// that wakes it arrives on a descriptor. Pass it to [`Self::wait`] so a
    /// held key repeats at the compositor's rate instead of the loop's.
    #[must_use]
    pub fn repeat_wait_ms(&self) -> i32 {
        let Some(due) = self.next_repeat_due_ms() else {
            return -1;
        };
        let remaining = due.saturating_sub(Self::now_ms());
        // Zero would spin; one millisecond is the shortest honest wait.
        i32::try_from(remaining).unwrap_or(i32::MAX).max(1)
    }

    /// Apply a coalesced resize: reshape the shm pool and tell the owner.
    fn apply_resize(&mut self) -> Result<()> {
        let Some(size) = self.geometry.apply_pending() else {
            return Ok(());
        };
        // The full resize dance, in the order the protocol requires. Getting
        // this wrong is not a crash -- it is the far worse silent failure of
        // a window whose buffers still describe the *old* geometry, so the
        // compositor reads each row at the previous stride and the image
        // shears into diagonal streaks.
        //
        // 1. Destroy the buffers carved out of the old pool layout.
        self.present.destroy_old_buffers(&mut self.window_tx);
        // 2. Re-mmap. A `Some` means the pool had to grow, which the
        //    compositor must be told about before it will honour buffers
        //    that reach past the old end.
        let grew = self
            .present
            .resize_shm(size.width, size.height)
            .map_err(|e| PlatformError::Conn(crate::conn::ConnError::Sys(e)))?;
        if let Some(new_capacity) = grew {
            self.present.grow_pool(&mut self.window_tx, new_capacity);
        }
        // 3. Carve new buffers at the new width/height/stride.
        self.present
            .recreate_buffers(&mut self.window_tx, size.width, size.height);
        // 4. Reset the swap-chain bookkeeping. One row: this window repaints
        //    every pixel each present, so it has no rows to track.
        self.present.reset_after_resize(1);
        self.queue.push_back(WindowEvent::Resized(size));
        Ok(())
    }

    /// Paint and present, if a present is both owed and permitted.
    fn maybe_present(&mut self, painter: &mut impl Painter) {
        // The rule the lifecycle type exists to protect: attaching a buffer
        // before the first configure is acked is a fatal protocol error.
        if !self.lifecycle.can_present() {
            return;
        }
        let PresentAttempt::Ready(slot) = self.present.begin_present() else {
            return;
        };
        let size = self.geometry.physical();

        {
            let mut canvas = self
                .present
                .slot_canvas(slot, size.width, size.height)
                .with_format(self.format);
            canvas.clear(painter.background());
            painter.paint(&mut canvas, size);
        }

        let damage = if self.present.take_full_damage() {
            Damage::full()
        } else {
            painter.damage()
        };
        SurfacePresenter::commit_frame(
            &mut self.present,
            &mut self.window_tx,
            self.shell.surface,
            slot,
            size,
            &damage,
        );
    }

    /// Tear down the surface and join the reader thread.
    ///
    /// # Teardown order is load-bearing
    ///
    /// The reader thread parks on the compositor socket and only returns
    /// when that socket reports EOF. The `Sender` owns the socket fd, so
    /// joining the reader *while still holding the sender* deadlocks until
    /// the reader's own no-progress watchdog gives up 4096 completions
    /// later -- which prints an alarming "connection wedged" line on every
    /// clean exit.
    ///
    /// So: send the destroy requests, flush them, **shut the socket down**,
    /// join the reader, and only then drop the sender.
    ///
    /// Shutting down rather than dropping is the whole point. Both unblock
    /// the reader, but a drop closes the descriptor the reader's in-flight
    /// `recvmsg` was submitted against, so the recv completes `EBADF` and a
    /// clean exit prints an I/O error. `shutdown` ends the connection while
    /// the descriptor stays valid, so the recv completes with a real EOF.
    pub fn close(self) -> Result<()> {
        // Partial move rather than `&mut` field juggling; `Toplevel` has no
        // `Drop` impl, so destructuring is allowed and makes the ordering
        // explicit rather than incidental.
        let Self {
            mut sender,
            mut request_rx,
            window_tx,
            shell,
            reader,
            ..
        } = self;

        shell.destroy(&window_tx);
        let _ = sender.drain_requests(&mut request_rx);
        // Ends the connection so the reader's blocking recv completes with a
        // zero-length read -- real EOF, which its loop already recognises --
        // instead of parking until its watchdog fires. The descriptor stays
        // open until `sender` drops below, *after* the join, so the recv
        // io_uring submitted against this fd number can never fail `EBADF`.
        let _ = sender.shutdown();

        // A reader that dies mid-session is still reported separately:
        // `tick()` observes it via `ReaderHealth` and returns
        // `PlatformError::ReaderStopped`.
        let joined = reader.map_or(Ok(()), |handle| {
            // A panic is never expected, so it still propagates.
            handle.join().unwrap_or(Err(PlatformError::ReaderPanicked))
        });
        // Only now is the descriptor unreferenced by the reader's ring.
        drop(sender);
        joined
    }
}

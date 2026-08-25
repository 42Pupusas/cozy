#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::cast_lossless,
    clippy::option_if_let_else,
    clippy::too_many_lines,
    clippy::needless_pass_by_value,
    clippy::collapsible_if,
    clippy::unnecessary_wraps
)]
//! Terminal window: open a Wayland window and render grid cells via software
//! framebuffer.  Adapted from paint's `app.rs` — all UI / demo logic replaced
//! with grid-cell blitting.

use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc;
use quetzalcoatl::spsc::Consumer;

use crate::conn::{ConnError, Sender, WindowSender};
use crate::cursor;
use crate::draw::Point;
use crate::error::{PlatformError, Result};
use crate::event::RawEvent;
use crate::id::{
    CallbackId, CompositorId, DataDeviceManagerId, KeyboardId, OutputId, PointerId, RegistryId,
    SeatId, ShmId, WmBaseId,
};
use crate::keymap;
use crate::protocol::Modifiers;
use crate::protocol::{
    self as proto, ButtonState, DataDeviceEvent, DataOfferEvent, DataSourceEvent, Format, Key,
    KeyState, KeyboardEvent, PointerButton, PointerEvent, PointerRequest, SeatCapability,
    SeatEvent, SeatRequest, SurfaceRequest, ToplevelEvent, ToplevelRequest, WmBaseEvent,
    XdgSurfaceEvent, XdgSurfaceRequest,
};
use crate::render;
use crate::toplevel::lifecycle::Lifecycle;
use crate::toplevel::shell::ShellSurface;
use crate::wire::MessageBuilder;

const EVENT_RING_CAP: usize = 1024;

/// Stable Wayland `app_id` for our toplevel. Fixed (not the caller's title) so
/// compositor window rules and tooling can match this window reliably even
/// after the shell rewrites the title at runtime.
const APP_ID: &str = "cozy.terminal";

/// Minimum interval between edge-of-window autoscroll steps while a
/// selection drag sits pinned to the top/bottom edge of the content area.
/// Paces the scroll speed independent of how often `tick()` happens to run
/// (which can be much more often than this while animations are in flight).
const AUTOSCROLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(40);

/// How close to the top/bottom edge of the content area (in pixels) the
/// pointer must be, while dragging a selection, to trigger autoscroll.
/// Deliberately a fixed pixel band rather than fractions-of-a-cell: Wayland
/// stops delivering `wl_pointer.motion` the instant the pointer crosses our
/// surface boundary (no cross-surface grab), so "drag past the edge" can
/// only ever be observed as "drag to the last few pixels *inside* the
/// edge" -- there's no coordinate to read once the pointer is actually
/// outside the window.
const AUTOSCROLL_MARGIN_PX: i32 = 12;

/// How long a bell's title-bar flash (see [`TermWindow::bell`]) stays up
/// before [`TermWindow::tick`] reverts it to the real title.
const BELL_FLASH: std::time::Duration = std::time::Duration::from_millis(600);

/// A keyboard/pointer input event from the compositor.
///
/// Produced by [`TermWindow`] for the caller to encode into PTY bytes (M5).
#[derive(Clone, Debug)]
pub enum InputEvent {
    Key {
        key: Option<Key>,
        state: Option<KeyState>,
        modifiers: Modifiers,
    },
    Close,
    Resize {
        cols: u16,
        rows: u16,
    },
    /// Mouse-wheel scroll, in lines (positive = scroll up/away, into history;
    /// negative = scroll down/toward, toward the prompt). Carries the cell
    /// under the pointer and the live modifiers so the consumer can route it:
    /// viewport scrollback scroll, wheel-button mouse report (when the app
    /// negotiated tracking — vim's `mouse=a`), or alternate-scroll arrow keys
    /// (alt screen, no tracking — `less`), which all need different bytes.
    Scroll {
        lines: i32,
        /// 0-indexed column of the cell under the pointer.
        col: u16,
        /// 0-indexed row of the cell under the pointer.
        row: u16,
        modifiers: Modifiers,
    },
    /// Clipboard text read from another client, to be written to the PTY
    /// (a paste). Carries the raw clipboard bytes.
    Paste(Vec<u8>),
    /// A pointer press/release/motion over the content area, for VT mouse
    /// reporting (`CSI M ...` / SGR). Emitted for every button event
    /// regardless of whether the app negotiated mouse tracking -- the
    /// consumer (the `Encoder`) is what decides whether/how to encode it,
    /// same division of labor as `InputEvent::Key`.
    Mouse {
        button: MouseButton,
        kind: MouseEventKind,
        /// 0-indexed column of the cell under the pointer.
        col: u16,
        /// 0-indexed row of the cell under the pointer.
        row: u16,
        modifiers: Modifiers,
    },
    /// Keyboard focus gained/lost (`wl_keyboard.enter`/`leave`), for xterm
    /// focus-event reporting (DECSET 1004: `CSI I` / `CSI O`).
    Focus(bool),
}

/// Mouse button identity.
///
/// Mirrors [`crate::protocol::PointerButton`] plus the wheel/no-button cases
/// `wl_pointer.axis`/motion-with-no-press need. Kept distinct from
/// `protocol::PointerButton` (which only models the wire button codes) so
/// `InputEvent::Mouse` can also carry wheel-scroll and bare-hover reports.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
    /// No button involved -- a bare motion report under any-event tracking.
    None,
}

/// Whether an [`InputEvent::Mouse`] is a fresh press, a release, or a motion
/// report (drag while a button is held, or bare hover under any-event
/// tracking).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MouseEventKind {
    Press,
    Release,
    Motion,
}

#[cfg(feature = "trace")]
use crate::debug_overlay::DebugOverlay;
use crate::surface::Frame;

/// FPS/frame-pacing debug-instrumentation dispatch -- see the module doc
/// there for why this is a separate file with block-level (not per-method)
/// `#[cfg(feature = "trace")]` gating.
mod window_debug;

/// A Wayland window that renders a terminal grid using a software framebuffer.
///
/// Feed [`grid::DirtySnapshot`]s via [`TermWindow::push_dirty`]; the window
/// blits them on the next frame.  Input arrives back through the
/// [`InputEvent`] iterator (`.next_input()`).
// This window owns a dozen genuinely-independent state flags (focused,
// needs_redraw, bracketed-paste, ...) that don't share a state machine --
// bundling them into enums would just be indirection for indirection's sake.
#[allow(clippy::struct_excessive_bools)]
pub struct TermWindow {
    sender: Sender,
    request_rx: mpsc::Consumer<crate::conn::WindowRequest>,
    window_tx: WindowSender,
    events: Consumer<RawEvent>,
    reader: Option<std::thread::JoinHandle<Result<()>>>,
    /// Tracks whether the reader thread still exists. [`Self::run_once`]
    /// blocks on `pop_block` and so learns of a dead reader for free, but
    /// [`Self::tick`]/[`Self::tick_with_painter`] drain with the
    /// non-blocking `pop()` and would otherwise spin forever against a
    /// closed ring after the compositor went away.
    reader_health: crate::ReaderHealth,
    /// Shared wakeup the reader signals after pushing events; also exposed to
    /// the app (via [`TermWindow::waker`]) so the parser thread can wake the UI
    /// loop after producing a dirty snapshot. The UI loop blocks in
    /// [`TermWindow::wait`] on this eventfd instead of busy-polling.
    wake: waker::Waker,

    #[allow(dead_code)]
    compositor: CompositorId,
    #[allow(dead_code)]
    shm: ShmId,
    wm_base: WmBaseId,
    /// The `wl_surface` -> `xdg_surface` -> `xdg_toplevel` chain, shared with
    /// [`crate::toplevel`] rather than rebuilt here. The destroy ordering it
    /// encapsulates is a protocol requirement (a toplevel is a child role
    /// object of its surface), so it is worth having one implementation of
    /// it rather than two that can drift.
    shell: ShellSurface,
    seat: SeatId,
    pointer: Option<PointerId>,
    keyboard: Option<KeyboardId>,

    /// `xdg_activation_v1`, when the compositor offers it. `None` means a
    /// program this window launches opens on whichever workspace it already
    /// occupied -- the behaviour every client had before the protocol.
    activation: Option<crate::toplevel::activation::Activation>,
    /// The serial of the last press or key-down, which is what makes a token
    /// request trustworthy. See [`crate::input_serial::InputSerial`].
    input_serial: crate::input_serial::InputSerial,
    /// Events pulled off the ring during a token exchange that were not the
    /// exchange's own answer.
    ///
    /// They must be routed afterwards rather than dropped: this consumer is
    /// the window's only source of pointer, keyboard, configure and frame
    /// events, and discarding them loses input and stalls presentation.
    deferred: std::collections::VecDeque<RawEvent>,

    /// Keyboard auto-repeat: the held key, negotiated rate/delay, and the
    /// `timerfd` that paces synthesized repeats. Extracted from this struct
    /// so the three fields' lockstep invariants live (and are tested) in
    /// one place; see [`crate::key_repeat::KeyRepeatState`].
    key_repeat: crate::key_repeat::KeyRepeatState,
    modifiers: Modifiers,
    /// Real XKB keymap translation, built from the compositor's
    /// `wl_keyboard.keymap` blob (see `crates/xkb`). `None` until the first
    /// `Keymap` event arrives, or permanently if `libxkbcommon` couldn't be
    /// loaded / the keymap failed to compile -- callers fall back to the
    /// physical-keycode path (`keymap::PhysicalKey::to_logical`) in either
    /// case, so a missing library never blocks the terminal from starting.
    xkb: Option<xkb::Translator>,

    cursor: Option<cursor::PointerCursor>,

    /// Smooth cursor-slide + blink animation state. Advanced by wall-clock
    /// time in [`Self::tick`]; see [`crate::cursor_anim`].
    cursor_anim: crate::cursor_anim::CursorAnimator,
    /// Whether this window is driven by a client-owned painter rather than
    /// the terminal grid renderer, and what that implies for the cursor
    /// animation. See [`crate::cursor_gate::CursorAnimGate`] for why a
    /// client-painted window must not run the animator at all.
    cursor_gate: crate::cursor_gate::CursorAnimGate,
    /// Wall-clock instant of the last [`Self::tick`] call, for computing the
    /// animator's `dt_ms`.
    last_tick: std::time::Instant,

    /// Frame-presentation state: the double-buffered shm swap chain, the
    /// per-slot stale-row bitmap, and the dirty/frame-ready/frame-callback
    /// handshake that gates when a present may fire. Extracted from this
    /// struct so the swap-chain lifecycle lives (and is tested) in one
    /// place; see [`crate::present::PresentState`].
    present: crate::present::PresentState,
    /// The persistent XRGB backbuffer plus the premultiplied-ARGB cache
    /// custom painters read on present. Extracted from this struct so the
    /// pixel-level buffer bookkeeping lives (and is tested) in one place;
    /// see [`crate::pixel_buffers::PixelBuffers`].
    pixels: crate::pixel_buffers::PixelBuffers,
    /// Glyph source (bitmap or loaded TTF/OTF face) + resolved color theme.
    renderer: render::Renderer,
    width: i32,
    height: i32,
    /// The `wl_output`-advertised buffer scale this window opened at (1 =
    /// normal DPI, 2 = `HiDPI` @2x, ...). Fixed for the window's lifetime
    /// (Wayland has no live output-scale-change event we handle -- a
    /// runtime DPI change would need a fresh `wl_surface.enter`/output
    /// lookup we don't do). Used to convert `xdg_toplevel.configure`'s
    /// logical-pixel width/height into the physical pixels our shm buffers
    /// are sized in; see the `Configure` handler.
    scale: i32,

    /// Cell dimensions in pixels (physical -- already multiplied by `scale`;
    /// see [`render::Renderer::with_scale`]).
    cell_w: i32,
    cell_h: i32,

    pointer_pos: Point,
    /// The button currently held down, if any -- carried on `Motion` events
    /// so `ButtonMotion`/`AnyEvent` mouse reporting knows which button (if
    /// any) is down during a drag. `wl_pointer.motion` itself carries no
    /// button info, so this is the only way to know.
    mouse_pressed: Option<MouseButton>,
    /// Whether the running application has negotiated mouse tracking
    /// (`mouse_mode != Off`), mirrored in by the UI loop each iteration.
    /// While true the app owns the pointer: bare clicks/drags are reported to
    /// it and must NOT also drive the terminal's own selection highlight
    /// (clicking in vim used to paint a stray selection over vim's UI).
    app_mouse: bool,
    /// True while a Shift+left drag is running a terminal-side selection
    /// *despite* `app_mouse` (the universal "bypass the app" convention —
    /// kitty/foot/xterm). While set, the left button's press/motion/release
    /// reports are suppressed so the app never sees the orphan half of a
    /// drag it didn't start.
    selection_bypass: bool,
    /// The xdg-shell mapping handshake. Shared with [`crate::toplevel`]
    /// rather than re-encoded here: the rule it protects (no buffer may be
    /// attached before the first configure is acked) is a protocol error
    /// that kills the client, and it is worth having exactly one tested
    /// implementation of it in the crate.
    lifecycle: Lifecycle,

    /// Latest window size proposed by `xdg_toplevel.configure`, applied once per
    /// frame so a drag's flood of configures collapses into one resize.
    pending_resize: Option<(i32, i32)>,
    /// The last cell-grid dimensions (cols, rows) we handed to the grid via an
    /// `InputEvent::Resize`. A window drag emits a flood of pixel-level
    /// configures whose sub-cell jitter leaves the *cell* count unchanged;
    /// re-reflowing the whole grid on each of those is what made the text
    /// judder. We only emit a fresh grid Resize when this actually changes.
    last_grid_size: (u16, u16),
    /// Pending dirty snapshots to blit on the next frame.
    pending_dirty: Vec<grid::DirtySnapshot>,
    /// Pending input events for the caller.
    input_queue: std::collections::VecDeque<InputEvent>,

    // ── Clipboard & selection ────────────────────────────────────────────
    /// Copy/paste over `wl_data_device`: the device, our outgoing source,
    /// the peer's offer, and the paste helper-thread channel. Extracted from
    /// this struct so the fiddly wire-protocol bookkeeping lives (and is
    /// tested) in one place; see [`crate::clipboard::ClipboardState`].
    clipboard: crate::clipboard::ClipboardState,
    /// The text-selection model: what's selected, the on-screen cell mirror it
    /// selects from, and the drag/scroll-follow state. Extracted from this
    /// struct so the selection logic lives (and is tested) in one place; see
    /// [`crate::selection::SelectionTracker`].
    selection: crate::selection::SelectionTracker,
    /// Classifies left-button presses as single or double clicks, so a
    /// double-click can select the word under the pointer. Kept beside the
    /// selection (rather than inside it) because click *timing* is a pointer
    /// concern; see [`crate::multi_click::MultiClickTracker`].
    multi_click: crate::multi_click::MultiClickTracker,
    /// Set when a `wl_keyboard.keymap` event just arrived; the next
    /// `RawEvent::Fd` is the keymap blob to `mmap` + compile via
    /// `crate::xkb`. Carries the event's `size` argument (the fd itself has
    /// no length of its own -- the wire event tells us how much to map).
    awaiting_keymap_fd: Option<usize>,
    /// Viewport rows that carried a highlight on the previous frame. Repainted
    /// clean on the next frame before the (possibly moved/cleared) selection
    /// is re-tinted, so highlight never lingers after deselect or drag.
    prev_selection_rows: Vec<u16>,
    /// What the pointer is over while Ctrl is held (`None` for "nothing"),
    /// either a real OSC 8 link or a detected plain-text URL -- see
    /// [`crate::link_scan::LinkTarget`]. Paired with the viewport row
    /// hover-underline was last drawn on (so the next frame knows what to
    /// repaint clean before redrawing or removing it). Set from pointer
    /// motion in `handle_pointer`; consumed by
    /// [`Self::apply_link_hover_underline`] in the present path.
    hover_link: Option<crate::link_scan::LinkTarget>,
    prev_hover_row: Option<u16>,
    /// Viewport rows the cursor overlay touched on the previous frame.
    /// Repainted clean before drawing the cursor at its new (possibly
    /// mid-slide) position, the same idempotent scheme as
    /// `prev_selection_rows` -- without this a sliding cursor would leave a
    /// trail of tinted cells behind it.
    prev_cursor_rows: Vec<u16>,

    /// FPS/frame-pacing debug instrumentation -- mode, rolling stats, and
    /// the overlay text's own dirty-row bookkeeping, fixed at startup from
    /// `TERMINAL_DEBUG_FPS`. Compiled out entirely without `trace` (see
    /// [`DebugOverlay`]), so a release build carries zero extra state or
    /// per-frame branches for it.
    #[cfg(feature = "trace")]
    debug: DebugOverlay,

    /// Wall-clock instant of the last edge-of-window autoscroll step taken
    /// while a selection drag's pointer sits pinned to the top/bottom edge
    /// of the content area. `None` when not currently autoscrolling (no
    /// drag, or the pointer isn't at an edge) -- a fresh press or a pointer
    /// move off the edge resets it so the next edge-pin starts its own
    /// pacing window rather than firing immediately off a stale timestamp.
    autoscroll_last: Option<std::time::Instant>,

    /// The real window title, as last set via [`Self::set_title`] (OSC 0/2).
    /// Restored after a bell flash (see [`Self::bell`]) expires.
    base_title: String,
    /// Deadline for the current bell title-flash, if one is showing; `None`
    /// when no bell is pending/active. Checked once per [`Self::tick`].
    bell_until: Option<std::time::Instant>,
}

impl Drop for TermWindow {
    /// Tear the window down explicitly so the compositor removes it promptly
    /// instead of waiting to notice the socket close. Destroy xdg objects
    /// before the underlying `wl_surface` (xdg-shell requires that order), all
    /// best-effort — we're shutting down, so errors are irrelevant.
    fn drop(&mut self) {
        // Same three requests, in the same protocol-mandated order, as
        // `ShellSurface::destroy` -- but issued through the `Sender` rather
        // than the `WindowSender` that method takes, because at drop time we
        // own the connection outright and want the requests flushed on the
        // way out.
        let _ = self.sender.request(MessageBuilder::new(
            self.shell.toplevel,
            ToplevelRequest::Destroy,
        ));
        let _ = self.sender.request(MessageBuilder::new(
            self.shell.xdg_surface,
            XdgSurfaceRequest::Destroy,
        ));
        let _ = self.sender.request(MessageBuilder::new(
            self.shell.surface,
            SurfaceRequest::Destroy,
        ));
    }
}

/// Result of [`TermWindow::left_button_bypass`]'s ownership decision for one
/// `wl_pointer.button` event -- whether Shift was held, whether this press
/// starts a terminal-selection bypass, and whether the app's own report
/// should be suppressed. A named struct rather than a 3-tuple purely so the
/// call site in `handle_pointer_button`/`drive_selection_from_left_button`
/// reads `bypass.starts` instead of `bypass.0`.
struct LeftButtonBypass {
    shift: bool,
    starts: bool,
    suppress_report: bool,
}

impl TermWindow {
    /// Open a Wayland terminal window of `cols × rows` cells.
    ///
    /// `wake` is the shared UI wakeup channel: the Wayland reader signals it
    /// after pushing events, and the caller hands clones to any other producer
    /// (e.g. the parser thread) so the UI loop's [`wait`](Self::wait) unblocks.
    ///
    /// `build_renderer` receives the bound `wl_output`'s integer buffer scale
    /// (1 if the compositor advertised none, or advertised no output at all)
    /// and must return a [`render::Renderer`] built at that scale -- normally
    /// `|scale| render::Renderer::with_scale(theme, font_cfg, scale)`. The
    /// renderer can't be built before this because it needs the scale to
    /// rasterize the font at native pixel density (see
    /// [`render::Renderer::with_scale`]); learning the scale, in turn,
    /// requires an open connection with `wl_output` bound, which is exactly
    /// the setup this function does.
    pub fn new(
        title: &str,
        cols: u16,
        rows: u16,
        wake: waker::Waker,
        cursor_cfg: crate::cursor_anim::CursorAnimConfig,
        build_renderer: impl FnOnce(i32) -> render::Renderer,
    ) -> Result<Self> {
        let (mut sender, repeat_timer, mut events, reader_handle) =
            crate::surface::SurfaceBootstrap::connect(
                EVENT_RING_CAP,
                "terminal-wayland-reader",
                &wake,
            )?;

        let Globals {
            compositor,
            shm,
            wm_base,
            seat,
            output,
            data_device_manager,
            activation,
        } = Registry::discover(&mut sender, &mut events)?;
        // Ask the compositor what buffer scale this output actually wants
        // (1 = normal DPI, 2 = HiDPI @2x, ...) *before* building the renderer,
        // so the font rasterizes at native pixel density instead of being
        // upscaled/blurred later by the compositor scaling our 1x buffer up
        // to fill the physical pixels. No output bound (rare, but some
        // headless/nested compositors don't advertise one at startup) means
        // we can't ask, so fall back to scale 1 -- the pre-HiDPI-support
        // behavior, not a regression.
        let scale = match output {
            Some(o) => Registry::output_scale(&mut sender, &mut events, o),
            None => 1,
        };
        let renderer = build_renderer(scale);

        let cell_w = renderer.cell_w();
        let cell_h = renderer.cell_h();
        let width = cols as i32 * cell_w;
        let height = rows as i32 * cell_h;

        // A translucent window (window.opacity < 1.0) needs per-pixel alpha
        // (Argb8888); an opaque one keeps the cheaper Xrgb8888 the compositor
        // doesn't have to blend -- zero behavior change for the common case.
        let buf_format = if renderer.needs_alpha() {
            Format::Argb8888
        } else {
            Format::Xrgb8888
        };
        let (frames, shm_mem, pool) =
            Frame::create_pair(&mut sender, shm, width, height, buf_format)?;

        // `decoration_manager: None` is deliberate, not an oversight: the
        // terminal has never asked for server-side decorations, and this
        // convergence is meant to be invisible on screen. Passing the real
        // manager here would hand the window a compositor titlebar it did
        // not have before -- a visible behaviour change that belongs in its
        // own commit, judged on its own merits.
        let shell =
            ShellSurface::create(&mut sender, compositor, wm_base, None, scale, title, APP_ID)?;

        let cursor = cursor::PointerCursor::create(&mut sender, shm, compositor, scale).ok();

        let (req_tx, req_rx) =
            mpsc::RingBuffer::<crate::conn::WindowRequest>::new(Capacity::exact(64)).split();
        let (id_start, id_end) = sender.alloc_id_block(256);
        let mut window_tx = WindowSender {
            tx: req_tx,
            next_id: id_start,
            id_end,
            free_ids: Vec::new(),
        };

        let clipboard =
            crate::clipboard::ClipboardState::new(data_device_manager, seat, &mut window_tx);

        // `PixelBuffers::new` uses `bg_packed()`, not `bg_xrgb()`: a
        // translucent window's initial backbuffer must carry the configured
        // alpha from frame one, not flash fully opaque until the first real
        // paint.
        let pixels = crate::pixel_buffers::PixelBuffers::new(
            width,
            height,
            renderer.bg_packed(),
            renderer.background_alpha(),
        );
        // Both shm frames start as blank memory (freshly mmap'd), so every
        // row is "stale" relative to the backbuffer until the first present
        // copies it in -- `PresentState::new` seeds that.
        let row_count = ((height + cell_h - 1) / cell_h).max(1) as usize;
        let present =
            crate::present::PresentState::new(frames, shm_mem, pool, buf_format, row_count);

        Ok(Self {
            sender,
            request_rx: req_rx,
            window_tx,
            events,
            reader: Some(reader_handle),
            reader_health: crate::ReaderHealth::new(),
            wake,
            compositor,
            shm,
            wm_base,
            shell,
            seat,
            pointer: None,
            keyboard: None,
            activation: activation.map(crate::toplevel::activation::Activation::new),
            input_serial: crate::input_serial::InputSerial::new(),
            deferred: std::collections::VecDeque::new(),
            key_repeat: crate::key_repeat::KeyRepeatState::new(repeat_timer),
            modifiers: Modifiers::default(),
            xkb: None,
            cursor,
            present,
            pixels,
            renderer,
            width,
            height,
            scale,
            cell_w,
            cell_h,
            pointer_pos: Point::new(0, 0),
            mouse_pressed: None,
            app_mouse: false,
            selection_bypass: false,
            lifecycle: Lifecycle::new(),
            pending_resize: None,
            last_grid_size: (cols, rows),
            pending_dirty: Vec::new(),
            input_queue: std::collections::VecDeque::new(),
            clipboard,
            selection: crate::selection::SelectionTracker::new(),
            multi_click: crate::multi_click::MultiClickTracker::new(),
            awaiting_keymap_fd: None,
            prev_selection_rows: Vec::new(),
            hover_link: None,
            prev_hover_row: None,
            prev_cursor_rows: Vec::new(),
            cursor_anim: crate::cursor_anim::CursorAnimator::new(cursor_cfg),
            cursor_gate: crate::cursor_gate::CursorAnimGate::new(),
            last_tick: std::time::Instant::now(),
            #[cfg(feature = "trace")]
            debug: DebugOverlay::new(),
            autoscroll_last: None,
            base_title: title.to_string(),
            bell_until: None,
        })
    }

    /// Request a full repaint for clients that draw their own canvas through
    /// [`Self::tick_with_painter`] instead of pushing terminal grid snapshots.
    pub const fn request_redraw(&mut self) {
        self.present.mark_dirty();
    }

    /// Physical output scale used by this toplevel's framebuffer and renderer.
    #[must_use]
    pub const fn scale(&self) -> i32 {
        self.scale
    }

    /// Queue a dirty snapshot to be blitted on the next frame.
    pub fn push_dirty(&mut self, snap: grid::DirtySnapshot) {
        if !snap.is_empty() {
            // Fold the snapshot into the selection model: it scroll-follows the
            // selection (both viewport and content scrolls) and keeps its cell
            // mirror in sync with the latest drawn rows.
            self.selection.ingest_snapshot(&snap);
            // Feed the cursor animator the grid's authoritative cursor state
            // -- this is what lets it start a slide even on a frame where no
            // cell content changed (a bare cursor-move escape).
            self.cursor_anim
                .set_target(snap.cursor.col, snap.cursor.row, snap.cursor.visible);
            self.pending_dirty.push(snap);
            self.present.mark_dirty();
        }
    }

    /// Pull the next pending input event, if any.
    pub fn next_input(&mut self) -> Option<InputEvent> {
        self.input_queue.pop_front()
    }

    /// The grid cell under the pointer, in viewport coordinates, or `(0,0)`
    /// when the pointer is outside the content area.
    fn pointer_cell(&self) -> (u16, u16) {
        let col = (self.pointer_pos.x / self.cell_w).clamp(0, self.width / self.cell_w - 1);
        let row = (self.pointer_pos.y / self.cell_h).clamp(0, self.height / self.cell_h - 1);
        (col as u16, row as u16)
    }

    /// Extract the currently selected text (viewport coords) from the cell
    /// mirror. `None` if there's no non-empty selection.
    #[must_use]
    pub fn selected_text(&self) -> Option<String> {
        self.selection.selected_text()
    }

    /// The OSC 8 `link_id` of the cell at viewport `(col, row)`, from the same
    /// on-screen cell mirror `selected_text` reads -- `0` ("no link") if the
    /// cell is out of bounds, blank, or genuinely unlinked. Used to decide
    /// whether a Ctrl+click should open a URL, and to drive the
    /// hover-underline.
    ///
    /// Only sees *real* OSC 8 links -- callers that also want to catch
    /// plain-text URLs (no application-side escape at all, e.g. `trunk
    /// serve`'s banner) should go through [`Self::link_target_at`] instead.
    #[must_use]
    pub fn link_id_at(&self, col: u16, row: u16) -> u32 {
        self.selection.link_id_at(col, row)
    }

    /// What's clickable at viewport `(col, row)`: a real OSC 8 link if the
    /// cell carries one, else a plain-text URL if [`crate::link_scan`]
    /// detects one running through this column, else `None`. Checking the
    /// real link first is what lets `UrlScanner::scan_line` skip
    /// already-linked cells cheaply (see its doc comment) without this
    /// caller needing to duplicate that precedence.
    #[must_use]
    pub fn link_target_at(&self, col: u16, row: u16) -> Option<crate::link_scan::LinkTarget> {
        let id = self.link_id_at(col, row);
        if id != 0 {
            return Some(crate::link_scan::LinkTarget::Real(id));
        }
        let line = self.selection.row_content(row)?.materialize_cells();
        crate::link_scan::UrlScanner::find_at(&line, col).map(crate::link_scan::LinkTarget::Url)
    }

    /// Recompute which link (if any) is under the pointer, and mark the
    /// window dirty if that changed the on-screen hover-underline. Called on
    /// every pointer motion.
    ///
    /// The underline itself is shown unconditionally (not gated on Ctrl) --
    /// matching kitty/`WezTerm`: it's the "this is clickable" affordance a
    /// user needs *before* deciding to hold Ctrl and click, so gating it on
    /// Ctrl already being held made it useless (you'd only ever see the
    /// underline on a link you'd already committed to clicking). Ctrl is
    /// still required to actually *open* it -- see
    /// [`crate::window::TermWindow`]'s caller of `try_open_hyperlink` (in
    /// `bins/terminal`), which checks the modifier at click time instead.
    fn update_hover_link(&mut self, col: u16, row: u16) {
        let new = self.link_target_at(col, row);
        if new != self.hover_link {
            self.hover_link = new;
            self.present.mark_dirty();
        }
    }

    /// Copy the current selection to the system clipboard (Ctrl+Shift+C).
    /// Publishes a `wl_data_source` offering `text/plain` and serves
    /// `clipboard_text` to peers on request. No-op without a selection or
    /// clipboard support.
    pub fn copy_selection(&mut self) {
        let Some(text) = self.selected_text() else {
            return;
        };
        if text.is_empty() {
            return;
        }
        self.write_clipboard(text);
    }

    /// Publish arbitrary `text` to the system clipboard, the same way
    /// [`Self::copy_selection`] publishes the visual selection. Split out so
    /// OSC 52 (`ESC ] 52 ; c ; <base64> BEL`, decoded by the caller) can push
    /// a program-requested clipboard write through the identical
    /// `wl_data_source` plumbing instead of duplicating it.
    pub fn write_clipboard(&mut self, text: String) {
        self.clipboard.copy(&mut self.window_tx, text);
    }

    /// Request a paste from the system clipboard (Ctrl+Shift+V): ask the
    /// current offer's source to transfer `text/plain` over a pipe. See
    /// [`crate::clipboard::ClipboardState::request_paste`] for the fd/thread
    /// mechanics.
    pub fn request_paste(&self) {
        self.clipboard.request_paste(&self.window_tx, &self.wake);
    }

    /// A clone of the UI wakeup channel. Hand this to any thread (e.g. the
    /// parser) that produces work the UI loop consumes; calling
    /// [`waker::Waker::signal`] on it wakes a [`wait`](Self::wait)ing UI loop.
    #[must_use]
    pub fn waker(&self) -> waker::Waker {
        self.wake.clone()
    }

    /// Block until the reader pushes a Wayland event, a signaller wakes us, or
    /// `timeout_ms` elapses (negative = forever). Drains the wakeup counter
    /// before returning so the next `wait` parks correctly. The caller must
    /// drain its rings (events, dirty) *after* this returns — any signal that
    /// races in between leaves the eventfd counter non-zero, so the following
    /// `wait` returns immediately rather than sleeping through the work.
    pub fn wait(&self, timeout_ms: i32) {
        let _ = self.wake.wait(timeout_ms);
        self.wake.drain();
    }

    /// Process pending Wayland events (non-blocking).
    /// Returns `true` while the window should stay open.
    pub fn tick(&mut self) -> Result<bool> {
        self.tick_inner(None)
    }

    /// Process events and paint a client-owned canvas while retaining the
    /// normal xdg-shell lifecycle and resize handling. The painter reports the
    /// changed pixel rectangle so software clients can animate a small region
    /// without copying an otherwise-static full window every frame.
    pub fn tick_with_painter(
        &mut self,
        mut paint: impl FnMut(&mut crate::draw::Canvas<'_>) -> crate::draw::Damage,
    ) -> Result<bool> {
        // Latch the window's role on first use: from here on the terminal
        // cursor animation is suppressed. See `CursorAnimGate`.
        self.cursor_gate.note_custom_paint();
        self.tick_inner(Some(&mut paint))
    }

    fn tick_inner(
        &mut self,
        custom_paint: Option<&mut dyn FnMut(&mut crate::draw::Canvas<'_>) -> crate::draw::Damage>,
    ) -> Result<bool> {
        if self.lifecycle.is_closing() {
            return Ok(false);
        }
        self.sender
            .drain_requests(&mut self.request_rx)
            .map_err(PlatformError::Conn)?;

        // Deferred events first, in arrival order: they came off the ring
        // before anything still queued, and routing them late would reorder
        // input against the events that followed them.
        self.handle_deferred()?;
        if self.lifecycle.is_closing() {
            return Ok(false);
        }

        // Drain all queued events non-blocking.
        while let Some(ev) = self.events.pop() {
            self.handle_event(ev)?;
            if self.lifecycle.is_closing() {
                return Ok(false);
            }
        }

        // Drain first, *then* ask whether the reader is gone: it may have
        // pushed the `wl_display.error` explaining its exit, and the loop
        // above must consume that before we tear down.
        if !self
            .reader_health
            .observe(self.events.is_closed(), self.events.is_empty())
            .is_alive()
        {
            self.lifecycle.close_requested();
            return Ok(false);
        }

        // Drain any paste bytes a helper thread finished reading.
        let input_queue = &mut self.input_queue;
        self.clipboard
            .drain_pastes(|bytes| input_queue.push_back(InputEvent::Paste(bytes)));

        // Revert a bell's title-bar flash once it's had its moment.
        if self
            .bell_until
            .is_some_and(|until| std::time::Instant::now() >= until)
        {
            self.bell_until = None;
            let title = self.base_title.clone();
            self.shell.set_title(&self.window_tx, &title);
        }

        self.advance_autoscroll();
        self.advance_cursor_anim();
        self.maybe_present(custom_paint)?;
        Ok(true)
    }

    /// Step the cursor slide/blink animation forward by the wall-clock time
    /// elapsed since the last tick, and mark the window dirty if anything
    /// visibly moved. Called once per [`Self::tick`]/[`Self::run_once`], so
    /// its cadence is exactly the UI loop's -- the caller
    /// ([`Self::wants_animation_frame`]) is responsible for polling often
    /// enough (~60fps) while an animation is in flight instead of idling on
    /// the full event-wait timeout.
    fn advance_cursor_anim(&mut self) {
        // A client-painted window draws no terminal cursor, so advancing the
        // animator could only ever produce spurious `mark_dirty` calls --
        // and because blink never stops animating, it would produce one on
        // *every* tick, forcing a full present at the tick rate forever.
        // Bail before touching `last_tick`: it is only ever read to compute
        // this function's own `dt_ms`, so leaving it stale is harmless, and
        // it keeps this a pure early-out.
        if !self.cursor_gate.may_advance_cursor() {
            return;
        }
        let now = std::time::Instant::now();
        let dt_ms = now.duration_since(self.last_tick).as_secs_f32() * 1000.0;
        self.last_tick = now;
        // Cap dt so a long stall (window minimized, debugger pause, ...)
        // doesn't make the cursor jump-slide in one giant step on resume.
        let dt_ms = dt_ms.min(100.0);
        if self.cursor_anim.advance(dt_ms) {
            self.present.mark_dirty();
        }
    }

    /// Rebuilds the damaged rows of the premultiplied custom-painter cache
    /// from the XRGB backbuffer. This runs on every present of a translucent
    /// custom-painted window (e.g. `monitor` under compositor opacity < 1.0);
    /// see [`crate::pixel_buffers::PixelBuffers::update_custom_argb`] for the
    /// SIMD kernel this delegates to.
    fn update_custom_argb(&mut self, damage: crate::draw::Rect) {
        if !self.renderer.needs_alpha() {
            return;
        }
        let alpha = self.renderer.background_alpha();
        self.pixels.update_custom_argb(self.width, damage, alpha);
    }

    /// Whether the caller should keep polling at animation-frame-rate
    /// (~60fps) instead of blocking on [`Self::wait`]'s full timeout -- true
    /// while the cursor is mid-slide, blinking and visible, or a selection
    /// drag is pinned at the top/bottom edge and needs to keep autoscrolling
    /// (see [`Self::advance_autoscroll`]) -- without this the UI loop would
    /// happily block up to 100ms between autoscroll steps, into a wait that
    /// nothing but a wall-clock timer wakes it from.
    #[must_use]
    pub fn wants_animation_frame(&self) -> bool {
        // The gate keeps this in agreement with `advance_cursor_anim`: a
        // client-painted window never advances the animator, so its
        // permanently-true blink must not be reported here.
        self.cursor_gate
            .wants_animation_frame(self.cursor_anim.is_animating(), self.autoscroll_active())
    }

    /// Whether the pointer currently sits within [`AUTOSCROLL_MARGIN_PX`] of
    /// the top or bottom edge of the content area while a selection drag is
    /// held -- the condition [`Self::advance_autoscroll`] acts on. Checked
    /// against a margin band (not the literal edge) since Wayland never
    /// reports pointer coordinates outside our surface -- see
    /// [`AUTOSCROLL_MARGIN_PX`].
    const fn autoscroll_active(&self) -> bool {
        self.selection.is_dragging()
            && (self.pointer_pos.y < AUTOSCROLL_MARGIN_PX
                || self.pointer_pos.y >= self.height - AUTOSCROLL_MARGIN_PX)
    }

    /// Edge-of-window autoscroll: while a selection drag's pointer sits
    /// pinned above the top or below the bottom of the content area (e.g. the
    /// user dragged past the window edge without releasing), keep scrolling
    /// the viewport a line at a time -- paced by [`AUTOSCROLL_INTERVAL`] so it
    /// doesn't fly by at tick-rate -- and re-pin the selection head to
    /// whichever row is now at that edge. Re-pinning matters because a
    /// viewport scroll's `shift()` (see [`crate::selection::SelectionTracker`])
    /// keeps the *existing* head glued to its text, sliding it away from the
    /// edge as new rows appear there; calling `drag_to` again every step pulls
    /// it back to the edge, so the net effect is the selection keeps biting
    /// into freshly revealed rows for as long as the pointer stays pinned --
    /// exactly like continuing to drag past the edge would, without the
    /// pointer actually having to move there.
    fn advance_autoscroll(&mut self) {
        if !self.autoscroll_active() {
            self.autoscroll_last = None;
            return;
        }
        let dir = if self.pointer_pos.y < AUTOSCROLL_MARGIN_PX {
            1
        } else {
            -1
        };
        let now = std::time::Instant::now();
        if self
            .autoscroll_last
            .is_some_and(|last| now.duration_since(last) < AUTOSCROLL_INTERVAL)
        {
            return;
        }
        self.autoscroll_last = Some(now);
        self.input_queue.push_back(InputEvent::Scroll {
            lines: dir,
            col: 0,
            row: 0,
            modifiers: self.modifiers,
        });
        let (cc, cr) = self.pointer_cell();
        if self.selection.drag_to((cc, i32::from(cr))) {
            self.present.mark_dirty();
        }
    }

    /// Mirror in whether the running app has negotiated mouse tracking
    /// (`mouse_mode != Off`). Called by the UI loop each iteration from the
    /// parser thread's `TermModes` snapshot. While set, the app owns bare
    /// left-clicks (no terminal selection; Shift+click bypasses). On the
    /// falling edge any in-progress bypass drag ends naturally via release.
    pub const fn set_app_mouse(&mut self, on: bool) {
        self.app_mouse = on;
        if !on {
            self.selection_bypass = false;
        }
    }

    /// Notify the cursor animator of user activity (a keystroke): resets the
    /// blink phase to full opacity so the cursor is always solid the instant
    /// you type, never mid-fade. Call this whenever the caller sends bytes to
    /// the PTY in response to a key press.
    pub const fn notify_activity(&mut self) {
        self.cursor_anim.on_activity();
        self.present.mark_dirty();
    }

    /// The `wl_output`-advertised buffer scale this window opened at, for
    /// rebuilding a [`render::Renderer`] at the right physical pixel density
    /// (e.g. after a config-file live-reload picks a new font).
    #[must_use]
    pub const fn output_scale(&self) -> i32 {
        self.scale
    }

    /// Swap in a freshly-built renderer (new theme/font/opacity) and force a
    /// full repaint so the new theme actually reaches the screen -- without
    /// this, only the next content change would repaint, leaving already-
    /// drawn rows showing colors from the *old* theme until they happen to
    /// scroll or get overwritten. Used by config live-reload; the caller
    /// builds the new `Renderer` (needs `output_scale()` for `HiDPI`, and may
    /// need to reopen a Wayland buffer if opacity's opaque/translucent split
    /// changed -- see [`Self::needs_alpha_changed`]) and hands it over here.
    pub fn set_renderer(&mut self, renderer: render::Renderer) {
        self.renderer = renderer;
        self.present.mark_dirty();
        // A full repaint needs every visible row queued as if freshly dirtied,
        // not just whatever changed since the last frame -- otherwise rows
        // that haven't changed content-wise (but need their pixels redrawn in
        // the new theme) are silently skipped by `repaint_rows`.
        self.pending_dirty.clear();
        self.present.mark_all_stale();
    }

    /// Live-patch one palette index (an OSC 4 write) and force a full
    /// repaint, same as [`Self::set_renderer`] but without rebuilding the
    /// font/glyph atlas -- OSC 4 only ever changes color, never the font, so
    /// there's no reason to pay atlas-rebuild cost for it. Any cell already
    /// on screen using index `n` needs repainting in the new color, hence
    /// the same "mark every row stale" dance `set_renderer` does.
    pub fn apply_palette_change(&mut self, n: u8, rgb: (u8, u8, u8)) {
        self.renderer.set_palette_color(n, rgb);
        self.present.mark_dirty();
        self.pending_dirty.clear();
        self.present.mark_all_stale();
    }

    /// Whether swapping to `renderer` would change the opaque/translucent
    /// `wl_shm` buffer format (`Xrgb8888` vs. `Argb8888`) this window was
    /// opened with. A live-reload that only touches colors never trips this;
    /// one that changes `[window] opacity` across the 1.0 boundary does, and
    /// the caller needs to know since the buffer format is otherwise fixed
    /// for the window's lifetime (see the `buf_format` field doc) --
    /// switching it live isn't implemented, so that specific edge case still
    /// needs a restart.
    #[must_use]
    pub fn needs_alpha_changed(&self, renderer: &render::Renderer) -> bool {
        renderer.needs_alpha() != (self.present.buf_format() == Format::Argb8888)
    }

    /// Update the cursor animation config (shape/blink/period) in place --
    /// used by config live-reload, which can change `[cursor]` without
    /// needing a full renderer rebuild.
    pub const fn set_cursor_config(&mut self, cfg: crate::cursor_anim::CursorAnimConfig) {
        self.cursor_anim.set_config(cfg);
        self.present.mark_dirty();
    }

    /// Set the window title (OSC 0/2: `ESC ] 0 ; text BEL` /
    /// `ESC ] 2 ; text BEL`). Best-effort -- a request-send failure just
    /// means the compositor doesn't see the new title, not a fatal error for
    /// the terminal. Remembers `title` as the base title so a bell flash
    /// (see [`Self::bell`]) knows what to restore afterward.
    pub fn set_title(&mut self, title: &str) {
        self.base_title = title.to_string();
        self.shell.set_title(&self.window_tx, title);
    }

    /// Mint an activation token for a program this window is about to
    /// launch, so the launched window may raise itself.
    ///
    /// Call this from the handler for the input event that caused the launch
    /// -- a Ctrl+click on a link. The token carries that event's serial and
    /// this window's surface, which together are the compositor's own
    /// evidence that a user acted.
    ///
    /// Returns `None` when the compositor offers no `xdg_activation_v1`,
    /// when no input has been seen yet, or when the compositor does not
    /// answer in time. All three mean "launch without a token": the child
    /// still starts, it simply appears where it already lived.
    ///
    /// `app_id` names the program being launched, not this one.
    pub fn activation_token(
        &mut self,
        app_id: Option<&str>,
    ) -> Option<crate::toplevel::activation::ActivationToken> {
        let activation = self.activation?;
        let serial = self.input_serial.get()?;
        // The id must come from this window's own reserved block: an id from
        // anywhere else names an object the connection already handed out,
        // and the compositor kills the connection over it.
        let token: crate::id::ActivationTokenId = self.window_tx.alloc_id();
        let result = activation.request(
            token,
            &mut self.sender,
            &mut self.events,
            &mut self.deferred,
            crate::toplevel::activation::LaunchEvidence {
                seat: self.seat,
                surface: self.shell.surface,
                serial,
                app_id,
            },
        );
        // A failed exchange must not take the terminal down: the launch
        // proceeds unactivated, which is the pre-protocol behaviour.
        result.unwrap_or_default()
    }

    /// Whether the compositor offers `xdg_activation_v1` at all.
    ///
    /// Separates the two silent reasons a token can be absent, so a caller
    /// reporting the outcome can say which one happened.
    #[must_use]
    pub const fn supports_activation(&self) -> bool {
        self.activation.is_some()
    }

    /// BEL (C0 0x07): visual bell. xdg-shell's core protocol has no
    /// urgency-hint/attention-demand request, so this is implemented as a
    /// brief title-bar flash (prefixing a bell glyph), which the compositor
    /// and any taskbar/dock surfaces it -- the same fallback approach other
    /// terminals take absent a native urgency hint. [`Self::tick`] reverts
    /// it after [`BELL_FLASH`]. Repeated bells while one is already showing
    /// just restart the timer rather than stacking indicators.
    pub fn bell(&mut self) {
        if self.bell_until.is_none() {
            let flash = format!("\u{1f514} {}", self.base_title);
            self.shell.set_title(&self.window_tx, &flash);
        }
        self.bell_until = Some(std::time::Instant::now() + BELL_FLASH);
    }

    /// Block for the next Wayland event, then drain all queued events.
    /// Returns `true` while the window should stay open.
    pub fn run_once(&mut self) -> Result<bool> {
        if self.lifecycle.is_closing() {
            return Ok(false);
        }
        self.sender
            .drain_requests(&mut self.request_rx)
            .map_err(PlatformError::Conn)?;

        // A token exchange may have left events here. Route them before
        // blocking: they are already older than anything the ring will
        // deliver, and blocking on a ring that may stay quiet would strand
        // input the user already produced.
        self.handle_deferred()?;
        if self.lifecycle.is_closing() {
            return Ok(false);
        }

        let Some(first) = self.events.pop_block() else {
            return Ok(false);
        };
        self.handle_event(first)?;
        while let Some(ev) = self.events.pop() {
            self.handle_event(ev)?;
            if self.lifecycle.is_closing() {
                break;
            }
        }
        self.advance_cursor_anim();
        self.maybe_present(None)?;
        Ok(!self.lifecycle.is_closing())
    }

    // ── private ───────────────────────────────────────────────────────────

    /// Route everything a token exchange set aside, oldest first.
    ///
    /// `pop_front` per iteration rather than draining into a local: the queue
    /// is only ever appended to by [`Self::activation_token`], which the
    /// window's own event handlers never call, so nothing can grow it while
    /// this walks it.
    fn handle_deferred(&mut self) -> Result<()> {
        while let Some(ev) = self.deferred.pop_front() {
            self.handle_event(ev)?;
            if self.lifecycle.is_closing() {
                return Ok(());
            }
        }
        Ok(())
    }

    /// Consume the fd batch that followed a clipboard `send`/`receive`, or a
    /// `wl_keyboard.keymap` event. `send` → we write our `clipboard_text` to
    /// the peer's pipe; `receive` (our paste request) → we read the peer's
    /// clipboard from the pipe; `keymap` → we `mmap` + compile it via
    /// `crate::xkb` so subsequent `Key` events resolve through the real XKB
    /// state instead of the physical-keycode fallback.
    /// Resolve one `wl_keyboard.key` press to our logical [`Key`] enum.
    ///
    /// `protocol::Key` is a fixed per-physical-key enum (letters carry their
    /// QWERTY identity, not an arbitrary Unicode codepoint -- that
    /// translation happens one layer up, in `bins/terminal`'s
    /// `ToInputKey`), so full non-Latin/remapped-letter layout support isn't
    /// representable here without a wider plumbing change. What XKB *does*
    /// let us fix at this layer: the navigation/editing/modifier/functional
    /// cluster, which is exactly what a compositor keymap remap like niri's
    /// `input.keyboard.xkb` (swapping Esc and `CapsLock`) touches, and exactly
    /// the class of bug reported (Esc/CapsLock producing the wrong effect,
    /// or leaking a raw escape into a TUI that never asked for it). For
    /// everything else -- letters, digits, punctuation -- we keep the
    /// physical (US-QWERTY) mapping: correct for the common case, and no
    /// worse than before XKB was wired in.
    fn resolve_logical_key(&self, physical: Option<Key>, evdev_code: u32) -> Option<Key> {
        if let Some(xkb) = &self.xkb {
            let sym = xkb.keysym(evdev_code);
            if sym != 0 {
                if let Some(k) = keymap::XkbKeysym::from_raw(sym).map(keymap::XkbKeysym::to_key) {
                    return Some(k);
                }
                // A real (non-NoSymbol) keysym came back but it's outside
                // our navigation/modifier/functional table -- almost
                // certainly a printable character. Fall through to the
                // physical mapping rather than dropping the press.
            }
        }
        physical.map(keymap::PhysicalKey::to_logical)
    }

    fn handle_clipboard_fds(&mut self, fds: Vec<std::os::fd::OwnedFd>) {
        use std::os::fd::AsFd as _;
        let Some(fd) = fds.into_iter().next() else {
            return;
        };
        if let Some(size) = self.awaiting_keymap_fd.take() {
            match xkb::Translator::from_keymap_fd(fd.as_fd(), size) {
                Ok(t) => self.xkb = Some(t),
                Err(e) => {
                    // Not fatal: `keymap::PhysicalKey::to_logical` (the
                    // physical-keycode fallback) still works, just without
                    // honoring the compositor's remap.
                    eprintln!("platform: XKB keymap translation unavailable: {e}");
                }
            }
            // `fd` (an mmap'd, now-unneeded fd) closes on scope exit.
            return;
        }
        if self.clipboard.is_awaiting_send_fd() {
            self.clipboard.write_send_fd(fd);
        }
        // Else: unsolicited fd. Dropping `fd` here closes it instead of
        // leaking (it's an OwnedFd, so this is automatic).
    }

    fn handle_event(&mut self, ev: RawEvent) -> Result<()> {
        // Clipboard fds arrive as a standalone `Fd` event immediately after the
        // message that triggered them (see conn.rs). Route by what we asked for.
        if let RawEvent::Fd(fds) = ev {
            self.handle_clipboard_fds(fds);
            return Ok(());
        }

        // Connection-level events every window handles identically -- the
        // `wl_display` error/delete_id routing, `wl_buffer.release`, and the
        // frame callback -- live in `surface::route_common`. `Fatal` is our
        // protocol error; `Handled` means it's fully dealt with; `Passthrough`
        // falls through to the terminal-specific dispatch below.
        match self.present.route_common(&ev, &mut self.window_tx) {
            crate::surface::Routed::Fatal(e) => return Err(e),
            crate::surface::Routed::Handled => return Ok(()),
            crate::surface::Routed::Passthrough => {}
        }

        // Key repeat timer.
        if matches!(ev, RawEvent::Repeat) {
            self.handle_key_repeat_tick();
            return Ok(());
        }

        if self.dispatch_shell_event(&ev)? {
            return Ok(());
        }

        // wl_pointer.*
        if let Some(pointer) = self.pointer {
            if ev.is(pointer) {
                self.dispatch_pointer_event(pointer, &ev);
                return Ok(());
            }
        }

        // wl_keyboard.*
        if let Some(keyboard) = self.keyboard {
            if ev.is(keyboard) {
                self.dispatch_keyboard_event(&ev);
                return Ok(());
            }
        }

        self.dispatch_clipboard_data_event(&ev);

        Ok(())
    }

    /// Dispatch the desktop-shell events every toplevel window handles:
    /// `xdg_wm_base.ping`, `xdg_surface.configure`, `xdg_toplevel.close`/
    /// `.configure`, and `wl_seat.capabilities`. Split out of `handle_event`
    /// -- these five `if`s were each contributing their own branch (and, for
    /// `xdg_surface.configure`, its own `?`) to `handle_event`'s cyclomatic
    /// complexity despite having nothing to do with the pointer/keyboard/
    /// clipboard dispatch that follows them, or with the connection-level
    /// routing (`route_common`) that precedes them. Returns whether one of
    /// them matched (and thus `handle_event` should return early).
    fn dispatch_shell_event(&mut self, ev: &RawEvent) -> Result<bool> {
        // xdg_wm_base.ping
        if ev.is(self.wm_base) && ev.event::<WmBaseEvent>() == Some(WmBaseEvent::Ping) {
            self.handle_wm_base_ping(ev);
            return Ok(true);
        }

        // xdg_surface.configure
        if ev.is(self.shell.xdg_surface)
            && ev.event::<XdgSurfaceEvent>() == Some(XdgSurfaceEvent::Configure)
        {
            self.handle_xdg_surface_configure(ev)?;
            return Ok(true);
        }

        // xdg_toplevel.close
        if ev.is(self.shell.toplevel) && ev.event::<ToplevelEvent>() == Some(ToplevelEvent::Close) {
            self.handle_toplevel_close();
            return Ok(true);
        }

        // xdg_toplevel.configure (resize)
        if ev.is(self.shell.toplevel)
            && ev.event::<ToplevelEvent>() == Some(ToplevelEvent::Configure)
        {
            self.handle_toplevel_configure(ev);
            return Ok(true);
        }

        // wl_seat.capabilities
        if ev.is(self.seat) && ev.event::<SeatEvent>() == Some(SeatEvent::Capabilities) {
            self.handle_seat_capabilities(ev);
            return Ok(true);
        }

        Ok(false)
    }

    /// Dispatch the three clipboard-related protocol objects --
    /// `wl_data_device` (selection offers from other clients),
    /// `wl_data_offer` (MIME types a peer advertises), and `wl_data_source`
    /// (a peer wants our clipboard, or replaced it) -- to
    /// [`crate::clipboard::ClipboardState`]. Split out of `handle_event` for
    /// the same reason as [`Self::dispatch_shell_event`]: three more
    /// independent `if let Some(id) = ...` checks that don't interact with
    /// anything else in the dispatcher. This is the tail of `handle_event`,
    /// so unlike the other extracted dispatchers it doesn't need to report
    /// back whether it matched -- there's nothing left to fall through to.
    fn dispatch_clipboard_data_event(&mut self, ev: &RawEvent) {
        // wl_data_device.* — clipboard selection offers from other clients.
        if let Some(dev) = self.clipboard.device_id() {
            if ev.is(dev) && ev.event::<DataDeviceEvent>() == Some(DataDeviceEvent::Selection) {
                // arg(0) = new wl_data_offer id (0 = selection cleared).
                self.clipboard.on_selection_event(ev.arg(0));
                return;
            }
        }

        // wl_data_offer.* — MIME types a peer advertises (we only need to know
        // the offer exists; we always request text/plain on paste).
        if let Some(offer) = self.clipboard.offer_id() {
            if ev.is(offer) {
                if let Some(offer_ev) = ev.event::<DataOfferEvent>() {
                    crate::clipboard::ClipboardState::on_offer_event(offer_ev);
                }
                return;
            }
        }

        // wl_data_source.* — a peer wants our clipboard (send) or replaced it
        // (cancelled). Both arrive as Wire events (the reader only routes
        // display/registry to Bytes); send's string arg is misparsed into the
        // inline u32 slots, but we don't need it — we always serve text/plain.
        if let Some(src) = self.clipboard.source_id() {
            if ev.is(src) {
                if let Some(src_ev) = ev.event::<DataSourceEvent>() {
                    self.clipboard.on_source_event(src_ev);
                }
            }
        }
    }

    /// Dispatch one `wl_pointer.*` event to its handler. Split out of
    /// `handle_event` -- the inner `match` was itself 4 branches contributing
    /// straight to that function's cyclomatic complexity for a chunk of
    /// dispatch that has nothing to do with the connection-level routing
    /// around it.
    fn dispatch_pointer_event(&mut self, pointer: PointerId, ev: &RawEvent) {
        match ev.event::<PointerEvent>() {
            Some(PointerEvent::Enter) => self.handle_pointer_enter(pointer, ev),
            Some(PointerEvent::Motion) => self.handle_pointer_motion(ev),
            Some(PointerEvent::Button) => self.handle_pointer_button(ev),
            Some(PointerEvent::Axis) => self.handle_pointer_axis(ev),
            _ => {}
        }
    }

    /// Dispatch one `wl_keyboard.*` event to its handler. Same rationale as
    /// [`Self::dispatch_pointer_event`]: an inline 6-armed match here inflated
    /// `handle_event`'s complexity for dispatch logic unrelated to every
    /// other protocol object it also routes.
    fn dispatch_keyboard_event(&mut self, ev: &RawEvent) {
        match ev.event::<KeyboardEvent>() {
            Some(KeyboardEvent::Keymap) => self.handle_keyboard_keymap(ev),
            Some(KeyboardEvent::Enter) => self.handle_keyboard_enter(ev),
            Some(KeyboardEvent::Key) => self.handle_keyboard_key(ev),
            Some(KeyboardEvent::RepeatInfo) => self.handle_keyboard_repeat_info(ev),
            Some(KeyboardEvent::Leave) => self.handle_keyboard_leave(),
            Some(KeyboardEvent::Modifiers) => self.handle_keyboard_modifiers(ev),
            _ => {}
        }
    }

    /// Re-apply the currently held key's motion on a repeat-timer tick (the
    /// reader thread's timerfd poll fired). Split out of `handle_event` so
    /// the dispatcher stays a flat list of one-line branches -- each handler
    /// below is independently testable and readable without wading through
    /// every other protocol object's logic.
    fn handle_key_repeat_tick(&mut self) {
        if let Some(key) = self.key_repeat.held_key() {
            self.input_queue.push_back(InputEvent::Key {
                key: Some(key),
                state: Some(KeyState::Pressed),
                modifiers: self.modifiers,
            });
        }
    }

    /// Answer an `xdg_wm_base.ping` with the matching `pong` -- keeps the
    /// compositor from deciding we've hung.
    fn handle_wm_base_ping(&self, ev: &RawEvent) {
        ShellSurface::pong(&self.window_tx, self.wm_base, ev.serial(0).raw());
    }

    /// Ack an `xdg_surface.configure` and flip to `Mapped` -- the surface is
    /// now allowed its first `wl_surface.commit`.
    fn handle_xdg_surface_configure(&mut self, ev: &RawEvent) -> Result<()> {
        self.shell
            .ack_configure(&self.window_tx, ev.serial(0).raw());
        self.lifecycle.acked();
        self.maybe_present(None)
    }

    /// Handle `xdg_toplevel.close` -- the compositor (or user) asked us to
    /// quit; queue it for the caller rather than exiting from inside dispatch.
    fn handle_toplevel_close(&mut self) {
        self.lifecycle.close_requested();
        self.input_queue.push_back(InputEvent::Close);
    }

    /// Record a proposed `xdg_toplevel.configure` (resize) for the next frame
    /// to apply. Args are `(width, height, states[])` in *logical*
    /// (surface-local) coordinates -- xdg-shell sizes are always pre-scale,
    /// regardless of our `wl_surface.buffer_scale`. Our shm buffers are
    /// physical pixels (`scale`x the logical size), so this must be scaled up
    /// before it's usable as a buffer/backbuffer dimension -- see
    /// `apply_pending_resize`, the only reader of `pending_resize`, which now
    /// receives already-scaled physical pixels and no longer needs to know
    /// about scale itself. A drag emits these dozens of times a second; just
    /// record the latest and coalesce into a single resize per frame.
    const fn handle_toplevel_configure(&mut self, ev: &RawEvent) {
        let w = ev.arg_i32(0) * self.scale;
        let h = ev.arg_i32(1) * self.scale;
        if w > 0 && h > 0 {
            self.pending_resize = Some((w, h));
        }
    }

    /// Handle `wl_seat.capabilities`: bind a pointer and/or keyboard the
    /// first time the seat advertises them (never re-binds one we already
    /// hold).
    fn handle_seat_capabilities(&mut self, ev: &RawEvent) {
        let caps = ev.arg(0);
        if self.pointer.is_none() && SeatCapability::Pointer.in_mask(caps) {
            let pointer: PointerId = self.window_tx.alloc_id();
            self.window_tx
                .request(MessageBuilder::new(self.seat, SeatRequest::GetPointer).new_id(pointer));
            self.pointer = Some(pointer);
        }
        if self.keyboard.is_none() && SeatCapability::Keyboard.in_mask(caps) {
            let keyboard: KeyboardId = self.window_tx.alloc_id();
            self.window_tx
                .request(MessageBuilder::new(self.seat, SeatRequest::GetKeyboard).new_id(keyboard));
            self.keyboard = Some(keyboard);
        }
    }

    /// `wl_pointer.enter`: remember the entry position and, if we have a
    /// custom cursor image, ask the compositor to show it (the request must
    /// be re-sent on every enter -- the compositor forgets it once the
    /// pointer leaves).
    fn handle_pointer_enter(&mut self, pointer: PointerId, ev: &RawEvent) {
        self.pointer_pos = ev.point(2, 3);
        if let Some(ref cursor) = self.cursor {
            self.window_tx.request(
                MessageBuilder::new(pointer, PointerRequest::SetCursor)
                    .u32(ev.serial(0).raw())
                    .object(cursor.surface)
                    .i32(cursor.hotspot_x)
                    .i32(cursor.hotspot_y),
            );
        }
    }

    /// `wl_pointer.motion`: track the cursor, extend an in-flight selection,
    /// refresh link-hover state, and forward a VT mouse-motion report (unless
    /// a Shift+drag selection bypass is hiding the whole drag from the app).
    fn handle_pointer_motion(&mut self, ev: &RawEvent) {
        self.pointer_pos = ev.point(1, 2);
        // Extend the selection only while the left button is
        // held — releasing ends the drag.
        let (cc, cr) = self.pointer_cell();
        if self.selection.drag_to((cc, i32::from(cr))) {
            self.present.mark_dirty();
        }
        self.update_hover_link(cc, cr);
        // VT mouse reporting: forward every motion report:
        // the `Encoder` (which knows the negotiated tracking
        // granularity) decides whether a bare hover
        // (`mouse_pressed == None`) or only a drag gets
        // encoded -- same division of labor as `Key`.
        // Except during a Shift+drag selection bypass: the
        // app never saw that press, so it must not see the
        // drag's motion either (orphan drag reports confuse
        // vim et al).
        if !(self.selection_bypass && self.mouse_pressed == Some(MouseButton::Left)) {
            self.input_queue.push_back(InputEvent::Mouse {
                button: self.mouse_pressed.unwrap_or(MouseButton::None),
                kind: MouseEventKind::Motion,
                col: cc,
                row: cr,
                modifiers: self.modifiers,
            });
        }
    }

    /// `wl_pointer.button`: decides left-button ownership between the app's
    /// VT mouse tracking and the terminal's own text selection (the
    /// kitty/foot/xterm Shift-bypass convention), reports the click to the
    /// app when appropriate, and drives the selection press/release.
    fn handle_pointer_button(&mut self, ev: &RawEvent) {
        let button = PointerButton::from_code(ev.arg(2));
        let state = ButtonState::from_raw(ev.arg(3));
        // arg(0) is the press serial. Recorded before any of the ownership
        // logic below, which may consume the event entirely: a Ctrl+click
        // that opens a link is exactly the case that needs this serial, and
        // it is also a case the app never sees.
        if matches!(state, Some(ButtonState::Pressed)) {
            self.input_serial.record_press(ev.arg(0));
        }
        let bypass = self.left_button_bypass(button, state);
        self.report_pointer_button_to_app(button, state, bypass.suppress_report);
        // arg(1) is the compositor's millisecond timestamp for this button
        // event -- the clock the double-click threshold is measured against.
        self.drive_selection_from_left_button(button, state, &bypass, ev.arg(1));
    }

    /// Left-button ownership decision (kitty/foot/xterm convention): while
    /// an app has negotiated mouse tracking it owns bare clicks (reported,
    /// no terminal selection); Shift+click bypasses the app and drives the
    /// terminal's own selection instead (selection runs, report suppressed
    /// for the whole drag). With no tracking the terminal owns the mouse
    /// outright. Split out of `handle_pointer_button` so the three booleans
    /// it used to compute inline (each folding in `button`/`state`/modifier
    /// state) are one named, testable-in-isolation calculation instead of
    /// contributing straight to that function's branch count.
    const fn left_button_bypass(
        &self,
        button: Option<PointerButton>,
        state: Option<ButtonState>,
    ) -> LeftButtonBypass {
        let shift =
            (self.modifiers.depressed | self.modifiers.latched | self.modifiers.locked) & 0x01 != 0;
        let is_left = matches!(button, Some(PointerButton::Left));
        let starts =
            is_left && matches!(state, Some(ButtonState::Pressed)) && (!self.app_mouse || shift);
        let ends = is_left && matches!(state, Some(ButtonState::Released)) && self.selection_bypass;
        // Suppress the app report for both halves of a bypassed drag -- an
        // orphan press or release confuses the app.
        let suppress_report = self.app_mouse && (starts && shift || ends);
        LeftButtonBypass {
            shift,
            starts,
            suppress_report,
        }
    }

    /// Queue the `InputEvent::Mouse` report for the app, unless this press/
    /// release is being consumed entirely by a terminal-selection bypass.
    fn report_pointer_button_to_app(
        &mut self,
        button: Option<PointerButton>,
        state: Option<ButtonState>,
        suppress_report: bool,
    ) {
        let (Some(button), Some(state)) = (button, state) else {
            return;
        };
        let (cc, cr) = self.pointer_cell();
        let mb = match button {
            PointerButton::Left => MouseButton::Left,
            PointerButton::Right => MouseButton::Right,
            PointerButton::Middle => MouseButton::Middle,
        };
        self.mouse_pressed = matches!(state, ButtonState::Pressed).then_some(mb);
        if suppress_report {
            return;
        }
        self.input_queue.push_back(InputEvent::Mouse {
            button: mb,
            kind: if matches!(state, ButtonState::Pressed) {
                MouseEventKind::Press
            } else {
                MouseEventKind::Release
            },
            col: cc,
            row: cr,
            modifiers: self.modifiers,
        });
    }

    /// Press/release the terminal's own text selection when the left button
    /// is (or was) bypassing the app, per [`Self::left_button_bypass`].
    fn drive_selection_from_left_button(
        &mut self,
        button: Option<PointerButton>,
        state: Option<ButtonState>,
        bypass: &LeftButtonBypass,
        time_ms: u32,
    ) {
        match (button, state) {
            (Some(PointerButton::Left), Some(ButtonState::Pressed)) => {
                if bypass.starts {
                    self.selection_bypass = self.app_mouse;
                    let (cc, cr) = self.pointer_cell();
                    // Press semantics (shift-extend / dismiss-click /
                    // begin-potential-selection) live in the model. Under
                    // app-mouse bypass shift is the *trigger*, not an
                    // extend request, so only pass it through when the
                    // terminal owns the mouse anyway.
                    self.selection
                        .press_left((cc, i32::from(cr)), bypass.shift && !self.app_mouse);
                    // A double-click promotes that press into a word
                    // selection. Order matters: `press_left` runs first so
                    // the drag bookkeeping (button held, anchor set) is
                    // established exactly as for a single click, then the
                    // word span overwrites the zero-width range -- which
                    // also means dragging on from the second click keeps
                    // extending the selection normally.
                    if matches!(
                        self.multi_click.press(time_ms, (cc, cr)),
                        crate::multi_click::ClickKind::Double
                    ) {
                        self.selection.select_word((cc, i32::from(cr)));
                    }
                    self.present.mark_dirty();
                }
            }
            (Some(PointerButton::Left), Some(ButtonState::Released)) => {
                // Release ends the drag. If the pointer never moved since
                // press, this was a bare click on empty space -- nothing
                // to keep.
                self.selection_bypass = false;
                self.selection.release_left();
                self.present.mark_dirty();
            }
            _ => {}
        }
    }

    /// `wl_pointer.axis`: converts a vertical scroll-wheel delta into whole
    /// scrollback lines and queues a `Scroll` input event.
    fn handle_pointer_axis(&mut self, ev: &RawEvent) {
        // Axis args: (time, axis, value). axis 0 = vertical.
        // value is wl_fixed (24.8): positive = wheel down
        // (content moves down = view scrolls DOWN toward the
        // prompt); negative = wheel up (into history).
        let axis = ev.arg(1);
        let value = ev.fixed(2);
        if axis == 0 {
            // Convert the scroll magnitude to whole lines. A
            // typical wheel detent is 15 (|value| ~ 15.0); map
            // each such step to 1 line, for fine-grained
            // line-at-a-time scrolling.
            let px = value.to_pixels().abs();
            let steps = ((px + 7) / 15).max(1);
            let lines = steps;
            let signed = if value.0 < 0 { lines } else { -lines };
            let (cc, cr) = self.pointer_cell();
            self.input_queue.push_back(InputEvent::Scroll {
                lines: signed,
                col: cc,
                row: cr,
                modifiers: self.modifiers,
            });
        }
    }

    /// `wl_keyboard.keymap`: stash the mapping's byte length; the fd itself
    /// rides out-of-band as the very next `Fd` event (Wayland attaches fds to
    /// a specific message, not as a standalone one).
    const fn handle_keyboard_keymap(&mut self, ev: &RawEvent) {
        self.awaiting_keymap_fd = Some(ev.arg(1) as usize);
    }

    /// `wl_keyboard.enter`: keyboard focus gained. Remembers the serial
    /// `set_selection` needs to prove clipboard ownership, and makes the
    /// cursor solid (not mid-fade) before it resumes blinking.
    fn handle_keyboard_enter(&mut self, ev: &RawEvent) {
        self.clipboard.set_focus_serial(ev.arg(0));
        self.cursor_anim.set_focused(true);
        self.present.mark_dirty();
        self.input_queue.push_back(InputEvent::Focus(true));
    }

    /// `wl_keyboard.key`: resolves the physical keycode to a logical key,
    /// drives the repeat timer, lets Esc clear an active selection, and
    /// queues the key for the caller.
    fn handle_keyboard_key(&mut self, ev: &RawEvent) {
        let physical = Key::from_code(ev.arg(2));
        let logical = self.resolve_logical_key(physical, ev.arg(2));
        let state = KeyState::from_raw(ev.arg(3));
        if matches!(state, Some(KeyState::Pressed)) {
            self.input_serial.record_key_down(ev.arg(0));
        }
        // Start/stop repeat timer. Modifier keys never repeat.
        match (logical, state) {
            (Some(k), Some(KeyState::Pressed)) if !physical.is_some_and(Key::is_modifier) => {
                self.key_repeat.on_key_press(k);
            }
            (Some(k), Some(KeyState::Released)) => {
                self.key_repeat.on_key_release(k);
            }
            _ => {}
        }
        // Esc clears any active selection (and still passes
        // through to the application as a normal Esc).
        if logical == Some(Key::Esc) && state == Some(KeyState::Pressed) && self.selection.clear() {
            self.present.mark_dirty();
        }
        self.input_queue.push_back(InputEvent::Key {
            key: logical,
            state,
            modifiers: self.modifiers,
        });
    }

    /// `wl_keyboard.repeat_info`: the compositor's preferred repeat rate/delay.
    const fn handle_keyboard_repeat_info(&mut self, ev: &RawEvent) {
        self.key_repeat.set_repeat_info(ev.arg(0), ev.arg(1));
    }

    /// `wl_keyboard.leave`: focus lost. Freezes the cursor fully hidden so it
    /// stops blinking -- the classic "which window is focused" cue: only the
    /// focused window's cursor is ever visible.
    fn handle_keyboard_leave(&mut self) {
        self.key_repeat.on_leave();
        self.cursor_anim.set_focused(false);
        self.present.mark_dirty();
        self.input_queue.push_back(InputEvent::Focus(false));
    }

    /// `wl_keyboard.modifiers`: updates the raw depressed/latched/locked/group
    /// masks, feeds them to XKB (which alone knows what they mean for the
    /// loaded keymap), and refreshes link-hover state since Ctrl toggling
    /// while hovering a link must react immediately, not wait for motion.
    fn handle_keyboard_modifiers(&mut self, ev: &RawEvent) {
        self.modifiers = Modifiers {
            depressed: ev.arg(1),
            latched: ev.arg(2),
            locked: ev.arg(3),
            group: ev.arg(4),
        };
        if let Some(xkb) = &mut self.xkb {
            // Feed the raw masks straight through -- XKB
            // owns what depressed/latched/locked mean for
            // whatever keymap is loaded, we don't decode
            // them ourselves.
            let _ = xkb.update_mask(
                self.modifiers.depressed,
                self.modifiers.latched,
                self.modifiers.locked,
                self.modifiers.group,
            );
        }
        // Ctrl toggling while the pointer sits over a link
        // must start/stop the hover-underline immediately,
        // not wait for the next pointer motion.
        let (cc, cr) = self.pointer_cell();
        self.update_hover_link(cc, cr);
    }

    /// Apply the most recent pending `configure` size, if any, as a single
    /// resize. Collapses a drag's flood of configures into one buffer rebuild.
    fn apply_pending_resize(&mut self) -> Result<()> {
        let Some((w, h)) = self.pending_resize.take() else {
            return Ok(());
        };
        if (w, h) == (self.width, self.height) {
            return Ok(());
        }
        self.resize(w, h)?;
        // The shm buffers/backbuffer always track the exact pixel size (so the
        // window paints crisply at any drag position), but the *grid* only
        // cares about whole cells. A drag's sub-cell pixel jitter leaves the
        // cell count unchanged; re-reflowing on each such configure is what made
        // long-line wrapping judder. Only emit a grid Resize when the cell
        // count actually moves.
        let cols = (w / self.cell_w) as u16;
        let rows = (h / self.cell_h) as u16;
        if (cols, rows) != self.last_grid_size {
            self.last_grid_size = (cols, rows);
            self.input_queue
                .push_back(InputEvent::Resize { cols, rows });
        }
        self.present.mark_dirty();
        Ok(())
    }

    /// Repaint the given viewport rows from the cell mirror into the
    /// backbuffer (clean, un-highlighted pixels). Called before tinting the
    /// current selection and after clearing one, so the highlight is
    /// idempotent (never compounds across frames) and fully reversible.
    /// Remap overlay-tracking row indices through the pixel-scrolls that
    /// `blit_snapshot` performed this frame. Each scroll band shifted the
    /// backbuffer scanlines for rows `top..=bot` up by `delta`, so a tracked
    /// row `r` in that band now lives at `r - delta`. A row that scrolled
    /// above `top` (i.e. `r - delta < top`) is gone from the visible
    /// backbuffer entirely and needs no cleanup; drop it. Rows outside the
    /// band are untouched. Applied in the same order the scrolls happened so
    /// multiple scrolls in one frame compose correctly.
    fn shift_overlay_rows(rows: &mut Vec<u16>, scrolls: &[grid::ScrollBand]) {
        for &grid::ScrollBand { top, bot, delta } in scrolls {
            rows.retain_mut(|r| {
                if *r >= top && *r <= bot {
                    if *r < top + delta {
                        // Scrolled off the top of the band; nothing to clean.
                        return false;
                    }
                    *r -= delta;
                }
                true
            });
        }
    }

    /// Remap overlay-tracking row indices through a viewport (scrollback)
    /// pixel-scroll, whose shift is signed and spans the whole screen `[0,
    /// rows)`. `s > 0` lifted the backbuffer up (content moved toward the
    /// bottom), so a tracked row `r` now lives at `r - s`; `s < 0` pushed it
    /// down to `r + |s|`. A row shifted outside `[0, rows)` scrolled off the
    /// visible backbuffer and needs no cleanup -- drop it. Applied in order so
    /// several shifts in one frame compose. The distinct upward-only
    /// [`Self::shift_overlay_rows`] handles the incremental content-scroll band;
    /// this handles the full-screen signed view scroll.
    fn shift_overlay_rows_signed(rows: &mut Vec<u16>, shifts: &[i32], row_count: u16) {
        for &s in shifts {
            rows.retain_mut(|r| {
                let moved = i64::from(*r) - i64::from(s);
                if moved < 0 || moved >= i64::from(row_count) {
                    return false;
                }
                *r = moved as u16;
                true
            });
        }
    }

    /// Add every row of each pixel-scrolled band `[top, bot]` to `touched`.
    /// A `copy_within` in the backbuffer rewrote all those rows, but the shm
    /// frame slots aren't scrolled -- they're the actually-presented pixels --
    /// so each moved row must be copied backbuffer->shm and damaged this frame,
    /// even though only the new bottom rows appear in the snapshot's dirty set.
    fn extend_with_scrolled_bands(touched: &mut Vec<u16>, scrolls: &[grid::ScrollBand]) {
        for &grid::ScrollBand { top, bot, .. } in scrolls {
            touched.extend(top..=bot);
        }
    }

    /// The pixel x-range `[x0, x1)` to underline for a hovered link spanning
    /// grid columns `lo..=hi`, or `None` when the span lies entirely off-grid.
    ///
    /// `hi` originates in the *cell mirror*, whose rows are as wide as the
    /// grid's column count -- not necessarily as wide as the window's `cols`
    /// (`width / cell_w`, floored). A window whose pixel width isn't an exact
    /// multiple of `cell_w` leaves a sub-cell dead strip along the right edge
    /// that belongs to no column, and `repaint_rows` only ever repaints
    /// `.take(cols)` cells -- so ink stamped past `cols * cell_w` is never
    /// cleaned and survives every later frame, exactly like the bottom-strip
    /// artifact fixed in `Renderer::draw_cursor`. Clamping to the grid-aligned
    /// width (rather than to `buf_w`, which includes the dead strip) keeps the
    /// underline inside pixels the repaint path can actually reclaim.
    fn underline_span(
        lo: usize,
        hi: usize,
        cols: usize,
        cell_w: usize,
        buf_w: usize,
    ) -> Option<(usize, usize)> {
        // Drop spans starting past the last visible column outright; clamping
        // them would smear an underline across a column the user can't see.
        if cols == 0 || lo >= cols {
            return None;
        }
        let hi = hi.min(cols - 1);
        let grid_max_x = (buf_w / cell_w.max(1)) * cell_w.max(1);
        let x0 = lo * cell_w;
        let x1 = ((hi + 1) * cell_w).min(grid_max_x).min(buf_w);
        (x0 < x1).then_some((x0, x1))
    }

    fn repaint_rows(&mut self, rows: &[u16]) {
        let cols = (self.width / self.cell_w).max(1) as u16;
        let mut canvas = self.pixels.canvas(self.width, self.height);
        for &row in rows {
            let Some(content) = self.selection.row_content(row) else {
                continue;
            };
            // Repair painting touches only a small dirty set. Materialize here
            // rather than retaining expanded cells for every idle frame.
            let cells = content.materialize_cells();
            for (col, cell) in cells.iter().enumerate().take(usize::from(cols)) {
                self.renderer.blit_cell(&mut canvas, col as u16, row, cell);
            }
        }
    }

    /// The viewport rows the current selection touches (empty if none).
    /// Selection rows are signed and may extend past the visible area
    /// (selected text scrolled into scrollback); only the on-screen span is
    /// returned, since off-screen rows need no repaint.
    fn selection_rows(&self) -> Vec<u16> {
        let rows = (self.height / self.cell_h).max(1);
        self.selection.visible_rows(rows)
    }

    /// Overlay the selection highlight onto the backbuffer: each selected
    /// cell's rect is alpha-blended toward a fixed highlight tint (a light
    /// steel-blue), giving the familiar "selected text" look without
    /// disturbing glyph pixels' relative contrast. Assumes the affected rows
    /// were just repainted clean (see [`Self::repaint_rows`]).
    fn apply_selection_highlight(&mut self) {
        // Highlight tint + strength (0..255). ~35% steel-blue.
        const HR: u32 = 0x40;
        const HG: u32 = 0x60;
        const HB: u32 = 0xa0;
        const ALPHA: u32 = 90;
        const INV: u32 = 255 - ALPHA;

        let Some(((ac, ar), (bc, br))) = self.selection.range() else {
            return;
        };
        if (ac, ar) == (bc, br) {
            return;
        }
        let ((sc, sr), (ec, er)) = if (ar, ac) <= (br, bc) {
            ((ac, ar), (bc, br))
        } else {
            ((bc, br), (ac, ar))
        };

        let cols = (self.width / self.cell_w).max(1) as u16;
        let rows = (self.height / self.cell_h).max(1);
        let buf_w = self.width as usize;

        // Clamp the signed selection span to the visible rows — off-screen
        // (scrolled-away) parts of the selection get no highlight.
        let vis_start = sr.max(0);
        let vis_end = er.min(rows - 1);
        for r in vis_start..=vis_end {
            let row = r as u16;
            let c0 = if r == sr { sc } else { 0 };
            let c1 = if r == er { ec } else { cols - 1 };
            let c1 = c1.min(cols - 1);
            let py0 = usize::from(row) * self.cell_h as usize;
            let px0 = usize::from(c0) * self.cell_w as usize;
            let px1 = (usize::from(c1) + 1) * self.cell_w as usize;
            let backbuffer = self.pixels.backbuffer_mut();
            for py in py0..py0 + self.cell_h as usize {
                let base = py * buf_w;
                if base >= backbuffer.len() {
                    break;
                }
                for px in px0..px1.min(buf_w) {
                    let idx = base + px;
                    if idx >= backbuffer.len() {
                        break;
                    }
                    let p = backbuffer[idx];
                    let r = (p >> 16) & 0xff;
                    let g = (p >> 8) & 0xff;
                    let b = p & 0xff;
                    let nr = (r * INV + HR * ALPHA) / 255;
                    let ng = (g * INV + HG * ALPHA) / 255;
                    let nb = (b * INV + HB * ALPHA) / 255;
                    // Preserve the top byte (alpha for translucent windows).
                    backbuffer[idx] = (p & 0xff00_0000) | (nr << 16) | (ng << 8) | nb;
                }
            }
        }
    }

    /// Draw a one-pixel underline across the hovered link's row-span (either
    /// the contiguous run of cells sharing a real OSC 8 link id, or a
    /// detected plain-text URL's own span), so the user gets the familiar
    /// "this is clickable" feedback before committing to the Ctrl+click.
    /// No-op when nothing is hovered -- the caller is responsible for
    /// repainting `prev_hover_row` clean first (same pattern as the
    /// selection highlight).
    fn apply_link_hover_underline(&mut self) {
        // Light blue underline, preserving whatever alpha byte the pixel
        // already carried (top byte, only meaningful on a translucent
        // window -- see `Renderer::pack_bg`).
        const UNDERLINE_RGB: u32 = 0x00A0_C0FF;

        let Some(target) = &self.hover_link else {
            return;
        };
        let (_, row) = self.pointer_cell();
        let Some(content) = self.selection.row_content(row) else {
            return;
        };
        // Hover work is pointer-driven, not frame-driven. Expand locally so
        // the retained selection model stays compact during ordinary output.
        let line = content.materialize_cells();
        // Find the span to underline: a real link's contiguous same-id run,
        // or a detected URL's own pre-computed span.
        let (lo, hi) = match target {
            crate::link_scan::LinkTarget::Real(id) => {
                let mut lo = None;
                let mut hi = None;
                for (col, cell) in line.iter().enumerate() {
                    if cell.link_id() == *id {
                        lo.get_or_insert(col);
                        hi = Some(col);
                    }
                }
                let (Some(lo), Some(hi)) = (lo, hi) else {
                    return;
                };
                (lo, hi)
            }
            crate::link_scan::LinkTarget::Url(span) => {
                (usize::from(span.start_col), usize::from(span.end_col))
            }
        };
        let y = usize::from(row) * self.cell_h as usize + self.cell_h as usize - 1;
        let buf_w = self.width as usize;
        if y >= self.height as usize {
            return;
        }
        let cols = (self.width / self.cell_w).max(1) as usize;
        let Some((x0, x1)) = Self::underline_span(lo, hi, cols, self.cell_w as usize, buf_w) else {
            return;
        };
        let base = y * buf_w;
        let backbuffer = self.pixels.backbuffer_mut();
        for x in x0..x1 {
            let idx = base + x;
            if idx < backbuffer.len() {
                backbuffer[idx] = (backbuffer[idx] & 0xFF00_0000) | UNDERLINE_RGB;
            }
        }
        self.prev_hover_row = Some(row);
    }

    /// Draw the cursor overlay at its current animated (possibly
    /// mid-slide, possibly mid-blink-fade) position, on top of whatever
    /// [`Self::apply_selection_highlight`] just painted. Returns the whole-
    /// number viewport rows the drawn cursor rect touches (0, 1, or -- for a
    /// cursor spanning a slide between two rows -- 2), so the next frame
    /// knows which rows to repaint clean first (see `prev_cursor_rows`).
    fn draw_cursor_overlay(&mut self) -> Vec<u16> {
        let (vc, vr) = self.cursor_anim.visual_pos();
        let pulse = self.cursor_anim.pulse();
        let alpha = self.cursor_anim.alpha();

        if alpha <= 0.0 && pulse.is_none() {
            return Vec::new();
        }

        let shape = self.cursor_anim.shape();
        {
            let mut canvas = self.pixels.canvas(self.width, self.height);
            if alpha > 0.0 {
                self.renderer.draw_cursor(&mut canvas, vc, vr, shape, alpha);
            }
            if let Some(t) = pulse {
                self.renderer.draw_cursor_pulse(&mut canvas, vc, vr, t);
            }
        }

        // The pulse ring extends past the cell's own bounds, so widen the
        // repaint hint by one row on each side when it's active.
        let row_top = vr.floor().max(0.0) as u16;
        let row_bot = vr.ceil().max(0.0) as u16;
        let (lo, hi) = if pulse.is_some() {
            (row_top.saturating_sub(1), row_bot.saturating_add(1))
        } else {
            (row_top, row_bot)
        };
        (lo..=hi).collect()
    }

    /// Coalesce a backlog of pending dirty snapshots into the fewest
    /// snapshots that, blitted in order, reproduce the same result.
    ///
    /// Under a hard scroll flood the parser publishes ~30 snapshots per
    /// vsync -- each a full-band pixel-scroll (`scroll_span`) plus its
    /// freshly-revealed rows -- and blitting every one individually did ~30
    /// full-screen `copy_within` memmoves for a single presented frame
    /// (measured on niri: a 29-47 deep backlog took 17-23 ms in one present,
    /// flipping the loop CPU-bound and dropping 60 -> ~43 fps).
    ///
    /// `DirtySnapshot::compose` folds a run into one snapshot that, blitted
    /// once, reproduces the ordered result: same-band scrolls sum their
    /// deltas (N shifts of the band collapse to one shift of the summed
    /// delta) and rows merge last-writer-wins, relabelled up by later
    /// scrolls. A snapshot that can't compose (viewport repaint, geometry
    /// change, or a different scroll band) closes the current accumulator and
    /// starts a fresh one, so ordering across barriers is preserved.
    fn fold_dirty_snapshots(pending: Vec<grid::DirtySnapshot>) -> Vec<grid::DirtySnapshot> {
        let mut folded: Vec<grid::DirtySnapshot> = Vec::new();
        for snap in pending {
            let composed = folded.last_mut().is_some_and(|acc| acc.compose(&snap));
            if !composed {
                folded.push(snap);
            }
        }
        folded
    }

    /// Premultiply only the pixels a custom painter actually touched this
    /// frame. A full repaint (or a painter that doesn't track regions)
    /// reports `Full` and rebuilds the whole image; a steady-state dashboard
    /// frame reports a handful of small rects (chart edges, moved bars) and
    /// pays for only those pixels -- the difference between premultiplying
    /// ~2M pixels and ~3K every frame. Partial premul is safe because the
    /// ARGB cache is persistent: untouched regions still hold last frame's
    /// correct premultiplied pixels.
    fn update_custom_argb_for_damage(&mut self, damage: &crate::draw::Damage) {
        if damage.is_full() {
            self.update_custom_argb(crate::draw::Rect::new(0, 0, self.width, self.height));
        } else {
            for rect in damage.rects() {
                self.update_custom_argb(*rect);
            }
        }
    }

    /// Convert a custom painter's damage into stale logical rows so the shm
    /// upload copies only the touched bands into both slots. A full repaint
    /// marks every row; a region set marks just the rows its rectangles span.
    /// Both slots must see the same rows since either may be the one
    /// presented next.
    fn extend_touched_rows_for_custom_damage(
        &self,
        damage: &crate::draw::Damage,
        touched_rows: &mut Vec<u16>,
    ) {
        if damage.is_full() {
            let rows = ((self.height + self.cell_h - 1) / self.cell_h).max(1);
            touched_rows.extend((0..rows).map(|row| u16::try_from(row).unwrap_or(u16::MAX)));
        } else {
            for rect in damage.rects() {
                // Ceiling-expand the rect's pixel span to whole cell rows: the
                // shm damage protocol is row-granular, and a partial final row
                // is still real framebuffer memory that must be copied rather
                // than left as transparent mmap-zero.
                let top = (rect.y.max(0)) / self.cell_h;
                let bottom = ((rect.bottom().max(0) + self.cell_h - 1) / self.cell_h)
                    .min(((self.height + self.cell_h - 1) / self.cell_h).max(1));
                touched_rows
                    .extend((top..bottom).map(|row| u16::try_from(row).unwrap_or(u16::MAX)));
            }
        }
    }

    /// Remap this frame's baked-overlay row trackers (cursor, selection,
    /// debug fps) through any pixel-scroll `copy_within` that `blit_snapshot`
    /// performed, and extend `touched_rows` so every row a scroll physically
    /// moved gets re-copied into the shm slot.
    ///
    /// A pixel-scroll shifts the backbuffer's content up (or, for a viewport
    /// scroll, up or down by a signed amount) -- and with it any overlay we
    /// baked last frame. The row lists that say where to repaint those clean
    /// now point to the WRONG screen position unless remapped by the same
    /// shift, or the dragged pixels get left behind as a static imprint.
    ///
    /// The two scroll kinds need different handling because they cover
    /// different spans: `viewport_shifts` are signed and span the whole
    /// screen (a viewport/scrollback pixel-scroll), so `touched_rows` already
    /// spans the screen and needs no further extension here. `scrolls` are an
    /// upward-only content-scroll BAND -- the snapshot's dirty rows only
    /// cover the genuinely-new bottom rows (the grid shifts its dirty marks
    /// up with the content), so without marking the WHOLE band stale the
    /// shifted-but-unchanged rows never get re-copied from the backbuffer
    /// into the shm slots (the actual presented pixels), producing the "only
    /// a few lines scroll, the rest is stale" flood artifact.
    fn remap_baked_overlays_for_this_frames_scrolls(
        &mut self,
        viewport_shifts: &[i32],
        scrolls: &[grid::ScrollBand],
        touched_rows: &mut Vec<u16>,
    ) {
        if !viewport_shifts.is_empty() {
            let rows = (self.height / self.cell_h).max(1) as u16;
            Self::shift_overlay_rows_signed(&mut self.prev_cursor_rows, viewport_shifts, rows);
            Self::shift_overlay_rows_signed(&mut self.prev_selection_rows, viewport_shifts, rows);
            self.debug_shift_prev_rows_signed(viewport_shifts, rows);
        }
        if !scrolls.is_empty() {
            Self::shift_overlay_rows(&mut self.prev_cursor_rows, scrolls);
            Self::shift_overlay_rows(&mut self.prev_selection_rows, scrolls);
            self.debug_shift_prev_rows(scrolls);
            Self::extend_with_scrolled_bands(touched_rows, scrolls);
        }
    }

    /// Drop all terminal-only overlay tracking for a custom-painter window.
    /// A painter-owned window has no terminal grid, selection, or text input.
    /// In particular, we must not composite the terminal cursor over a
    /// dashboard merely because its normal toplevel has keyboard focus. The
    /// painter clears its own full frame, so stale terminal overlays need no
    /// cleanup on this path either.
    fn clear_terminal_overlays_for_custom_paint(&mut self) {
        self.prev_selection_rows.clear();
        self.prev_cursor_rows.clear();
        self.prev_hover_row = None;
        let _ = self.debug_take_prev_rows();
    }

    /// Composite the terminal's own overlays (selection highlight, link-hover
    /// underline, blinking cursor, debug fps readout) onto the backbuffer for
    /// this frame, appending every row any of them touched to `touched_rows`.
    ///
    /// Repaints the rows that *were* highlighted/tracked last frame (clean)
    /// first, plus the rows each overlay touches now (so a moved drag or
    /// sliding cursor doesn't double-tint), then redraws each overlay on top.
    /// This keeps every overlay idempotent and fully removable on deselect /
    /// blur / hover-exit -- the reason the "clean" pass gathers ALL stale rows
    /// (selection + hover + cursor + fps) before any single overlay repaints,
    /// since e.g. the cursor may be sliding through a row the selection logic
    /// doesn't know about, and it must be clean before either draws over it.
    fn composite_terminal_overlays(&mut self, touched_rows: &mut Vec<u16>) {
        let mut rows_to_repaint = std::mem::take(&mut self.prev_selection_rows);
        for r in self.selection_rows() {
            if !rows_to_repaint.contains(&r) {
                rows_to_repaint.push(r);
            }
        }
        if let Some(r) = self.prev_hover_row.take()
            && !rows_to_repaint.contains(&r)
        {
            rows_to_repaint.push(r);
        }
        if self.hover_link.is_some() {
            let (_, row) = self.pointer_cell();
            if !rows_to_repaint.contains(&row) {
                rows_to_repaint.push(row);
            }
        }
        for r in std::mem::take(&mut self.prev_cursor_rows) {
            if !rows_to_repaint.contains(&r) {
                rows_to_repaint.push(r);
            }
        }
        for r in self.debug_take_prev_rows() {
            if !rows_to_repaint.contains(&r) {
                rows_to_repaint.push(r);
            }
        }
        self.repaint_rows(&rows_to_repaint);
        touched_rows.extend_from_slice(&rows_to_repaint);
        self.apply_selection_highlight();
        touched_rows.extend(self.selection_rows());
        self.prev_selection_rows = self.selection_rows();
        self.apply_link_hover_underline();
        if let Some(r) = self.prev_hover_row {
            touched_rows.push(r);
        }
        let new_cursor_rows = self.draw_cursor_overlay();
        touched_rows.extend_from_slice(&new_cursor_rows);
        self.prev_cursor_rows = new_cursor_rows;
        let new_fps_rows = self.debug_draw_overlay();
        touched_rows.extend_from_slice(&new_fps_rows);
    }

    /// Copy every row `slot` is stale for from its true source (the shared
    /// backbuffer for a terminal-grid window, the persistent ARGB cache for a
    /// custom-painter one) into that shm slot, clearing each row's stale bit
    /// as it's caught up. Contiguous stale rows are coalesced into one
    /// `(y_px, height_px)` damage rect each, so the common single-row case
    /// (e.g. one line of typing) costs one Wayland `Damage` message instead
    /// of one per row. Returns the damage rects for [`Self::submit_frame`].
    fn copy_stale_rows_into_slot(&mut self, slot: usize, custom_painted: bool) -> Vec<(i32, i32)> {
        let buf_w = self.width;
        let cols_bytes = buf_w as usize;
        let cell_h = self.cell_h as usize;
        // The buffer height need not be an exact multiple of cell_h -- the
        // compositor hands us whatever configure size it likes. `stale_rows`
        // only tracks whole cell rows (`height / cell_h`, floored), so the
        // `height - row_count*cell_h` leftover scanlines at the very bottom
        // belong to no row's band. When a stale run reaches the last tracked
        // row, extend its copy + damage down to the true buffer bottom so that
        // partial strip is filled too -- otherwise the freshly-mmap'd (zeroed)
        // shm bottom shows through as a blank line.
        let buf_h = self.height as usize;
        // Borrow the source before borrowing `present` mutably below. The
        // caches and swap chain are disjoint state, but holding this narrow
        // slice makes that separation explicit to both Rust and readers.
        let (custom_source, custom_fill) = if custom_painted {
            if self.renderer.needs_alpha() {
                (
                    self.pixels.custom_argb(),
                    crate::pixel_buffers::PixelBuffers::premultiply_pixel(
                        self.renderer.bg_packed(),
                        self.renderer.background_alpha(),
                    ),
                )
            } else {
                (self.pixels.backbuffer(), self.renderer.bg_packed())
            }
        } else {
            // This slice is never read on the terminal-grid path. Supplying an
            // empty one keeps the later copy loop branch-free with respect to
            // the swap-chain borrow.
            (&[] as &[u32], self.renderer.bg_packed())
        };
        let (dst, stale) = self.present.slot_pixels_and_stale(slot);
        let last_row = stale.len().saturating_sub(1);
        let mut damage_rects: Vec<(i32, i32)> = Vec::new(); // (y_px, height_px)
        let mut row = 0usize;
        while row < stale.len() {
            if !stale[row] {
                row += 1;
                continue;
            }
            let run_start = row;
            let mut reaches_bottom = false;
            while row < stale.len() && stale[row] {
                stale[row] = false;
                if row == last_row {
                    reaches_bottom = true;
                }
                row += 1;
            }
            let y0 = run_start * cell_h;
            // The run's natural pixel end, stretched to the real buffer bottom
            // when it includes the last row so the sub-cell remainder is caught.
            let y1 = if reaches_bottom { buf_h } else { row * cell_h };
            let src_off = y0 * cols_bytes;
            let len = (y1 - y0) * cols_bytes;
            if let Some(dst_slice) = dst.get_mut(src_off..src_off + len) {
                if custom_painted {
                    // A custom surface can be made dirty by a resize/configure
                    // even when the app had no new telemetry to paint. It must
                    // therefore use its ARGB cache on *every* such present,
                    // rather than only the tick that happened to repaint.
                    let available = custom_source.len().saturating_sub(src_off);
                    let copy_len = available.min(dst_slice.len());
                    dst_slice[..copy_len]
                        .copy_from_slice(&custom_source[src_off..src_off + copy_len]);
                    if copy_len < dst_slice.len() {
                        dst_slice[copy_len..].fill(custom_fill);
                    }
                } else if let Some(src_slice) = self.pixels.backbuffer().get(src_off..src_off + len)
                {
                    dst_slice.copy_from_slice(src_slice);
                }
            }
            damage_rects.push((y0 as i32, (y1 - y0) as i32));
        }
        damage_rects
    }

    /// Attach `slot`'s buffer, damage every rect the stale-row copy touched,
    /// request the next frame callback, and commit -- the standard Wayland
    /// present sequence. `finish_present` records the callback so
    /// `route_common` can clear `frame_ready` when the compositor answers it.
    fn submit_frame(&mut self, slot: usize, damage_rects: Vec<(i32, i32)>) {
        let buf_w = self.width;
        let buf = self.present.frame_buffer(slot);
        self.window_tx.request(
            MessageBuilder::new(self.shell.surface, SurfaceRequest::Attach)
                .object(buf)
                .i32(0)
                .i32(0),
        );
        for (y_px, height_px) in damage_rects {
            self.window_tx.request(
                MessageBuilder::new(self.shell.surface, SurfaceRequest::Damage)
                    .i32(0)
                    .i32(y_px)
                    .i32(buf_w)
                    .i32(height_px),
            );
        }
        let cb: CallbackId = self.window_tx.alloc_id();
        self.window_tx
            .request(MessageBuilder::new(self.shell.surface, SurfaceRequest::Frame).new_id(cb));
        self.window_tx.request(MessageBuilder::new(
            self.shell.surface,
            SurfaceRequest::Commit,
        ));

        self.present.finish_present(cb);
    }

    fn maybe_present(
        &mut self,
        custom_paint: Option<&mut dyn FnMut(&mut crate::draw::Canvas<'_>) -> crate::draw::Damage>,
    ) -> Result<()> {
        if !self.lifecycle.can_present() {
            return Ok(());
        }
        self.apply_pending_resize()?;
        self.debug_pre_present();
        let slot = match self.present.begin_present() {
            crate::present::PresentAttempt::NotDirty => return Ok(()),
            crate::present::PresentAttempt::NotReady => {
                self.debug_note_skipped_not_ready();
                return Ok(());
            }
            crate::present::PresentAttempt::NoFreeSlot => {
                self.debug_note_skipped_no_slot();
                return Ok(());
            }
            crate::present::PresentAttempt::Ready(slot) => slot,
        };
        // Time the real work below (blit + shm copy + damage), excluding the
        // early-return checks and the compositor wait that gated us getting
        // here. See `FrameStats::record_busy`.
        let present_start = std::time::Instant::now();

        // Blit pending dirty snapshots into the persistent backbuffer, which
        // always holds the full image, then copy only the rows that actually
        // changed into the free shm frame -- tracked in `touched_rows` and
        // reconciled against `stale_rows` below.
        let pending = std::mem::take(&mut self.pending_dirty);
        let folded = Self::fold_dirty_snapshots(pending);
        let mut touched_rows: Vec<u16> = Vec::new();
        let mut scrolls: Vec<grid::ScrollBand> = Vec::new();
        // Signed whole-screen shifts from viewport (scrollback) pixel-scrolls
        // that `blit_snapshot` performed this frame. Unlike `scrolls` (an
        // upward-only band), these are signed and cover the full screen, so
        // baked overlays (fps readout, a resting cursor/selection) get dragged
        // by the memmove and their row trackers must be remapped to match.
        let mut canvas = self.pixels.canvas(self.width, self.height);
        let (viewport_shifts, painted_rows, painted_cells) = self.renderer.blit_folded_snapshots(
            &mut canvas,
            &folded,
            &mut scrolls,
            &mut touched_rows,
        );
        // `Canvas` borrows the backbuffer mutably and holds no resources of
        // its own (no `Drop` impl) -- this ends that borrow early so the code
        // below can use `self` again; `drop()` here is a borrow-checker nudge,
        // not resource cleanup.
        // Sub-phase split of the (misnamed) `blit` region for custom-painter
        // windows: the snapshot-blit loop above is empty for them, so the time
        // the trace was attributing to `blit` is really the paint callback and
        // the premultiply. Capture a fence after the snapshot loop, another
        // after the paint callback, so a slow present names WHICH of the three
        // owns the spike instead of lumping them under `blit`.
        let t_snapshot = present_start.elapsed();
        let custom_painted = custom_paint.is_some();
        let custom_damage = custom_paint.map(|paint| paint(&mut canvas));
        let _ = canvas;
        let t_paint = present_start.elapsed();
        if let Some(damage) = &custom_damage {
            self.update_custom_argb_for_damage(damage);
            self.extend_touched_rows_for_custom_damage(damage, &mut touched_rows);
        }
        // Sub-phase timers (only read when DebugFps::Log fires below on a slow
        // present): `t_blit` = the coalesce+blit_snapshot loop, the only phase
        // the `blit` bench covers. The gap to `t_overlay` is overlay repaint;
        // the gap from there to the end is the stale-row shm memcpy + Wayland
        // Attach/Damage/Commit submission -- the phases the bench never sees,
        // and the prime suspects for the ~16ms `present-busy` spikes.
        let t_blit = present_start.elapsed();
        let folded_len = folded.len();
        self.remap_baked_overlays_for_this_frames_scrolls(
            &viewport_shifts,
            &scrolls,
            &mut touched_rows,
        );
        if custom_painted {
            self.clear_terminal_overlays_for_custom_paint();
        } else {
            self.composite_terminal_overlays(&mut touched_rows);
        }
        let t_overlay = present_start.elapsed();

        // Every row touched this frame is now stale relative to *both* shm
        // slots (whichever one isn't presented into this time still needs
        // catching up next time it's used).
        self.present.mark_rows_stale(touched_rows);

        let damage_rects = self.copy_stale_rows_into_slot(slot, custom_painted);
        self.submit_frame(slot, damage_rects);
        let total = present_start.elapsed();
        // On a slow present (>5ms -- never the 60fps steady state, always the
        // spikes we're chasing), break the timed region into its three phases
        // so we can see WHICH one owns the ~16ms.
        self.debug_record_busy_and_log(
            total,
            t_snapshot,
            t_paint,
            t_blit,
            t_overlay,
            folded_len,
            painted_rows,
            painted_cells,
        );
        Ok(())
    }

    fn resize(&mut self, w: i32, h: i32) -> Result<()> {
        // Destroy only the old *buffers* — the pool itself is reused. The server
        // answers each Destroy with `wl_display.delete_id`, which recycles those
        // ids; without it every resize step would leak 2 buffer ids. (Before,
        // the pool was destroyed and rebuilt too, which also re-`mmap`ped and
        // re-sent the shm fd on every drag frame — now avoided.)
        self.present.destroy_old_buffers(&mut self.window_tx);

        // Grow the existing pool in place instead of tearing it down: `ftruncate`
        // the memfd + `mremap` our mapping, and only when it actually grew tell
        // the compositor via `wl_shm_pool.resize`. A shrink keeps the oversized
        // pool and skips the round-trip entirely (lazy shrink). Either way the
        // shm fd is never re-sent, so a resize storm stops thrashing the pool.
        let grew = self
            .present
            .resize_shm(w, h)
            .map_err(|e| PlatformError::Conn(ConnError::Sys(e)))?;

        if let Some(new_cap) = grew {
            // Route the resize through the same `window_tx` MPSC ring as the
            // buffer creates below so it stays ordered ahead of them (the pool
            // must be big enough before the new buffers are carved out of it).
            self.present.grow_pool(&mut self.window_tx, new_cap);
        }

        self.present.recreate_buffers(&mut self.window_tx, w, h);
        // Carry the old image into the new backbuffer (top-left aligned) instead
        // of clearing to background, and rebuild the premultiplied custom-
        // painter cache at the new size. The grid reflow + full snapshot
        // arrives a round-trip later via the parser thread; until then we
        // keep showing the previous content so the window doesn't flash
        // blank mid-drag. See [`crate::pixel_buffers::PixelBuffers::resize`]
        // for why `bg_packed()` (not `bg_xrgb()`) is used for the fill.
        self.pixels.resize(
            self.width,
            self.height,
            w,
            h,
            self.renderer.bg_packed(),
            self.renderer.background_alpha(),
        );
        // A configure can end between cell boundaries. The carried old image
        // is useful for complete grid rows while the reflow snapshot is in
        // flight, but no grid cell owns the remaining bottom scanlines. Clear
        // them now so a cropped old glyph cannot become a permanently stuck
        // partial line (dirty snapshots intentionally paint whole rows only).
        self.pixels
            .clear_partial_cell_row(w, h, self.cell_h, self.renderer.bg_packed());

        // `self.present`'s pool is unchanged — we reuse it across resizes now.
        self.width = w;
        self.height = h;
        // Both shm buffers are brand-new (freshly created, uninitialized)
        // memory, so every row is stale relative to the backbuffer in both
        // slots -- the next two presents each need a full copy to actually
        // fill their buffer, not just the rows that changed since the old
        // size. Both buffers are also brand-new and free, and any in-flight
        // frame callback referred to the now-destroyed surface contents, so
        // force the next present to fire immediately (never leave the
        // surface showing a detached/blank buffer -- the cause of resize
        // flashing).
        let row_count = ((h + self.cell_h - 1) / self.cell_h).max(1) as usize;
        self.present.reset_after_resize(row_count);
        Ok(())
    }

    fn join_reader(&mut self) -> Result<()> {
        if let Some(handle) = self.reader.take() {
            match handle.join() {
                Ok(result) => result,
                Err(_) => Err(PlatformError::ReaderPanicked),
            }
        } else {
            Ok(())
        }
    }

    /// Gracefully close the window and join the reader thread.
    ///
    /// The socket is shut down first. Without it the reader stays parked on
    /// a connection nobody will ever write to again, and only its
    /// no-progress watchdog releases it -- 4096 completions later, printing
    /// a "connection wedged" line on an ordinary exit.
    ///
    /// `shutdown` rather than closing the descriptor: the reader's recv was
    /// submitted against this fd *number*, so closing it first completes
    /// that recv with `EBADF` and turns a clean exit into a reported I/O
    /// error. The descriptor stays open until `self` drops, after the join.
    pub fn close(mut self) -> Result<()> {
        let _ = self.sender.shutdown();
        self.join_reader()
    }
}

// ── helpers copied from paint/app.rs ─────────────────────────────────────────

struct Registry;

/// The core globals [`Registry::discover`] binds off the wire. Grouped into
/// a named tuple struct (rather than a bare 6-tuple) purely to give clippy's
/// `type_complexity` lint something readable to point at -- destructured at
/// the one call site exactly like the tuple it replaced.
struct Globals {
    compositor: CompositorId,
    shm: ShmId,
    wm_base: WmBaseId,
    seat: SeatId,
    output: Option<OutputId>,
    data_device_manager: Option<DataDeviceManagerId>,
    /// `xdg_activation_v1` -- the factory for the tokens that let a program
    /// this window launches raise itself onto the current workspace.
    /// Optional like the others: a compositor without it means a Ctrl+clicked
    /// link opens where that application already lived.
    activation: Option<crate::id::ActivationId>,
}

/// Accumulates globals as `wl_registry.global` events arrive, one field per
/// interface this window cares about. Pulled out of `Registry::discover`'s
/// body (which was one `while` loop containing a 6-armed match, each arm
/// duplicating the same "already bound? else bind and stash" shape) so the
/// per-event dispatch and the "have we seen everything required yet?" check
/// are each a single named, independently readable method instead of being
/// interleaved inline.
#[derive(Default)]
struct GlobalsBuilder {
    compositor: Option<CompositorId>,
    shm: Option<ShmId>,
    wm_base: Option<WmBaseId>,
    seat: Option<SeatId>,
    output: Option<OutputId>,
    data_device_manager: Option<DataDeviceManagerId>,
    activation: Option<crate::id::ActivationId>,
}

impl crate::surface::GlobalCollector for GlobalsBuilder {
    /// Whether every global `Registry::discover`'s caller cannot proceed
    /// without has been bound. `wl_output` and `wl_data_device_manager` are
    /// optional (not every compositor advertises them), so they're excluded
    /// from this check -- the registry burst loop stops as soon as the
    /// required set is complete, giving the optional ones only whatever
    /// chance they had to arrive first.
    fn is_complete(&self) -> bool {
        self.compositor.is_some()
            && self.shm.is_some()
            && self.wm_base.is_some()
            && self.seat.is_some()
    }

    /// Bind `interface` if it's one this window needs and hasn't already
    /// bound (a compositor could in principle advertise a duplicate; the
    /// first one wins), delegating the repeated "already bound? else
    /// negotiate a version and bind" shape to `Self::bind_if_missing` so
    /// each match arm here is a single call rather than an inline `&&`-guard
    /// plus a 2-line body -- the guard conditions alone had been contributing
    /// 6 extra decision points to this function's complexity. A no-op for
    /// any interface not in the list below.
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
            s if s == proto::XDG_WM_BASE.name => Self::bind_if_missing(
                &mut self.wm_base,
                sender,
                registry,
                name,
                interface,
                version,
                proto::XDG_WM_BASE.version,
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
            s if s == proto::WL_DATA_DEVICE_MANAGER.name => Self::bind_if_missing(
                &mut self.data_device_manager,
                sender,
                registry,
                name,
                interface,
                version,
                proto::WL_DATA_DEVICE_MANAGER.version,
            ),
            s if s == proto::XDG_ACTIVATION_V1.name => Self::bind_if_missing(
                &mut self.activation,
                sender,
                registry,
                name,
                interface,
                version,
                proto::XDG_ACTIVATION_V1.version,
            ),
            _ => Ok(()),
        }
    }
}

impl GlobalsBuilder {
    /// Consume the builder into the final [`Globals`], failing if any
    /// required global never arrived (`has_every_required_global` guards
    /// the caller's loop, so in practice these `ok_or`s only fire if the
    /// loop was exited some other way -- kept as a defensive check rather
    /// than an `unwrap`, since a protocol violation here shouldn't panic).
    fn finish(self) -> Result<Globals> {
        Ok(Globals {
            compositor: self.compositor.ok_or(PlatformError::ReaderStopped)?,
            shm: self.shm.ok_or(PlatformError::ReaderStopped)?,
            wm_base: self.wm_base.ok_or(PlatformError::ReaderStopped)?,
            seat: self.seat.ok_or(PlatformError::ReaderStopped)?,
            output: self.output,
            data_device_manager: self.data_device_manager,
            activation: self.activation,
        })
    }
}

impl Registry {
    /// Bind the globals every window needs off the wire, including (if the
    /// compositor advertises one) the first `wl_output` -- so
    /// [`Self::output_scale`] can read its buffer scale before we size any
    /// buffers. `wl_output` is optional: some setups advertise none at
    /// startup, in which case the caller falls back to scale 1.
    ///
    /// Was a free `bind_globals(sender, events)` function -- moved onto
    /// `Registry` as its entry point.
    fn discover(sender: &mut Sender, events: &mut Consumer<RawEvent>) -> Result<Globals> {
        let mut globals = GlobalsBuilder::default();
        crate::surface::RegistryDiscovery::collect(sender, events, &mut globals)?;
        globals.finish()
    }

    /// Read `output`'s advertised buffer scale via `wl_display.sync`: send a
    /// `wl_display.sync` request (which the server answers with a
    /// `wl_callback.done` only after every request queued before it has been
    /// fully processed and every event that follows from them has been sent),
    /// and collect `wl_output.scale` events until that `done` arrives.
    ///
    /// This guarantees we've seen every `scale` event the compositor is ever
    /// going to send for this output before we act on it -- unlike blocking
    /// on the very first event, which could race ahead of `scale` if the
    /// compositor happens to send `geometry`/`mode` first. Returns `1` (no
    /// scaling) if the output never reports a scale (some compositors only
    /// send it when it's non-default) or times out.
    ///
    /// Delegates to the role-agnostic
    /// [`crate::surface::SurfaceBootstrap::output_scale`].
    fn output_scale(sender: &mut Sender, events: &mut Consumer<RawEvent>, output: OutputId) -> i32 {
        crate::surface::SurfaceBootstrap::output_scale(sender, events, output)
    }
}

#[cfg(test)]
mod tests {
    use super::TermWindow;

    /// Terse `ScrollBand` for the pixel-scroll tests below.
    fn band(top: u16, bot: u16, delta: u16) -> grid::ScrollBand {
        grid::ScrollBand { top, bot, delta }
    }

    #[test]
    fn scrolled_band_marks_every_moved_row_touched_not_just_new_ones() {
        // Regression: a fast-scroll frame's snapshot only lists the genuinely-
        // new bottom rows as dirty (the grid shifts its dirty marks up with the
        // content). But the pixel-scroll copy_within rewrote the WHOLE band in
        // the backbuffer, and the shm slots aren't scrolled, so every moved row
        // must be re-copied. If only the new rows were marked, the screen would
        // freeze all but the bottom few lines -- the reported flood artifact.
        let mut touched = vec![9u16, 10, 11]; // the 3 new bottom rows
        TermWindow::extend_with_scrolled_bands(&mut touched, &[band(0, 11, 3)]);
        touched.sort_unstable();
        touched.dedup();
        // The full band 0..=11 must now be present, not just 9,10,11.
        assert_eq!(touched, (0u16..=11).collect::<Vec<_>>());
    }

    #[test]
    fn underline_stops_at_the_last_full_column_not_the_buffer_edge() {
        // 100px wide, 8px cells => 12 columns (96px) + a 4px dead strip that
        // no column owns and `repaint_rows` never cleans. A link span running
        // to the mirror's full width must stop at 96, not smear to 100.
        let span = TermWindow::underline_span(0, 20, 12, 8, 100);
        assert_eq!(span, Some((0, 96)));
    }

    #[test]
    fn underline_of_an_ordinary_span_is_left_exactly_as_it_was() {
        // The common case must be untouched by the clamp: columns 2..=5 of an
        // 8px grid is precisely pixels 16..48.
        let span = TermWindow::underline_span(2, 5, 12, 8, 100);
        assert_eq!(span, Some((16, 48)));
    }

    #[test]
    fn underline_span_starting_past_the_last_column_is_dropped() {
        // A span living entirely in the off-grid remainder has nothing visible
        // to underline; clamping it would paint a bogus mark on the last
        // column, so it must be dropped instead.
        assert_eq!(TermWindow::underline_span(12, 20, 12, 8, 100), None);
    }

    #[test]
    fn underline_fits_exactly_when_the_width_is_a_whole_number_of_cells() {
        // No dead strip at all (96 = 12 * 8): the last column still underlines
        // to the very edge, proving the clamp costs nothing in the exact case.
        assert_eq!(TermWindow::underline_span(11, 11, 12, 8, 96), Some((88, 96)));
    }

    #[test]
    fn shift_overlay_rows_remaps_the_cursor_imprint_up_by_the_scroll() {
        // A cursor baked at row 5 last frame. This frame the whole screen
        // (rows 0..=9) pixel-scrolled up by 1, so the imprint now lives at
        // row 4 -- that's where we must repaint clean, not row 5.
        let mut rows = vec![5u16];
        TermWindow::shift_overlay_rows(&mut rows, &[band(0, 9, 1)]);
        assert_eq!(rows, vec![4]);
    }

    #[test]
    fn shift_overlay_rows_drops_rows_scrolled_off_the_top() {
        // A row inside the shifted band but within `delta` of its top scrolled
        // off-screen entirely; there's nothing left to clean, so it's dropped.
        let mut rows = vec![0u16, 1, 2];
        TermWindow::shift_overlay_rows(&mut rows, &[band(0, 9, 2)]);
        // row 0,1 scrolled off (0-2 < 0, 1-2 < 0); row 2 -> 0.
        assert_eq!(rows, vec![0]);
    }

    #[test]
    fn shift_overlay_rows_leaves_rows_outside_the_band_untouched() {
        // Only rows 3..=6 scrolled; a tracked row above/below the band keeps
        // its position.
        let mut rows = vec![1u16, 5, 8];
        TermWindow::shift_overlay_rows(&mut rows, &[band(3, 6, 1)]);
        assert_eq!(rows, vec![1, 4, 8]);
    }

    #[test]
    fn shift_overlay_rows_composes_multiple_scrolls_in_order() {
        // Two scrolls in one frame compose: row 7 -> 6 -> 5.
        let mut rows = vec![7u16];
        TermWindow::shift_overlay_rows(&mut rows, &[band(0, 9, 1), band(0, 9, 1)]);
        assert_eq!(rows, vec![5]);
    }

    #[test]
    fn shift_overlay_rows_noop_without_scrolls() {
        let mut rows = vec![2u16, 4];
        TermWindow::shift_overlay_rows(&mut rows, &[]);
        assert_eq!(rows, vec![2, 4]);
    }

    #[test]
    fn shift_overlay_rows_signed_positive_lifts_imprint_up() {
        // A viewport scroll-back toward the bottom shifted the whole 10-row
        // screen up by 3 (s = +3): an overlay at row 7 now lives at row 4.
        let mut rows = vec![7u16];
        TermWindow::shift_overlay_rows_signed(&mut rows, &[3], 10);
        assert_eq!(rows, vec![4]);
    }

    #[test]
    fn shift_overlay_rows_signed_negative_pushes_imprint_down() {
        // Scroll up into history shifted the screen DOWN by 2 (s = -2): an
        // overlay at row 4 now lives at row 6.
        let mut rows = vec![4u16];
        TermWindow::shift_overlay_rows_signed(&mut rows, &[-2], 10);
        assert_eq!(rows, vec![6]);
    }

    #[test]
    fn shift_overlay_rows_signed_drops_rows_shifted_off_either_edge() {
        // s = +3 lifts everything up 3: rows 0,1,2 fall off the top; row 9 -> 6.
        let mut rows = vec![0u16, 2, 9];
        TermWindow::shift_overlay_rows_signed(&mut rows, &[3], 10);
        assert_eq!(rows, vec![6]);
        // s = -3 pushes down 3: rows 8,9 fall off the bottom; row 1 -> 4.
        let mut rows = vec![1u16, 8, 9];
        TermWindow::shift_overlay_rows_signed(&mut rows, &[-3], 10);
        assert_eq!(rows, vec![4]);
    }

    #[test]
    fn shift_overlay_rows_signed_composes_multiple_shifts_in_order() {
        // Two shifts in one frame compose: row 8 -(+2)-> 6 -(+2)-> 4.
        let mut rows = vec![8u16];
        TermWindow::shift_overlay_rows_signed(&mut rows, &[2, 2], 10);
        assert_eq!(rows, vec![4]);
    }
}

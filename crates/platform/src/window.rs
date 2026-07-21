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
use crate::error::{Error, Result};
use crate::event::RawEvent;
use crate::id::{
    CallbackId, CompositorId, DataDeviceManagerId, DisplayId, KeyboardId, ObjectId,
    OutputId, PointerId, RegistryId, SeatId,
    ShmId, SurfaceId, ToplevelId, WmBaseId, XdgSurfaceId,
};
use crate::keymap;
use crate::protocol::Modifiers;
use crate::protocol::{
    self as proto, ButtonState, CompositorRequest, DataDeviceEvent, DataOfferEvent,
    DataSourceEvent, DisplayEvent, DisplayRequest, Format, Key, KeyState, KeyboardEvent,
    PointerButton, PointerEvent, PointerRequest, RegistryEvent, SeatCapability, SeatEvent,
    SeatRequest, SurfaceRequest, ToplevelEvent, ToplevelRequest, WmBaseEvent,
    WmBaseRequest, XdgSurfaceEvent, XdgSurfaceRequest,
};
use crate::render;
use crate::wire::{MessageBuilder, WireEvent};

const EVENT_RING_CAP: usize = 1024;

/// Stable Wayland `app_id` for our toplevel. Fixed (not the caller's title) so
/// compositor window rules and tooling can match this window reliably even
/// after the shell rewrites the title at runtime.
const APP_ID: &str = "forfun.terminal";

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

/// Mouse button identity, mirroring [`crate::protocol::PointerButton`] plus
/// the wheel/no-button cases `wl_pointer.axis`/motion-with-no-press need.
/// Kept distinct from `protocol::PointerButton` (which only models the wire
/// button codes) so `InputEvent::Mouse` can also carry wheel-scroll and
/// bare-hover reports.
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

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Lifecycle {
    Unconfigured,
    Mapped,
    Closing,
}

use crate::surface::Frame;
#[cfg(feature = "trace")]
use crate::debug_overlay::DebugOverlay;

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
    surface: SurfaceId,
    xdg_surface: XdgSurfaceId,
    toplevel: ToplevelId,
    seat: SeatId,
    pointer: Option<PointerId>,
    keyboard: Option<KeyboardId>,

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
    /// Wall-clock instant of the last [`Self::tick`] call, for computing the
    /// animator's `dt_ms`.
    last_tick: std::time::Instant,

    /// Frame-presentation state: the double-buffered shm swap chain, the
    /// per-slot stale-row bitmap, and the dirty/frame-ready/frame-callback
    /// handshake that gates when a present may fire. Extracted from this
    /// struct so the swap-chain lifecycle lives (and is tested) in one
    /// place; see [`crate::present::PresentState`].
    present: crate::present::PresentState,
    /// Persistent full-window backbuffer. Dirty rows are blitted here (it keeps
    /// the complete last-known image), then copied into whichever shm frame is
    /// free on present. Without this, incremental dirty-row blits would only
    /// ever touch one of the two shm frames, leaving the rest black.
    backbuffer: Vec<u32>,
    /// Glyph source (bitmap or loaded TTF/OTF face) + resolved color theme.
    renderer: render::Renderer,
    width: i32,
    height: i32,
    /// The `wl_output`-advertised buffer scale this window opened at (1 =
    /// normal DPI, 2 = HiDPI @2x, ...). Fixed for the window's lifetime
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
    lifecycle: Lifecycle,

    /// Latest window size proposed by `xdg_toplevel.configure`, applied once per
    /// frame so a drag's flood of configures collapses into one resize.
    pending_resize: Option<(i32, i32)>,
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
    /// Set when a `wl_keyboard.keymap` event just arrived; the next
    /// `RawEvent::Fd` is the keymap blob to `mmap` + compile via
    /// `crate::xkb`. Carries the event's `size` argument (the fd itself has
    /// no length of its own -- the wire event tells us how much to map).
    awaiting_keymap_fd: Option<usize>,
    /// Viewport rows that carried a highlight on the previous frame. Repainted
    /// clean on the next frame before the (possibly moved/cleared) selection
    /// is re-tinted, so highlight never lingers after deselect or drag.
    prev_selection_rows: Vec<u16>,
    /// `link_id` of the cell the pointer is over while Ctrl is held (`0` for
    /// "none"), and the viewport row that hover-underline was last drawn on
    /// (so the next frame knows what to repaint clean before redrawing or
    /// removing it). Set from pointer motion in `handle_pointer`; consumed by
    /// [`Self::apply_link_hover_underline`] in the present path.
    hover_link_id: u32,
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
        let _ = self
            .sender
            .request(MessageBuilder::new(self.toplevel, ToplevelRequest::Destroy));
        let _ = self.sender.request(MessageBuilder::new(
            self.xdg_surface,
            XdgSurfaceRequest::Destroy,
        ));
        let _ = self
            .sender
            .request(MessageBuilder::new(self.surface, SurfaceRequest::Destroy));
    }
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
            crate::surface::connect(EVENT_RING_CAP, "terminal-wayland-reader", &wake)?;

        let Globals {
            compositor,
            shm,
            wm_base,
            seat,
            output,
            data_device_manager,
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

        let surface: SurfaceId = sender.alloc_id();
        sender.request(
            MessageBuilder::new(compositor, CompositorRequest::CreateSurface).new_id(surface),
        )?;
        // Tell the compositor our buffer is already at `scale`x native
        // density -- matches the physical pixels the font was rasterized at
        // above, so it composites us 1:1 with no resampling blur.
        sender.request(MessageBuilder::new(surface, SurfaceRequest::SetBufferScale).i32(scale))?;

        let xdg_surface: XdgSurfaceId = sender.alloc_id();
        sender.request(
            MessageBuilder::new(wm_base, WmBaseRequest::GetXdgSurface)
                .new_id(xdg_surface)
                .object(surface),
        )?;

        let toplevel: ToplevelId = sender.alloc_id();
        sender.request(
            MessageBuilder::new(xdg_surface, XdgSurfaceRequest::GetToplevel).new_id(toplevel),
        )?;
        sender.request(MessageBuilder::new(toplevel, ToplevelRequest::SetTitle).string(title))?;
        // Also advertise a fixed app_id. The title is human-facing and shells
        // rewrite it at runtime (OSC 0/2), so it's a poor match key; app_id is
        // stable, letting tools (e.g. scripts/stress/resize_storm.sh) target
        // *this* window without also grabbing whatever terminal launched it.
        sender.request(MessageBuilder::new(toplevel, ToplevelRequest::SetAppId).string(APP_ID))?;
        sender.request(MessageBuilder::new(surface, SurfaceRequest::Commit))?;

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

        // Use `bg_packed()`, not `bg_xrgb()`: a translucent window's initial
        // backbuffer must carry the configured alpha from frame one, not
        // flash fully opaque until the first real paint.
        let backbuffer = vec![renderer.bg_packed(); (width * height) as usize];
        // Both shm frames start as blank memory (freshly mmap'd), so every
        // row is "stale" relative to the backbuffer until the first present
        // copies it in -- `PresentState::new` seeds that.
        let row_count = (rows as usize).max(1);
        let present =
            crate::present::PresentState::new(frames, shm_mem, pool, buf_format, row_count);

        Ok(Self {
            sender,
            request_rx: req_rx,
            window_tx,
            events,
            reader: Some(reader_handle),
            wake,
            compositor,
            shm,
            wm_base,
            surface,
            xdg_surface,
            toplevel,
            seat,
            pointer: None,
            keyboard: None,
            key_repeat: crate::key_repeat::KeyRepeatState::new(repeat_timer),
            modifiers: Modifiers::default(),
            xkb: None,
            cursor,
            present,
            backbuffer,
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
            lifecycle: Lifecycle::Unconfigured,
            pending_resize: None,
            pending_dirty: Vec::new(),
            input_queue: std::collections::VecDeque::new(),
            clipboard,
            selection: crate::selection::SelectionTracker::new(),
            awaiting_keymap_fd: None,
            prev_selection_rows: Vec::new(),
            hover_link_id: 0,
            prev_hover_row: None,
            prev_cursor_rows: Vec::new(),
            cursor_anim: crate::cursor_anim::CursorAnimator::new(cursor_cfg),
            last_tick: std::time::Instant::now(),
            #[cfg(feature = "trace")]
            debug: DebugOverlay::new(),
            autoscroll_last: None,
            base_title: title.to_string(),
            bell_until: None,
        })
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
    #[must_use]
    pub fn link_id_at(&self, col: u16, row: u16) -> u32 {
        self.selection
            .mirror()
            .get(usize::from(row))
            .and_then(|line| line.get(usize::from(col)))
            .map_or(0, |cell| cell.link_id)
    }

    /// Recompute which link (if any) is under the pointer given the current
    /// Ctrl state, and mark the window dirty if that changed the on-screen
    /// hover-underline (either a different link, or Ctrl toggled while over
    /// the same one). Called on every pointer motion and every modifier
    /// change, since either can start or end a hover.
    fn update_hover_link(&mut self, col: u16, row: u16) {
        // XKB modifier index 2 (bit 0x04) is Ctrl -- same bit `press_left`'s
        // shift check above reads for its own index (0x01).
        let ctrl =
            (self.modifiers.depressed | self.modifiers.latched | self.modifiers.locked) & 0x04 != 0;
        let new = if ctrl { self.link_id_at(col, row) } else { 0 };
        if new != self.hover_link_id {
            self.hover_link_id = new;
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
    pub fn request_paste(&mut self) {
        self.clipboard.request_paste(&mut self.window_tx, &self.wake);
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
        if self.lifecycle == Lifecycle::Closing {
            return Ok(false);
        }
        self.sender
            .drain_requests(&mut self.request_rx)
            .map_err(Error::Conn)?;

        // Drain all queued events non-blocking.
        while let Some(ev) = self.events.pop() {
            self.handle_event(ev)?;
            if self.lifecycle == Lifecycle::Closing {
                return Ok(false);
            }
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
            self.window_tx.request(
                MessageBuilder::new(self.toplevel, ToplevelRequest::SetTitle).string(&title),
            );
        }

        self.advance_autoscroll();
        self.advance_cursor_anim();
        self.maybe_present()?;
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

    /// Whether the caller should keep polling at animation-frame-rate
    /// (~60fps) instead of blocking on [`Self::wait`]'s full timeout -- true
    /// while the cursor is mid-slide, blinking and visible, or a selection
    /// drag is pinned at the top/bottom edge and needs to keep autoscrolling
    /// (see [`Self::advance_autoscroll`]) -- without this the UI loop would
    /// happily block up to 100ms between autoscroll steps, into a wait that
    /// nothing but a wall-clock timer wakes it from.
    #[must_use]
    pub fn wants_animation_frame(&self) -> bool {
        self.cursor_anim.is_animating() || self.autoscroll_active()
    }

    /// Whether the pointer currently sits within [`AUTOSCROLL_MARGIN_PX`] of
    /// the top or bottom edge of the content area while a selection drag is
    /// held -- the condition [`Self::advance_autoscroll`] acts on. Checked
    /// against a margin band (not the literal edge) since Wayland never
    /// reports pointer coordinates outside our surface -- see
    /// [`AUTOSCROLL_MARGIN_PX`].
    fn autoscroll_active(&self) -> bool {
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
    pub fn set_app_mouse(&mut self, on: bool) {
        self.app_mouse = on;
        if !on {
            self.selection_bypass = false;
        }
    }

    /// Notify the cursor animator of user activity (a keystroke): resets the
    /// blink phase to full opacity so the cursor is always solid the instant
    /// you type, never mid-fade. Call this whenever the caller sends bytes to
    /// the PTY in response to a key press.
    pub fn notify_activity(&mut self) {
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
    /// builds the new `Renderer` (needs `output_scale()` for HiDPI, and may
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
    pub fn set_cursor_config(&mut self, cfg: crate::cursor_anim::CursorAnimConfig) {
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
        self.window_tx
            .request(MessageBuilder::new(self.toplevel, ToplevelRequest::SetTitle).string(title));
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
            self.window_tx.request(
                MessageBuilder::new(self.toplevel, ToplevelRequest::SetTitle).string(&flash),
            );
        }
        self.bell_until = Some(std::time::Instant::now() + BELL_FLASH);
    }

    /// Block for the next Wayland event, then drain all queued events.
    /// Returns `true` while the window should stay open.
    pub fn run_once(&mut self) -> Result<bool> {
        if self.lifecycle == Lifecycle::Closing {
            return Ok(false);
        }
        self.sender
            .drain_requests(&mut self.request_rx)
            .map_err(Error::Conn)?;

        let Some(first) = self.events.pop_block() else {
            return Ok(false);
        };
        self.handle_event(first)?;
        while let Some(ev) = self.events.pop() {
            self.handle_event(ev)?;
            if self.lifecycle == Lifecycle::Closing {
                break;
            }
        }
        self.advance_cursor_anim();
        self.maybe_present()?;
        Ok(self.lifecycle != Lifecycle::Closing)
    }

    // ── private ───────────────────────────────────────────────────────────

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
    /// `input.keyboard.xkb` (swapping Esc and CapsLock) touches, and exactly
    /// the class of bug reported (Esc/CapsLock producing the wrong effect,
    /// or leaking a raw escape into a TUI that never asked for it). For
    /// everything else -- letters, digits, punctuation -- we keep the
    /// physical (US-QWERTY) mapping: correct for the common case, and no
    /// worse than before XKB was wired in.
    fn resolve_logical_key(&self, physical: Option<Key>, evdev_code: u32) -> Option<Key> {
        if let Some(xkb) = &self.xkb {
            let sym = xkb.keysym(evdev_code);
            if sym != 0 {
                if let Some(k) = keymap::keysym_to_key(sym) {
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
            if let Some(key) = self.key_repeat.held_key() {
                self.input_queue.push_back(InputEvent::Key {
                    key: Some(key),
                    state: Some(KeyState::Pressed),
                    modifiers: self.modifiers,
                });
            }
            return Ok(());
        }

        // xdg_wm_base.ping
        if ev.is(self.wm_base) && ev.event::<WmBaseEvent>() == Some(WmBaseEvent::Ping) {
            self.window_tx.request(
                MessageBuilder::new(self.wm_base, WmBaseRequest::Pong).u32(ev.serial(0).raw()),
            );
            return Ok(());
        }

        // xdg_surface.configure
        if ev.is(self.xdg_surface)
            && ev.event::<XdgSurfaceEvent>() == Some(XdgSurfaceEvent::Configure)
        {
            self.window_tx.request(
                MessageBuilder::new(self.xdg_surface, XdgSurfaceRequest::AckConfigure)
                    .u32(ev.serial(0).raw()),
            );
            self.lifecycle = Lifecycle::Mapped;
            self.maybe_present()?;
            return Ok(());
        }

        // xdg_toplevel.close
        if ev.is(self.toplevel) && ev.event::<ToplevelEvent>() == Some(ToplevelEvent::Close) {
            self.lifecycle = Lifecycle::Closing;
            self.input_queue.push_back(InputEvent::Close);
            return Ok(());
        }

        // xdg_toplevel.configure (resize)
        if ev.is(self.toplevel) && ev.event::<ToplevelEvent>() == Some(ToplevelEvent::Configure) {
            // xdg_toplevel.configure args: (width, height, states[]), in
            // *logical* (surface-local) coordinates -- xdg-shell sizes are
            // always pre-scale, regardless of our `wl_surface.buffer_scale`.
            // Our shm buffers are physical pixels (`scale`x the logical
            // size), so this must be scaled up before it's usable as a
            // buffer/backbuffer dimension -- see `apply_pending_resize`, the
            // only reader of `pending_resize`, which now receives already-
            // scaled physical pixels and no longer needs to know about scale
            // itself. A drag emits these dozens of times a second; just
            // record the latest and coalesce into a single resize per frame.
            let w = ev.arg_i32(0) * self.scale;
            let h = ev.arg_i32(1) * self.scale;
            if w > 0 && h > 0 {
                self.pending_resize = Some((w, h));
            }
            return Ok(());
        }

        // wl_seat.capabilities
        if ev.is(self.seat) && ev.event::<SeatEvent>() == Some(SeatEvent::Capabilities) {
            let caps = ev.arg(0);
            if self.pointer.is_none() && SeatCapability::Pointer.in_mask(caps) {
                let pointer: PointerId = self.window_tx.alloc_id();
                self.window_tx.request(
                    MessageBuilder::new(self.seat, SeatRequest::GetPointer).new_id(pointer),
                );
                self.pointer = Some(pointer);
            }
            if self.keyboard.is_none() && SeatCapability::Keyboard.in_mask(caps) {
                let keyboard: KeyboardId = self.window_tx.alloc_id();
                self.window_tx.request(
                    MessageBuilder::new(self.seat, SeatRequest::GetKeyboard).new_id(keyboard),
                );
                self.keyboard = Some(keyboard);
            }
            return Ok(());
        }

        // wl_pointer.*
        if let Some(pointer) = self.pointer {
            if ev.is(pointer) {
                match ev.event::<PointerEvent>() {
                    Some(PointerEvent::Enter) => {
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
                    Some(PointerEvent::Motion) => {
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
                        if !(self.selection_bypass
                            && self.mouse_pressed == Some(MouseButton::Left))
                        {
                            self.input_queue.push_back(InputEvent::Mouse {
                                button: self.mouse_pressed.unwrap_or(MouseButton::None),
                                kind: MouseEventKind::Motion,
                                col: cc,
                                row: cr,
                                modifiers: self.modifiers,
                            });
                        }
                    }
                    Some(PointerEvent::Button) => {
                        let button = PointerButton::from_code(ev.arg(2));
                        let state = ButtonState::from_raw(ev.arg(3));
                        let shift = (self.modifiers.depressed
                            | self.modifiers.latched
                            | self.modifiers.locked)
                            & 0x01
                            != 0;
                        // Ownership of the left button (kitty/foot/xterm
                        // convention): while an app has negotiated mouse
                        // tracking it owns bare clicks (reported, no terminal
                        // selection); Shift+click bypasses the app and drives
                        // the terminal's own selection instead (selection
                        // runs, report suppressed for the whole drag). With
                        // no tracking the terminal owns the mouse outright.
                        let is_left = matches!(button, Some(PointerButton::Left));
                        let bypass_starts = is_left
                            && matches!(state, Some(ButtonState::Pressed))
                            && (!self.app_mouse || shift);
                        let bypass_ends = is_left
                            && matches!(state, Some(ButtonState::Released))
                            && self.selection_bypass;
                        // Suppress the app report for both halves of a bypassed
                        // drag — an orphan press or release confuses the app.
                        let suppress_report =
                            self.app_mouse && (bypass_starts && shift || bypass_ends);
                        if let (Some(button), Some(state)) = (button, state) {
                            let (cc, cr) = self.pointer_cell();
                            let mb = match button {
                                PointerButton::Left => MouseButton::Left,
                                PointerButton::Right => MouseButton::Right,
                                PointerButton::Middle => MouseButton::Middle,
                            };
                            self.mouse_pressed =
                                matches!(state, ButtonState::Pressed).then_some(mb);
                            if !suppress_report {
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
                        }
                        match (button, state) {
                            (Some(PointerButton::Left), Some(ButtonState::Pressed)) => {
                                if bypass_starts {
                                    self.selection_bypass = self.app_mouse;
                                    let (cc, cr) = self.pointer_cell();
                                    // Press semantics (shift-extend / dismiss-click /
                                    // begin-potential-selection) live in the model.
                                    // Under app-mouse bypass shift is the *trigger*,
                                    // not an extend request, so only pass it through
                                    // when the terminal owns the mouse anyway.
                                    self.selection
                                        .press_left((cc, i32::from(cr)), shift && !self.app_mouse);
                                    self.present.mark_dirty();
                                }
                            }
                            (Some(PointerButton::Left), Some(ButtonState::Released)) => {
                                // Release ends the drag. If the pointer never
                                // moved since press, this was a bare click on
                                // empty space — nothing to keep.
                                self.selection_bypass = false;
                                self.selection.release_left();
                                self.present.mark_dirty();
                            }
                            _ => {}
                        }
                    }
                    Some(PointerEvent::Axis) => {
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
                    _ => {}
                }
                return Ok(());
            }
        }

        // wl_keyboard.*
        if let Some(keyboard) = self.keyboard {
            if ev.is(keyboard) {
                match ev.event::<KeyboardEvent>() {
                    Some(KeyboardEvent::Keymap) => {
                        // arg(0) = format (always XKB_V1 in practice), the
                        // fd rides out-of-band as the very next `Fd` event,
                        // arg(1) = size (the mapping's byte length -- the
                        // fd carries no length of its own).
                        self.awaiting_keymap_fd = Some(ev.arg(1) as usize);
                    }
                    Some(KeyboardEvent::Enter) => {
                        // Keyboard focus gained: arg(0) is the serial we must
                        // present to `set_selection` to prove we own focus.
                        self.clipboard.set_focus_serial(ev.arg(0));
                        // Gaining focus makes the cursor solid (not
                        // mid-fade) immediately, then lets it resume
                        // blinking -- see `set_focused`.
                        self.cursor_anim.set_focused(true);
                        self.present.mark_dirty();
                        self.input_queue.push_back(InputEvent::Focus(true));
                    }
                    Some(KeyboardEvent::Key) => {
                        let physical = Key::from_code(ev.arg(2));
                        let logical = self.resolve_logical_key(physical, ev.arg(2));
                        let state = KeyState::from_raw(ev.arg(3));
                        // Start/stop repeat timer. Modifier keys never repeat.
                        match (logical, state) {
                            (Some(k), Some(KeyState::Pressed))
                                if !physical.is_some_and(Key::is_modifier) =>
                            {
                                self.key_repeat.on_key_press(k);
                            }
                            (Some(k), Some(KeyState::Released)) => {
                                self.key_repeat.on_key_release(k);
                            }
                            _ => {}
                        }
                        // Esc clears any active selection (and still passes
                        // through to the application as a normal Esc).
                        if logical == Some(Key::Esc) && state == Some(KeyState::Pressed) {
                            if self.selection.clear() {
                                self.present.mark_dirty();
                            }
                        }
                        self.input_queue.push_back(InputEvent::Key {
                            key: logical,
                            state,
                            modifiers: self.modifiers,
                        });
                    }
                    Some(KeyboardEvent::RepeatInfo) => {
                        self.key_repeat.set_repeat_info(ev.arg(0), ev.arg(1));
                    }
                    Some(KeyboardEvent::Leave) => {
                        self.key_repeat.on_leave();
                        // Losing focus freezes the cursor fully hidden so it
                        // stops blinking -- the classic "which window is
                        // focused" cue: only the focused window's cursor is
                        // ever visible.
                        self.cursor_anim.set_focused(false);
                        self.present.mark_dirty();
                        self.input_queue.push_back(InputEvent::Focus(false));
                    }
                    Some(KeyboardEvent::Modifiers) => {
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
                    _ => {}
                }
                return Ok(());
            }
        }

        // wl_data_device.* — clipboard selection offers from other clients.
        if let Some(dev) = self.clipboard.device_id() {
            if ev.is(dev) && ev.event::<DataDeviceEvent>() == Some(DataDeviceEvent::Selection) {
                // arg(0) = new wl_data_offer id (0 = selection cleared).
                self.clipboard.on_selection_event(ev.arg(0));
                return Ok(());
            }
        }

        // wl_data_offer.* — MIME types a peer advertises (we only need to know
        // the offer exists; we always request text/plain on paste).
        if let Some(offer) = self.clipboard.offer_id() {
            if ev.is(offer) {
                if let Some(offer_ev) = ev.event::<DataOfferEvent>() {
                    self.clipboard.on_offer_event(offer_ev);
                }
                return Ok(());
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
                return Ok(());
            }
        }

        Ok(())
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
        let cols = (w / self.cell_w) as u16;
        let rows = (h / self.cell_h) as u16;
        self.input_queue
            .push_back(InputEvent::Resize { cols, rows });
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

    fn repaint_rows(&mut self, rows: &[u16]) {
        let cols = (self.width / self.cell_w).max(1) as u16;
        let mut canvas = crate::draw::Canvas::new(&mut self.backbuffer, self.width, self.height);
        for &row in rows {
            let Some(line) = self.selection.mirror().get(usize::from(row)) else {
                continue;
            };
            // Clone the row's cells so we don't hold a borrow on `self` while
            // the renderer needs `&mut self`.
            let cells = line.clone();
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
        // Highlight tint + strength (0..255). ~35% steel-blue.
        const HR: u32 = 0x40;
        const HG: u32 = 0x60;
        const HB: u32 = 0xa0;
        const ALPHA: u32 = 90;
        const INV: u32 = 255 - ALPHA;

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
            for py in py0..py0 + self.cell_h as usize {
                let base = py * buf_w;
                if base >= self.backbuffer.len() {
                    break;
                }
                for px in px0..px1.min(buf_w) {
                    let idx = base + px;
                    if idx >= self.backbuffer.len() {
                        break;
                    }
                    let p = self.backbuffer[idx];
                    let r = (p >> 16) & 0xff;
                    let g = (p >> 8) & 0xff;
                    let b = p & 0xff;
                    let nr = (r * INV + HR * ALPHA) / 255;
                    let ng = (g * INV + HG * ALPHA) / 255;
                    let nb = (b * INV + HB * ALPHA) / 255;
                    // Preserve the top byte (alpha for translucent windows).
                    self.backbuffer[idx] = (p & 0xff00_0000) | (nr << 16) | (ng << 8) | nb;
                }
            }
        }
    }

    /// Draw a one-pixel underline across the hovered link's row-span (the
    /// contiguous run of cells sharing `hover_link_id` that includes the
    /// pointer's column), so the user gets the familiar "this is clickable"
    /// feedback before committing to the Ctrl+click. `None` when nothing is
    /// hovered -- the caller is responsible for repainting `prev_hover_row`
    /// clean first (same pattern as the selection highlight).
    fn apply_link_hover_underline(&mut self) {
        if self.hover_link_id == 0 {
            return;
        }
        let (_, row) = self.pointer_cell();
        let Some(line) = self.selection.mirror().get(usize::from(row)) else {
            return;
        };
        // Find the contiguous run of cells sharing the hovered link id.
        // (`line` came straight from `row` above, so no need to re-check it.)
        let id = self.hover_link_id;
        let mut lo = None;
        let mut hi = None;
        for (col, cell) in line.iter().enumerate() {
            if cell.link_id == id {
                lo.get_or_insert(col);
                hi = Some(col);
            }
        }
        let (Some(lo), Some(hi)) = (lo, hi) else {
            return;
        };
        let y = usize::from(row) * self.cell_h as usize + self.cell_h as usize - 1;
        let buf_w = self.width as usize;
        if y >= self.height as usize {
            return;
        }
        let x0 = lo * self.cell_w as usize;
        let x1 = ((hi + 1) * self.cell_w as usize).min(buf_w);
        // Light blue underline, preserving whatever alpha byte the pixel
        // already carried (top byte, only meaningful on a translucent
        // window -- see `Renderer::pack_bg`).
        const UNDERLINE_RGB: u32 = 0x00A0_C0FF;
        let base = y * buf_w;
        for x in x0..x1 {
            let idx = base + x;
            if idx < self.backbuffer.len() {
                self.backbuffer[idx] = (self.backbuffer[idx] & 0xFF00_0000) | UNDERLINE_RGB;
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
            let mut canvas =
                crate::draw::Canvas::new(&mut self.backbuffer, self.width, self.height);
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

    /// Debug-instrumentation hook: note the current `pending_dirty` backlog
    /// depth and emit the once-a-second frame-pacing log line. No-op without
    /// `trace` (see [`crate::debug_overlay`]).
    #[cfg(feature = "trace")]
    fn debug_pre_present(&mut self) {
        self.debug.note_backlog(self.pending_dirty.len());
        self.debug.log_frame_pacing();
    }

    #[cfg(not(feature = "trace"))]
    fn debug_pre_present(&mut self) {}

    /// Debug-instrumentation hook: record that a present was skipped because
    /// the frame callback hadn't arrived yet (vsync pacing). No-op without
    /// `trace`.
    #[cfg(feature = "trace")]
    fn debug_note_skipped_not_ready(&mut self) {
        self.debug.note_skipped_not_ready();
    }

    #[cfg(not(feature = "trace"))]
    fn debug_note_skipped_not_ready(&mut self) {}

    /// Debug-instrumentation hook: record that a present was skipped because
    /// no shm buffer slot was free (buffer starvation). No-op without `trace`.
    #[cfg(feature = "trace")]
    fn debug_note_skipped_no_slot(&mut self) {
        self.debug.note_skipped_no_slot();
    }

    #[cfg(not(feature = "trace"))]
    fn debug_note_skipped_no_slot(&mut self) {}

    /// Debug-instrumentation hook: remap the fps overlay's dirty-row tracker
    /// by a signed whole-screen shift (a viewport pixel-scroll). No-op
    /// without `trace`.
    #[cfg(feature = "trace")]
    fn debug_shift_prev_rows_signed(&mut self, shifts: &[i32], row_count: u16) {
        Self::shift_overlay_rows_signed(&mut self.debug.prev_rows, shifts, row_count);
    }

    #[cfg(not(feature = "trace"))]
    fn debug_shift_prev_rows_signed(&mut self, _shifts: &[i32], _row_count: u16) {}

    /// Debug-instrumentation hook: remap the fps overlay's dirty-row tracker
    /// by an upward scroll band. No-op without `trace`.
    #[cfg(feature = "trace")]
    fn debug_shift_prev_rows(&mut self, scrolls: &[grid::ScrollBand]) {
        Self::shift_overlay_rows(&mut self.debug.prev_rows, scrolls);
    }

    #[cfg(not(feature = "trace"))]
    fn debug_shift_prev_rows(&mut self, _scrolls: &[grid::ScrollBand]) {}

    /// Debug-instrumentation hook: take the fps overlay's previously-touched
    /// rows for this frame's repaint-clean pass. Empty without `trace`.
    #[cfg(feature = "trace")]
    fn debug_take_prev_rows(&mut self) -> Vec<u16> {
        std::mem::take(&mut self.debug.prev_rows)
    }

    #[cfg(not(feature = "trace"))]
    fn debug_take_prev_rows(&mut self) -> Vec<u16> {
        Vec::new()
    }

    /// Debug-instrumentation hook: record this present in the rolling fps
    /// average and draw the "NN.N fps" overlay text, returning the rows it
    /// touched. Always empty (and free) without `trace`.
    #[cfg(feature = "trace")]
    fn debug_draw_overlay(&mut self) -> Vec<u16> {
        self.debug.record_present();
        let rows = self.debug.draw(
            &mut self.backbuffer,
            self.width,
            self.height,
            self.cell_w,
            &mut self.renderer,
        );
        self.debug.prev_rows.clone_from(&rows);
        rows
    }

    #[cfg(not(feature = "trace"))]
    fn debug_draw_overlay(&mut self) -> Vec<u16> {
        Vec::new()
    }

    /// Debug-instrumentation hook: record this present's busy time and, on a
    /// slow present, log the phase breakdown. No-op without `trace`.
    #[cfg(feature = "trace")]
    fn debug_record_busy_and_log(
        &mut self,
        total: std::time::Duration,
        t_blit: std::time::Duration,
        t_overlay: std::time::Duration,
        folded_len: usize,
        painted_rows: usize,
        painted_cells: usize,
    ) {
        self.debug.record_busy(total);
        self.debug
            .log_slow_present(total, t_blit, t_overlay, folded_len, painted_rows, painted_cells);
    }

    #[cfg(not(feature = "trace"))]
    fn debug_record_busy_and_log(
        &mut self,
        _total: std::time::Duration,
        _t_blit: std::time::Duration,
        _t_overlay: std::time::Duration,
        _folded_len: usize,
        _painted_rows: usize,
        _painted_cells: usize,
    ) {
    }

    fn maybe_present(&mut self) -> Result<()> {
        if self.lifecycle != Lifecycle::Mapped {
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
        // Coalesce the whole backlog before blitting. Under a hard scroll flood
        // the parser publishes ~30 snapshots per vsync -- each a full-band
        // pixel-scroll (`scroll_span`) plus its freshly-revealed rows -- and
        // the old `for snap in &pending` loop blitted every one: ~30 full-
        // screen `copy_within` memmoves for a single presented frame (measured
        // on niri: a 29-47 deep backlog took 17-23 ms in one present, flipping
        // the loop CPU-bound and dropping 60 -> ~43 fps).
        //
        // `DirtySnapshot::compose` folds a run into one snapshot that, blitted
        // once, reproduces the ordered result: same-band scrolls sum their
        // deltas (N shifts of the band collapse to one shift of the summed
        // delta) and rows merge last-writer-wins, relabelled up by later
        // scrolls. A snapshot that can't compose (viewport repaint, geometry
        // change, or a different scroll band) closes the current accumulator
        // and starts a fresh one, so ordering across barriers is preserved.
        let mut folded: Vec<grid::DirtySnapshot> = Vec::new();
        for snap in pending {
            let composed = folded.last_mut().is_some_and(|acc| acc.compose(&snap));
            if !composed {
                folded.push(snap);
            }
        }
        let mut touched_rows: Vec<u16> = Vec::new();
        let mut scrolls: Vec<grid::ScrollBand> = Vec::new();
        // Signed whole-screen shifts from viewport (scrollback) pixel-scrolls
        // that `blit_snapshot` performed this frame. Unlike `scrolls` (an
        // upward-only band), these are signed and cover the full screen, so
        // baked overlays (fps readout, a resting cursor/selection) get dragged
        // by the memmove and their row trackers must be remapped to match.
        let mut viewport_shifts: Vec<i32> = Vec::new();
        let mut painted_rows: usize = 0;
        let mut painted_cells: usize = 0;
        let mut canvas = crate::draw::Canvas::new(&mut self.backbuffer, self.width, self.height);
        for snap in &folded {
            if let Some(scroll) = self.renderer.blit_snapshot(&mut canvas, snap) {
                scrolls.push(scroll);
            }
            // A viewport frame that actually pixel-shifted carries a signed
            // shift AND an offset (a full-frame repaint carries `shift: None`).
            if snap.viewport_offset().is_some() {
                if let Some(s) = snap.viewport_shift() {
                    if s != 0 {
                        viewport_shifts.push(s);
                    }
                }
            }
            painted_rows += snap.rows.len();
            painted_cells += snap.rows.iter().map(|r| r.cells.len()).sum::<usize>();
            touched_rows.extend(snap.rows.iter().map(|r| r.row));
        }
        // `Canvas` borrows `self.backbuffer` mutably and holds no resources of
        // its own (no `Drop` impl) -- this ends that borrow early so the code
        // below can use `self` again; `drop()` here is a borrow-checker nudge,
        // not resource cleanup.
        let _ = canvas;
        // Sub-phase timers (only read when DebugFps::Log fires below on a slow
        // present): `t_blit` = the coalesce+blit_snapshot loop, the only phase
        // the `blit` bench covers. The gap to `t_overlay` is overlay repaint;
        // the gap from there to the end is the stale-row shm memcpy + Wayland
        // Attach/Damage/Commit submission -- the phases the bench never sees,
        // and the prime suspects for the ~16ms `present-busy` spikes.
        let t_blit = present_start.elapsed();
        let folded_len = folded.len();
        // A pixel-scroll `copy_within` shifted the backbuffer up -- and with it
        // any overlay we baked last frame (cursor, selection, fps). The row
        // lists that tell us where to repaint those clean now point one screen-
        // position too low, so remap them by the same shift before use, or the
        // dragged-up overlay pixels get left behind as a static imprint.
        // A viewport pixel-scroll `copy_within`-shifted the whole backbuffer by
        // a signed amount, dragging any baked overlay with it. Remap the
        // overlay-row trackers the same way so last frame's imprint is cleaned
        // at its NEW position, not its old one. The full row set already made
        // `touched_rows` span the screen, so the shm re-copy needs nothing
        // extra here (unlike the incremental band, which must extend it).
        if !viewport_shifts.is_empty() {
            let rows = (self.height / self.cell_h).max(1) as u16;
            Self::shift_overlay_rows_signed(&mut self.prev_cursor_rows, &viewport_shifts, rows);
            Self::shift_overlay_rows_signed(&mut self.prev_selection_rows, &viewport_shifts, rows);
            self.debug_shift_prev_rows_signed(&viewport_shifts, rows);
        }
        if !scrolls.is_empty() {
            Self::shift_overlay_rows(&mut self.prev_cursor_rows, &scrolls);
            Self::shift_overlay_rows(&mut self.prev_selection_rows, &scrolls);
            self.debug_shift_prev_rows(&scrolls);
            // The pixel-scroll `copy_within` rewrote EVERY backbuffer row in
            // the band `[top, bot]` (their content shifted up), but the
            // snapshot's dirty rows -- what feeds `touched_rows` -- only cover
            // the genuinely-new bottom rows, since the grid shifts its dirty
            // marks up with the content. Without marking the whole band stale,
            // the shifted rows never get re-copied from the backbuffer into the
            // shm slots (which are the actual presented pixels), so the screen
            // freezes everything except the bottom few new rows -- the "only a
            // few lines scroll, the rest is stale" flood artifact. Mark the
            // full band so every moved row is re-copied + damaged.
            Self::extend_with_scrolled_bands(&mut touched_rows, &scrolls);
        }
        // Selection highlight. Repaint the rows that *were* highlighted last
        // frame (clean), plus the rows the selection touches now (so a moved
        // drag doesn't double-tint), then tint the current selection. This
        // keeps the effect idempotent and removable on deselect. The cursor
        // overlay's old rows are included too, since the cursor may be
        // sliding through (or resting on) a row the selection logic doesn't
        // know about, and it must be repainted clean before either the
        // selection tint or the cursor itself redraw on top.
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
        if self.hover_link_id != 0 {
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
        let t_overlay = present_start.elapsed();

        // Every row touched this frame is now stale relative to *both* shm
        // slots (whichever one isn't presented into this time still needs
        // catching up next time it's used).
        self.present.mark_rows_stale(touched_rows);

        // Copy + damage only the rows this slot is actually stale for --
        // typically just the handful that changed (e.g. one line of typing),
        // not the whole window. Contiguous stale rows are coalesced into one
        // Damage rect each, to keep the common single-row case at one
        // message instead of one per row.
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
            if let (Some(src_slice), Some(dst_slice)) = (
                self.backbuffer.get(src_off..src_off + len),
                dst.get_mut(src_off..src_off + len),
            ) {
                dst_slice.copy_from_slice(src_slice);
            }
            damage_rects.push((y0 as i32, (y1 - y0) as i32));
        }

        let buf = self.present.frame_buffer(slot);
        self.window_tx.request(
            MessageBuilder::new(self.surface, SurfaceRequest::Attach)
                .object(buf)
                .i32(0)
                .i32(0),
        );
        for (y_px, height_px) in damage_rects {
            self.window_tx.request(
                MessageBuilder::new(self.surface, SurfaceRequest::Damage)
                    .i32(0)
                    .i32(y_px)
                    .i32(buf_w)
                    .i32(height_px),
            );
        }
        let cb: CallbackId = self.window_tx.alloc_id();
        self.window_tx
            .request(MessageBuilder::new(self.surface, SurfaceRequest::Frame).new_id(cb));
        self.window_tx
            .request(MessageBuilder::new(self.surface, SurfaceRequest::Commit));

        self.present.finish_present(cb);
        let total = present_start.elapsed();
        // On a slow present (>5ms -- never the 60fps steady state, always the
        // spikes we're chasing), break the timed region into its three phases
        // so we can see WHICH one owns the ~16ms.
        self.debug_record_busy_and_log(total, t_blit, t_overlay, folded_len, painted_rows, painted_cells);
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
            .map_err(|e| Error::Conn(ConnError::Sys(e)))?;

        if let Some(new_cap) = grew {
            // Route the resize through the same `window_tx` MPSC ring as the
            // buffer creates below so it stays ordered ahead of them (the pool
            // must be big enough before the new buffers are carved out of it).
            self.present.grow_pool(&mut self.window_tx, new_cap);
        }

        self.present.recreate_buffers(&mut self.window_tx, w, h);
        // Carry the old image into the new backbuffer (top-left aligned) instead
        // of clearing to background. The grid reflow + full snapshot arrives a
        // round-trip later via the parser thread; until then we keep showing the
        // previous content so the window doesn't flash blank mid-drag. Use
        // `bg_packed()` (not `bg_xrgb()`) so newly-exposed area on a grow
        // carries the configured window alpha instead of flashing opaque.
        let new_w = w as usize;
        let new_h = h as usize;
        let mut new_back = vec![self.renderer.bg_packed(); new_w * new_h];
        let old_w = self.width as usize;
        let old_h = self.height as usize;
        let copy_w = old_w.min(new_w);
        let copy_h = old_h.min(new_h);
        for row in 0..copy_h {
            let src = &self.backbuffer[row * old_w..row * old_w + copy_w];
            new_back[row * new_w..row * new_w + copy_w].copy_from_slice(src);
        }
        self.backbuffer = new_back;

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
        let row_count = ((h / self.cell_h).max(1)) as usize;
        self.present.reset_after_resize(row_count);
        Ok(())
    }

    fn join_reader(&mut self) -> Result<()> {
        if let Some(handle) = self.reader.take() {
            match handle.join() {
                Ok(result) => result,
                Err(_) => Err(Error::ReaderPanicked),
            }
        } else {
            Ok(())
        }
    }

    /// Gracefully close the window and join the reader thread.
    pub fn close(mut self) -> Result<()> {
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
        let registry: RegistryId = sender.alloc_id();
        sender.request(
            MessageBuilder::new(DisplayId(1), DisplayRequest::GetRegistry).new_id(registry),
        )?;

        let mut compositor: Option<CompositorId> = None;
        let mut shm: Option<ShmId> = None;
        let mut wm_base: Option<WmBaseId> = None;
        let mut seat: Option<SeatId> = None;
        let mut output: Option<OutputId> = None;
        let mut data_device_manager: Option<DataDeviceManagerId> = None;

        // A registry burst is unbounded and unordered -- there is no event
        // that says "no more globals", so we can only wait for the globals we
        // actually require (`wl_output` isn't one; see `sync_roundtrip`
        // below for how we still give it a chance to appear).
        while compositor.is_none() || shm.is_none() || wm_base.is_none() || seat.is_none() {
            let event = events.pop_block().ok_or(Error::ReaderStopped)?;
            let RawEvent::Bytes(bytes) = event else {
                continue;
            };
            let Some((mut ev, _)) = WireEvent::parse(&bytes) else {
                continue;
            };

            if ev.object_id == DisplayId(1).raw()
                && DisplayEvent::from_opcode(ev.opcode) == Some(DisplayEvent::Error)
            {
                return Err(Error::Protocol {
                    object: ev.u32(),
                    code: ev.u32(),
                    message: ev.string(),
                });
            }

            if ev.object_id == registry.raw()
                && RegistryEvent::from_opcode(ev.opcode) == Some(RegistryEvent::Global)
            {
                let name = ev.u32();
                let interface = ev.string();
                let version = ev.u32();
                match interface.as_str() {
                    s if s == proto::WL_COMPOSITOR.name && compositor.is_none() => {
                        let v = proto::WL_COMPOSITOR.version.min(version);
                        compositor = Some(Self::bind(sender, registry, name, &interface, v)?);
                    }
                    s if s == proto::WL_SHM.name && shm.is_none() => {
                        let v = proto::WL_SHM.version.min(version);
                        shm = Some(Self::bind(sender, registry, name, &interface, v)?);
                    }
                    s if s == proto::XDG_WM_BASE.name && wm_base.is_none() => {
                        let v = proto::XDG_WM_BASE.version.min(version);
                        wm_base = Some(Self::bind(sender, registry, name, &interface, v)?);
                    }
                    s if s == proto::WL_SEAT.name && seat.is_none() => {
                        let v = proto::WL_SEAT.version.min(version);
                        seat = Some(Self::bind(sender, registry, name, &interface, v)?);
                    }
                    s if s == proto::WL_OUTPUT.name && output.is_none() => {
                        let v = proto::WL_OUTPUT.version.min(version);
                        output = Some(Self::bind(sender, registry, name, &interface, v)?);
                    }
                    s if s == proto::WL_DATA_DEVICE_MANAGER.name
                        && data_device_manager.is_none() =>
                    {
                        let v = proto::WL_DATA_DEVICE_MANAGER.version.min(version);
                        data_device_manager =
                            Some(Self::bind(sender, registry, name, &interface, v)?);
                    }
                    _ => {}
                }
            }
        }
        Ok(Globals {
            compositor: compositor.ok_or(Error::ReaderStopped)?,
            shm: shm.ok_or(Error::ReaderStopped)?,
            wm_base: wm_base.ok_or(Error::ReaderStopped)?,
            seat: seat.ok_or(Error::ReaderStopped)?,
            output,
            data_device_manager,
        })
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
    /// Delegates to the role-agnostic [`crate::surface::output_scale`].
    fn output_scale(sender: &mut Sender, events: &mut Consumer<RawEvent>, output: OutputId) -> i32 {
        crate::surface::output_scale(sender, events, output)
    }

    /// Bind one global by name/interface/version off the registry.
    /// Delegates to the role-agnostic [`crate::surface::bind`].
    fn bind<T: ObjectId>(
        sender: &mut Sender,
        registry: RegistryId,
        name: u32,
        interface: &str,
        version: u32,
    ) -> Result<T> {
        crate::surface::bind(sender, registry, name, interface, version)
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

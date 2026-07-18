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
use quetzalcoatl::spsc::{Consumer, RingBuffer};

use crate::conn::{Conn, ConnError, Sender, WindowSender};
use crate::cursor;
use crate::draw::Point;
use crate::error::{Error, Result};
use crate::event::RawEvent;
use crate::id::{
    BufferId, CallbackId, CompositorId, DataDeviceId, DataDeviceManagerId, DataOfferId,
    DataSourceId, DisplayId, KeyboardId, ObjectId, OutputId, PointerId, PoolId, RegistryId, SeatId,
    ShmId, SurfaceId, ToplevelId, WmBaseId, XdgSurfaceId,
};
use crate::keymap;
use crate::protocol::Modifiers;
use crate::protocol::{
    self as proto, BufferEvent, BufferRequest, ButtonState, CallbackEvent, CompositorRequest,
    DataDeviceEvent, DataDeviceManagerRequest, DataDeviceRequest, DataOfferEvent, DataOfferRequest,
    DataSourceEvent, DataSourceRequest, DisplayEvent, DisplayRequest, Format, Key, KeyState,
    KeyboardEvent, OutputEvent, PointerButton, PointerEvent, PointerRequest, RegistryEvent,
    RegistryRequest, SeatCapability, SeatEvent, SeatRequest, ShmPoolRequest, ShmRequest,
    SurfaceRequest, ToplevelEvent, ToplevelRequest, WmBaseEvent, WmBaseRequest, XdgSurfaceEvent,
    XdgSurfaceRequest,
};
use crate::render;
use crate::sys::{RepeatTimer, ShmBuffer};
use crate::wire::{MessageBuilder, WireEvent};

const EVENT_RING_CAP: usize = 1024;

/// Stable Wayland `app_id` for our toplevel. Fixed (not the caller's title) so
/// compositor window rules and tooling can match this window reliably even
/// after the shell rewrites the title at runtime.
const APP_ID: &str = "forfun.terminal";

const DEFAULT_REPEAT_RATE: u32 = 25;
const DEFAULT_REPEAT_DELAY: u32 = 600;

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
    /// negative = scroll down/toward, toward the prompt).
    Scroll {
        lines: i32,
    },
    /// Clipboard text read from another client, to be written to the PTY
    /// (a paste). Carries the raw clipboard bytes.
    Paste(Vec<u8>),
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Lifecycle {
    Unconfigured,
    Mapped,
    Closing,
}

/// Debug FPS instrumentation mode -- see the `debug_fps` field on
/// [`TermWindow`]. Set once at startup from the `TERMINAL_DEBUG_FPS`
/// environment variable and fixed for the window's lifetime.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum DebugFps {
    /// No instrumentation -- zero cost (one enum compare per frame).
    Off,
    /// Draw a live "NN.N fps" readout in the top-right corner every frame.
    Overlay,
    /// Print "terminal: NN.N fps" to stderr about once a second.
    Log,
}

impl DebugFps {
    /// `TERMINAL_DEBUG_FPS=1` / `=true` → [`Self::Overlay`];
    /// `TERMINAL_DEBUG_FPS=log` → [`Self::Log`]; unset or anything else →
    /// [`Self::Off`].
    fn from_env() -> Self {
        match std::env::var("TERMINAL_DEBUG_FPS") {
            Ok(v) if v == "log" => Self::Log,
            Ok(v) if v == "1" || v.eq_ignore_ascii_case("true") => Self::Overlay,
            _ => Self::Off,
        }
    }
}

#[derive(Copy, Clone, Debug)]
struct Frame {
    buffer: BufferId,
    free: bool,
}

/// Rolling window of present timestamps, used to compute a live "presented
/// FPS" figure -- how often [`TermWindow::maybe_present`] actually flips a
/// frame, which (since presents are gated on the compositor's
/// `wl_callback.done`) is the true on-screen refresh rate, not just how busy
/// the render loop is.
struct FrameStats {
    /// Timestamps of the last up-to-`CAP` presents, oldest first.
    times: std::collections::VecDeque<std::time::Instant>,
    /// Wall-clock instant `times` was last logged to stderr (only used by
    /// [`DebugFps::Log`]).
    last_log: std::time::Instant,
    /// Accumulated CPU time spent doing the actual work inside
    /// `maybe_present` (blit + shm copy + damage bookkeeping) since the last
    /// log, and how many presents that covers. Divided out at log time to
    /// answer the only question that matters under a flood: is a low fps
    /// CPU-bound (busy ~ frame interval) or compositor-bound (busy <<
    /// interval, i.e. we're blocked waiting on `wl_callback.done`)?
    busy_since_log: std::time::Duration,
    presents_since_log: u32,
    /// Largest `pending_dirty` backlog (snapshots awaiting one present) seen
    /// since the last log. A monotonic ramp here is the signature of the
    /// present being gated while the parser keeps queueing frames.
    max_backlog_since_log: usize,
    /// Times `maybe_present` bailed because the compositor still held both shm
    /// buffers (`wl_buffer.release` lagging), vs. because we hadn't yet gotten
    /// the frame callback. Distinguishes buffer-starvation from vsync pacing.
    skipped_no_slot: u32,
    skipped_not_ready: u32,
}

/// How many recent presents [`FrameStats`] keeps around to average over.
const FRAME_STATS_CAP: usize = 120;

/// Presents older than this (relative to the newest present) are dropped
/// from the stats window. Without an age bound the average spans idle gaps:
/// while scrolled into history the cursor is hidden, so the blink heartbeat
/// stops presenting, and every pause between wheel detents stays inside the
/// 120-present window — the readout sagged to ~40 and then froze there while
/// idling in scrollback, even though each presented frame was full-rate. An
/// age bound makes fps() mean "presents per second over the last second of
/// activity", which is what the overlay claims to show.
const FRAME_STATS_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(1);

impl FrameStats {
    fn new() -> Self {
        Self {
            times: std::collections::VecDeque::with_capacity(FRAME_STATS_CAP),
            last_log: std::time::Instant::now(),
            busy_since_log: std::time::Duration::ZERO,
            presents_since_log: 0,
            max_backlog_since_log: 0,
            skipped_no_slot: 0,
            skipped_not_ready: 0,
        }
    }

    /// Drop presents older than [`FRAME_STATS_MAX_AGE`] relative to `now`.
    /// Called both when a present is recorded AND before every read of
    /// [`Self::fps`] (overlay/log), so an idle stretch (no presents while
    /// parked in scrollback) decays the reading to 0 instead of freezing the
    /// last sagged average on screen forever.
    fn prune(&mut self, now: std::time::Instant) {
        while self
            .times
            .front()
            .is_some_and(|&t| now.duration_since(t) > FRAME_STATS_MAX_AGE)
        {
            self.times.pop_front();
        }
    }

    fn note_backlog(&mut self, depth: usize) {
        self.max_backlog_since_log = self.max_backlog_since_log.max(depth);
    }

    /// Add the CPU time one `maybe_present` spent on real work (not blocked on
    /// the compositor).
    fn record_busy(&mut self, dur: std::time::Duration) {
        self.busy_since_log += dur;
        self.presents_since_log += 1;
    }

    /// Mean busy microseconds per present since the last log, then reset the
    /// accumulator. `0.0` if no presents happened.
    ///
    /// This is a diagnostics-only average (logged frame-pacing stats); a
    /// `u128` microsecond count losing precision in the f64 conversion at
    /// any value this process could plausibly accumulate is not observable
    /// in the logged output.
    #[allow(clippy::cast_precision_loss)]
    fn drain_busy_us(&mut self) -> f64 {
        let n = self.presents_since_log;
        let total = std::mem::take(&mut self.busy_since_log);
        self.presents_since_log = 0;
        if n == 0 {
            0.0
        } else {
            total.as_micros() as f64 / f64::from(n)
        }
    }

    /// Record a present that just happened.
    fn record(&mut self) {
        let now = std::time::Instant::now();
        self.times.push_back(now);
        while self.times.len() > FRAME_STATS_CAP {
            self.times.pop_front();
        }
        // Age-bound the window so idle gaps (no presents while parked in
        // scrollback — the hidden cursor stops the blink heartbeat) don't
        // get averaged into the next reading as phantom slowness.
        self.prune(now);
    }

    /// Presented frames per second, averaged over the retained window.
    /// `0.0` until at least two presents have been recorded.
    fn fps(&self) -> f32 {
        let Some(&first) = self.times.front() else {
            return 0.0;
        };
        let Some(&last) = self.times.back() else {
            return 0.0;
        };
        let span = last.duration_since(first).as_secs_f32();
        if span <= 0.0 || self.times.len() < 2 {
            return 0.0;
        }
        #[allow(clippy::cast_precision_loss)] // FRAME_STATS_CAP is tiny (120)
        let intervals = (self.times.len() - 1) as f32;
        intervals / span
    }
}

impl Frame {
    /// Allocate a double-buffered shm pool and the two [`Frame`] buffers
    /// backed by it.
    ///
    /// Was a free `create_frames(sender, shm, width, height)` function --
    /// moved onto `Frame` since it constructs the array of them.
    fn create_pair(
        sender: &mut Sender,
        shm: ShmId,
        width: i32,
        height: i32,
        format: Format,
    ) -> Result<([Self; 2], ShmBuffer, PoolId)> {
        let shm_mem = ShmBuffer::new(width as usize, height as usize, 2)
            .map_err(|e| Error::Conn(ConnError::Sys(e)))?;
        let stride = width * 4;
        let pool: PoolId = sender.alloc_id();
        let msg = MessageBuilder::new(shm, ShmRequest::CreatePool)
            .new_id(pool)
            .i32(shm_mem.size_bytes() as i32)
            .finish();
        sender.send_with_fds(msg, &[shm_mem.fd()])?;
        let mut frames = [Self {
            buffer: BufferId(0),
            free: true,
        }; 2];
        for (i, slot) in frames.iter_mut().enumerate() {
            let buffer: BufferId = sender.alloc_id();
            sender.request(
                MessageBuilder::new(pool, ShmPoolRequest::CreateBuffer)
                    .new_id(buffer)
                    .i32(shm_mem.frame_offset(i) as i32)
                    .i32(width)
                    .i32(height)
                    .i32(stride)
                    .u32(format.code()),
            )?;
            slot.buffer = buffer;
        }
        Ok((frames, shm_mem, pool))
    }
}

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

    repeat_timer: RepeatTimer,
    repeat_rate: u32,
    repeat_delay: u32,
    held_key: Option<Key>,
    modifiers: Modifiers,

    cursor: Option<cursor::Cursor>,

    /// Smooth cursor-slide + blink animation state. Advanced by wall-clock
    /// time in [`Self::tick`]; see [`crate::cursor_anim`].
    cursor_anim: crate::cursor_anim::CursorAnimator,
    /// Wall-clock instant of the last [`Self::tick`] call, for computing the
    /// animator's `dt_ms`.
    last_tick: std::time::Instant,

    frames: [Frame; 2],
    shm_mem: ShmBuffer,
    /// The shm pool currently backing `frames`. Created once and reused across
    /// resizes: it grows in place (`ftruncate` + `mremap` + `wl_shm_pool.resize`)
    /// and shrinks lazily (the pool stays oversized). Only the per-resize
    /// buffers are torn down and recreated; the pool id itself lives for the
    /// window's lifetime.
    pool: PoolId,
    /// `wl_shm` pixel format backing `frames` -- `Argb8888` when the
    /// renderer has a window opacity < 1.0 configured, else the cheaper
    /// `Xrgb8888`. Fixed for the window's lifetime (set once from
    /// `renderer.needs_alpha()` in [`Self::new`]); `resize` reuses it so a
    /// live-resized translucent window doesn't silently go opaque.
    buf_format: Format,
    /// Persistent full-window backbuffer. Dirty rows are blitted here (it keeps
    /// the complete last-known image), then copied into whichever shm frame is
    /// free on present. Without this, incremental dirty-row blits would only
    /// ever touch one of the two shm frames, leaving the rest black.
    backbuffer: Vec<u32>,
    /// Per-frame-slot "needs re-copying from the backbuffer" bitmap, indexed
    /// by cell row, index-parallel with `frames`. A row is set whenever the
    /// backbuffer changes there; presenting into a slot copies + damages only
    /// that slot's set rows (which may be a superset of this frame's changes,
    /// if the slot sat out a present or two) then clears them. Without this,
    /// every present -- even a single keystroke moving the cursor one row --
    /// would memcpy the whole backbuffer and damage the entire surface,
    /// forcing the compositor to recomposite the full window every frame.
    stale_rows: [Vec<bool>; 2],
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
    dirty: bool,
    frame_ready: bool,
    frame_cb: Option<CallbackId>,
    lifecycle: Lifecycle,

    /// Latest window size proposed by `xdg_toplevel.configure`, applied once per
    /// frame so a drag's flood of configures collapses into one resize.
    pending_resize: Option<(i32, i32)>,
    /// Pending dirty snapshots to blit on the next frame.
    pending_dirty: Vec<grid::DirtySnapshot>,
    /// Pending input events for the caller.
    input_queue: std::collections::VecDeque<InputEvent>,

    // ── Clipboard & selection ────────────────────────────────────────────
    /// `(manager, per-seat device)` if the compositor supports `wl_data_device`.
    data_device: Option<(DataDeviceManagerId, DataDeviceId)>,
    /// The live data source we publish while our selection is on the clipboard.
    clipboard_source: Option<DataSourceId>,
    /// Text we last copied (served to other clients on `data_source.send`).
    clipboard_text: String,
    /// The clipboard offer from another client (for paste), with its MIME types.
    clipboard_offer: Option<DataOfferId>,
    /// The text-selection model: what's selected, the on-screen cell mirror it
    /// selects from, and the drag/scroll-follow state. Extracted from this
    /// struct so the selection logic lives (and is tested) in one place; see
    /// [`crate::selection::SelectionTracker`].
    selection: crate::selection::SelectionTracker,
    /// Set when a `data_source.send` just arrived; the next `RawEvent::Fd` is the
    /// pipe to write our clipboard text into.
    awaiting_send_fd: bool,
    /// Paste channel: helper threads that read a paste pipe send the bytes
    /// here; the UI loop drains it in `tick`. (std mpsc — multiple paste
    /// helper threads may be in flight, so a multi-producer channel.)
    paste_tx: std::sync::mpsc::Sender<Vec<u8>>,
    paste_rx: std::sync::mpsc::Receiver<Vec<u8>>,
    /// The serial from the most recent `wl_keyboard.enter` (keyboard-focus)
    /// event. This is the serial the compositor expects in
    /// `wl_data_device.set_selection`: it proves we hold keyboard focus at the
    /// moment of the copy, and — unlike a stale `wl_pointer.button` /
    /// `wl_keyboard.key` serial from an arbitrary earlier press — it stays
    /// valid for the whole focused period. Using a stale serial is exactly
    /// what triggers the compositor's "invalid arguments" protocol error.
    focus_serial: u32,
    /// Viewport rows that carried a highlight on the previous frame. Repainted
    /// clean on the next frame before the (possibly moved/cleared) selection
    /// is re-tinted, so highlight never lingers after deselect or drag.
    prev_selection_rows: Vec<u16>,
    /// Viewport rows the cursor overlay touched on the previous frame.
    /// Repainted clean before drawing the cursor at its new (possibly
    /// mid-slide) position, the same idempotent scheme as
    /// `prev_selection_rows` -- without this a sliding cursor would leave a
    /// trail of tinted cells behind it.
    prev_cursor_rows: Vec<u16>,

    /// Debug FPS instrumentation mode, fixed at startup from
    /// `TERMINAL_DEBUG_FPS`. `Off` costs one enum compare per frame.
    debug_fps: DebugFps,
    /// Rolling present-time window backing the FPS readout/log; only
    /// populated when `debug_fps != DebugFps::Off`.
    frame_stats: FrameStats,
    /// Viewport rows the FPS overlay text touched last frame -- repainted
    /// clean before redrawing it, the same idempotent scheme as
    /// `prev_cursor_rows`/`prev_selection_rows`.
    prev_fps_rows: Vec<u16>,
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
        let (mut sender, reader, repeat_timer) = Conn::connect()?;
        let (producer, mut events) =
            RingBuffer::<RawEvent>::new(Capacity::exact(EVENT_RING_CAP)).split();
        // The reader signals this after pushing events so the UI thread, which
        // polls instead of blocking on a single ring, wakes.
        let reader_wake = wake.clone();
        let reader_handle = std::thread::Builder::new()
            .name("terminal-wayland-reader".into())
            .spawn(move || {
                let r = reader.run(producer, Some(reader_wake)).map_err(Error::Conn);
                if let Err(e) = &r {
                    eprintln!("platform: reader error: {e}");
                }
                r
            })
            .map_err(Error::ReaderSpawn)?;

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

        let cursor = cursor::Cursor::create(&mut sender, shm, compositor, scale).ok();

        let (req_tx, req_rx) =
            mpsc::RingBuffer::<crate::conn::WindowRequest>::new(Capacity::exact(64)).split();
        let (id_start, id_end) = sender.alloc_id_block(256);
        let mut window_tx = WindowSender {
            tx: req_tx,
            next_id: id_start,
            id_end,
            free_ids: Vec::new(),
        };

        // Clipboard: create the per-seat data device, if the compositor
        // advertised a `wl_data_device_manager`. Without one (rare), copy/paste
        // is simply unavailable and the fields stay `None`.
        let data_device = data_device_manager.map(|mgr| {
            let dev: DataDeviceId = window_tx.alloc_id();
            window_tx.request(
                MessageBuilder::new(mgr, DataDeviceManagerRequest::GetDataDevice)
                    .new_id(dev)
                    .object(seat),
            );
            (mgr, dev)
        });

        // Paste channel: helper threads that read a paste pipe send the bytes
        // here; the UI loop drains it each `tick`.
        let (paste_tx, paste_rx) = std::sync::mpsc::channel::<Vec<u8>>();

        // Use `bg_packed()`, not `bg_xrgb()`: a translucent window's initial
        // backbuffer must carry the configured alpha from frame one, not
        // flash fully opaque until the first real paint.
        let backbuffer = vec![renderer.bg_packed(); (width * height) as usize];
        // Both shm frames start as blank memory (freshly mmap'd), so every
        // row is "stale" relative to the backbuffer until the first present
        // copies it in.
        let row_count = (rows as usize).max(1);
        let stale_rows = [vec![true; row_count], vec![true; row_count]];

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
            repeat_timer,
            repeat_rate: DEFAULT_REPEAT_RATE,
            repeat_delay: DEFAULT_REPEAT_DELAY,
            held_key: None,
            modifiers: Modifiers::default(),
            cursor,
            frames,
            shm_mem,
            pool,
            buf_format,
            backbuffer,
            stale_rows,
            renderer,
            width,
            height,
            scale,
            cell_w,
            cell_h,
            pointer_pos: Point::new(0, 0),
            dirty: true,
            frame_ready: true,
            frame_cb: None,
            lifecycle: Lifecycle::Unconfigured,
            pending_resize: None,
            pending_dirty: Vec::new(),
            input_queue: std::collections::VecDeque::new(),
            data_device,
            clipboard_source: None,
            clipboard_text: String::new(),
            clipboard_offer: None,
            selection: crate::selection::SelectionTracker::new(),
            awaiting_send_fd: false,
            paste_tx,
            paste_rx,
            focus_serial: 0,
            prev_selection_rows: Vec::new(),
            prev_cursor_rows: Vec::new(),
            cursor_anim: crate::cursor_anim::CursorAnimator::new(cursor_cfg),
            last_tick: std::time::Instant::now(),
            debug_fps: DebugFps::from_env(),
            frame_stats: FrameStats::new(),
            prev_fps_rows: Vec::new(),
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
            self.dirty = true;
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
        self.clipboard_text = text;
        let Some((mgr, dev)) = self.data_device else {
            return;
        };
        // `set_selection` requires the serial proving we hold keyboard focus
        // (the `wl_keyboard.enter` serial). Without focus (shouldn't happen —
        // copy only makes sense on a focused window) we keep the text locally
        // but skip the Wayland publish rather than risk an invalid serial.
        let serial = self.focus_serial;
        if serial == 0 {
            eprintln!("platform: copy skipped — no keyboard focus serial yet");
            return;
        }
        let src: DataSourceId = self.window_tx.alloc_id();
        self.window_tx.request(
            MessageBuilder::new(mgr, DataDeviceManagerRequest::CreateDataSource).new_id(src),
        );
        self.window_tx
            .request(MessageBuilder::new(src, DataSourceRequest::Offer).string("text/plain"));
        // Wire arg order is (source, serial): libwayland marshals args in XML
        // declaration order, and wl_data_device.set_selection declares
        // `source` before `serial`. Sending (serial, source) makes the
        // compositor read the source id as the serial and vice versa ->
        // "invalid arguments" protocol error.
        self.window_tx.request(
            MessageBuilder::new(dev, DataDeviceRequest::SetSelection)
                .object(src)
                .u32(serial),
        );
        self.clipboard_source = Some(src);
    }

    /// Request a paste from the system clipboard (Ctrl+Shift+V): ask the
    /// current offer's source to transfer `text/plain` over a pipe. We hand
    /// the pipe's write end to the peer (out-of-band fd); it writes the
    /// clipboard contents and closes. A helper thread blocks reading our end
    /// and pushes the bytes back as an [`InputEvent::Paste`].
    pub fn request_paste(&mut self) {
        let Some(offer) = self.clipboard_offer else {
            return;
        };
        let Ok((mut reader, writer)) = std::os::unix::net::UnixStream::pair() else {
            return;
        };
        // `receive(mime_type, fd)`: the fd travels as ancillary data, not a
        // wire word, so the message body is just the mime string.
        self.window_tx.send_with_fds(
            MessageBuilder::new(offer, DataOfferRequest::Receive)
                .string("text/plain")
                .finish(),
            vec![writer.into()],
        );
        // Read on a helper thread (blocks until the peer writes + closes),
        // then hand the bytes to the UI loop via the paste channel.
        let tx = self.paste_tx.clone();
        let wake = self.wake.clone();
        std::thread::spawn(move || {
            use std::io::Read as _;
            let mut buf = Vec::new();
            if reader.read_to_end(&mut buf).is_ok() && !buf.is_empty() {
                let _ = tx.send(buf);
                wake.signal();
            }
        });
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
        while let Ok(bytes) = self.paste_rx.try_recv() {
            self.input_queue.push_back(InputEvent::Paste(bytes));
        }

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
            self.dirty = true;
        }
    }

    /// Whether the caller should keep polling at animation-frame-rate
    /// (~60fps) instead of blocking on [`Self::wait`]'s full timeout -- true
    /// while the cursor is mid-slide, or blinking and visible.
    #[must_use]
    pub fn wants_animation_frame(&self) -> bool {
        self.cursor_anim.is_animating()
    }

    /// Notify the cursor animator of user activity (a keystroke): resets the
    /// blink phase to full opacity so the cursor is always solid the instant
    /// you type, never mid-fade. Call this whenever the caller sends bytes to
    /// the PTY in response to a key press.
    pub fn notify_activity(&mut self) {
        self.cursor_anim.on_activity();
        self.dirty = true;
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

    /// Consume the fd batch that followed a clipboard `send`/`receive`.
    /// `send` → we write our `clipboard_text` to the peer's pipe; `receive`
    /// (our paste request) → we read the peer's clipboard from the pipe.
    fn handle_clipboard_fds(&mut self, fds: Vec<std::os::fd::OwnedFd>) {
        use std::io::Write as _;
        use std::os::fd::FromRawFd as _;
        let Some(fd) = fds.into_iter().next() else {
            return;
        };
        let raw = std::os::fd::IntoRawFd::into_raw_fd(fd);
        if self.awaiting_send_fd {
            self.awaiting_send_fd = false;
            // Write our selection, then close (dropping the File on scope
            // exit signals EOF to the pasting client). Best-effort.
            let mut f = unsafe { std::fs::File::from_raw_fd(raw) };
            let _ = f.write_all(self.clipboard_text.as_bytes());
        } else {
            // Unsolicited fd (e.g. the XKB keymap fd, which we don't parse).
            // Re-wrap so it closes on drop instead of leaking.
            let _f = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
        }
    }

    fn handle_event(&mut self, ev: RawEvent) -> Result<()> {
        // Clipboard fds arrive as a standalone `Fd` event immediately after the
        // message that triggered them (see conn.rs). Route by what we asked for.
        if let RawEvent::Fd(fds) = ev {
            self.handle_clipboard_fds(fds);
            return Ok(());
        }

        // wl_display events arrive as raw bytes (the reader routes object 1 to
        // `Bytes`). Handle error (fatal) and delete_id (id recycling) here.
        if let RawEvent::Bytes(ref bytes) = ev {
            let (mut wire, _) = WireEvent::parse(bytes).ok_or(Error::ReaderStopped)?;
            if wire.object_id == DisplayId(1).raw() {
                match DisplayEvent::from_opcode(wire.opcode) {
                    Some(DisplayEvent::Error) => {
                        return Err(Error::Protocol {
                            object: wire.u32(),
                            code: wire.u32(),
                            message: wire.string(),
                        });
                    }
                    Some(DisplayEvent::DeleteId) => {
                        self.window_tx.recycle_id(wire.u32());
                        return Ok(());
                    }
                    None => {}
                }
            }
            return Ok(());
        }

        // Key repeat timer.
        if matches!(ev, RawEvent::Repeat) {
            if let Some(key) = self.held_key {
                self.input_queue.push_back(InputEvent::Key {
                    key: Some(key),
                    state: Some(KeyState::Pressed),
                    modifiers: self.modifiers,
                });
            }
            return Ok(());
        }

        // wl_buffer.release
        if let Some(frame) = self.frames.iter_mut().find(|f| ev.is(f.buffer)) {
            if ev.event::<BufferEvent>() == Some(BufferEvent::Release) {
                frame.free = true;
            }
            return Ok(());
        }

        // wl_callback.done (frame callback)
        if let Some(cb) = self.frame_cb {
            if ev.is(cb) && ev.event::<CallbackEvent>() == Some(CallbackEvent::Done) {
                self.frame_ready = true;
                self.frame_cb = None;
                return Ok(());
            }
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
                            self.dirty = true;
                        }
                    }
                    Some(PointerEvent::Button) => {
                        let button = PointerButton::from_code(ev.arg(2));
                        let state = ButtonState::from_raw(ev.arg(3));
                        match (button, state) {
                            (Some(PointerButton::Left), Some(ButtonState::Pressed)) => {
                                let shift = (self.modifiers.depressed
                                    | self.modifiers.latched
                                    | self.modifiers.locked)
                                    & 0x01
                                    != 0;
                                let (cc, cr) = self.pointer_cell();
                                // Press semantics (shift-extend / dismiss-click /
                                // begin-potential-selection) live in the model.
                                self.selection.press_left((cc, i32::from(cr)), shift);
                                self.dirty = true;
                            }
                            (Some(PointerButton::Left), Some(ButtonState::Released)) => {
                                // Release ends the drag. If the pointer never
                                // moved since press, this was a bare click on
                                // empty space — nothing to keep.
                                self.selection.release_left();
                                self.dirty = true;
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
                            self.input_queue
                                .push_back(InputEvent::Scroll { lines: signed });
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
                    Some(KeyboardEvent::Enter) => {
                        // Keyboard focus gained: arg(0) is the serial we must
                        // present to `set_selection` to prove we own focus.
                        self.focus_serial = ev.arg(0);
                    }
                    Some(KeyboardEvent::Key) => {
                        let physical = Key::from_code(ev.arg(2));
                        let logical = physical.map(keymap::PhysicalKey::to_logical);
                        let state = KeyState::from_raw(ev.arg(3));
                        // Start/stop repeat timer. Modifier keys never repeat.
                        match (logical, state) {
                            (Some(k), Some(KeyState::Pressed))
                                if !physical.is_some_and(Key::is_modifier) =>
                            {
                                self.held_key = Some(k);
                                // A failed arm means this key just won't
                                // auto-repeat -- annoying but not fatal, so
                                // we don't abort the event loop over it. But
                                // silence here previously meant "held_key is
                                // set but the timer never fires", which is a
                                // real (if rare) bug worth seeing in logs.
                                if let Err(e) = self.repeat_timer.arm(
                                    u64::from(self.repeat_delay),
                                    u64::from(1000 / self.repeat_rate.max(1)),
                                ) {
                                    eprintln!("platform: failed to arm key-repeat timer: {e}");
                                }
                            }
                            (Some(k), Some(KeyState::Released)) if self.held_key == Some(k) => {
                                self.held_key = None;
                                if let Err(e) = self.repeat_timer.disarm() {
                                    eprintln!("platform: failed to disarm key-repeat timer: {e}");
                                }
                            }
                            _ => {}
                        }
                        // Esc clears any active selection (and still passes
                        // through to the application as a normal Esc).
                        if logical == Some(Key::Esc) && state == Some(KeyState::Pressed) {
                            if self.selection.clear() {
                                self.dirty = true;
                            }
                        }
                        self.input_queue.push_back(InputEvent::Key {
                            key: logical,
                            state,
                            modifiers: self.modifiers,
                        });
                    }
                    Some(KeyboardEvent::RepeatInfo) => {
                        self.repeat_rate = ev.arg(0);
                        self.repeat_delay = ev.arg(1);
                    }
                    Some(KeyboardEvent::Leave) => {
                        self.held_key = None;
                        if let Err(e) = self.repeat_timer.disarm() {
                            eprintln!("platform: failed to disarm key-repeat timer: {e}");
                        }
                    }
                    Some(KeyboardEvent::Modifiers) => {
                        self.modifiers = Modifiers {
                            depressed: ev.arg(1),
                            latched: ev.arg(2),
                            locked: ev.arg(3),
                            group: ev.arg(4),
                        };
                    }
                    _ => {}
                }
                return Ok(());
            }
        }

        // wl_data_device.* — clipboard selection offers from other clients.
        if let Some((_, dev)) = self.data_device {
            if ev.is(dev) && ev.event::<DataDeviceEvent>() == Some(DataDeviceEvent::Selection) {
                // arg(0) = new wl_data_offer id (0 = selection cleared).
                let raw = ev.arg(0);
                self.clipboard_offer = if raw == 0 {
                    None
                } else {
                    Some(DataOfferId::from_raw(raw))
                };
                return Ok(());
            }
        }

        // wl_data_offer.* — MIME types a peer advertises (we only need to know
        // the offer exists; we always request text/plain on paste).
        if let Some(offer) = self.clipboard_offer {
            if ev.is(offer) {
                if ev.event::<DataOfferEvent>() == Some(DataOfferEvent::Offer) {
                    // Could track the mime list; we just accept text/plain.
                }
                return Ok(());
            }
        }

        // wl_data_source.* — a peer wants our clipboard (send) or replaced it
        // (cancelled). Both arrive as Wire events (the reader only routes
        // display/registry to Bytes); send's string arg is misparsed into the
        // inline u32 slots, but we don't need it — we always serve text/plain.
        if let Some(src) = self.clipboard_source {
            if ev.is(src) {
                match ev.event::<DataSourceEvent>() {
                    Some(DataSourceEvent::Send) => {
                        // The fd to write into arrives as the next RawEvent::Fd.
                        self.awaiting_send_fd = true;
                    }
                    Some(DataSourceEvent::Cancelled) => {
                        self.clipboard_source = None;
                    }
                    _ => {}
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
        self.dirty = true;
        Ok(())
    }

    /// Mark viewport rows as needing to be re-copied from the backbuffer into
    /// *both* shm frame slots next time each is presented into. Called
    /// whenever the backbuffer actually changes at those rows (content blit,
    /// selection tint, cursor overlay, FPS overlay), so [`Self::maybe_present`]
    /// only ever copies/damages what's actually different, instead of the
    /// whole window on every frame.
    fn mark_rows_stale(&mut self, rows: impl IntoIterator<Item = u16>) {
        for row in rows {
            let r = usize::from(row);
            for slot in &mut self.stale_rows {
                if let Some(flag) = slot.get_mut(r) {
                    *flag = true;
                }
            }
        }
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

    /// Debug FPS instrumentation: draw a "NN.N fps" readout into the top-right
    /// corner of the backbuffer (`DebugFps::Overlay`), or print the same
    /// figure to stderr about once a second (`DebugFps::Log`). No-op (and
    /// essentially free -- one enum compare) when `debug_fps` is `Off`.
    /// Returns the viewport rows the overlay text touched, for
    /// `prev_fps_rows` bookkeeping (same idempotent repaint scheme as the
    /// cursor/selection overlays).
    fn draw_fps_overlay(&mut self) -> Vec<u16> {
        match self.debug_fps {
            DebugFps::Off => Vec::new(),
            // The stderr readout is emitted from `log_frame_pacing`, called at
            // the top of every `maybe_present` (including its skip paths) so
            // the counter split keeps reporting even while presents are being
            // starved -- the exact regime we want to diagnose. Nothing to draw
            // into the backbuffer here.
            DebugFps::Log => Vec::new(),
            DebugFps::Overlay => {
                self.frame_stats.prune(std::time::Instant::now());
                let text = format!("{:.1} fps", self.frame_stats.fps());
                let cols = (self.width / self.cell_w).max(1) as u16;
                let text_cols = text.chars().count() as u16;
                if text_cols == 0 || text_cols > cols {
                    return Vec::new();
                }
                let start_col = cols - text_cols;
                // High-contrast fixed readout: white on black, independent of
                // the active theme, so it's always legible regardless of
                // what's underneath.
                let attrs = grid::Attrs::with_colors(
                    grid::Color::Rgb(0xff, 0xff, 0xff),
                    grid::Color::Rgb(0x00, 0x00, 0x00),
                );
                let mut canvas =
                    crate::draw::Canvas::new(&mut self.backbuffer, self.width, self.height);
                for (i, ch) in text.chars().enumerate() {
                    let cell = grid::Cell {
                        ch,
                        attrs,
                        wide: false,
                        spacer: false,
                    };
                    self.renderer
                        .blit_cell(&mut canvas, start_col + i as u16, 0, &cell);
                }
                vec![0u16]
            }
        }
    }

    /// Emit the frame-pacing diagnostic to stderr ~once a second when
    /// `TERMINAL_DEBUG_FPS=log`. Called at the *top* of every `maybe_present`
    /// -- crucially including the calls that early-return without presenting
    /// (`skipped_not_ready` / `skipped_no_slot`) -- so the counter split stays
    /// live even under a hard stall, when successful presents (and thus the
    /// old log site in `draw_fps_overlay`) go quiet exactly when the numbers
    /// matter most.
    ///
    /// The line answers the scroll-flood question directly:
    /// - `skipped no-slot`  ≫ `not-ready`  → buffer starvation (both shm
    ///   frames held by the compositor) → more frames would help (triple
    ///   buffering).
    /// - `skipped not-ready` ≫ `no-slot`   → pure vsync pacing (waiting on
    ///   `wl_callback.done`); more buffers won't help.
    /// - `-> CPU-bound`                     → render work itself is the wall.
    fn log_frame_pacing(&mut self) {
        if self.debug_fps != DebugFps::Log {
            return;
        }
        let now = std::time::Instant::now();
        if now.duration_since(self.frame_stats.last_log).as_secs_f32() < 1.0 {
            return;
        }
        self.frame_stats.last_log = now;
        self.frame_stats.prune(now);
        let fps = self.frame_stats.fps();
        let busy_us = self.frame_stats.drain_busy_us();
        // Frame interval at this fps, in microseconds. `busy_us` is CPU time
        // actually spent in maybe_present; the rest of the interval was spent
        // blocked on `wl_callback.done`.
        let interval_us = if fps > 0.0 {
            1_000_000.0 / f64::from(fps)
        } else {
            0.0
        };
        let bound = if interval_us > 0.0 && busy_us / interval_us > 0.6 {
            "CPU-bound"
        } else {
            "compositor-bound"
        };
        let backlog = self.frame_stats.max_backlog_since_log;
        let no_slot = self.frame_stats.skipped_no_slot;
        let not_ready = self.frame_stats.skipped_not_ready;
        self.frame_stats.max_backlog_since_log = 0;
        self.frame_stats.skipped_no_slot = 0;
        self.frame_stats.skipped_not_ready = 0;
        eprintln!(
            "terminal: {fps:.1} fps | present-busy {busy_us:.0} us/frame \
             (interval {interval_us:.0} us) -> {bound} | backlog max {backlog} | \
             skipped no-slot {no_slot} not-ready {not_ready}"
        );
    }

    fn maybe_present(&mut self) -> Result<()> {
        if self.lifecycle != Lifecycle::Mapped {
            return Ok(());
        }
        self.apply_pending_resize()?;
        self.frame_stats.note_backlog(self.pending_dirty.len());
        self.log_frame_pacing();
        if !self.dirty || !self.frame_ready {
            if self.dirty && !self.frame_ready {
                self.frame_stats.skipped_not_ready += 1;
            }
            return Ok(());
        }
        let Some(slot) = self.frames.iter().position(|f| f.free) else {
            self.frame_stats.skipped_no_slot += 1;
            return Ok(());
        };
        self.frames[slot].free = false;
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
            Self::shift_overlay_rows_signed(&mut self.prev_fps_rows, &viewport_shifts, rows);
        }
        if !scrolls.is_empty() {
            Self::shift_overlay_rows(&mut self.prev_cursor_rows, &scrolls);
            Self::shift_overlay_rows(&mut self.prev_selection_rows, &scrolls);
            Self::shift_overlay_rows(&mut self.prev_fps_rows, &scrolls);
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
        for r in std::mem::take(&mut self.prev_cursor_rows) {
            if !rows_to_repaint.contains(&r) {
                rows_to_repaint.push(r);
            }
        }
        for r in std::mem::take(&mut self.prev_fps_rows) {
            if !rows_to_repaint.contains(&r) {
                rows_to_repaint.push(r);
            }
        }
        self.repaint_rows(&rows_to_repaint);
        touched_rows.extend_from_slice(&rows_to_repaint);
        self.apply_selection_highlight();
        touched_rows.extend(self.selection_rows());
        self.prev_selection_rows = self.selection_rows();
        let new_cursor_rows = self.draw_cursor_overlay();
        touched_rows.extend_from_slice(&new_cursor_rows);
        self.prev_cursor_rows = new_cursor_rows;
        self.frame_stats.record();
        let new_fps_rows = self.draw_fps_overlay();
        touched_rows.extend_from_slice(&new_fps_rows);
        self.prev_fps_rows = new_fps_rows;
        let t_overlay = present_start.elapsed();

        // Every row touched this frame is now stale relative to *both* shm
        // slots (whichever one isn't presented into this time still needs
        // catching up next time it's used).
        self.mark_rows_stale(touched_rows);

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
        let dst = self.shm_mem.pixels(slot);
        let stale = &mut self.stale_rows[slot];
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

        let buf = self.frames[slot].buffer;
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
        self.frame_cb = Some(cb);
        self.window_tx
            .request(MessageBuilder::new(self.surface, SurfaceRequest::Commit));

        self.frame_ready = false;
        self.dirty = false;
        let total = present_start.elapsed();
        self.frame_stats.record_busy(total);
        // On a slow present (>5ms -- never the 60fps steady state, always the
        // spikes we're chasing), break the timed region into its three phases
        // so we can see WHICH one owns the ~16ms. Gated behind DebugFps::Log so
        // it's free otherwise. `submit` includes the stale-row shm memcpy plus
        // the Attach/Damage/Commit ring writes -- if the socket is applying
        // backpressure (compositor can't drain), the write blocks here and this
        // phase, not blit, will own the time.
        if self.debug_fps == DebugFps::Log && total.as_micros() > 5_000 {
            let blit_us = t_blit.as_micros();
            let overlay_us = t_overlay.saturating_sub(t_blit).as_micros();
            let submit_us = total.saturating_sub(t_overlay).as_micros();
            eprintln!(
                "terminal:   slow present {}us = blit {blit_us}us ({folded_len} folded, \
                 {painted_rows} rows / {painted_cells} cells) \
                 + overlay {overlay_us}us + shm-copy/submit {submit_us}us",
                total.as_micros()
            );
        }
        Ok(())
    }

    fn resize(&mut self, w: i32, h: i32) -> Result<()> {
        // Destroy only the old *buffers* — the pool itself is reused. The server
        // answers each Destroy with `wl_display.delete_id`, which recycles those
        // ids; without it every resize step would leak 2 buffer ids. (Before,
        // the pool was destroyed and rebuilt too, which also re-`mmap`ped and
        // re-sent the shm fd on every drag frame — now avoided.)
        for frame in &self.frames {
            self.window_tx
                .request(MessageBuilder::new(frame.buffer, BufferRequest::Destroy));
        }

        // Grow the existing pool in place instead of tearing it down: `ftruncate`
        // the memfd + `mremap` our mapping, and only when it actually grew tell
        // the compositor via `wl_shm_pool.resize`. A shrink keeps the oversized
        // pool and skips the round-trip entirely (lazy shrink). Either way the
        // shm fd is never re-sent, so a resize storm stops thrashing the pool.
        let grew = self
            .shm_mem
            .resize(w as usize, h as usize)
            .map_err(|e| Error::Conn(ConnError::Sys(e)))?;
        let stride = w * 4;

        if let Some(new_cap) = grew {
            // Route the resize through the same `window_tx` MPSC ring as the
            // buffer creates below so it stays ordered ahead of them (the pool
            // must be big enough before the new buffers are carved out of it).
            self.window_tx.request(
                MessageBuilder::new(self.pool, ShmPoolRequest::Resize).i32(new_cap as i32),
            );
        }

        for (i, frame) in self.frames.iter_mut().enumerate() {
            let buffer: BufferId = self.window_tx.alloc_id();
            self.window_tx.request(
                MessageBuilder::new(self.pool, ShmPoolRequest::CreateBuffer)
                    .new_id(buffer)
                    .i32(self.shm_mem.frame_offset(i) as i32)
                    .i32(w)
                    .i32(h)
                    .i32(stride)
                    .u32(self.buf_format.code()),
            );
            frame.buffer = buffer;
            frame.free = true;
        }
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

        // `self.pool` is unchanged — we reuse it across resizes now.
        self.width = w;
        self.height = h;
        self.dirty = true;
        // Both shm buffers are brand-new (freshly created, uninitialized)
        // memory, so every row is stale relative to the backbuffer in both
        // slots -- the next two presents each need a full copy to actually
        // fill their buffer, not just the rows that changed since the old
        // size.
        let row_count = ((h / self.cell_h).max(1)) as usize;
        self.stale_rows = [vec![true; row_count], vec![true; row_count]];
        // Both buffers are brand-new and free, and any in-flight frame callback
        // referred to the now-destroyed surface contents. Force the next
        // present to fire immediately so we never leave the surface showing a
        // detached/blank buffer (the cause of resize flashing).
        self.frame_ready = true;
        self.frame_cb = None;
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
    fn output_scale(sender: &mut Sender, events: &mut Consumer<RawEvent>, output: OutputId) -> i32 {
        let cb: CallbackId = sender.alloc_id();
        if sender
            .request(MessageBuilder::new(DisplayId(1), DisplayRequest::Sync).new_id(cb))
            .is_err()
        {
            return 1;
        }

        let mut scale = 1i32;
        loop {
            let Some(event) = events.pop_block() else {
                return scale;
            };
            match event {
                RawEvent::Bytes(bytes) => {
                    let Some((mut ev, _)) = WireEvent::parse(&bytes) else {
                        continue;
                    };
                    if ev.object_id == output.raw()
                        && OutputEvent::from_opcode(ev.opcode) == Some(OutputEvent::Scale)
                    {
                        scale = ev.i32();
                    }
                }
                RawEvent::Wire {
                    object_id, opcode, ..
                } => {
                    if object_id == cb.raw()
                        && CallbackEvent::from_opcode(opcode) == Some(CallbackEvent::Done)
                    {
                        return scale;
                    }
                }
                RawEvent::Repeat => {}
                // Fd-carrying events never occur during output-scale probing.
                RawEvent::Fd(_) => {}
            }
        }
    }

    /// Bind one global by name/interface/version off the registry.
    ///
    /// Was a free `bind(sender, registry, name, interface, version)`
    /// function -- moved onto `Registry` alongside `discover`, its only
    /// caller.
    fn bind<T: ObjectId>(
        sender: &mut Sender,
        registry: RegistryId,
        name: u32,
        interface: &str,
        version: u32,
    ) -> Result<T> {
        let new_obj: T = sender.alloc_id();
        sender.request(
            MessageBuilder::new(registry, RegistryRequest::Bind)
                .u32(name)
                .string(interface)
                .u32(version)
                .new_id(new_obj),
        )?;
        Ok(new_obj)
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

//! A `zwlr_layer_shell_v1` status-bar window: a thin, edge-anchored,
//! exclusive-zone strip drawn with a software framebuffer.
//!
//! This is the layer-shell sibling of `platform::TermWindow` — same
//! connection plumbing (`platform::conn::Conn`, the wire encoder, the raw
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

use platform::conn::{Conn, ConnError, Sender, WindowSender};
use platform::draw::{Canvas, Color};
use platform::event::RawEvent;
use platform::font;
use platform::id::{
    BufferId, CompositorId, LayerShellId, LayerSurfaceId, ObjectId, OutputId, PoolId, RegistryId,
    ShmId, SurfaceId,
};
use platform::protocol::{
    self as proto, Anchor, BufferEvent, BufferRequest, CallbackEvent, CompositorRequest,
    DisplayEvent, DisplayRequest, Format, KeyboardInteractivity, Layer, LayerShellRequest,
    LayerSurfaceEvent, LayerSurfaceRequest, RegistryEvent, RegistryRequest, ShmPoolRequest,
    ShmRequest, SurfaceRequest,
};
use platform::sys::ShmBuffer;
use platform::wire::{MessageBuilder, WireEvent};
use platform::{Error, Result};
use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc;
use quetzalcoatl::spsc::{Consumer, RingBuffer};

const EVENT_RING_CAP: usize = 256;
const APP_ID: &str = "statusbar";

/// Placeholder width the pool is first sized at, before the compositor's
/// initial `configure` tells us the real (output-width) size. Matches
/// `TermWindow`'s "size known before first commit" ordering closely enough
/// that we never hit the "second create_pool" rejection — the *first* frame
/// pair (buffers + pool) is created up front, and every later size change
/// reuses the same pool via `resize`.
const INITIAL_WIDTH: i32 = 1;

/// Which edge of the output the bar docks to.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Edge {
    Top,
    Bottom,
}

impl Edge {
    fn anchor_bits(self) -> u32 {
        match self {
            Edge::Top => Anchor::bits(&[Anchor::Top, Anchor::Left, Anchor::Right]),
            Edge::Bottom => Anchor::bits(&[Anchor::Bottom, Anchor::Left, Anchor::Right]),
        }
    }
}

#[derive(Copy, Clone)]
struct Frame {
    buffer: BufferId,
    free: bool,
}

/// A layer-shell status-bar window: anchors to a screen edge, reserves an
/// exclusive zone, and blits whatever [`BarWindow::draw`] paints.
pub struct BarWindow {
    sender: Sender,
    request_rx: mpsc::Consumer<platform::conn::WindowRequest>,
    window_tx: WindowSender,
    events: Consumer<RawEvent>,
    #[allow(dead_code)]
    reader: Option<std::thread::JoinHandle<Result<()>>>,
    wake: waker::Waker,

    #[allow(dead_code)]
    compositor: CompositorId,
    #[allow(dead_code)]
    layer_shell: LayerShellId,
    surface: SurfaceId,
    layer_surface: LayerSurfaceId,

    #[allow(dead_code)]
    edge: Edge,
    height: i32,
    scale: i32,

    width: i32,
    frames: [Frame; 2],
    shm_mem: ShmBuffer,
    /// The `wl_shm_pool` backing `frames`, created once in [`BarWindow::new`]
    /// and reused for the window's whole lifetime — see the module doc for
    /// why a second `create_pool` gets rejected.
    pool: PoolId,
    buf_format: Format,

    backbuffer: Vec<u32>,
    /// Background the canvas is cleared to before each frame's `paint`
    /// closure runs. Defaults to a dark slate; set via
    /// [`BarWindow::set_background`] (mirrors `[bar] background` / the
    /// terminal's own `[colors] background` -- see `config::Config::bar_background`).
    background: Color,
    dirty: bool,
    frame_ready: bool,
    frame_cb: Option<platform::id::CallbackId>,
    configured: bool,
    closing: bool,
}

impl Drop for BarWindow {
    fn drop(&mut self) {
        let _ = self.sender.request(MessageBuilder::new(
            self.layer_surface,
            LayerSurfaceRequest::Destroy,
        ));
        let _ = self
            .sender
            .request(MessageBuilder::new(self.surface, SurfaceRequest::Destroy));
    }
}

struct Globals {
    compositor: CompositorId,
    shm: ShmId,
    layer_shell: LayerShellId,
    output: Option<OutputId>,
}

impl BarWindow {
    /// Open a layer-shell bar `height` pixels thick, docked at `edge`, and
    /// reserving that much exclusive space.
    pub fn new(edge: Edge, height: i32, wake: waker::Waker) -> Result<Self> {
        let (mut sender, reader, _repeat_timer) = Conn::connect()?;
        let (producer, mut events) =
            RingBuffer::<RawEvent>::new(Capacity::exact(EVENT_RING_CAP)).split();
        let reader_wake = wake.clone();
        let reader_handle = std::thread::Builder::new()
            .name("statusbar-wayland-reader".into())
            .spawn(move || {
                let r = reader.run(producer, Some(reader_wake)).map_err(Error::Conn);
                if let Err(e) = &r {
                    eprintln!("statusbar: reader error: {e}");
                }
                r
            })
            .map_err(Error::ReaderSpawn)?;

        let Globals {
            compositor,
            shm,
            layer_shell,
            output,
        } = Self::discover(&mut sender, &mut events)?;

        let scale = match output {
            Some(o) => Self::output_scale(&mut sender, &mut events, o),
            None => 1,
        };

        let px_height = height * scale;
        let buf_format = Format::Xrgb8888;

        // Create the shm pool + initial frame pair *before* any layer-shell
        // setup — matching TermWindow's ordering, and the ordering the
        // compositor apparently requires (a `create_pool` issued after the
        // layer surface's first configure/commit round-trip gets rejected
        // with "invalid arguments" on niri/wlroots). The real width arrives
        // via the first `configure`; `resize` grows this same pool in place.
        let (frames, shm_mem, pool) =
            Self::create_frame_pair(&mut sender, shm, INITIAL_WIDTH, px_height, buf_format)?;

        let surface: SurfaceId = sender.alloc_id();
        sender.request(
            MessageBuilder::new(compositor, CompositorRequest::CreateSurface).new_id(surface),
        )?;
        sender.request(MessageBuilder::new(surface, SurfaceRequest::SetBufferScale).i32(scale))?;

        let layer_surface: LayerSurfaceId = sender.alloc_id();
        sender.request(
            MessageBuilder::new(layer_shell, LayerShellRequest::GetLayerSurface)
                .new_id(layer_surface)
                .object(surface)
                .object(OutputId(0)) // null: let the compositor pick the output
                .u32(Layer::Top as u32)
                .string(APP_ID),
        )?;

        sender.request(
            MessageBuilder::new(layer_surface, LayerSurfaceRequest::SetAnchor)
                .u32(edge.anchor_bits()),
        )?;
        sender.request(
            MessageBuilder::new(layer_surface, LayerSurfaceRequest::SetSize)
                .i32(0)
                .i32(height),
        )?;
        sender.request(
            MessageBuilder::new(layer_surface, LayerSurfaceRequest::SetExclusiveZone)
                .i32(height),
        )?;
        sender.request(
            MessageBuilder::new(layer_surface, LayerSurfaceRequest::SetMargin)
                .i32(0)
                .i32(0)
                .i32(0)
                .i32(0),
        )?;
        sender.request(
            MessageBuilder::new(
                layer_surface,
                LayerSurfaceRequest::SetKeyboardInteractivity,
            )
            .u32(KeyboardInteractivity::None as u32),
        )?;
        // First commit carries no buffer -- this is what solicits the initial
        // `configure`, which tells us the real (output-width) pixel size.
        sender.request(MessageBuilder::new(surface, SurfaceRequest::Commit))?;

        let (req_tx, req_rx) =
            mpsc::RingBuffer::<platform::conn::WindowRequest>::new(Capacity::exact(64)).split();
        let (id_start, id_end) = sender.alloc_id_block(64);
        let window_tx = WindowSender {
            tx: req_tx,
            next_id: id_start,
            id_end,
            free_ids: Vec::new(),
        };

        Ok(Self {
            sender,
            request_rx: req_rx,
            window_tx,
            events,
            reader: Some(reader_handle),
            wake,
            compositor,
            layer_shell,
            surface,
            layer_surface,
            edge,
            height: px_height,
            scale,
            width: INITIAL_WIDTH,
            frames,
            shm_mem,
            pool,
            buf_format,
            backbuffer: vec![Color::BLACK.to_xrgb8888(); (INITIAL_WIDTH * px_height) as usize],
            background: Color::rgb(0x1e, 0x1e, 0x2e),
            dirty: false,
            frame_ready: true,
            frame_cb: None,
            configured: false,
            closing: false,
        })
    }

    /// Physical cell height in pixels the bar was opened at (already scaled).
    #[must_use]
    pub fn height(&self) -> i32 {
        self.height
    }

    #[must_use]
    pub fn scale(&self) -> i32 {
        self.scale
    }

    /// Mark the bar dirty so the next `tick` repaints and presents.
    pub fn request_redraw(&mut self) {
        self.dirty = true;
    }

    /// Set the color the canvas is cleared to before each frame's `paint`
    /// closure runs, and request a redraw so the change is visible
    /// immediately.
    pub fn set_background(&mut self, color: Color) {
        self.background = color;
        self.dirty = true;
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

    /// Process pending Wayland events (non-blocking) and present if dirty.
    /// Returns `false` once the layer surface is destroyed / connection lost.
    pub fn tick(&mut self, mut paint: impl FnMut(&mut Canvas)) -> Result<bool> {
        if self.closing {
            return Ok(false);
        }
        self.sender
            .drain_requests(&mut self.request_rx)
            .map_err(Error::Conn)?;

        while let Some(ev) = self.events.pop() {
            self.handle_event(ev)?;
            if self.closing {
                return Ok(false);
            }
        }

        if self.configured && self.dirty && self.frame_ready {
            self.present(&mut paint)?;
        }
        Ok(true)
    }

    fn handle_event(&mut self, ev: RawEvent) -> Result<()> {
        if let RawEvent::Bytes(ref bytes) = ev {
            let (mut wire, _) = WireEvent::parse(bytes).ok_or(Error::ReaderStopped)?;
            if wire.object_id == platform::id::DisplayId(1).raw() {
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

        if let Some(frame) = self.frames.iter_mut().find(|f| ev.is(f.buffer)) {
            if ev.event::<BufferEvent>() == Some(BufferEvent::Release) {
                frame.free = true;
            }
            return Ok(());
        }

        if let Some(cb) = self.frame_cb {
            if ev.is(cb) && ev.event::<CallbackEvent>() == Some(CallbackEvent::Done) {
                self.frame_ready = true;
                self.frame_cb = None;
                return Ok(());
            }
        }

        if ev.is(self.layer_surface) {
            match ev.event::<LayerSurfaceEvent>() {
                Some(LayerSurfaceEvent::Configure) => {
                    let serial = ev.arg(0);
                    let w = ev.arg(1) as i32 * self.scale;
                    self.window_tx.request(
                        MessageBuilder::new(self.layer_surface, LayerSurfaceRequest::AckConfigure)
                            .u32(serial),
                    );
                    if w > 0 {
                        self.resize(w)?;
                    }
                    self.configured = true;
                    self.dirty = true;
                    return Ok(());
                }
                Some(LayerSurfaceEvent::Closed) => {
                    self.closing = true;
                    return Ok(());
                }
                None => {}
            }
        }
        Ok(())
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

        for frame in &self.frames {
            self.window_tx
                .request(MessageBuilder::new(frame.buffer, BufferRequest::Destroy));
        }

        let grew = self
            .shm_mem
            .resize(width as usize, self.height as usize)
            .map_err(|e| Error::Conn(ConnError::Sys(e)))?;
        let stride = width * 4;

        if let Some(new_cap) = grew {
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
                    .i32(width)
                    .i32(self.height)
                    .i32(stride)
                    .u32(self.buf_format.code()),
            );
            frame.buffer = buffer;
            frame.free = true;
        }

        self.backbuffer = vec![Color::BLACK.to_xrgb8888(); (width * self.height) as usize];
        Ok(())
    }

    /// Build a fresh `wl_shm_pool` plus the two `wl_buffer`s carved out of
    /// it. Called exactly once, from [`BarWindow::new`] — every later resize
    /// goes through [`BarWindow::resize`] instead, which reuses this same
    /// pool.
    fn create_frame_pair(
        sender: &mut Sender,
        shm: ShmId,
        width: i32,
        height: i32,
        format: Format,
    ) -> Result<([Frame; 2], ShmBuffer, PoolId)> {
        let shm_mem = ShmBuffer::new(width as usize, height as usize, 2)
            .map_err(|e| Error::Conn(ConnError::Sys(e)))?;
        let stride = width * 4;
        let pool: PoolId = sender.alloc_id();
        let msg = MessageBuilder::new(shm, ShmRequest::CreatePool)
            .new_id(pool)
            .i32(shm_mem.size_bytes() as i32)
            .finish();
        sender.send_with_fds(msg, &[shm_mem.fd()])?;
        let mut frames = [Frame {
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

    fn present(&mut self, paint: &mut impl FnMut(&mut Canvas)) -> Result<()> {
        let Some(slot) = self.frames.iter().position(|f| f.free) else {
            return Ok(());
        };
        self.frames[slot].free = false;

        {
            let mut canvas = Canvas::new(&mut self.backbuffer, self.width, self.height);
            canvas.fill_rect(canvas.bounds(), self.background);
            paint(&mut canvas);
        }

        let dst = self.shm_mem.pixels(slot);
        dst.copy_from_slice(&self.backbuffer);

        let buffer = self.frames[slot].buffer;
        self.window_tx.request(
            MessageBuilder::new(self.surface, SurfaceRequest::Attach)
                .object(buffer)
                .i32(0)
                .i32(0),
        );
        self.window_tx.request(
            MessageBuilder::new(self.surface, SurfaceRequest::Damage)
                .i32(0)
                .i32(0)
                .i32(self.width)
                .i32(self.height),
        );
        let cb: platform::id::CallbackId = self.window_tx.alloc_id();
        self.window_tx
            .request(MessageBuilder::new(self.surface, SurfaceRequest::Frame).new_id(cb));
        self.frame_cb = Some(cb);
        self.window_tx
            .request(MessageBuilder::new(self.surface, SurfaceRequest::Commit));
        self.frame_ready = false;
        self.dirty = false;
        Ok(())
    }

    fn discover(sender: &mut Sender, events: &mut Consumer<RawEvent>) -> Result<Globals> {
        let registry: RegistryId = sender.alloc_id();
        sender.request(
            MessageBuilder::new(platform::id::DisplayId(1), DisplayRequest::GetRegistry)
                .new_id(registry),
        )?;

        let mut compositor: Option<CompositorId> = None;
        let mut shm: Option<ShmId> = None;
        let mut layer_shell: Option<LayerShellId> = None;
        let mut output: Option<OutputId> = None;

        while compositor.is_none() || shm.is_none() || layer_shell.is_none() {
            let event = events.pop_block().ok_or(Error::ReaderStopped)?;
            let RawEvent::Bytes(bytes) = event else {
                continue;
            };
            let Some((mut ev, _)) = WireEvent::parse(&bytes) else {
                continue;
            };

            if ev.object_id == platform::id::DisplayId(1).raw()
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
                    s if s == proto::ZWLR_LAYER_SHELL_V1.name && layer_shell.is_none() => {
                        let v = proto::ZWLR_LAYER_SHELL_V1.version.min(version);
                        layer_shell = Some(Self::bind(sender, registry, name, &interface, v)?);
                    }
                    s if s == proto::WL_OUTPUT.name && output.is_none() => {
                        let v = proto::WL_OUTPUT.version.min(version);
                        output = Some(Self::bind(sender, registry, name, &interface, v)?);
                    }
                    _ => {}
                }
            }
        }
        Ok(Globals {
            compositor: compositor.ok_or(Error::ReaderStopped)?,
            shm: shm.ok_or(Error::ReaderStopped)?,
            layer_shell: layer_shell.ok_or(Error::ReaderStopped)?,
            output,
        })
    }

    fn output_scale(sender: &mut Sender, events: &mut Consumer<RawEvent>, output: OutputId) -> i32 {
        let cb: platform::id::CallbackId = sender.alloc_id();
        if sender
            .request(
                MessageBuilder::new(platform::id::DisplayId(1), DisplayRequest::Sync).new_id(cb),
            )
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
                        && proto::OutputEvent::from_opcode(ev.opcode)
                            == Some(proto::OutputEvent::Scale)
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
                RawEvent::Repeat | RawEvent::Fd(_) => {}
            }
        }
    }

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

/// Draw left-aligned text at `(x, y)` using the embedded bitmap font,
/// returning the x just past the last glyph. Thin re-export wrapper so
/// module code doesn't need to reach into `platform::draw` directly.
pub fn draw_text(canvas: &mut Canvas, text: &str, x: i32, y: i32, color: Color) -> i32 {
    canvas.draw_text(text, x, y, color);
    x + text.chars().count() as i32 * font::GLYPH_W
}

/// Right-align `text` so its last glyph ends at `right_x`.
pub fn draw_text_right(canvas: &mut Canvas, text: &str, right_x: i32, y: i32, color: Color) {
    let w = text.chars().count() as i32 * font::GLYPH_W;
    canvas.draw_text(text, right_x - w, y, color);
}

/// Vertical center for one line of the embedded bitmap font within a bar of
/// `bar_height` pixels.
#[must_use]
pub fn text_baseline_y(bar_height: i32) -> i32 {
    ((bar_height - font::GLYPH_H) / 2).max(0)
}

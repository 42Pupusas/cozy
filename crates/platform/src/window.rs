#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_possible_wrap,
         clippy::cast_lossless, clippy::option_if_let_else, clippy::too_many_lines,
         clippy::needless_pass_by_value, clippy::collapsible_if, clippy::unnecessary_wraps)]
//! Terminal window: open a Wayland window and render grid cells via software
//! framebuffer.  Adapted from paint's `app.rs` — all UI / demo logic replaced
//! with grid-cell blitting.

use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc;
use quetzalcoatl::spsc::{Consumer, RingBuffer};

use crate::conn::{self, ConnError, Sender, WindowSender, drain_requests};
use crate::cursor;
use crate::draw::Point;
use crate::error::{Error, Result};
use crate::event::Event;
use crate::id::{
    BufferId, CallbackId, CompositorId, DisplayId, KeyboardId, ObjectId, PointerId, PoolId,
    RegistryId, SeatId, ShmId, SurfaceId, ToplevelId, WmBaseId, XdgSurfaceId,
};
use crate::keymap;
use crate::protocol::{
    self as proto, BufferEvent, ButtonState, CallbackEvent, CompositorRequest, DisplayEvent,
    DisplayRequest,
    Format, Key, KeyState, KeyboardEvent, PointerButton, PointerEvent, PointerRequest,
    RegistryEvent, RegistryRequest, SeatCapability, SeatEvent, SeatRequest, ShmPoolRequest,
    ShmRequest, SurfaceRequest, ToplevelEvent, ToplevelRequest, WmBaseEvent, WmBaseRequest,
    XdgSurfaceEvent, XdgSurfaceRequest,
};
use crate::protocol::Modifiers;
use crate::render;
use crate::sys::{RepeatTimer, ShmBuffer};
use crate::wire::{Event as WireEvent, MessageBuilder};

const EVENT_RING_CAP: usize = 1024;

const DEFAULT_REPEAT_RATE: u32 = 25;
const DEFAULT_REPEAT_DELAY: u32 = 600;

/// A keyboard/pointer input event from the compositor.
///
/// Produced by [`TermWindow`] for the caller to encode into PTY bytes (M5).
#[derive(Clone, Debug)]
pub enum InputEvent {
    Key { key: Option<Key>, state: Option<KeyState>, modifiers: Modifiers },
    Close,
    Resize { cols: u16, rows: u16 },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Lifecycle {
    Unconfigured,
    Mapped,
    Closing,
}

#[derive(Copy, Clone, Debug)]
struct Frame {
    buffer: BufferId,
    free: bool,
}

/// A Wayland window that renders a terminal grid using a software framebuffer.
///
/// Feed [`grid::DirtySnapshot`]s via [`TermWindow::push_dirty`]; the window
/// blits them on the next frame.  Input arrives back through the
/// [`InputEvent`] iterator (`.next_input()`).
pub struct TermWindow {
    sender: Sender,
    request_rx: mpsc::Consumer<crate::conn::WindowRequest>,
    window_tx: WindowSender,
    events: Consumer<Event>,
    reader: Option<std::thread::JoinHandle<Result<()>>>,

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

    frames: [Frame; 2],
    shm_mem: ShmBuffer,
    /// Persistent full-window backbuffer. Dirty rows are blitted here (it keeps
    /// the complete last-known image), then copied into whichever shm frame is
    /// free on present. Without this, incremental dirty-row blits would only
    /// ever touch one of the two shm frames, leaving the rest black.
    backbuffer: Vec<u32>,
    width: i32,
    height: i32,
    #[allow(dead_code)]
    scale: i32,

    /// Cell dimensions in pixels.
    cell_w: i32,
    cell_h: i32,

    pointer_pos: Point,
    dirty: bool,
    frame_ready: bool,
    frame_cb: Option<CallbackId>,
    lifecycle: Lifecycle,

    /// Pending dirty snapshots to blit on the next frame.
    pending_dirty: Vec<grid::DirtySnapshot>,
    /// Pending input events for the caller.
    input_queue: std::collections::VecDeque<InputEvent>,
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
        let _ = self
            .sender
            .request(MessageBuilder::new(self.xdg_surface, XdgSurfaceRequest::Destroy));
        let _ = self
            .sender
            .request(MessageBuilder::new(self.surface, SurfaceRequest::Destroy));
    }
}

impl TermWindow {
    /// Open a Wayland terminal window of `cols × rows` cells.
    /// Open a Wayland terminal window with `cols × rows` cells.
    pub fn new(title: &str, cols: u16, rows: u16) -> Result<Self> {
        let cell_w = render::CELL_W;
        let cell_h = render::CELL_H;
        let width = cols as i32 * cell_w;
        let height = rows as i32 * cell_h;

        let (mut sender, reader, repeat_timer) = conn::connect()?;
        let (producer, mut events) =
            RingBuffer::<Event>::new(Capacity::exact(EVENT_RING_CAP)).split();
        let reader_handle = std::thread::Builder::new()
            .name("terminal-wayland-reader".into())
            .spawn(move || {
                let r = reader.run(producer).map_err(Error::Conn);
                if let Err(e) = &r {
                    eprintln!("platform: reader error: {e}");
                }
                r
            })
            .map_err(Error::ReaderSpawn)?;

        let (compositor, shm, wm_base, seat) = bind_globals(&mut sender, &mut events)?;
        let (frames, shm_mem) = create_frames(&mut sender, shm, width, height)?;

        let surface: SurfaceId = sender.alloc_id();
        sender.request(
            MessageBuilder::new(compositor, CompositorRequest::CreateSurface).new_id(surface),
        )?;
        sender.request(MessageBuilder::new(surface, SurfaceRequest::SetBufferScale).i32(1))?;

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
        sender.request(MessageBuilder::new(surface, SurfaceRequest::Commit))?;

        let cursor = create_cursor(&mut sender, shm, compositor).ok();

        let (req_tx, req_rx) =
            mpsc::RingBuffer::<crate::conn::WindowRequest>::new(Capacity::exact(64)).split();
        let (id_start, id_end) = sender.alloc_id_block(256);
        let window_tx = WindowSender { tx: req_tx, next_id: id_start, id_end };

        let backbuffer = vec![render::DEFAULT_BG_XRGB; (width * height) as usize];

        Ok(Self {
            sender,
            request_rx: req_rx,
            window_tx,
            events,
            reader: Some(reader_handle),
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
            backbuffer,
            width,
            height,
            scale: 1,
            cell_w,
            cell_h,
            pointer_pos: Point::new(0, 0),
            dirty: true,
            frame_ready: true,
            frame_cb: None,
            lifecycle: Lifecycle::Unconfigured,
            pending_dirty: Vec::new(),
            input_queue: std::collections::VecDeque::new(),
        })
    }

    /// Queue a dirty snapshot to be blitted on the next frame.
    pub fn push_dirty(&mut self, snap: grid::DirtySnapshot) {
        if !snap.is_empty() {
            self.pending_dirty.push(snap);
            self.dirty = true;
        }
    }

    /// Pull the next pending input event, if any.
    pub fn next_input(&mut self) -> Option<InputEvent> {
        self.input_queue.pop_front()
    }

    /// Process pending Wayland events (non-blocking).
    /// Returns `true` while the window should stay open.
    pub fn tick(&mut self) -> Result<bool> {
        if self.lifecycle == Lifecycle::Closing {
            return Ok(false);
        }
        drain_requests(&mut self.request_rx, &mut self.sender)
            .map_err(Error::Conn)?;

        // Drain all queued events non-blocking.
        while let Some(ev) = self.events.pop() {
            self.handle_event(ev)?;
            if self.lifecycle == Lifecycle::Closing {
                return Ok(false);
            }
        }
        self.maybe_present()?;
        Ok(true)
    }

    /// Block for the next Wayland event, then drain all queued events.
    /// Returns `true` while the window should stay open.
    pub fn run_once(&mut self) -> Result<bool> {
        if self.lifecycle == Lifecycle::Closing {
            return Ok(false);
        }
        drain_requests(&mut self.request_rx, &mut self.sender)
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
        self.maybe_present()?;
        Ok(self.lifecycle != Lifecycle::Closing)
    }

    // ── private ───────────────────────────────────────────────────────────

    fn handle_event(&mut self, ev: Event) -> Result<()> {
        // wl_display.error
        if let Event::Bytes(ref bytes) = ev {
            let (mut wire, _) = WireEvent::parse(bytes).ok_or(Error::ReaderStopped)?;
            if wire.object_id == DisplayId(1).raw()
                && DisplayEvent::from_opcode(wire.opcode) == Some(DisplayEvent::Error)
            {
                return Err(Error::Protocol {
                    object: wire.u32(),
                    code: wire.u32(),
                    message: wire.string(),
                });
            }
            // Registry global (during setup - already handled, ignore here).
            return Ok(());
        }

        // Key repeat timer.
        if matches!(ev, Event::Repeat) {
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
            // xdg_toplevel.configure args: (width, height, states[]).
            let w = ev.arg_i32(0);
            let h = ev.arg_i32(1);
            if w > 0 && h > 0 && (w, h) != (self.width, self.height) {
                self.resize(w, h)?;
                let new_cols = (w / self.cell_w) as u16;
                let new_rows = (h / self.cell_h) as u16;
                self.input_queue.push_back(InputEvent::Resize { cols: new_cols, rows: new_rows });
                self.dirty = true;
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
                                    .i32(cursor::HOTSPOT_X)
                                    .i32(cursor::HOTSPOT_Y),
                            );
                        }
                    }
                    Some(PointerEvent::Motion) => {
                        self.pointer_pos = ev.point(1, 2);
                    }
                    Some(PointerEvent::Button) => {
                        let button = PointerButton::from_code(ev.arg(2));
                        let state = ButtonState::from_raw(ev.arg(3));
                        // Right-click paste etc. handled by crates/input (M5).
                        let _ = (button, state);
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
                    Some(KeyboardEvent::Key) => {
                        let physical = Key::from_code(ev.arg(2));
                        let logical = physical.map(keymap::physical_to_logical);
                        let state = KeyState::from_raw(ev.arg(3));
                        // Start/stop repeat timer. Modifier keys never repeat.
                        match (logical, state) {
                            (Some(k), Some(KeyState::Pressed))
                                if !physical.is_some_and(Key::is_modifier) =>
                            {
                                self.held_key = Some(k);
                                let _ = self.repeat_timer.arm(u64::from(self.repeat_delay), u64::from(1000 / self.repeat_rate.max(1)));
                            }
                            (Some(k), Some(KeyState::Released)) if self.held_key == Some(k) => {
                                self.held_key = None;
                                let _ = self.repeat_timer.disarm();
                            }
                            _ => {}
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
                        let _ = self.repeat_timer.disarm();
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

        Ok(())
    }

    fn maybe_present(&mut self) -> Result<()> {
        if self.lifecycle != Lifecycle::Mapped {
            return Ok(());
        }
        if !self.dirty || !self.frame_ready {
            return Ok(());
        }
        let Some(slot) = self.frames.iter().position(|f| f.free) else {
            return Ok(());
        };
        let frame = &mut self.frames[slot];
        frame.free = false;

        // Blit pending dirty snapshots into the persistent backbuffer, which
        // always holds the full image, then copy it into the free shm frame.
        for snap in self.pending_dirty.drain(..) {
            render::blit_snapshot(&mut self.backbuffer, self.width as u32, &snap);
        }
        self.shm_mem.pixels(slot).copy_from_slice(&self.backbuffer);

        let buf_w = self.width;
        let buf_h = self.height;
        let stride = buf_w * 4;
        let buf = frame.buffer;

        self.window_tx.request(
            MessageBuilder::new(self.surface, SurfaceRequest::Attach)
                .object(buf)
                .i32(0)
                .i32(0),
        );
        self.window_tx.request(
            MessageBuilder::new(self.surface, SurfaceRequest::Damage)
                .i32(0)
                .i32(0)
                .i32(buf_w)
                .i32(buf_h),
        );
        let cb: CallbackId = self.window_tx.alloc_id();
        self.window_tx.request(
            MessageBuilder::new(self.surface, SurfaceRequest::Frame).new_id(cb),
        );
        self.frame_cb = Some(cb);
        self.window_tx.request(MessageBuilder::new(self.surface, SurfaceRequest::Commit));

        self.frame_ready = false;
        self.dirty = false;
        let _ = stride;
        Ok(())
    }

    fn resize(&mut self, w: i32, h: i32) -> Result<()> {
        self.shm_mem = ShmBuffer::new(w as usize, h as usize, 2)
            .map_err(|e| Error::Conn(ConnError::Sys(e)))?;
        let stride = w * 4;

        // Route the whole pool+buffer sequence through the `window_tx` MPSC ring
        // so it stays correctly ordered with any already-queued present
        // requests (Attach/Damage/Frame/Commit). `create_pool` carries the shm
        // fd, so we `dup_fd` it and hand ownership to the ring (the original fd
        // stays open for our mmap).
        let pool: PoolId = self.window_tx.alloc_id();
        let dup = self
            .shm_mem
            .dup_fd()
            .map_err(|e| Error::Conn(ConnError::Io(e)))?;
        let pool_msg = MessageBuilder::new(self.shm, ShmRequest::CreatePool)
            .new_id(pool)
            .i32(self.shm_mem.size_bytes() as i32)
            .finish();
        self.window_tx.send_with_fds(pool_msg, vec![dup]);

        for (i, frame) in self.frames.iter_mut().enumerate() {
            let buffer: BufferId = self.window_tx.alloc_id();
            self.window_tx.request(
                MessageBuilder::new(pool, ShmPoolRequest::CreateBuffer)
                    .new_id(buffer)
                    .i32(self.shm_mem.frame_offset(i) as i32)
                    .i32(w)
                    .i32(h)
                    .i32(stride)
                    .u32(Format::Xrgb8888.code()),
            );
            frame.buffer = buffer;
            frame.free = true;
        }
        self.width = w;
        self.height = h;
        self.backbuffer = vec![render::DEFAULT_BG_XRGB; (w * h) as usize];
        self.dirty = true;
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

fn create_frames(
    sender: &mut Sender,
    shm: ShmId,
    width: i32,
    height: i32,
) -> Result<([Frame; 2], ShmBuffer)> {
    let shm_mem = ShmBuffer::new(width as usize, height as usize, 2)
        .map_err(|e| Error::Conn(ConnError::Sys(e)))?;
    let stride = width * 4;
    let pool: PoolId = sender.alloc_id();
    let msg = MessageBuilder::new(shm, ShmRequest::CreatePool)
        .new_id(pool)
        .i32(shm_mem.size_bytes() as i32)
        .finish();
    sender.send_with_fds(msg, &[shm_mem.fd()])?;
    let mut frames = [Frame { buffer: BufferId(0), free: true }; 2];
    for (i, slot) in frames.iter_mut().enumerate() {
        let buffer: BufferId = sender.alloc_id();
        sender.request(
            MessageBuilder::new(pool, ShmPoolRequest::CreateBuffer)
                .new_id(buffer)
                .i32(shm_mem.frame_offset(i) as i32)
                .i32(width)
                .i32(height)
                .i32(stride)
                .u32(Format::Xrgb8888.code()),
        )?;
        slot.buffer = buffer;
    }
    Ok((frames, shm_mem))
}

fn create_cursor(
    sender: &mut Sender,
    shm: ShmId,
    compositor: CompositorId,
) -> Result<cursor::Cursor> {
    let cs = cursor::CURSOR_SIZE as usize;
    let mut cshm = ShmBuffer::new(cs, cs, 1).map_err(|e| Error::Conn(ConnError::Sys(e)))?;
    cursor::write_left_ptr(cshm.pixels(0));
    let cursor_pool: PoolId = sender.alloc_id();
    let msg = MessageBuilder::new(shm, ShmRequest::CreatePool)
        .new_id(cursor_pool)
        .i32(cshm.size_bytes() as i32)
        .finish();
    sender.send_with_fds(msg, &[cshm.fd()])?;
    let cursor_surface: SurfaceId = sender.alloc_id();
    sender.request(
        MessageBuilder::new(compositor, CompositorRequest::CreateSurface).new_id(cursor_surface),
    )?;
    let cursor_buffer: BufferId = sender.alloc_id();
    sender.request(
        MessageBuilder::new(cursor_pool, ShmPoolRequest::CreateBuffer)
            .new_id(cursor_buffer)
            .i32(0)
            .i32(cursor::CURSOR_SIZE)
            .i32(cursor::CURSOR_SIZE)
            .i32(cursor::CURSOR_SIZE * 4)
            .u32(Format::Argb8888.code()),
    )?;
    sender.request(
        MessageBuilder::new(cursor_surface, SurfaceRequest::Attach)
            .object(cursor_buffer)
            .i32(0)
            .i32(0),
    )?;
    sender.request(MessageBuilder::new(cursor_surface, SurfaceRequest::Commit))?;
    Ok(cursor::Cursor { surface: cursor_surface, buffer: cursor_buffer, shm: cshm })
}

fn bind_globals(
    sender: &mut Sender,
    events: &mut Consumer<Event>,
) -> Result<(CompositorId, ShmId, WmBaseId, SeatId)> {
    let registry: RegistryId = sender.alloc_id();
    sender.request(
        MessageBuilder::new(DisplayId(1), DisplayRequest::GetRegistry).new_id(registry),
    )?;

    let mut compositor: Option<CompositorId> = None;
    let mut shm: Option<ShmId> = None;
    let mut wm_base: Option<WmBaseId> = None;
    let mut seat: Option<SeatId> = None;

    while compositor.is_none() || shm.is_none() || wm_base.is_none() || seat.is_none() {
        let event = events.pop_block().ok_or(Error::ReaderStopped)?;
        let Event::Bytes(bytes) = event else { continue };
        let Some((mut ev, _)) = WireEvent::parse(&bytes) else { continue };

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
                    compositor = Some(bind(sender, registry, name, &interface, v)?);
                }
                s if s == proto::WL_SHM.name && shm.is_none() => {
                    let v = proto::WL_SHM.version.min(version);
                    shm = Some(bind(sender, registry, name, &interface, v)?);
                }
                s if s == proto::XDG_WM_BASE.name && wm_base.is_none() => {
                    let v = proto::XDG_WM_BASE.version.min(version);
                    wm_base = Some(bind(sender, registry, name, &interface, v)?);
                }
                s if s == proto::WL_SEAT.name && seat.is_none() => {
                    let v = proto::WL_SEAT.version.min(version);
                    seat = Some(bind(sender, registry, name, &interface, v)?);
                }
                _ => {}
            }
        }
    }
    Ok((
        compositor.ok_or(Error::ReaderStopped)?,
        shm.ok_or(Error::ReaderStopped)?,
        wm_base.ok_or(Error::ReaderStopped)?,
        seat.ok_or(Error::ReaderStopped)?,
    ))
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



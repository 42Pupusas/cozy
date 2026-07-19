//! Role-agnostic Wayland surface plumbing shared by every window this crate
//! (and its dependents) opens, regardless of shell role.
//!
//! Both `TermWindow` (xdg-shell toplevel) and `statusbar::BarWindow`
//! (wlr-layer-shell) need the exact same low-level scaffolding beneath their
//! role-specific surface: a double-buffered `wl_shm` pool, the output-scale
//! probe, one-off registry `bind`s, and background packing. Those pieces are
//! genuinely identical between the two -- they touch no toplevel/layer state
//! -- so they live here once instead of being copy-pasted per window.
//!
//! What is *not* here is anything role-specific: registry `discover` (each
//! role binds a different shell global -- `xdg_wm_base` vs
//! `zwlr_layer_shell_v1` -- plus its own extras like `wl_data_device_manager`),
//! and the configure/ack/present handshake. Those stay on each window.

use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::spsc::{Consumer, RingBuffer};

use crate::conn::{Conn, ConnError, Sender, WindowSender};
use crate::draw::Color as DrawColor;
use crate::error::{Error, Result};
use crate::event::RawEvent;
use crate::id::{BufferId, CallbackId, DisplayId, ObjectId, OutputId, PoolId, RegistryId, ShmId};
use crate::protocol::{
    BufferEvent, CallbackEvent, DisplayEvent, DisplayRequest, Format, OutputEvent, RegistryRequest,
    ShmPoolRequest, ShmRequest,
};
use crate::sys::{RepeatTimer, ShmBuffer};
use crate::wire::{MessageBuilder, WireEvent};

/// One of the two `wl_buffer`s carved out of a window's shm pool, plus
/// whether the compositor currently holds it (`free == false` between
/// `attach`/`commit` and the matching `wl_buffer.release`).
///
/// Both windows double-buffer: they present into whichever `Frame` is
/// `free`, mark it busy, and flip `free` back on release. The struct is
/// deliberately tiny and `Copy` so it lives inline in a `[Frame; 2]`.
#[derive(Copy, Clone, Debug)]
pub struct Frame {
    pub buffer: BufferId,
    pub free: bool,
}

impl Frame {
    /// Allocate a double-buffered shm pool and the two [`Frame`] buffers
    /// backed by it.
    ///
    /// Created exactly once per window, at construction: the `wl_shm_pool` is
    /// a factory object that later resizes reuse in place (see each window's
    /// `resize`), never a second `wl_shm.create_pool` -- some compositors
    /// reject that after a surface's first `ack_configure`/commit round-trip.
    pub fn create_pair(
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

/// Read `output`'s advertised buffer scale via `wl_display.sync`: send a
/// `wl_display.sync` request (which the server answers with a
/// `wl_callback.done` only after every request queued before it has been
/// fully processed and every event that follows from them has been sent),
/// and collect `wl_output.scale` events until that `done` arrives.
///
/// This guarantees we've seen every `scale` event the compositor is ever
/// going to send for this output before we act on it -- unlike blocking on
/// the very first event, which could race ahead of `scale` if the compositor
/// happens to send `geometry`/`mode` first. Returns `1` (no scaling) if the
/// output never reports a scale (some compositors only send it when it's
/// non-default) or times out.
#[must_use]
pub fn output_scale(sender: &mut Sender, events: &mut Consumer<RawEvent>, output: OutputId) -> i32 {
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
pub fn bind<T: ObjectId>(
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

/// Pack a background color at `bg_alpha`: plain XRGB8888 when fully opaque
/// (byte-identical to the pre-transparency path), or premultiplied ARGB8888
/// otherwise. The buffer format (`Xrgb8888` vs `Argb8888`) a window
/// negotiates follows the same `bg_alpha == 255` split.
#[must_use]
pub fn pack_bg(bg: DrawColor, bg_alpha: u8) -> u32 {
    if bg_alpha == 255 {
        bg.to_xrgb8888()
    } else {
        bg.premultiplied_argb8888(bg_alpha)
    }
}

/// The connection scaffolding every window stands up identically: an open
/// [`Conn`], the reader half running on its own named thread pushing
/// [`RawEvent`]s into an SPSC ring, and the (disarmed) key-repeat timer.
///
/// Returns the main-thread [`Sender`], the [`RepeatTimer`], the event
/// [`Consumer`] the window drains each tick, and the reader `JoinHandle`.
/// `ring_cap` sizes the event ring (the terminal runs deeper than the bar);
/// `thread_name` labels the reader thread and prefixes its error log so a
/// crash is attributable to the right window. This is the byte-for-byte
/// common prologue of `TermWindow::new`/`BarWindow::new` -- everything from
/// `Conn::connect` through spawning the reader -- lifted out so neither
/// hand-rolls the thread + ring wiring.
type Bootstrap = (
    Sender,
    RepeatTimer,
    Consumer<RawEvent>,
    std::thread::JoinHandle<Result<()>>,
);

pub fn connect(
    ring_cap: usize,
    thread_name: &'static str,
    wake: &waker::Waker,
) -> Result<Bootstrap> {
    let (sender, reader, repeat_timer) = Conn::connect()?;
    let (producer, events) = RingBuffer::<RawEvent>::new(Capacity::exact(ring_cap)).split();
    // The reader signals `wake` after pushing events so the UI thread -- which
    // polls its rings rather than blocking on this one -- unblocks from `wait`.
    let reader_wake = wake.clone();
    let reader_handle = std::thread::Builder::new()
        .name(thread_name.to_string())
        .spawn(move || {
            let r = reader.run(producer, Some(reader_wake)).map_err(Error::Conn);
            if let Err(e) = &r {
                eprintln!("{thread_name}: reader error: {e}");
            }
            r
        })
        .map_err(Error::ReaderSpawn)?;
    Ok((sender, repeat_timer, events, reader_handle))
}

/// What [`route_common`] did with a `RawEvent`.
pub enum Routed {
    /// Fully handled here (id recycled, frame freed, or present re-armed);
    /// the window loop can move to the next event.
    Handled,
    /// A fatal `wl_display.error` (or an unparseable message): the caller
    /// should propagate this and tear down.
    Fatal(Error),
    /// Not a shared event (pointer, configure, seat, ...); the caller
    /// handles it role-specifically.
    Passthrough,
}

/// Route the connection-level events every window handles *identically*, so
/// neither `handle_event` re-implements them:
///
/// * `wl_display.error` -> [`Routed::Fatal`]; `wl_display.delete_id` recycles
///   the id back into `window_tx`'s free-list.
/// * `wl_buffer.release` -> marks that [`Frame`] free again.
/// * the frame `wl_callback.done` -> clears `frame_cb` and re-arms
///   `frame_ready` for the next present.
///
/// Anything else -- including `RawEvent::Fd`/`RawEvent::Repeat`, which only
/// one window cares about -- returns [`Routed::Passthrough`] untouched, so the
/// caller keeps full control of its role-specific dispatch. The `&mut` params
/// are exactly the shared surface state (no terminal grid / bar module state
/// leaks in), keeping the borrow tight.
pub fn route_common(
    ev: &RawEvent,
    window_tx: &mut WindowSender,
    frames: &mut [Frame],
    frame_ready: &mut bool,
    frame_cb: &mut Option<CallbackId>,
) -> Routed {
    // wl_display events arrive as raw bytes (the reader routes object 1 here).
    if let RawEvent::Bytes(bytes) = ev {
        let Some((mut wire, _)) = WireEvent::parse(bytes) else {
            return Routed::Fatal(Error::ReaderStopped);
        };
        if wire.object_id == DisplayId(1).raw() {
            match DisplayEvent::from_opcode(wire.opcode) {
                Some(DisplayEvent::Error) => {
                    return Routed::Fatal(Error::Protocol {
                        object: wire.u32(),
                        code: wire.u32(),
                        message: wire.string(),
                    });
                }
                Some(DisplayEvent::DeleteId) => {
                    window_tx.recycle_id(wire.u32());
                    return Routed::Handled;
                }
                None => {}
            }
        }
        // Any other Bytes event is consumed here too (both windows `return`
        // after their display block); nothing downstream reads them.
        return Routed::Handled;
    }

    // wl_buffer.release: the compositor is done reading that frame.
    if let Some(frame) = frames.iter_mut().find(|f| ev.is(f.buffer)) {
        if ev.event::<BufferEvent>() == Some(BufferEvent::Release) {
            frame.free = true;
        }
        return Routed::Handled;
    }

    // wl_callback.done: the frame we asked to be notified about was shown.
    if let Some(cb) = *frame_cb {
        if ev.is(cb) && ev.event::<CallbackEvent>() == Some(CallbackEvent::Done) {
            *frame_ready = true;
            *frame_cb = None;
            return Routed::Handled;
        }
    }

    Routed::Passthrough
}

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

use crate::conn::{ConnError, Sender, WaylandConn, WindowSender};
use crate::draw::Color as DrawColor;
use crate::error::{PlatformError, Result};
use crate::event::RawEvent;
use crate::id::{
    BufferId, CallbackId, DisplayId, ObjectId, OutputId, PoolId, RegistryId, ShmId, SurfaceId,
};
use crate::protocol::{
    BufferEvent, CallbackEvent, DisplayEvent, DisplayRequest, Format, OutputEvent, RegistryEvent,
    RegistryRequest, ShmPoolRequest, ShmRequest, SurfaceRequest,
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
        Self::create_set::<2>(sender, shm, width, height, format)
    }

    /// [`Self::create_pair`] for a surface opened after the event loop has
    /// taken the connection: the pool fd is duplicated onto the request ring.
    pub fn create_pair_queued(
        tx: &mut WindowSender,
        shm: ShmId,
        width: i32,
        height: i32,
        format: Format,
    ) -> Result<([Self; 2], ShmBuffer, PoolId)> {
        let shm_mem = ShmBuffer::new(
            usize::try_from(width).unwrap_or(0),
            usize::try_from(height).unwrap_or(0),
            2,
        )
        .map_err(|e| PlatformError::Conn(ConnError::Sys(e)))?;
        let pool_fd = shm_mem
            .dup_fd()
            .map_err(|e| PlatformError::Conn(ConnError::Io(e)))?;
        let pool: PoolId = tx.alloc_id();
        let msg = MessageBuilder::new(shm, ShmRequest::CreatePool)
            .new_id(pool)
            .i32(i32::try_from(shm_mem.size_bytes()).unwrap_or(i32::MAX))
            .finish();
        tx.send_with_fds(msg, vec![pool_fd]);
        let mut frames = [Self {
            buffer: BufferId(0),
            free: true,
        }; 2];
        for (i, slot) in frames.iter_mut().enumerate() {
            let buffer: BufferId = tx.alloc_id();
            tx.request(
                MessageBuilder::new(pool, ShmPoolRequest::CreateBuffer)
                    .new_id(buffer)
                    .i32(i32::try_from(shm_mem.frame_offset(i)).unwrap_or(i32::MAX))
                    .i32(width)
                    .i32(height)
                    .i32(width * 4)
                    .u32(format.code()),
            );
            slot.buffer = buffer;
        }
        Ok((frames, shm_mem, pool))
    }

    pub fn create_single(
        sender: &mut Sender,
        shm: ShmId,
        width: i32,
        height: i32,
        format: Format,
    ) -> Result<(Self, ShmBuffer, PoolId)> {
        let ([frame], shm_mem, pool) = Self::create_set::<1>(sender, shm, width, height, format)?;
        Ok((frame, shm_mem, pool))
    }

    fn create_set<const COUNT: usize>(
        sender: &mut Sender,
        shm: ShmId,
        width: i32,
        height: i32,
        format: Format,
    ) -> Result<([Self; COUNT], ShmBuffer, PoolId)> {
        let shm_mem = ShmBuffer::new(
            usize::try_from(width).unwrap_or(0),
            usize::try_from(height).unwrap_or(0),
            COUNT,
        )
        .map_err(|e| PlatformError::Conn(ConnError::Sys(e)))?;
        let stride = width * 4;
        let pool: PoolId = sender.alloc_id();
        let msg = MessageBuilder::new(shm, ShmRequest::CreatePool)
            .new_id(pool)
            .i32(i32::try_from(shm_mem.size_bytes()).unwrap_or(i32::MAX))
            .finish();
        sender.send_with_fds(&msg, &[shm_mem.fd()])?;
        let mut frames = [Self {
            buffer: BufferId(0),
            free: true,
        }; COUNT];
        for (i, slot) in frames.iter_mut().enumerate() {
            let buffer: BufferId = sender.alloc_id();
            sender.request(
                MessageBuilder::new(pool, ShmPoolRequest::CreateBuffer)
                    .new_id(buffer)
                    .i32(i32::try_from(shm_mem.frame_offset(i)).unwrap_or(i32::MAX))
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

/// The connection scaffolding every window stands up identically: an open
/// [`WaylandConn`], the reader half running on its own named thread pushing
/// [`RawEvent`]s into an SPSC ring, and the (disarmed) key-repeat timer.
type Bootstrap = (
    Sender,
    RepeatTimer,
    Consumer<RawEvent>,
    std::thread::JoinHandle<Result<()>>,
);

/// Role-agnostic Wayland surface bootstrap steps shared by every window.
///
/// See the module doc for why these live here once instead of being
/// copy-pasted per window. A marker type (like [`crate::volume::Volume`] in
/// the `statusbar` crate): every method is a one-off protocol exchange or a
/// pure computation, there's no state to hold between calls.
pub struct SurfaceBootstrap;

/// Receives `wl_registry.global` events during a window's initial discovery.
///
/// A shell role owns the concrete collector because it alone knows which
/// globals are required versus optional. `RegistryDiscovery` owns the shared
/// wire loop: every client must request a registry, reject display errors, and
/// feed globals to its collector until the required set is available.
pub trait GlobalCollector {
    /// Whether the client now has every global it needs to continue startup.
    fn is_complete(&self) -> bool;

    /// Inspect and, when appropriate, bind one advertised global.
    fn bind_global(
        &mut self,
        sender: &mut Sender,
        registry: RegistryId,
        name: u32,
        interface: &str,
        version: u32,
    ) -> Result<()>;

    /// Bind one global into `slot` if it's still empty, negotiating the
    /// version as `known_version.min(advertised_version)`.
    ///
    /// Every shell role's `bind_global` match has the same per-arm shape --
    /// "already bound? else negotiate a version and bind" -- because that
    /// shape has nothing to do with which role or interface it's binding.
    /// It used to be re-typed as a private associated fn in each of five
    /// `GlobalsBuilder`s (`platform::window`, `platform::toplevel::globals`,
    /// `statusbar::bar_window`, `launcher::launcher_window`,
    /// `wallpaper::main`); living here once means a role only ever writes
    /// the part that *is* role-specific -- which interface, which slot.
    fn bind_if_missing<T: ObjectId>(
        slot: &mut Option<T>,
        sender: &mut Sender,
        registry: RegistryId,
        name: u32,
        interface: &str,
        advertised_version: u32,
        known_version: u32,
    ) -> Result<()> {
        if slot.is_some() {
            return Ok(());
        }
        let v = known_version.min(advertised_version);
        *slot = Some(SurfaceBootstrap::bind(
            sender, registry, name, interface, v,
        )?);
        Ok(())
    }
}

/// The role-neutral registry startup transaction.
///
/// This marker owns no connection state; it makes the protocol exchange a
/// named method rather than another copy-pasted `while` loop in each shell
/// role.
pub struct RegistryDiscovery;

impl RegistryDiscovery {
    /// Request `wl_registry`, pump events until `collector` has its required
    /// globals, and return the registry object used for those bindings.
    pub fn collect<C: GlobalCollector>(
        sender: &mut Sender,
        events: &mut Consumer<RawEvent>,
        collector: &mut C,
    ) -> Result<RegistryId> {
        let registry: RegistryId = sender.alloc_id();
        sender.request(
            MessageBuilder::new(DisplayId(1), DisplayRequest::GetRegistry).new_id(registry),
        )?;

        // The initial registry burst has neither a fixed order nor a terminal
        // event. Waiting only for the caller's required set is therefore the
        // only bounded startup condition.
        while !collector.is_complete() {
            let event = events.pop_block().ok_or(PlatformError::ReaderStopped)?;
            let RawEvent::Bytes(bytes) = event else {
                continue;
            };
            let Some((mut ev, _)) = WireEvent::parse(&bytes) else {
                continue;
            };

            if ev.object_id == DisplayId(1).raw()
                && DisplayEvent::from_opcode(ev.opcode) == Some(DisplayEvent::Error)
            {
                return Err(PlatformError::Protocol {
                    object: ev.u32(),
                    code: ev.u32(),
                    message: ev.string(),
                });
            }

            if ev.object_id == registry.raw()
                && RegistryEvent::from_opcode(ev.opcode) == Some(RegistryEvent::Global)
            {
                collector.bind_global(sender, registry, ev.u32(), &ev.string(), ev.u32())?;
            }
        }
        Ok(registry)
    }

    /// Finish the initial registry burst with a `wl_display.sync` round-trip.
    ///
    /// [`Self::collect`] may stop as soon as the required singleton globals
    /// arrive, but a wallpaper needs *every* already-advertised `wl_output`,
    /// including ones that happen later in that same unordered burst. The
    /// callback is ordered after `get_registry`, so its `done` proves every
    /// global generated by that request has been delivered to `collector`.
    pub fn collect_through_sync<C: GlobalCollector>(
        sender: &mut Sender,
        events: &mut Consumer<RawEvent>,
        registry: RegistryId,
        collector: &mut C,
    ) -> Result<()> {
        let callback: CallbackId = sender.alloc_id();
        sender.request(MessageBuilder::new(DisplayId(1), DisplayRequest::Sync).new_id(callback))?;
        loop {
            let event = events.pop_block().ok_or(PlatformError::ReaderStopped)?;
            match event {
                RawEvent::Bytes(bytes) => {
                    let Some((mut ev, _)) = WireEvent::parse(&bytes) else {
                        continue;
                    };
                    if ev.object_id == DisplayId(1).raw()
                        && DisplayEvent::from_opcode(ev.opcode) == Some(DisplayEvent::Error)
                    {
                        return Err(PlatformError::Protocol {
                            object: ev.u32(),
                            code: ev.u32(),
                            message: ev.string(),
                        });
                    }
                    if ev.object_id == registry.raw()
                        && RegistryEvent::from_opcode(ev.opcode) == Some(RegistryEvent::Global)
                    {
                        collector.bind_global(
                            sender,
                            registry,
                            ev.u32(),
                            &ev.string(),
                            ev.u32(),
                        )?;
                    }
                }
                RawEvent::Wire {
                    object_id, opcode, ..
                } if object_id == callback.raw()
                    && CallbackEvent::from_opcode(opcode) == Some(CallbackEvent::Done) =>
                {
                    return Ok(());
                }
                RawEvent::Wire { .. } | RawEvent::Repeat | RawEvent::Fd(_) => {}
            }
        }
    }
}

/// The common wire transaction that submits one fully painted shm frame.
///
/// Painting and buffer copying remain caller-owned: different window roles
/// have different renderers. Once a slot is ready, though, all roles must send
/// this exact Wayland request sequence and update the same [`PresentState`].
pub struct SurfacePresenter;

impl SurfacePresenter {
    /// Attach `slot`, damage its complete `width` × `height` area, request a
    /// frame callback, commit, and arm `present` to await that callback.
    pub fn commit_full_frame(
        present: &mut crate::present::PresentState,
        window_tx: &mut WindowSender,
        surface: SurfaceId,
        slot: usize,
        width: i32,
        height: i32,
    ) {
        let buffer = present.frame_buffer(slot);
        window_tx.request(
            MessageBuilder::new(surface, SurfaceRequest::Attach)
                .object(buffer)
                .i32(0)
                .i32(0),
        );
        window_tx.request(
            MessageBuilder::new(surface, SurfaceRequest::Damage)
                .i32(0)
                .i32(0)
                .i32(width)
                .i32(height),
        );
        let cb: CallbackId = window_tx.alloc_id();
        window_tx.request(MessageBuilder::new(surface, SurfaceRequest::Frame).new_id(cb));
        window_tx.request(MessageBuilder::new(surface, SurfaceRequest::Commit));
        present.finish_present(cb);
    }

    /// Attach `slot`, damage the buffer-pixel rects in `damage` (the whole
    /// `size` when it is full), request a frame callback, commit, and arm
    /// `present` to await that callback.
    pub fn commit_frame(
        present: &mut crate::present::PresentState,
        window_tx: &mut WindowSender,
        surface: SurfaceId,
        slot: usize,
        size: crate::toplevel::PhysicalSize,
        damage: &crate::draw::Damage,
    ) {
        let buffer = present.frame_buffer(slot);
        window_tx.request(
            MessageBuilder::new(surface, SurfaceRequest::Attach)
                .object(buffer)
                .i32(0)
                .i32(0),
        );
        let whole = crate::draw::Rect::new(0, 0, size.width, size.height);
        let rects = if damage.is_full() {
            std::slice::from_ref(&whole)
        } else {
            damage.rects()
        };
        for rect in rects {
            window_tx.request(
                MessageBuilder::new(surface, SurfaceRequest::DamageBuffer)
                    .i32(rect.x)
                    .i32(rect.y)
                    .i32(rect.w)
                    .i32(rect.h),
            );
        }
        let cb: CallbackId = window_tx.alloc_id();
        window_tx.request(MessageBuilder::new(surface, SurfaceRequest::Frame).new_id(cb));
        window_tx.request(MessageBuilder::new(surface, SurfaceRequest::Commit));
        present.finish_present(cb);
    }
}

impl SurfaceBootstrap {
    /// Read `output`'s advertised buffer scale via `wl_display.sync`: send a
    /// `wl_display.sync` request (which the server answers with a
    /// `wl_callback.done` only after every request queued before it has been
    /// fully processed and every event that follows from them has been
    /// sent), and collect `wl_output.scale` events until that `done`
    /// arrives.
    ///
    /// This guarantees we've seen every `scale` event the compositor is ever
    /// going to send for this output before we act on it -- unlike blocking
    /// on the very first event, which could race ahead of `scale` if the
    /// compositor happens to send `geometry`/`mode` first. Returns `1` (no
    /// scaling) if the output never reports a scale (some compositors only
    /// send it when it's non-default) or times out.
    #[must_use]
    pub fn output_scale(
        sender: &mut Sender,
        events: &mut Consumer<RawEvent>,
        output: OutputId,
    ) -> i32 {
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
                // Neither occurs during output-scale probing.
                RawEvent::Repeat | RawEvent::Fd(_) => {}
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

    /// Read `output`'s logical size, preserving unrelated events.
    ///
    /// Delegates to [`crate::output_probe::OutputProbe`]: callers must replay
    /// its spill, because a plain drop-what-you-don't-want probe loop can eat
    /// the one-shot `wl_seat.capabilities` and leave the client with no input.
    #[must_use]
    pub fn output_probe(
        sender: &mut Sender,
        events: &mut Consumer<RawEvent>,
        output: OutputId,
    ) -> crate::output_probe::OutputProbe {
        crate::output_probe::OutputProbe::read(sender, events, output)
    }

    /// Pack a background color at `bg_alpha`: plain XRGB8888 when fully
    /// opaque (byte-identical to the pre-transparency path), or
    /// premultiplied ARGB8888 otherwise. The buffer format (`Xrgb8888` vs
    /// `Argb8888`) a window negotiates follows the same `bg_alpha == 255`
    /// split.
    #[must_use]
    pub fn pack_bg(bg: DrawColor, bg_alpha: u8) -> u32 {
        if bg_alpha == 255 {
            bg.to_xrgb8888()
        } else {
            bg.premultiplied_argb8888(bg_alpha)
        }
    }

    /// The connection scaffolding every window stands up identically: an
    /// open [`WaylandConn`], the reader half running on its own named
    /// thread pushing [`RawEvent`]s into an SPSC ring, and the (disarmed)
    /// key-repeat timer.
    ///
    /// Returns the main-thread [`Sender`], the [`RepeatTimer`], the event
    /// [`Consumer`] the window drains each tick, and the reader
    /// `JoinHandle`. `ring_cap` sizes the event ring (the terminal runs
    /// deeper than the bar); `thread_name` labels the reader thread and
    /// prefixes its error log so a crash is attributable to the right
    /// window. This is the byte-for-byte common prologue of
    /// `TermWindow::new`/`BarWindow::new` -- everything from
    /// `WaylandConn::connect` through spawning the reader -- lifted out so
    /// neither hand-rolls the thread + ring wiring.
    pub fn connect(
        ring_cap: usize,
        thread_name: &'static str,
        wake: &waker::Waker,
    ) -> Result<Bootstrap> {
        let (sender, reader, repeat_timer) = WaylandConn::connect()?;
        let (producer, events) = RingBuffer::<RawEvent>::new(Capacity::exact(ring_cap)).split();
        // The reader signals `wake` after pushing events so the UI thread --
        // which polls its rings rather than blocking on this one -- unblocks
        // from `wait`.
        let reader_wake = wake.clone();
        let reader_handle = std::thread::Builder::new()
            .name(thread_name.to_string())
            .spawn(move || {
                let _exit = ReaderExitSignal(reader_wake.clone());
                let r = reader
                    .run(&producer, Some(&reader_wake))
                    .map_err(PlatformError::Conn);
                if let Err(e) = &r {
                    eprintln!("{thread_name}: reader error: {e}");
                }
                r
            })
            .map_err(PlatformError::ReaderSpawn)?;
        Ok((sender, repeat_timer, events, reader_handle))
    }
}

/// Wakes the UI loop when the reader thread ends by any route (clean close,
/// error, or panic), so a loop that blocks without a timeout learns of it.
struct ReaderExitSignal(waker::Waker);

impl Drop for ReaderExitSignal {
    fn drop(&mut self) {
        self.0.signal();
    }
}

/// What [`route_common`] did with a `RawEvent`.
pub enum Routed {
    /// Fully handled here (id recycled, frame freed, or present re-armed);
    /// the window loop can move to the next event.
    Handled,
    /// A fatal `wl_display.error` (or an unparseable message): the caller
    /// should propagate this and tear down.
    Fatal(PlatformError),
    /// Not a shared event (pointer, configure, seat, ...); the caller
    /// handles it role-specifically.
    Passthrough,
}

impl SurfaceBootstrap {
    /// Route the connection-level events every window handles *identically*,
    /// so neither `handle_event` re-implements them:
    ///
    /// * `wl_display.error` -> [`Routed::Fatal`]; `wl_display.delete_id`
    ///   recycles the id back into `window_tx`'s free-list.
    /// * `wl_buffer.release` -> marks that [`Frame`] free again.
    /// * the frame `wl_callback.done` -> clears `frame_cb` and re-arms
    ///   `frame_ready` for the next present.
    ///
    /// Anything else -- including `RawEvent::Fd`/`RawEvent::Repeat`, which
    /// only one window cares about -- returns [`Routed::Passthrough`]
    /// untouched, so the caller keeps full control of its role-specific
    /// dispatch. The `&mut` params are exactly the shared surface state (no
    /// terminal grid / bar module state leaks in), keeping the borrow
    /// tight.
    pub fn route_common(
        ev: &RawEvent,
        window_tx: &mut WindowSender,
        frames: &mut [Frame],
        frame_ready: &mut bool,
        frame_cb: &mut Option<CallbackId>,
    ) -> Routed {
        // wl_display events arrive as raw bytes (the reader routes object 1
        // here).
        if let RawEvent::Bytes(bytes) = ev {
            let Some((mut wire, _)) = WireEvent::parse(bytes) else {
                return Routed::Fatal(PlatformError::ReaderStopped);
            };
            if wire.object_id == DisplayId(1).raw() {
                match DisplayEvent::from_opcode(wire.opcode) {
                    Some(DisplayEvent::Error) => {
                        return Routed::Fatal(PlatformError::Protocol {
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
            // Any other Bytes event is consumed here too (both windows
            // `return` after their display block); nothing downstream reads
            // them.
            return Routed::Handled;
        }

        // wl_buffer.release: the compositor is done reading that frame.
        if let Some(frame) = frames.iter_mut().find(|f| ev.is(f.buffer)) {
            if ev.event::<BufferEvent>() == Some(BufferEvent::Release) {
                frame.free = true;
            }
            return Routed::Handled;
        }

        // wl_callback.done: the frame we asked to be notified about was
        // shown.
        if let Some(cb) = *frame_cb
            && ev.is(cb)
            && ev.event::<CallbackEvent>() == Some(CallbackEvent::Done)
        {
            *frame_ready = true;
            *frame_cb = None;
            return Routed::Handled;
        }

        Routed::Passthrough
    }
}

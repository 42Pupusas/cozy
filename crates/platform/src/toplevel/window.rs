//! [`Toplevel`] -- a generic, content-agnostic application window.
//!
//! The owner supplies a [`Painter`]; this type supplies a correctly mapped,
//! correctly resized surface and calls back whenever pixels are needed. It
//! knows nothing about grids, rows, columns, file listings, or widgets.

use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc;
use quetzalcoatl::spsc::Consumer;

use crate::conn::{Sender, WindowSender};
use crate::draw::{Canvas, Color};
use crate::error::{Error, Result};
use crate::event::RawEvent;
use crate::id::SeatId;
use crate::present::{PresentAttempt, PresentState};
use crate::protocol::{
    Format, SeatCapability, SeatEvent, SeatRequest, ToplevelEvent, WmBaseEvent, XdgSurfaceEvent,
};
use crate::surface::{Frame, RegistryDiscovery, Routed, SurfaceBootstrap, SurfacePresenter};
use crate::toplevel::geometry::{Geometry, LogicalSize, PhysicalSize};
use crate::toplevel::globals::{ToplevelGlobals, ToplevelGlobalsBuilder};
use crate::toplevel::lifecycle::Lifecycle;
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
    reader: Option<std::thread::JoinHandle<Result<()>>>,
    reader_health: crate::ReaderHealth,
    wake: waker::Waker,

    seat: SeatId,
    wm_base: crate::id::WmBaseId,
    shell: ShellSurface,

    geometry: Geometry,
    lifecycle: Lifecycle,
    present: PresentState,

    /// Persistent pixel buffer the painter draws into, copied into whichever
    /// shm slot presents next. Kept separate from the shm slots so a paint
    /// never races the compositor reading a buffer it still holds.
    backbuffer: Vec<u32>,

    /// Events queued for the owner to drain.
    queue: std::collections::VecDeque<WindowEvent>,
}

impl Toplevel {
    /// Open a window of `size` physical pixels titled `title`.
    ///
    /// `app_id` should be stable for the process -- compositors match window
    /// rules on it, unlike the title, which apps rewrite at runtime.
    pub fn new(
        title: &str,
        app_id: &str,
        size: LogicalSize,
        wake: waker::Waker,
        background: Color,
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
        } = builder.finish().ok_or(Error::ReaderStopped)?;

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

        // Opaque windows use the cheaper XRGB format the compositor never
        // has to blend.
        let (frames, shm_mem, pool) = Frame::create_pair(
            &mut sender,
            shm,
            physical.width,
            physical.height,
            Format::Xrgb8888,
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
        let window_tx = WindowSender {
            tx: req_tx,
            next_id: id_start,
            id_end,
            free_ids: Vec::new(),
        };

        let px = physical.pixel_count();
        let present = PresentState::new(
            frames,
            shm_mem,
            pool,
            Format::Xrgb8888,
            physical.height_usize().max(1),
        );

        Ok(Self {
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
            backbuffer: vec![background.to_xrgb8888(); px],
            queue: std::collections::VecDeque::new(),
        })
    }

    /// The window's current size in physical pixels.
    #[must_use]
    pub const fn size(&self) -> PhysicalSize {
        self.geometry.physical()
    }

    /// A handle the owner can use to wake this window's event loop from
    /// another thread.
    #[must_use]
    pub fn waker(&self) -> waker::Waker {
        self.wake.clone()
    }

    /// Change the window title.
    pub fn set_title(&self, title: &str) {
        self.shell.set_title(&self.window_tx, title);
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
            self.handle_event(&ev)?;
        }
        // Drain first, *then* observe: a reader that hit a protocol error
        // pushes the explaining event before exiting, and checking liveness
        // first would discard it and turn a diagnosable fault into a silent
        // one. See `ReaderHealth::observe`.
        if !self
            .reader_health
            .observe(self.events.is_closed(), true)
            .is_alive()
        {
            return Err(Error::ReaderStopped);
        }
        self.apply_resize(painter)?;
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
    fn handle_event(&mut self, ev: &RawEvent) -> Result<()> {
        match self.present.route_common(ev, &mut self.window_tx) {
            Routed::Handled => return Ok(()),
            Routed::Fatal(e) => return Err(e),
            Routed::Passthrough => {}
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

    /// Bind pointer/keyboard the first time the seat advertises them.
    ///
    /// The ids are created but not yet consumed: input handling is the next
    /// increment. Binding now keeps the seat contract in one place and
    /// means adding input later is purely additive.
    fn bind_seat_devices(&mut self, caps: u32) {
        if SeatCapability::Pointer.in_mask(caps) {
            let pointer: crate::id::PointerId = self.window_tx.alloc_id();
            self.window_tx
                .request(MessageBuilder::new(self.seat, SeatRequest::GetPointer).new_id(pointer));
        }
        if SeatCapability::Keyboard.in_mask(caps) {
            let keyboard: crate::id::KeyboardId = self.window_tx.alloc_id();
            self.window_tx
                .request(MessageBuilder::new(self.seat, SeatRequest::GetKeyboard).new_id(keyboard));
        }
    }

    /// Apply a coalesced resize: grow the shm pool, resize the backbuffer,
    /// and tell the owner.
    fn apply_resize(&mut self, painter: &impl Painter) -> Result<()> {
        let Some(size) = self.geometry.apply_pending() else {
            return Ok(());
        };
        self.present
            .resize_shm(size.width, size.height)
            .map_err(|e| Error::Conn(crate::conn::ConnError::Sys(e)))?;
        // Refill rather than merely resize: the newly exposed region would
        // otherwise show whatever stale pixels the Vec's spare capacity
        // happened to hold.
        let px = size.pixel_count();
        self.backbuffer.clear();
        self.backbuffer
            .resize(px, painter.background().to_xrgb8888());
        self.present.mark_all_stale();
        self.present.mark_dirty();
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
            let mut canvas = Canvas::new(&mut self.backbuffer, size.width, size.height);
            painter.paint(&mut canvas, size);
        }

        // Copy the finished image into the slot the compositor will read.
        let (pixels, stale) = self.present.slot_pixels_and_stale(slot);
        let n = pixels.len().min(self.backbuffer.len());
        pixels[..n].copy_from_slice(&self.backbuffer[..n]);
        for flag in stale.iter_mut() {
            *flag = false;
        }

        SurfacePresenter::commit_full_frame(
            &mut self.present,
            &mut self.window_tx,
            self.shell.surface,
            slot,
            size.width,
            size.height,
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
    /// So: send the destroy requests, flush them, then **drop the sender**
    /// (closing the socket, which the compositor sees as a disconnect and
    /// the reader sees as EOF), and only then join.
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
        // Closes the socket, so the reader's blocking recv completes and its
        // loop returns promptly instead of parking until its watchdog fires.
        drop(sender);

        // The reader's I/O result is deliberately discarded here, and only
        // here -- on a shutdown *we* initiated.
        //
        // The reader submits its `recvmsg` against the socket's raw fd
        // *number*. Dropping the `Sender` closes that number, so an
        // in-flight recv completes with `EBADF`. (A dup'd `OwnedFd` keeps
        // the underlying description alive but does not help: io_uring has
        // already captured the integer.) Short of a shutdown protocol
        // between the two threads, this error is the unavoidable and
        // *expected* signature of a clean close.
        //
        // A reader that dies mid-session is still reported: `tick()`
        // observes it via `ReaderHealth` and returns `Error::ReaderStopped`.
        reader.map_or(Ok(()), |handle| {
            // A panic is never expected, so it still propagates.
            handle.join().map_or(Err(Error::ReaderPanicked), |_| Ok(()))
        })
    }
}

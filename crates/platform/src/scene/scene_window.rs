//! [`SceneWindow`] -- an xdg-shell toplevel drawn entirely as solid
//! sections: no client-side pixel buffer at all.

use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc;
use quetzalcoatl::spsc::Consumer;

use crate::conn::{Sender, WindowRequest, WindowSender};
use crate::error::{PlatformError, Result};
use crate::event::RawEvent;
use crate::id::{CallbackId, WmBaseId};
use crate::protocol::{SurfaceRequest, ToplevelEvent, WmBaseEvent, XdgSurfaceEvent};
use crate::surface::{RegistryDiscovery, Routed, SurfaceBootstrap};
use crate::toplevel::geometry::{Geometry, LogicalSize, PhysicalSize};
use crate::toplevel::lifecycle::Lifecycle;
use crate::toplevel::shell::ShellSurface;
use crate::wire::MessageBuilder;

use super::frame_pacing::FramePacing;
use super::scene_globals::SceneGlobalsBuilder;
use super::solid_frame::SolidFrame;
use super::surface_tree::{SurfaceTree, TreeUpdate};

pub struct SceneWindow {
    sender: Sender,
    request_rx: mpsc::Consumer<WindowRequest>,
    window_tx: WindowSender,
    events: Consumer<RawEvent>,
    reader: Option<std::thread::JoinHandle<Result<()>>>,
    reader_health: crate::ReaderHealth,
    wake: waker::Waker,
    wm_base: WmBaseId,
    shell: ShellSurface,
    geometry: Geometry,
    lifecycle: Lifecycle,
    pacing: FramePacing,
    tree: SurfaceTree,
}

impl SceneWindow {
    const EVENT_RING_CAP: usize = 256;
    /// Every section change is a few requests; a full re-layout of a few
    /// hundred sections goes out in one tick.
    const REQUEST_RING_CAP: usize = 16_384;
    const ID_BLOCK: u32 = 65_536;

    pub fn new(title: &str, app_id: &str, size: LogicalSize, wake: waker::Waker) -> Result<Self> {
        let (mut sender, _repeat, mut events, reader) =
            SurfaceBootstrap::connect(Self::EVENT_RING_CAP, "scene-wayland-reader", &wake)?;
        let mut builder = SceneGlobalsBuilder::default();
        let registry = RegistryDiscovery::collect(&mut sender, &mut events, &mut builder)?;
        RegistryDiscovery::collect_through_sync(&mut sender, &mut events, registry, &mut builder)?;
        let globals = builder.finish()?;
        let scale = globals.output.map_or(1, |o| {
            SurfaceBootstrap::output_scale(&mut sender, &mut events, o)
        });
        let geometry = Geometry::new(
            PhysicalSize {
                width: size.width * scale,
                height: size.height * scale,
            },
            scale,
        );
        let shell = ShellSurface::create(
            &mut sender,
            globals.compositor,
            globals.wm_base,
            None,
            scale,
            title,
            app_id,
        )?;
        let (tx, request_rx) =
            mpsc::RingBuffer::<WindowRequest>::new(Capacity::exact(Self::REQUEST_RING_CAP)).split();
        let (next_id, id_end) = sender.alloc_id_block(Self::ID_BLOCK);
        let mut window_tx = WindowSender {
            tx,
            next_id,
            id_end,
            free_ids: Vec::new(),
        };
        let tree = SurfaceTree::new(&mut window_tx, &globals, shell.surface)?;
        Ok(Self {
            sender,
            request_rx,
            window_tx,
            events,
            reader: Some(reader),
            reader_health: crate::ReaderHealth::new(),
            wake,
            wm_base: globals.wm_base,
            shell,
            geometry,
            lifecycle: Lifecycle::new(),
            pacing: FramePacing::new(),
            tree,
        })
    }

    #[must_use]
    pub const fn size(&self) -> PhysicalSize {
        self.geometry.physical()
    }

    #[must_use]
    pub const fn is_closing(&self) -> bool {
        self.lifecycle.is_closing()
    }

    #[must_use]
    pub const fn tree(&self) -> &SurfaceTree {
        &self.tree
    }

    pub const fn request_redraw(&mut self) {
        self.pacing.mark_dirty();
    }

    /// Block until an event or a signal arrives, or `timeout_ms` passes.
    pub fn wait(&self, timeout_ms: i32) {
        if self.wake.wait(timeout_ms) {
            self.wake.drain();
        }
    }

    /// Handle pending events and, when a commit is owed and allowed, build
    /// the frame and send the sections that changed.
    pub fn tick(
        &mut self,
        build: impl FnOnce(PhysicalSize) -> SolidFrame,
    ) -> Result<Option<TreeUpdate>> {
        self.sender.drain_requests(&mut self.request_rx)?;
        while let Some(ev) = self.events.pop() {
            self.handle_event(&ev)?;
        }
        if !self
            .reader_health
            .observe(self.events.is_closed(), true)
            .is_alive()
        {
            return Err(PlatformError::ReaderStopped);
        }
        if self.geometry.apply_pending().is_some() {
            self.pacing.mark_dirty();
        }
        let update = self.maybe_commit(build)?;
        self.sender.drain_requests(&mut self.request_rx)?;
        Ok(update)
    }

    fn maybe_commit(
        &mut self,
        build: impl FnOnce(PhysicalSize) -> SolidFrame,
    ) -> Result<Option<TreeUpdate>> {
        if !self.lifecycle.can_present() || !self.pacing.can_commit() {
            return Ok(None);
        }
        let size = self.geometry.physical();
        let frame = build(size);
        let update = self.tree.apply(
            &mut self.window_tx,
            &frame,
            (size.width, size.height),
            self.geometry.scale(),
        )?;
        let callback: CallbackId = self.window_tx.alloc_id();
        self.window_tx.request(
            MessageBuilder::new(self.shell.surface, SurfaceRequest::Frame).new_id(callback),
        );
        self.window_tx.request(MessageBuilder::new(
            self.shell.surface,
            SurfaceRequest::Commit,
        ));
        self.pacing.committed(callback);
        Ok(Some(update))
    }

    fn handle_event(&mut self, ev: &RawEvent) -> Result<()> {
        match self.pacing.route(ev, &mut self.window_tx) {
            Routed::Handled => return Ok(()),
            Routed::Fatal(e) => return Err(e),
            Routed::Passthrough => {}
        }
        if ev.is(self.wm_base) && ev.event::<WmBaseEvent>() == Some(WmBaseEvent::Ping) {
            ShellSurface::pong(&self.window_tx, self.wm_base, ev.serial(0).raw());
        } else if ev.is(self.shell.xdg_surface)
            && ev.event::<XdgSurfaceEvent>() == Some(XdgSurfaceEvent::Configure)
        {
            self.lifecycle.configure_received(ev.serial(0).raw());
            if let Some(serial) = self.lifecycle.take_ack() {
                self.shell.ack_configure(&self.window_tx, serial);
                self.lifecycle.acked();
            }
            self.pacing.mark_dirty();
        } else if ev.is(self.shell.toplevel) {
            match ev.event::<ToplevelEvent>() {
                Some(ToplevelEvent::Close) => self.lifecycle.close_requested(),
                Some(ToplevelEvent::Configure) => self.geometry.propose(LogicalSize {
                    width: ev.arg_i32(0),
                    height: ev.arg_i32(1),
                }),
                None => {}
            }
        }
        Ok(())
    }

    /// Destroy every section, then the shell, then shut the socket so the
    /// reader sees EOF; see [`crate::toplevel::Toplevel::close`].
    pub fn close(self) -> Result<()> {
        let Self {
            mut sender,
            mut request_rx,
            window_tx,
            shell,
            tree,
            reader,
            ..
        } = self;
        tree.destroy(&window_tx);
        shell.destroy(&window_tx);
        let _ = sender.drain_requests(&mut request_rx);
        let _ = sender.shutdown();
        let joined = reader.map_or(Ok(()), |handle| {
            handle.join().unwrap_or(Err(PlatformError::ReaderPanicked))
        });
        drop(sender);
        joined
    }
}

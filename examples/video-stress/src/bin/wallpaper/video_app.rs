//! The event loop that drives one video background per output.
//!
//! Playback is paced by the compositor. Each surface asks for a frame
//! callback when it commits, and paints again only when that callback
//! arrives, so the clip advances at the output refresh rate. The clock
//! chooses which frame that is, so a slow paint skips frames instead of
//! playing the clip in slow motion.

use crate::video_globals::{VideoGlobals, VideoGlobalsBuilder, VideoOutput, VideoOutputs};
use crate::video_surface::VideoSurface;
use platform::PlatformError;
use platform::Result;
use platform::conn::Sender;
use platform::event::RawEvent;
use platform::id::{DisplayId, ObjectId, RegistryId};
use platform::protocol::{DisplayEvent, RegistryEvent};
use platform::surface::{RegistryDiscovery, SurfaceBootstrap};
use platform::wire::WireEvent;
use quetzalcoatl::spsc::Consumer;
use std::time::Instant;
use video_stress::{FramePack, PlaybackClock, PlaybackStats, PlayerOptions, Tick, VideoPalette};

const EVENT_RING_CAP: usize = 256;

/// Owns the connection, the outputs, and the clip every surface plays.
pub struct VideoApp {
    sender: Sender,
    events: Consumer<RawEvent>,
    #[allow(dead_code)]
    reader: std::thread::JoinHandle<Result<()>>,
    globals: VideoGlobals,
    registry: RegistryId,
    outputs: VideoOutputs,
    surfaces: Vec<VideoSurface>,
    pack: FramePack,
    clock: PlaybackClock,
    stats: PlaybackStats,
    options: PlayerOptions,
    palette: VideoPalette,
    started: Instant,
}

impl VideoApp {
    /// Connect, discover outputs, and create one surface per output.
    pub fn connect(
        wake: &waker::Waker,
        options: PlayerOptions,
        pack: FramePack,
        palette: VideoPalette,
    ) -> Result<Self> {
        let (mut sender, _repeat_timer, mut events, reader) =
            SurfaceBootstrap::connect(EVENT_RING_CAP, "video-wallpaper-reader", wake)?;
        let mut collector = VideoGlobalsBuilder::default();
        let registry = RegistryDiscovery::collect(&mut sender, &mut events, &mut collector)?;
        RegistryDiscovery::collect_through_sync(
            &mut sender,
            &mut events,
            registry,
            &mut collector,
        )?;
        let globals = collector.finish()?;
        let mut outputs = VideoOutputs::new();
        for (name, version) in collector.output_globals {
            outputs.bind_global(&mut sender, registry, name, version)?;
        }
        let rate = options.rate(pack.header().fps);
        let mut app = Self {
            sender,
            events,
            reader,
            globals,
            registry,
            outputs,
            surfaces: Vec::new(),
            pack,
            clock: PlaybackClock::start(rate),
            stats: PlaybackStats::start("wallpaper"),
            options,
            palette,
            started: Instant::now(),
        };
        for output in app.outputs.outputs().to_vec() {
            app.create_surface(output)?;
        }
        Ok(app)
    }

    /// The rate playback is paced at.
    #[must_use]
    pub fn rate(&self) -> u32 {
        self.options.rate(self.pack.header().fps)
    }

    /// Run until the clip limit is reached or the compositor disconnects.
    ///
    /// The per-output report is taken before teardown, which releases the
    /// surfaces and so forgets their sizes and frame counts.
    pub fn run(&mut self) -> Result<(PlaybackStats, String)> {
        while let Some(event) = self.events.pop_block() {
            self.handle_event(&event)?;
            if self
                .options
                .is_complete(self.stats.presented(), self.started.elapsed())
            {
                break;
            }
        }
        let report = self.surface_report();
        self.teardown()?;
        Ok((
            std::mem::replace(&mut self.stats, PlaybackStats::start("wallpaper")),
            report,
        ))
    }

    /// Dispatch one event, then paint any surface the compositor freed.
    fn handle_event(&mut self, event: &RawEvent) -> Result<()> {
        if let RawEvent::Bytes(bytes) = event {
            return self.handle_bytes(bytes);
        }
        for surface in &mut self.surfaces {
            surface.handle_event(&mut self.sender, event)?;
        }
        // A configure gives a surface its size but sends no callback, so the
        // first frame must be driven from here. Every later frame is driven
        // by the callback the previous commit armed.
        self.present_due_frame()
    }

    /// Handle the events that arrive as raw wire bytes.
    fn handle_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        let Some((mut event, _)) = WireEvent::parse(bytes) else {
            return Ok(());
        };
        if event.object_id == DisplayId(1).raw()
            && DisplayEvent::from_opcode(event.opcode) == Some(DisplayEvent::Error)
        {
            return Err(PlatformError::Protocol {
                object: event.u32(),
                code: event.u32(),
                message: event.string(),
            });
        }
        if event.object_id != self.registry.raw() {
            return Ok(());
        }
        match RegistryEvent::from_opcode(event.opcode) {
            Some(RegistryEvent::Global) => {
                self.handle_global(event.u32(), &event.string(), event.u32())
            }
            Some(RegistryEvent::GlobalRemove) => self.remove_output(event.u32()),
            None => Ok(()),
        }
    }

    /// Create a surface for a newly announced output.
    fn handle_global(&mut self, name: u32, interface: &str, version: u32) -> Result<()> {
        if interface == platform::protocol::WL_OUTPUT.name
            && let Some(output) =
                self.outputs
                    .bind_global(&mut self.sender, self.registry, name, version)?
        {
            self.create_surface(output)?;
        }
        Ok(())
    }

    /// Ask the clock which frame is due, and paint every ready surface.
    ///
    /// A surface that is ready but not yet due re-arms its callback instead
    /// of painting, which keeps the callback chain alive between frames.
    fn present_due_frame(&mut self) -> Result<()> {
        if !self.surfaces.iter().any(VideoSurface::is_ready) {
            return Ok(());
        }
        let Tick::Present { index, skipped } = self.clock.tick() else {
            return self.rearm_ready();
        };
        let start = Instant::now();
        let mut painted = false;
        for surface in &mut self.surfaces {
            if !surface.is_ready() {
                continue;
            }
            surface.fit_to(&self.pack);
            if surface.present(&mut self.sender, &self.pack, index)? {
                painted = true;
            } else {
                surface.rearm(&mut self.sender)?;
            }
        }
        if painted {
            self.stats.record(start.elapsed(), skipped);
            self.report();
        }
        Ok(())
    }

    /// Re-arm every ready surface so the callback chain survives a wait.
    fn rearm_ready(&mut self) -> Result<()> {
        for surface in &mut self.surfaces {
            if surface.is_ready() {
                surface.rearm(&mut self.sender)?;
            }
        }
        Ok(())
    }

    /// One line per output describing its size and painted frame count.
    #[must_use]
    fn surface_report(&self) -> String {
        self.surfaces
            .iter()
            .map(|surface| match surface.dimensions() {
                Some((width, height)) => format!(
                    "  output {} | {width}x{height} | {} frames\n",
                    surface.output().raw(),
                    surface.painted()
                ),
                None => format!("  output {} | unconfigured\n", surface.output().raw()),
            })
            .collect()
    }

    /// Print progress at the configured interval.
    fn report(&self) {
        if self.options.should_report(self.stats.presented()) {
            println!("{}", self.stats.summary());
        }
    }

    /// Create one background surface for `output`.
    fn create_surface(&mut self, output: VideoOutput) -> Result<()> {
        let compositor = self.globals.compositor;
        let layer_shell = self.globals.layer_shell;
        let shm = self.globals.shm;
        let fit = self.options.fit;
        let surface = VideoSurface::create(
            &mut self.sender,
            compositor,
            layer_shell,
            shm,
            output.id,
            fit,
            self.palette,
        )?;
        self.surfaces.push(surface);
        Ok(())
    }

    /// Drop the surface that followed a removed output.
    fn remove_output(&mut self, global_name: u32) -> Result<()> {
        let Some(output) = self.outputs.remove_global(global_name) else {
            return Ok(());
        };
        if let Some(index) = self
            .surfaces
            .iter()
            .position(|surface| surface.output() == output.id)
        {
            let mut surface = self.surfaces.remove(index);
            surface.destroy(&mut self.sender)?;
        }
        Ok(())
    }

    /// Release every surface before the run ends.
    fn teardown(&mut self) -> Result<()> {
        for mut surface in std::mem::take(&mut self.surfaces) {
            surface.destroy(&mut self.sender)?;
        }
        Ok(())
    }
}

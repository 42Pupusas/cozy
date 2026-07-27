//! Wayland front-end for the workspace's swaybg replacement.
//!
//! This Wayland front-end shares one decoded PNG across all live outputs,
//! creating one `Layer::Background` surface per output and scaling image pixels
//! independently after each output's first configure.

mod appearance;
mod background_surface;
mod output_set;
mod wallpaper_pixels;

use appearance::AppearanceRules;
use background_surface::BackgroundSurface;
use output_set::{Output, OutputSet};
use platform::conn::Sender;
use platform::event::RawEvent;
use platform::id::{CompositorId, LayerShellId, ObjectId, RegistryId, ShmId};
use platform::protocol::{self as proto, DisplayEvent, OutputEvent, RegistryEvent};
use platform::surface::{GlobalCollector, RegistryDiscovery, SurfaceBootstrap};
use platform::wire::WireEvent;
use platform::{Error, Result};
use quetzalcoatl::spsc::Consumer;

/// The ring only carries low-rate registry/output lifecycle events in this
/// slice; it can grow when the forthcoming surface/presentation events need it.
const EVENT_RING_CAP: usize = 128;

/// Core globals shared by every per-output background surface.
struct Globals {
    compositor: CompositorId,
    shm: ShmId,
    layer_shell: LayerShellId,
}

/// Collects just the singleton globals that all wallpaper outputs share.
/// `wl_output` globals intentionally remain live after discovery, so they are
/// handled by [`WallpaperApp`] rather than being folded into this bootstrap.
#[derive(Default)]
struct GlobalsBuilder {
    compositor: Option<CompositorId>,
    shm: Option<ShmId>,
    layer_shell: Option<LayerShellId>,
    /// Initial output globals are recorded, not bound, until discovery's sync
    /// callback has arrived. Binding them earlier lets their one-shot `name`
    /// events be swallowed by the discovery loop before the app can apply
    /// per-output rules.
    output_globals: Vec<(u32, u32)>,
}

impl GlobalsBuilder {
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
        *slot = Some(SurfaceBootstrap::bind(
            sender,
            registry,
            name,
            interface,
            known_version.min(advertised_version),
        )?);
        Ok(())
    }

    fn finish(&self) -> Result<Globals> {
        Ok(Globals {
            compositor: self.compositor.ok_or(Error::ReaderStopped)?,
            shm: self.shm.ok_or(Error::ReaderStopped)?,
            layer_shell: self.layer_shell.ok_or(Error::ReaderStopped)?,
        })
    }
}

impl GlobalCollector for GlobalsBuilder {
    fn is_complete(&self) -> bool {
        self.compositor.is_some() && self.shm.is_some() && self.layer_shell.is_some()
    }

    fn bind_global(
        &mut self,
        sender: &mut Sender,
        registry: RegistryId,
        name: u32,
        interface: &str,
        version: u32,
    ) -> Result<()> {
        match interface {
            interface_name if interface_name == proto::WL_COMPOSITOR.name => Self::bind_if_missing(
                &mut self.compositor,
                sender,
                registry,
                name,
                interface,
                version,
                proto::WL_COMPOSITOR.version,
            ),
            interface_name if interface_name == proto::WL_SHM.name => Self::bind_if_missing(
                &mut self.shm,
                sender,
                registry,
                name,
                interface,
                version,
                proto::WL_SHM.version,
            ),
            interface_name if interface_name == proto::ZWLR_LAYER_SHELL_V1.name => {
                Self::bind_if_missing(
                    &mut self.layer_shell,
                    sender,
                    registry,
                    name,
                    interface,
                    version,
                    proto::ZWLR_LAYER_SHELL_V1.version,
                )
            }
            interface_name if interface_name == proto::WL_OUTPUT.name => {
                self.output_globals.push((name, version));
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

/// Owns the live Wayland connection and the registry-to-output translation.
struct WallpaperApp {
    sender: Sender,
    events: Consumer<RawEvent>,
    #[allow(dead_code)]
    reader: std::thread::JoinHandle<Result<()>>,
    #[allow(dead_code)]
    globals: Globals,
    registry: RegistryId,
    outputs: OutputSet,
    backgrounds: Vec<BackgroundSurface>,
    appearances: AppearanceRules,
}

impl WallpaperApp {
    fn connect(wake: &waker::Waker, appearances: AppearanceRules) -> Result<Self> {
        let (mut sender, _repeat_timer, mut events, reader) =
            SurfaceBootstrap::connect(EVENT_RING_CAP, "wallpaper-wayland-reader", wake)?;
        let mut collector = GlobalsBuilder::default();
        let registry = RegistryDiscovery::collect(&mut sender, &mut events, &mut collector)?;
        // `collect` may stop once the singleton globals arrive. Finish with a
        // sync round-trip so every output from that initial registry burst is
        // bound before we create any background surfaces.
        RegistryDiscovery::collect_through_sync(
            &mut sender,
            &mut events,
            registry,
            &mut collector,
        )?;
        let globals = collector.finish()?;
        let mut outputs = OutputSet::new();
        for (name, version) in collector.output_globals {
            let _ = outputs.bind_global(&mut sender, registry, name, version)?;
        }
        let mut app = Self {
            sender,
            events,
            reader,
            globals,
            registry,
            outputs,
            backgrounds: Vec::new(),
            appearances,
        };
        for output in app.outputs.outputs().to_vec() {
            app.create_background(output)?;
        }
        Ok(app)
    }

    /// Run until the compositor closes the Wayland connection.
    fn run(&mut self) -> Result<()> {
        while let Some(event) = self.events.pop_block() {
            self.handle_event(event)?;
        }
        Ok(())
    }

    fn handle_event(&mut self, event: RawEvent) -> Result<()> {
        if let RawEvent::Wire { .. } = event {
            self.handle_output_name(&event)?;
            for background in &mut self.backgrounds {
                background.handle_event(&mut self.sender, &event)?;
            }
            return Ok(());
        }
        let RawEvent::Bytes(bytes) = event else {
            return Ok(());
        };
        let Some((mut event, _)) = WireEvent::parse(&bytes) else {
            return Ok(());
        };

        if event.object_id == platform::id::DisplayId(1).raw()
            && DisplayEvent::from_opcode(event.opcode) == Some(DisplayEvent::Error)
        {
            return Err(Error::Protocol {
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

    fn handle_global(&mut self, name: u32, interface: &str, version: u32) -> Result<()> {
        if interface == proto::WL_OUTPUT.name
            && let Some(output) =
                self.outputs
                    .bind_global(&mut self.sender, self.registry, name, version)?
        {
            self.create_background(output)?;
        }
        Ok(())
    }

    /// Capture the version-4 `wl_output.name` event, which carries a string
    /// and therefore arrives as raw bytes instead of the inline event form.
    fn handle_output_name(&mut self, event: &RawEvent) -> Result<()> {
        if event.event::<OutputEvent>() != Some(OutputEvent::Name) {
            return Ok(());
        }
        let Some(output) = self
            .outputs
            .by_id(platform::id::OutputId(event.object_id()))
        else {
            return Ok(());
        };
        let Some(bytes) = event.wire_bytes() else {
            return Ok(());
        };
        let Some((mut wire, _)) = WireEvent::parse(bytes) else {
            return Ok(());
        };
        let name = wire.string();
        if let Some(background) = self
            .backgrounds
            .iter_mut()
            .find(|background| background.output() == output.id)
        {
            background.set_appearance(&mut self.sender, self.appearances.for_output(&name))?;
        }
        Ok(())
    }

    fn create_background(&mut self, output: Output) -> Result<()> {
        self.backgrounds.push(BackgroundSurface::create(
            &mut self.sender,
            self.globals.compositor,
            self.globals.layer_shell,
            self.globals.shm,
            output.id,
            self.appearances.for_output("*"),
        )?);
        Ok(())
    }

    fn remove_output(&mut self, global_name: u32) -> Result<()> {
        let Some(output) = self.outputs.remove_global(global_name) else {
            return Ok(());
        };
        if let Some(index) = self
            .backgrounds
            .iter()
            .position(|background| background.output() == output.id)
        {
            let mut background = self.backgrounds.remove(index);
            background.destroy(&mut self.sender)?;
        }
        Ok(())
    }
}

/// Binary entry point kept on a marker owner rather than as a free function.
/// Binary entry point kept on a marker owner rather than as a free function.
struct Application;

impl Application {
    /// Print meta-command output without starting Wayland; help/version must
    /// be useful even when launched outside a graphical session.
    fn handle_meta_command() -> bool {
        let mut args = std::env::args_os();
        let program = args.next().unwrap_or_default();
        let arguments: Vec<_> = args.collect();
        if arguments
            .iter()
            .any(|argument| argument == "-h" || argument == "--help")
        {
            println!("{}", AppearanceRules::usage(&program));
            return true;
        }
        if arguments
            .iter()
            .any(|argument| argument == "-v" || argument == "--version")
        {
            println!("wallpaper 0.1.0");
            return true;
        }
        false
    }

    fn run() -> std::result::Result<(), String> {
        let appearances = AppearanceRules::parse(std::env::args_os())?;
        let wake = waker::Waker::new().map_err(|error| Error::ReaderSpawn(error).to_string())?;
        WallpaperApp::connect(&wake, appearances)
            .and_then(|mut app| app.run())
            .map_err(|error| error.to_string())
    }

    fn report_error(error: &str) -> ! {
        eprintln!("wallpaper: {error}");
        std::process::exit(1);
    }
}

fn main() {
    if Application::handle_meta_command() {
        return;
    }
    if let Err(error) = Application::run() {
        Application::report_error(&error);
    }
}

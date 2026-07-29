//! `shot` — a screenshot tool replacing the `slurp | grim -g -` pipe.
//!
//! One binary instead of two processes and a shell pipe: it discovers outputs,
//! captures through `zwlr_screencopy_unstable_v1`, and writes a PNG. The
//! interesting logic deliberately lives elsewhere -- `crates/screencopy` owns
//! the capture protocol, `ixiptla-core` the encoding -- so this file is just
//! wiring: connect, discover, pick a target, capture, write.

use clipboard::ScreenshotPayload;
use clipboard_wayland::ClipboardSession;
mod drag_selection;
mod headless;
mod output_registry;
mod seat_devices;
mod selection_overlay;
mod selection_painter;
mod shot_archive;
mod shot_options;
mod shot_writer;

use output_registry::OutputRegistry;
use platform::conn::Sender;
use platform::event::RawEvent;
use platform::id::{CompositorId, LayerShellId, RegistryId, ScreencopyManagerId, ShmId};
use platform::protocol as proto;
use platform::surface::{GlobalCollector, RegistryDiscovery, SurfaceBootstrap};
use platform::{Error, Result};
use quetzalcoatl::spsc::Consumer;
use screencopy::{CaptureRegion, ScreencopySession};
use seat_devices::SeatDevices;
use selection_overlay::SelectionOverlay;
use shot_archive::ShotArchive;
use shot_options::{CaptureTarget, ShotOptions};
use shot_writer::ShotWriter;

/// Capture is a short-lived, low-event-rate operation: a registry burst, a
/// handful of frame events, done. It never needs the deep ring a live window
/// does.
const EVENT_RING_CAP: usize = 64;

/// The two globals a capture needs, plus the outputs it may target.
struct Globals {
    shm: ShmId,
    screencopy: ScreencopyManagerId,
    /// The version `screencopy` was bound at -- below 3 the compositor never
    /// sends `buffer_done`, which the session must know.
    screencopy_version: u32,
    data_control: Option<(platform::id::DataControlManagerId, u32)>,
    /// Only the interactive picker needs these, so they stay optional: a
    /// compositor without layer-shell can still take a plain screenshot, and
    /// refusing to start over a global that this run will never use would be
    /// gratuitous.
    compositor: Option<CompositorId>,
    layer_shell: Option<LayerShellId>,
    /// The seat's registry coordinates rather than a bound object: the picker
    /// binds a *fresh* seat when it needs one, because `wl_seat.capabilities`
    /// fires once per object and discovery would have consumed it.
    seat: Option<(u32, u32)>,
}

/// Collects the globals required to capture.
///
/// `wl_output`s are bound as they're announced rather than recorded for later,
/// because their `name` events -- which `--output` matches on -- only start
/// flowing once the object exists.
#[derive(Default)]
struct GlobalsBuilder {
    shm: Option<ShmId>,
    screencopy: Option<(ScreencopyManagerId, u32)>,
    data_control: Option<(platform::id::DataControlManagerId, u32)>,
    compositor: Option<CompositorId>,
    layer_shell: Option<LayerShellId>,
    seat: Option<(u32, u32)>,
    outputs: OutputRegistry,
}

impl GlobalsBuilder {
    fn finish(self) -> std::result::Result<(Globals, OutputRegistry), String> {
        let shm = self
            .shm
            .ok_or_else(|| "compositor does not offer wl_shm".to_string())?;
        // The likeliest real-world failure: GNOME/Mutter has never
        // implemented wlr-screencopy, so say what's missing rather than
        // reporting a generic startup error.
        let (screencopy, screencopy_version) = self.screencopy.ok_or_else(|| {
            format!(
                "compositor does not support {} (screenshots need a wlroots-based \
                 compositor such as sway, niri, or Hyprland)",
                proto::ZWLR_SCREENCOPY_MANAGER_V1.name
            )
        })?;
        Ok((
            Globals {
                shm,
                screencopy,
                screencopy_version,
                data_control: self.data_control,
                compositor: self.compositor,
                layer_shell: self.layer_shell,
                seat: self.seat,
            },
            self.outputs,
        ))
    }
}

impl GlobalCollector for GlobalsBuilder {
    fn is_complete(&self) -> bool {
        // Outputs are deliberately excluded: there is no way to know how many
        // will be announced, so the sync round-trip in `collect_through_sync`
        // is what bounds discovery.
        self.shm.is_some() && self.screencopy.is_some()
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
            n if n == proto::WL_SHM.name && self.shm.is_none() => {
                self.shm = Some(SurfaceBootstrap::bind(
                    sender,
                    registry,
                    name,
                    interface,
                    proto::WL_SHM.version.min(version),
                )?);
            }
            n if n == proto::ZWLR_SCREENCOPY_MANAGER_V1.name && self.screencopy.is_none() => {
                // Record the negotiated version, not the advertised one: the
                // handshake's shape depends on what we actually bound.
                let bound = proto::ZWLR_SCREENCOPY_MANAGER_V1.version.min(version);
                let manager = SurfaceBootstrap::bind(sender, registry, name, interface, bound)?;
                self.screencopy = Some((manager, bound));
            }
            n if n == proto::ZWLR_DATA_CONTROL_MANAGER_V1.name && self.data_control.is_none() => {
                let bound = proto::ZWLR_DATA_CONTROL_MANAGER_V1.version.min(version);
                let manager = SurfaceBootstrap::bind(sender, registry, name, interface, bound)?;
                self.data_control = Some((manager, bound));
            }
            n if n == proto::WL_COMPOSITOR.name && self.compositor.is_none() => {
                self.compositor = Some(SurfaceBootstrap::bind(
                    sender,
                    registry,
                    name,
                    interface,
                    proto::WL_COMPOSITOR.version.min(version),
                )?);
            }
            n if n == proto::ZWLR_LAYER_SHELL_V1.name && self.layer_shell.is_none() => {
                self.layer_shell = Some(SurfaceBootstrap::bind(
                    sender,
                    registry,
                    name,
                    interface,
                    proto::ZWLR_LAYER_SHELL_V1.version.min(version),
                )?);
            }
            // Deliberately *not* bound here. Binding during discovery means
            // the seat's one-shot `capabilities` event is delivered into a
            // loop that discards everything but registry globals -- so the
            // picker would later find no pointer and no keyboard, and map an
            // undismissable fullscreen overlay. Record the coordinates and
            // let `SeatDevices` bind a fresh object when it can listen.
            n if n == proto::WL_SEAT.name && self.seat.is_none() => {
                self.seat = Some((name, version));
            }
            n if n == proto::WL_OUTPUT.name => {
                self.outputs.bind(sender, registry, name, version)?;
            }
            _ => {}
        }
        Ok(())
    }
}

/// Owns the connection for one capture run.
struct ShotApp {
    sender: Sender,
    events: Consumer<RawEvent>,
    #[expect(
        dead_code,
        reason = "the reader thread must outlive the capture; dropping its handle \
                  would not stop it, but holding it documents the lifetime"
    )]
    reader: std::thread::JoinHandle<Result<()>>,
    session: ScreencopySession,
    outputs: OutputRegistry,
    /// Kept for the interactive picker, which needs a compositor, layer-shell
    /// and seat that a plain capture never touches.
    globals: Globals,
    /// Retained so the picker can bind a fresh seat at the moment it can
    /// actually listen for that seat's capabilities.
    registry: RegistryId,
    wake: waker::Waker,
}

impl ShotApp {
    /// Connect, discover globals, and learn every output's connector name.
    fn connect(wake: waker::Waker) -> std::result::Result<Self, String> {
        let (mut sender, _repeat, mut events, reader) =
            SurfaceBootstrap::connect(EVENT_RING_CAP, "shot-wayland-reader", &wake)
                .map_err(|e| e.to_string())?;
        let mut collector = GlobalsBuilder::default();
        let registry = RegistryDiscovery::collect(&mut sender, &mut events, &mut collector)
            .map_err(|e| e.to_string())?;
        // `collect` stops as soon as shm + screencopy arrive, which may be
        // before every output has been announced. The sync round-trip closes
        // that gap so `--output` sees them all.
        RegistryDiscovery::collect_through_sync(&mut sender, &mut events, registry, &mut collector)
            .map_err(|e| e.to_string())?;
        let (globals, mut outputs) = collector.finish()?;
        outputs
            .collect_names(&mut sender, &mut events)
            .map_err(|e| e.to_string())?;
        let session =
            ScreencopySession::new(globals.screencopy, globals.screencopy_version, globals.shm);
        Ok(Self {
            sender,
            events,
            reader,
            session,
            outputs,
            globals,
            registry,
            wake,
        })
    }

    /// Capture whatever `options` asked for and write it out.
    ///
    /// A cancelled interactive selection is not an error: it returns `Ok`
    /// having written nothing, so a keybind that opens the picker and gets
    /// dismissed exits 0 and leaves no file behind.
    fn run(&mut self, options: &ShotOptions) -> std::result::Result<(), String> {
        let Some((output, region)) = self.resolve_capture(&options.target)? else {
            return Ok(());
        };
        let frame = self
            .session
            .capture(
                &mut self.sender,
                &mut self.events,
                output,
                region,
                options.overlay_cursor,
            )
            .map_err(|e| e.to_string())?;
        let png = ShotWriter::encode(&frame)?;
        // Archive first, and always. Clipboard publishing parks this process
        // for as long as it owns the selection, so anything sequenced after it
        // would not happen until the user copied something else -- and would
        // never happen at all if the process were killed while waiting.
        let archived = options.save.then(|| Self::archive(&png)).transpose()?;
        if options.clipboard {
            // A saved copy doubles as the clipboard's `text/uri-list` target,
            // so pasting into a file manager lands the permanent file rather
            // than a temporary that this process deletes on exit.
            let path = options.output_path.as_deref().or(archived.as_deref());
            self.publish_clipboard(&png, path)?;
        } else if archived.is_none() || options.output_path.is_some() {
            // With `--save` alone the bytes are already on disk; writing them
            // again to stdout would dump a PNG into the user's terminal.
            ShotWriter::write(&png, options.output_path.as_deref())?;
        }
        Ok(())
    }

    /// Write a timestamped copy into the screenshot archive.
    ///
    /// The path is reported on stdout so a keybind wrapper or script can pick
    /// it up, and so a user running this by hand can see where it went.
    fn archive(png: &[u8]) -> std::result::Result<std::path::PathBuf, String> {
        let now = localtime::LocalTime::now()
            .ok_or_else(|| "cannot read the local time to name the file".to_string())?;
        let path = ShotArchive::from_environment()?.reserve(&now)?;
        ShotWriter::write(png, Some(&path))?;
        println!("{}", path.display());
        Ok(path)
    }

    /// Resolve what to capture, running the interactive picker when asked.
    ///
    /// `None` means the user cancelled and nothing should be captured.
    fn resolve_capture(
        &mut self,
        target: &CaptureTarget,
    ) -> std::result::Result<Option<(platform::id::OutputId, Option<CaptureRegion>)>, String> {
        if let CaptureTarget::Selection(name) = target {
            let output = name
                .as_deref()
                .map_or_else(|| self.first_output(), |name| self.output_named(name))?;
            return Ok(self
                .select_region(output)?
                .map(|region| (output, Some(region))));
        }
        self.resolve_target(target).map(Some)
    }

    /// Publish the PNG and remain alive until another clipboard source replaces it.
    fn publish_clipboard(
        &mut self,
        png: &[u8],
        output_path: Option<&std::path::Path>,
    ) -> std::result::Result<(), String> {
        let Some((manager, _version)) = self.globals.data_control else {
            return Err("compositor does not offer zwlr_data_control_manager_v1; clipboard mode needs data-control support".to_string());
        };
        let path = output_path.map_or_else(
            || std::env::temp_dir().join(format!("shot-clipboard-{}.png", std::process::id())),
            std::path::Path::to_path_buf,
        );
        ShotWriter::write(png, Some(&path))?;
        let uri = format!("file://{}", path.display());
        let payload = ScreenshotPayload::from_png(png.to_vec(), uri);
        let Some((seat_global, seat_version)) = self.globals.seat else {
            return Err("clipboard mode needs a wl_seat".to_string());
        };
        let cleanup_path = output_path.is_none().then_some(path);
        let mut owner = ClipboardSession::publish(
            &mut self.sender,
            manager,
            self.registry,
            seat_global,
            seat_version,
            &payload,
            cleanup_path,
        )
        .map_err(|e| e.to_string())?;
        owner.run(&mut self.events);
        Ok(())
    }

    /// Show the overlay on `output` and wait for a region or a cancel.
    fn select_region(
        &mut self,
        output: platform::id::OutputId,
    ) -> std::result::Result<Option<CaptureRegion>, String> {
        let (Some(compositor), Some(layer_shell), Some((seat_name, seat_version))) = (
            self.globals.compositor,
            self.globals.layer_shell,
            self.globals.seat,
        ) else {
            return Err(format!(
                "interactive selection needs {} and a seat, which this compositor \
                 does not offer (use --geometry instead)",
                proto::ZWLR_LAYER_SHELL_V1.name
            ));
        };
        // Bind input *before* mapping the overlay. If this fails we must not
        // put anything on screen: a fullscreen surface with no way to receive
        // Escape or a click is a locked screen, not a broken feature.
        let devices = SeatDevices::bind(
            &mut self.sender,
            &mut self.events,
            self.registry,
            seat_name,
            seat_version,
        )
        .map_err(|e| e.to_string())??;
        let mut overlay = SelectionOverlay::create(
            &mut self.sender,
            compositor,
            layer_shell,
            devices,
            self.globals.shm,
            output,
        )
        .map_err(|e| e.to_string())?;
        overlay
            .pick(&mut self.sender, &mut self.events, &self.wake)
            .map_err(|e| e.to_string())
    }

    /// The first advertised output, which single-monitor setups make
    /// unambiguous.
    fn output_named(&self, name: &str) -> std::result::Result<platform::id::OutputId, String> {
        self.outputs
            .by_name(name)
            .map(|output| output.id)
            .ok_or_else(|| {
                format!(
                    "no output named `{name}`; available: {}",
                    self.outputs.known_names()
                )
            })
    }

    fn first_output(&self) -> std::result::Result<platform::id::OutputId, String> {
        self.outputs
            .outputs()
            .first()
            .map(|o| o.id)
            .ok_or_else(|| "compositor advertised no outputs".to_string())
    }

    /// Turn a requested target into the output object and optional region.
    ///
    /// A region is captured from the output it lies on; with one output that
    /// is unambiguous, and multi-output region capture is left for the
    /// interactive overlay to resolve rather than guessed at here.
    fn resolve_target(
        &self,
        target: &CaptureTarget,
    ) -> std::result::Result<(platform::id::OutputId, Option<CaptureRegion>), String> {
        match target {
            CaptureTarget::AllOutputs => Ok((self.first_output()?, None)),
            CaptureTarget::Region(region) => Ok((self.first_output()?, Some(*region))),
            CaptureTarget::OutputRegion(name, region) => {
                Ok((self.output_named(name)?, Some(*region)))
            }
            // Handled by `resolve_capture`, which can run an event loop.
            CaptureTarget::Selection(_) => Err(
                "internal: interactive selection must be resolved before this point".to_string(),
            ),
            CaptureTarget::Output(name) => self
                .outputs
                .by_name(name)
                .map(|output| (output.id, None))
                .ok_or_else(|| {
                    format!(
                        "no output named `{name}`; available: {}",
                        self.outputs.known_names()
                    )
                }),
        }
    }
}

/// Binary entry point, kept on an owner rather than as free functions.
struct Application;

impl Application {
    /// Handle `--help`/`--version` without touching Wayland, so they work
    /// outside a graphical session.
    fn handle_meta_command() -> bool {
        let mut args = std::env::args_os();
        let program = args.next().unwrap_or_default();
        let arguments: Vec<_> = args.collect();
        if arguments.iter().any(|a| a == "-h" || a == "--help") {
            print!("{}", ShotOptions::usage(&program));
            return true;
        }
        if arguments.iter().any(|a| a == "-v" || a == "--version") {
            println!("shot 0.1.0");
            return true;
        }
        false
    }

    fn run() -> std::result::Result<(), String> {
        let options = ShotOptions::parse(std::env::args_os())?;
        let wake = waker::Waker::new().map_err(|e| Error::ReaderSpawn(e).to_string())?;
        ShotApp::connect(wake)?.run(&options)
    }

    fn report_error(error: &str) -> ! {
        eprintln!("shot: {error}");
        std::process::exit(1);
    }
}

fn main() {
    if let Ok(Some(options)) = headless_stress::StressOptions::parse(std::env::args()) {
        headless::ShotStress::run(options);
        return;
    }
    if Application::handle_meta_command() {
        return;
    }
    if let Err(error) = Application::run() {
        Application::report_error(&error);
    }
}

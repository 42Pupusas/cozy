//! `clipboard` — a clipboard manager replacing `wl-paste --watch cliphist store`.
//!
//! One binary instead of a watcher, a store, and two shell pipes: it follows
//! the compositor's selection through `zwlr_data_control_unstable_v1`, keeps a
//! bounded history on disk, and re-publishes any entry on request.
//!
//! As with `bins/shot`, the interesting logic lives elsewhere on purpose —
//! `crates/clipboard` owns retention and persistence, `crates/clipboard-wayland`
//! owns the protocol — so this file is wiring: connect, discover, dispatch.

mod clipboard_options;
mod entry_preview;

use clipboard::{ClipboardPayload, HistoryStore};
use clipboard_options::{ClipboardOptions, Command};
use clipboard_wayland::{ClipboardSession, SelectionWatcher, WatcherLoop};
use entry_preview::EntryPreview;
use platform::Result;
use platform::conn::Sender;
use platform::event::RawEvent;
use platform::id::{DataControlManagerId, RegistryId};
use platform::protocol as proto;
use platform::surface::{GlobalCollector, RegistryDiscovery};
use quetzalcoatl::spsc::Consumer;

/// A clipboard daemon sees a burst per copy, not per frame; it never needs the
/// deep ring a rendering window does.
const EVENT_RING_CAP: usize = 64;

/// The globals a clipboard client needs.
struct Globals {
    data_control: DataControlManagerId,
    /// The seat's registry coordinates rather than a bound object: both the
    /// watcher and the owner bind a *fresh* seat, because seat events are
    /// delivered per object and discovery would have consumed them.
    seat: (u32, u32),
}

/// Collects those globals from the registry burst.
#[derive(Default)]
struct GlobalsBuilder {
    data_control: Option<DataControlManagerId>,
    seat: Option<(u32, u32)>,
}

impl GlobalsBuilder {
    fn finish(self) -> std::result::Result<Globals, String> {
        let data_control = self.data_control.ok_or_else(|| {
            format!(
                "compositor does not support {} (clipboard management needs a \
                 wlroots-based compositor such as sway, niri, or Hyprland)",
                proto::ZWLR_DATA_CONTROL_MANAGER_V1.name
            )
        })?;
        let seat = self
            .seat
            .ok_or_else(|| "compositor advertised no wl_seat".to_string())?;
        Ok(Globals { data_control, seat })
    }
}

impl GlobalCollector for GlobalsBuilder {
    fn is_complete(&self) -> bool {
        self.data_control.is_some() && self.seat.is_some()
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
            n if n == proto::ZWLR_DATA_CONTROL_MANAGER_V1.name && self.data_control.is_none() => {
                self.data_control = Some(platform::surface::SurfaceBootstrap::bind(
                    sender,
                    registry,
                    name,
                    interface,
                    proto::ZWLR_DATA_CONTROL_MANAGER_V1.version.min(version),
                )?);
            }
            n if n == proto::WL_SEAT.name && self.seat.is_none() => {
                self.seat = Some((name, version));
            }
            _ => {}
        }
        Ok(())
    }
}

/// Owns the connection for one clipboard run.
struct ClipboardApp {
    sender: Sender,
    events: Consumer<RawEvent>,
    #[expect(
        dead_code,
        reason = "the reader thread must outlive the run; dropping its handle \
                  would not stop it, but holding it documents the lifetime"
    )]
    reader: std::thread::JoinHandle<Result<()>>,
    globals: Globals,
    registry: RegistryId,
}

impl ClipboardApp {
    /// Connect and discover the data-control manager and seat.
    fn connect(wake: &waker::Waker) -> std::result::Result<Self, String> {
        let (mut sender, _repeat, mut events, reader) =
            platform::surface::SurfaceBootstrap::connect(
                EVENT_RING_CAP,
                "clipboard-wayland-reader",
                wake,
            )
            .map_err(|e| e.to_string())?;
        let mut collector = GlobalsBuilder::default();
        let registry = RegistryDiscovery::collect(&mut sender, &mut events, &mut collector)
            .map_err(|e| e.to_string())?;
        Ok(Self {
            sender,
            events,
            reader,
            globals: collector.finish()?,
            registry,
        })
    }

    /// Watch the clipboard until the compositor goes away.
    ///
    /// Every recorded selection is persisted immediately rather than on exit:
    /// a daemon is killed by logout or a crash far more often than it exits
    /// cleanly, and history lost that way is history the user thought they had.
    fn watch(&mut self, store: &HistoryStore, capacity: usize) -> std::result::Result<(), String> {
        let mut history = store.load(capacity).map_err(|e| e.to_string())?;
        let mut watcher = SelectionWatcher::bind(
            &mut self.sender,
            self.globals.data_control,
            self.registry,
            self.globals.seat.0,
            self.globals.seat.1,
        )
        .map_err(|e| e.to_string())?;

        loop {
            let mut captured = Vec::new();
            if !WatcherLoop::pump(
                &mut watcher,
                &mut self.sender,
                &mut self.events,
                |payload| captured.push(payload),
            ) {
                // The reader stopped: the compositor is gone, so there is
                // nothing left to watch.
                return Ok(());
            }
            let mut dirty = false;
            for payload in captured {
                dirty |= history.push(payload).is_some();
            }
            if dirty && let Err(error) = store.save(&history) {
                // A failed save must not kill the daemon: the clipboard still
                // works, and the next copy may well succeed.
                eprintln!("clipboard: could not save history: {error}");
            }
        }
    }

    /// Re-publish one stored entry and hold it until something replaces it.
    fn restore(&mut self, payload: &ClipboardPayload) -> std::result::Result<(), String> {
        let mut owner = ClipboardSession::publish(
            &mut self.sender,
            self.globals.data_control,
            self.registry,
            self.globals.seat.0,
            self.globals.seat.1,
            payload,
            None,
        )
        .map_err(|e| e.to_string())?;
        owner.run(&mut self.events);
        Ok(())
    }
}

/// The commands that never touch Wayland.
struct HistoryCommands;

impl HistoryCommands {
    fn list(store: &HistoryStore, capacity: usize) -> std::result::Result<(), String> {
        let history = store.load(capacity).map_err(|e| e.to_string())?;
        for entry in history.entries() {
            println!("{}", EntryPreview::line(entry));
        }
        Ok(())
    }

    fn decode(store: &HistoryStore, capacity: usize, id: u64) -> std::result::Result<(), String> {
        use std::io::Write as _;
        let history = store.load(capacity).map_err(|e| e.to_string())?;
        let entry = history
            .get(id)
            .ok_or_else(|| format!("no history entry with id {id}"))?;
        // Richest representation first, matching what a paste would receive.
        let bytes = [
            "image/png",
            "text/uri-list",
            "text/plain;charset=utf-8",
            "text/plain",
        ]
        .into_iter()
        .find_map(|mime| entry.payload().requested(mime))
        .ok_or_else(|| format!("entry {id} has no decodable representation"))?;
        std::io::stdout()
            .write_all(bytes)
            .map_err(|e| format!("could not write to stdout: {e}"))
    }

    fn clear(store: &HistoryStore) -> std::result::Result<(), String> {
        match std::fs::remove_file(store.path()) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("could not clear history: {error}")),
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
        if arguments.is_empty() || arguments.iter().any(|a| a == "-h" || a == "--help") {
            print!("{}", ClipboardOptions::usage(&program));
            return true;
        }
        if arguments.iter().any(|a| a == "-v" || a == "--version") {
            println!("clipboard 0.1.0");
            return true;
        }
        false
    }

    fn run() -> std::result::Result<(), String> {
        let options = ClipboardOptions::parse(std::env::args_os())?;
        let store = HistoryStore::new(options.store_path());
        Self::ensure_store_directory(&store)?;

        match options.command {
            Command::List => HistoryCommands::list(&store, options.capacity),
            Command::Decode(id) => HistoryCommands::decode(&store, options.capacity, id),
            Command::Clear => HistoryCommands::clear(&store),
            Command::Daemon => {
                let wake = waker::Waker::new().map_err(|e| e.to_string())?;
                ClipboardApp::connect(&wake)?.watch(&store, options.capacity)
            }
            Command::Restore(id) => {
                let history = store.load(options.capacity).map_err(|e| e.to_string())?;
                let payload = history
                    .get(id)
                    .ok_or_else(|| format!("no history entry with id {id}"))?
                    .payload()
                    .clone();
                let wake = waker::Waker::new().map_err(|e| e.to_string())?;
                ClipboardApp::connect(&wake)?.restore(&payload)
            }
        }
    }

    /// Create the history directory on first use, so a fresh machine works
    /// without the user being told to `mkdir` anything.
    fn ensure_store_directory(store: &HistoryStore) -> std::result::Result<(), String> {
        let Some(parent) = store.path().parent() else {
            return Ok(());
        };
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {e}", parent.display()))
    }

    fn report_error(error: &str) -> ! {
        eprintln!("clipboard: {error}");
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

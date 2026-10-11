use std::path::PathBuf;

use portal_core::{ChooserDialog, ChooserKind, ChooserOutcome, ChooserRequest, Selection};
use portal_ui::ChooserWindow;
use waker::{Interest, PollSet};

struct Probe {
    directory: bool,
    multiple: bool,
    folder: Option<PathBuf>,
}

impl Probe {
    fn parse(args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut probe = Self {
            directory: false,
            multiple: false,
            folder: None,
        };
        for arg in args {
            match arg.as_str() {
                "--folders" => probe.directory = true,
                "--multiple" => probe.multiple = true,
                flag if flag.starts_with("--") => return Err(format!("unknown flag {flag}")),
                path => probe.folder = Some(PathBuf::from(path)),
            }
        }
        Ok(probe)
    }

    fn request(&self) -> ChooserRequest {
        ChooserRequest {
            app_id: String::from("chooser_probe"),
            parent_window: String::new(),
            title: String::from("Chooser probe"),
            kind: ChooserKind::Open {
                multiple: self.multiple,
                directory: self.directory,
            },
            accept_label: None,
            modal: true,
            filters: Vec::new(),
            current_filter: None,
            choices: Vec::new(),
            current_folder: self.folder.clone(),
        }
    }

    fn run(&self) -> Result<ChooserOutcome, String> {
        let mut dialog = ChooserWindow::open(&self.request())?;
        loop {
            if let Some(outcome) = dialog.advance() {
                return Ok(outcome);
            }
            let mut set = PollSet::new();
            set.watch(dialog.wake_fd(), Interest::Readable);
            for fd in dialog.watch_fds() {
                set.watch(fd, Interest::Readable);
            }
            let _ = set.wait(dialog.wait_ms().unwrap_or(-1));
        }
    }

    fn report(outcome: &ChooserOutcome) {
        match outcome {
            ChooserOutcome::Picked(Selection { paths, .. }) => {
                println!("picked {} path(s)", paths.len());
                for path in paths {
                    println!("  {}", Selection::uri_of(path));
                }
            }
            ChooserOutcome::Cancelled => println!("cancelled"),
            ChooserOutcome::Failed => println!("failed"),
        }
    }
}

fn main() {
    let probe = match Probe::parse(std::env::args().skip(1)) {
        Ok(probe) => probe,
        Err(message) => {
            eprintln!("chooser_probe: {message}");
            eprintln!("usage: chooser_probe [--folders] [--multiple] [FOLDER]");
            std::process::exit(2);
        }
    };
    match probe.run() {
        Ok(outcome) => Probe::report(&outcome),
        Err(message) => {
            eprintln!("chooser_probe: {message}");
            std::process::exit(1);
        }
    }
}

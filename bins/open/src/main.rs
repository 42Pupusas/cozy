//! `open` — an `xdg-open` replacement.
//!
//! `xdg-open` is a 1000-line shell script that guesses the desktop
//! environment and then delegates to whichever tool that environment ships.
//! On a session built from this workspace there is no such tool to delegate
//! to, so the guessing has nothing to find.
//!
//! This binary resolves the association itself, through `crates/opener` —
//! the same code path the file browser uses when a row is activated. As with
//! `bins/clipboard`, the logic lives in the crate and this file is wiring:
//! parse, resolve, spawn, report.
//!
//! # Exit status
//!
//! A command-line tool is judged by its exit status, so unlike the browser
//! (which swallows failures to stay alive) this reports them:
//!
//! * `0` — every target opened.
//! * `1` — at least one did not, with the reason on stderr.
//! * `2` — the command line itself was wrong.

mod open_options;

use open_options::{Command, OpenOptions};
use opener::{ActivationToken, Handlers, OpenError, Opener, Target};

/// Exit statuses, named rather than scattered as integer literals.
struct ExitStatus;

impl ExitStatus {
    const FAILED: i32 = 1;
    const USAGE: i32 = 2;
}

/// Renders what would happen, for `--dry-run` and `--list`.
struct Report;

impl Report {
    /// Print the command that `target` would run.
    fn dry_run(handlers: &Handlers, target: &Target) -> Result<(), OpenError> {
        let command = handlers
            .command_for_target(target)
            .ok_or(OpenError::NoHandler)?;
        let mut line = command.program.clone();
        for arg in &command.args {
            line.push(' ');
            line.push_str(arg);
        }
        println!("{line}");
        Ok(())
    }

    /// Print every application that could open `target`, best first.
    fn list(handlers: &Handlers, target: &Target) -> Result<(), OpenError> {
        let entries = handlers.entries_for_target(target);
        if entries.is_empty() {
            return Err(OpenError::NoHandler);
        }
        for entry in entries {
            println!("{}\t{}", entry.name, entry.exec);
        }
        Ok(())
    }
}

/// Binary entry point.
struct Application;

impl Application {
    /// Every failure here ends in an exit status rather than a return value:
    /// a bad command line and an unopenable target are reported differently,
    /// and both are terminal.
    fn run() {
        let options = match OpenOptions::parse(std::env::args_os()) {
            Ok(options) => options,
            Err(error) => Self::exit_with_usage(&error),
        };

        let targets = match options.command {
            Command::Help => {
                print!("{}", OpenOptions::usage(&Self::program_name()));
                return;
            }
            Command::Version => {
                println!("open 0.1.0");
                return;
            }
            Command::Open(targets) => targets,
        };

        // Taken before anything is spawned, and taken rather than read: a
        // token authorises one activation, so it must leave our environment
        // as we pick it up. Whatever ran us -- a terminal, a launcher --
        // minted it while it had focus, which is why a browser already
        // running elsewhere comes to the front instead of opening a tab on
        // a workspace nobody is looking at.
        let token = ActivationToken::take_from_env();

        // Loaded once, not per target: the glob and association databases
        // are a fair slice of the filesystem, and `open *.png` is ordinary.
        let opener = Opener::new(Handlers::load()).with_activation(token);
        let mut failed = false;

        for raw in &targets {
            let target = Target::parse(raw);
            let outcome = if options.list {
                Report::list(opener.handlers(), &target)
            } else if options.dry_run {
                Report::dry_run(opener.handlers(), &target)
            } else {
                opener.try_open(&target)
            };
            if let Err(error) = outcome {
                eprintln!("open: {raw}: {error}");
                failed = true;
            }
        }

        if failed {
            std::process::exit(ExitStatus::FAILED);
        }
    }

    fn program_name() -> std::ffi::OsString {
        std::env::args_os().next().unwrap_or_else(|| "open".into())
    }

    /// A malformed command line is a different failure from a target that
    /// would not open, so it gets its own status.
    fn exit_with_usage(error: &str) -> ! {
        eprintln!("open: {error}");
        eprint!("{}", OpenOptions::usage(&Self::program_name()));
        std::process::exit(ExitStatus::USAGE);
    }
}

fn main() {
    Application::run();
}

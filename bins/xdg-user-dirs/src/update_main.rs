//! `xdg-user-dirs-update` -- create and update `user-dirs.dirs`.
//!
//! A drop-in replacement for the tool of the same name. The session runs it
//! early at login, so its job is to leave the configuration complete and
//! every directory it names either real or pointed at `$HOME`.
//!
//! The behaviour lives in [`Updater`]; this file is wiring.

mod update_options;
mod updater;

use update_options::{Command, UpdateOptions};
use updater::Updater;
use user_dirs::ConfigPaths;

/// Binary entry point.
struct Application;

impl Application {
    const USAGE_STATUS: i32 = 1;
    const FAILED_STATUS: i32 = 1;

    fn run() {
        let options = match UpdateOptions::parse(std::env::args_os()) {
            Ok(options) => options,
            Err(error) => Self::exit_with_usage(&error),
        };
        if options.command == Command::Help {
            print!("{}", UpdateOptions::usage(&Self::program_name()));
            return;
        }

        let paths = ConfigPaths::from_env();
        if let Err(error) = Updater::new(&paths, &options).run() {
            eprintln!("xdg-user-dirs-update: {error}");
            std::process::exit(Self::FAILED_STATUS);
        }
    }

    fn program_name() -> String {
        std::env::args_os()
            .next()
            .map_or_else(|| "xdg-user-dirs-update".to_owned(), |name| {
                name.to_string_lossy().into_owned()
            })
    }

    fn exit_with_usage(error: &str) -> ! {
        eprintln!("xdg-user-dirs-update: {error}");
        eprint!("{}", UpdateOptions::usage(&Self::program_name()));
        std::process::exit(Self::USAGE_STATUS);
    }
}

fn main() {
    Application::run();
}

//! `xdg-user-dir` -- print the path of one XDG user directory.
//!
//! A drop-in replacement for the tool of the same name. Scripts across the
//! desktop call it, so the contract is fixed by what they expect:
//!
//! * exactly one argument, the directory name;
//! * the path on stdout, with a trailing newline;
//! * a usage line on stderr and status 1 for anything else;
//! * an unknown name is *not* an error -- it prints `$HOME`.
//!
//! The resolution policy lives in `crates/user-dirs`; this file is wiring.

use user_dirs::{ConfigPaths, DirLookup};

/// Binary entry point.
struct Application;

impl Application {
    /// The status the real tool exits with when the arguments are wrong.
    const USAGE_STATUS: i32 = 1;

    fn run() {
        let mut args = std::env::args_os().skip(1);
        let (Some(name), None) = (args.next(), args.next()) else {
            Self::exit_with_usage();
        };
        let paths = ConfigPaths::from_env();
        let resolved = DirLookup::new(&paths).resolve(&name.to_string_lossy());
        // `println!` on a path would go through Display and mangle any name
        // that is not UTF-8, so the bytes go out as they came off the disk.
        Self::print_path(&resolved);
    }

    /// Write a path to stdout verbatim, followed by a newline.
    fn print_path(path: &std::path::Path) {
        use std::io::Write;
        use std::os::unix::ffi::OsStrExt;
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        let _ = out.write_all(path.as_os_str().as_bytes());
        let _ = out.write_all(b"\n");
    }

    fn exit_with_usage() -> ! {
        let program = std::env::args_os()
            .next()
            .unwrap_or_else(|| "xdg-user-dir".into());
        eprintln!("Usage {} <dir-type>", program.to_string_lossy());
        std::process::exit(Self::USAGE_STATUS);
    }
}

fn main() {
    Application::run();
}

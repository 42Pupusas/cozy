//! Command-line options for `open`.

use std::ffi::OsString;

/// What the binary was asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Open these targets.
    Open(Vec<String>),
    /// Print usage.
    Help,
    /// Print the version.
    Version,
}

/// Parsed invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenOptions {
    pub command: Command,
    /// Print the resolved command instead of running it.
    pub dry_run: bool,
    /// List every application that could open each target.
    pub list: bool,
}

impl OpenOptions {
    /// Parse an argument list.
    ///
    /// `--` ends option parsing, so a file whose name begins with `-` stays
    /// openable. Without it, `open -- -weird.txt` would have no way to say
    /// "this is a filename".
    pub fn parse<I: IntoIterator<Item = OsString>>(args: I) -> Result<Self, String> {
        let mut targets = Vec::new();
        let mut dry_run = false;
        let mut list = false;
        let mut command = None;
        let mut literal = false;

        for arg in args.into_iter().skip(1) {
            let arg = arg.to_string_lossy().into_owned();
            if literal {
                targets.push(arg);
                continue;
            }
            match arg.as_str() {
                "--" => literal = true,
                "-h" | "--help" => command = Some(Command::Help),
                "-v" | "--version" => command = Some(Command::Version),
                "-n" | "--dry-run" => dry_run = true,
                "-l" | "--list" => list = true,
                other if other.starts_with('-') && other.len() > 1 => {
                    return Err(format!("unknown option `{other}`"));
                }
                _ => targets.push(arg),
            }
        }

        let command = match command {
            Some(meta) => meta,
            None if targets.is_empty() => {
                return Err("no file or URL given".to_owned());
            }
            None => Command::Open(targets),
        };

        Ok(Self {
            command,
            dry_run,
            list,
        })
    }

    /// Usage text.
    #[must_use]
    pub fn usage(program: &std::ffi::OsStr) -> String {
        format!(
            "usage: {} [options] <file-or-url>...\n\
             \n\
             Open each file or URL with its associated application.\n\
             \n\
             options:\n  \
               -l, --list          list the applications that could open each target\n  \
               -n, --dry-run       print the command that would run, without running it\n  \
               -h, --help          show this message\n  \
               -v, --version       show the version\n  \
               --                  treat every later argument as a target\n",
            std::path::Path::new(program).display(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{Command, OpenOptions};
    use std::ffi::OsString;

    struct Args;

    impl Args {
        fn of(items: &[&str]) -> Vec<OsString> {
            std::iter::once(OsString::from("open"))
                .chain(items.iter().map(OsString::from))
                .collect()
        }
    }

    #[test]
    fn a_bare_argument_is_the_target() {
        let options = OpenOptions::parse(Args::of(&["notes.txt"])).unwrap();
        assert_eq!(options.command, Command::Open(vec!["notes.txt".to_owned()]));
        assert!(!options.dry_run);
        assert!(!options.list);
    }

    #[test]
    fn several_targets_are_all_kept_in_order() {
        let options = OpenOptions::parse(Args::of(&["a.txt", "b.png"])).unwrap();
        assert_eq!(
            options.command,
            Command::Open(vec!["a.txt".to_owned(), "b.png".to_owned()])
        );
    }

    #[test]
    fn flags_are_recognised() {
        let options = OpenOptions::parse(Args::of(&["-n", "-l", "a.txt"])).unwrap();
        assert!(options.dry_run);
        assert!(options.list);
    }

    /// Without `--`, a filename beginning with `-` is unopenable.
    #[test]
    fn a_double_dash_ends_option_parsing() {
        let options = OpenOptions::parse(Args::of(&["--", "-weird.txt"])).unwrap();
        assert_eq!(options.command, Command::Open(vec!["-weird.txt".to_owned()]));
    }

    /// After `--`, something that looks like a flag is still a target.
    #[test]
    fn an_option_after_a_double_dash_is_a_target() {
        let options = OpenOptions::parse(Args::of(&["--", "--help"])).unwrap();
        assert_eq!(options.command, Command::Open(vec!["--help".to_owned()]));
    }

    /// A lone `-` is a legitimate relative filename, not an option.
    #[test]
    fn a_lone_dash_is_a_target() {
        let options = OpenOptions::parse(Args::of(&["-"])).unwrap();
        assert_eq!(options.command, Command::Open(vec!["-".to_owned()]));
    }

    #[test]
    fn no_target_is_an_error() {
        assert!(OpenOptions::parse(Args::of(&[])).is_err());
    }

    #[test]
    fn unknown_options_are_rejected() {
        assert!(OpenOptions::parse(Args::of(&["--wat", "a.txt"])).is_err());
    }

    #[test]
    fn help_and_version_need_no_target() {
        assert_eq!(
            OpenOptions::parse(Args::of(&["--help"])).unwrap().command,
            Command::Help
        );
        assert_eq!(
            OpenOptions::parse(Args::of(&["-v"])).unwrap().command,
            Command::Version
        );
    }

    /// `xdg-open --help file` prints help. Matching that avoids surprising a
    /// script that passes both.
    #[test]
    fn help_wins_over_a_target() {
        assert_eq!(
            OpenOptions::parse(Args::of(&["--help", "a.txt"]))
                .unwrap()
                .command,
            Command::Help
        );
    }
}

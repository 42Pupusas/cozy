//! Command-line options for the clipboard manager.

use std::ffi::OsString;
use std::path::PathBuf;

/// Default retained entries. Deep enough to be useful, shallow enough that the
/// history file stays small even when entries are screenshots.
const DEFAULT_CAPACITY: usize = 200;

/// What the binary was asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Watch the clipboard and record every new selection.
    Daemon,
    /// Print stored entries, newest first, as `id<TAB>preview`.
    List,
    /// Re-publish one stored entry to the clipboard.
    Restore(u64),
    /// Write one stored entry's bytes to stdout.
    Decode(u64),
    /// Forget everything.
    Clear,
}

/// Parsed invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipboardOptions {
    pub command: Command,
    pub capacity: usize,
    /// Where history lives; `None` means the XDG default.
    pub store_path: Option<PathBuf>,
}

impl ClipboardOptions {
    /// Parse an argument list.
    pub fn parse<I: IntoIterator<Item = OsString>>(args: I) -> Result<Self, String> {
        let mut args = args.into_iter().skip(1);
        let mut command = None;
        let mut capacity = DEFAULT_CAPACITY;
        let mut store_path = None;

        while let Some(arg) = args.next() {
            let arg = arg.to_string_lossy().into_owned();
            match arg.as_str() {
                "daemon" => command = Some(Command::Daemon),
                "list" => command = Some(Command::List),
                "clear" => command = Some(Command::Clear),
                "restore" | "decode" => {
                    let id = Self::next_value(&mut args, &arg)?;
                    let id = id
                        .parse::<u64>()
                        .map_err(|_| format!("`{arg}` needs a numeric entry id, got `{id}`"))?;
                    command = Some(if arg == "restore" {
                        Command::Restore(id)
                    } else {
                        Command::Decode(id)
                    });
                }
                "-n" | "--capacity" => {
                    let value = Self::next_value(&mut args, &arg)?;
                    capacity = value
                        .parse()
                        .map_err(|_| format!("--capacity needs a number, got `{value}`"))?;
                }
                "-f" | "--file" => {
                    store_path = Some(PathBuf::from(Self::next_value(&mut args, &arg)?));
                }
                other => return Err(format!("unknown argument `{other}`")),
            }
        }

        Ok(Self {
            command: command.ok_or_else(|| "no command given (try `daemon`)".to_string())?,
            capacity,
            store_path,
        })
    }

    fn next_value(
        args: &mut impl Iterator<Item = OsString>,
        flag: &str,
    ) -> Result<String, String> {
        args.next()
            .map(|v| v.to_string_lossy().into_owned())
            .ok_or_else(|| format!("`{flag}` needs a value"))
    }

    /// Where history is stored by default.
    ///
    /// `$XDG_DATA_HOME` rather than a config or cache directory: history is
    /// user data that should survive a cache wipe, but is not configuration.
    #[must_use]
    pub fn default_store_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let data_home = std::env::var("XDG_DATA_HOME")
            .unwrap_or_else(|_| format!("{home}/.local/share"));
        PathBuf::from(data_home).join("clipboard").join("history.bin")
    }

    /// The resolved store path.
    #[must_use]
    pub fn store_path(&self) -> PathBuf {
        self.store_path
            .clone()
            .unwrap_or_else(Self::default_store_path)
    }

    /// Usage text.
    #[must_use]
    pub fn usage(program: &std::ffi::OsStr) -> String {
        format!(
            "usage: {} <command> [options]\n\
             \n\
             commands:\n  \
               daemon              watch the clipboard and record selections\n  \
               list                print stored entries, newest first\n  \
               restore ID          re-publish a stored entry to the clipboard\n  \
               decode ID           write a stored entry's bytes to stdout\n  \
               clear               forget every stored entry\n\
             \n\
             options:\n  \
               -n, --capacity N    entries to retain (default {DEFAULT_CAPACITY})\n  \
               -f, --file PATH     history file (default $XDG_DATA_HOME/clipboard/history.bin)\n  \
               -h, --help          show this message\n",
            std::path::Path::new(program).display(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{ClipboardOptions, Command, DEFAULT_CAPACITY};
    use std::ffi::OsString;

    struct Args;

    impl Args {
        fn of(items: &[&str]) -> Vec<OsString> {
            std::iter::once(OsString::from("clipboard"))
                .chain(items.iter().map(OsString::from))
                .collect()
        }
    }

    #[test]
    fn daemon_is_the_watching_command() {
        let options = ClipboardOptions::parse(Args::of(&["daemon"])).unwrap();
        assert_eq!(options.command, Command::Daemon);
        assert_eq!(options.capacity, DEFAULT_CAPACITY);
    }

    #[test]
    fn entry_commands_carry_a_numeric_id() {
        assert_eq!(
            ClipboardOptions::parse(Args::of(&["restore", "12"]))
                .unwrap()
                .command,
            Command::Restore(12)
        );
        assert_eq!(
            ClipboardOptions::parse(Args::of(&["decode", "3"]))
                .unwrap()
                .command,
            Command::Decode(3)
        );
    }

    #[test]
    fn a_non_numeric_id_is_reported_rather_than_panicking() {
        let error = ClipboardOptions::parse(Args::of(&["restore", "abc"])).unwrap_err();
        assert!(error.contains("numeric entry id"), "{error}");
    }

    #[test]
    fn no_command_is_an_error_not_a_silent_daemon() {
        assert!(ClipboardOptions::parse(Args::of(&[])).is_err());
    }

    #[test]
    fn capacity_and_file_are_overridable() {
        let options =
            ClipboardOptions::parse(Args::of(&["daemon", "-n", "5", "-f", "/tmp/h.bin"])).unwrap();
        assert_eq!(options.capacity, 5);
        assert_eq!(options.store_path(), std::path::Path::new("/tmp/h.bin"));
    }

    #[test]
    fn a_flag_missing_its_value_is_an_error() {
        assert!(ClipboardOptions::parse(Args::of(&["daemon", "-n"])).is_err());
    }

    #[test]
    fn unknown_arguments_are_rejected() {
        assert!(ClipboardOptions::parse(Args::of(&["daemon", "--wat"])).is_err());
    }
}

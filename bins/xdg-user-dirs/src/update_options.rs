//! [`UpdateOptions`] -- the `xdg-user-dirs-update` command line.

use std::ffi::OsString;
use std::path::PathBuf;
use user_dirs::UserDir;

/// What the run should do.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    /// Print the usage text and exit.
    Help,
    /// Update the configuration.
    Update,
}

/// The parsed command line.
#[derive(Debug, PartialEq, Eq)]
pub struct UpdateOptions {
    pub command: Command,
    /// `--force`: recreate directories rather than resetting missing ones.
    pub force: bool,
    /// `--dummy-output PATH`: write here and create no directories.
    pub dummy_output: Option<PathBuf>,
    /// Each `--set NAME PATH`, in the order given.
    pub assignments: Vec<(UserDir, PathBuf)>,
}

impl UpdateOptions {
    /// Parse an argument list, skipping the program name.
    pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Self, String> {
        let mut options = Self {
            command: Command::Update,
            force: false,
            dummy_output: None,
            assignments: Vec::new(),
        };
        let mut args = args.into_iter().skip(1);
        while let Some(arg) = args.next() {
            match arg.to_string_lossy().as_ref() {
                "--help" | "-h" => {
                    options.command = Command::Help;
                    return Ok(options);
                }
                "--force" => options.force = true,
                "--dummy-output" => {
                    let path = args
                        .next()
                        .ok_or_else(|| "--dummy-output needs a path".to_owned())?;
                    options.dummy_output = Some(PathBuf::from(path));
                }
                "--set" => {
                    let name = args.next().ok_or_else(|| "--set needs a name".to_owned())?;
                    let path = args
                        .next()
                        .ok_or_else(|| "--set needs a path".to_owned())?
                        .into();
                    let name = name.to_string_lossy().into_owned();
                    let dir = UserDir::from_cli_name(&name)
                        .ok_or_else(|| format!("unknown directory name: {name}"))?;
                    options.assignments.push((dir, path));
                }
                other => return Err(format!("unknown option: {other}")),
            }
        }
        Ok(options)
    }

    /// The usage text, matching the real tool's option list.
    #[must_use]
    pub fn usage(program: &str) -> String {
        let names: Vec<_> = UserDir::ALL.iter().map(|d| d.cli_name()).collect();
        format!(
            "Usage: {program} [OPTION...] [--set NAME PATH...]\n\
             \n\
             Update the XDG user directory configuration.\n\
             \n\
             Options:\n\
             \x20 --help               Print this message and exit\n\
             \x20 --force              Force a full reset of the configuration\n\
             \x20 --dummy-output PATH  Write the configuration to PATH\n\
             \x20 --set NAME PATH      Set one directory to an absolute path\n\
             \n\
             NAME is one of: {}\n",
            names.join(", ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{Command, UpdateOptions};
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use user_dirs::UserDir;

    fn parse(args: &[&str]) -> Result<UpdateOptions, String> {
        let mut full = vec![OsString::from("xdg-user-dirs-update")];
        full.extend(args.iter().map(OsString::from));
        UpdateOptions::parse(full)
    }

    #[test]
    fn no_arguments_means_a_plain_update() {
        let options = parse(&[]).expect("parse");
        assert_eq!(options.command, Command::Update);
        assert!(!options.force);
        assert!(options.assignments.is_empty());
    }

    #[test]
    fn force_is_recognised() {
        assert!(parse(&["--force"]).expect("parse").force);
    }

    #[test]
    fn help_short_circuits() {
        assert_eq!(parse(&["--help"]).expect("parse").command, Command::Help);
    }

    #[test]
    fn dummy_output_takes_a_path() {
        let options = parse(&["--dummy-output", "/tmp/out"]).expect("parse");
        assert_eq!(options.dummy_output, Some(PathBuf::from("/tmp/out")));
    }

    #[test]
    fn set_takes_a_name_and_a_path() {
        let options = parse(&["--set", "MUSIC", "/mnt/music"]).expect("parse");
        assert_eq!(
            options.assignments,
            [(UserDir::Music, PathBuf::from("/mnt/music"))]
        );
    }

    /// The manual page shows `--set NAME PATH...`, so several assignments in
    /// one run must all be kept.
    #[test]
    fn several_assignments_are_all_kept() {
        let options = parse(&["--set", "MUSIC", "/a", "--set", "VIDEOS", "/b"]).expect("parse");
        assert_eq!(options.assignments.len(), 2);
        assert_eq!(options.assignments[1].0, UserDir::Videos);
    }

    #[test]
    fn an_unknown_directory_name_is_refused() {
        let error = parse(&["--set", "NOSUCHTHING", "/tmp"]).expect_err("should fail");
        assert!(error.contains("NOSUCHTHING"), "got: {error}");
    }

    #[test]
    fn an_unknown_option_is_refused() {
        assert!(parse(&["--wat"]).is_err());
    }

    /// A flag whose value is missing must be reported, never silently
    /// treated as if the flag were absent.
    #[test]
    fn a_flag_without_its_value_is_refused() {
        assert!(parse(&["--set"]).is_err());
        assert!(parse(&["--set", "MUSIC"]).is_err());
        assert!(parse(&["--dummy-output"]).is_err());
    }

    /// Every name the usage text advertises must actually parse.
    #[test]
    fn every_advertised_name_is_accepted() {
        for dir in UserDir::ALL {
            let options = parse(&["--set", dir.cli_name(), "/tmp/x"]).expect("parse");
            assert_eq!(options.assignments, [(dir, PathBuf::from("/tmp/x"))]);
        }
        let text = UpdateOptions::usage("xdg-user-dirs-update");
        for dir in UserDir::ALL {
            assert!(text.contains(dir.cli_name()), "{} missing", dir.cli_name());
        }
    }

    /// A non-UTF-8 path must survive parsing: it is a legal filename.
    #[test]
    fn a_non_utf8_path_survives() {
        use std::os::unix::ffi::OsStringExt;
        let raw = OsString::from_vec(vec![b'/', 0xff, 0xfe]);
        let args = vec![
            OsString::from("xdg-user-dirs-update"),
            OsString::from("--set"),
            OsString::from("MUSIC"),
            raw.clone(),
        ];
        let options = UpdateOptions::parse(args).expect("parse");
        assert_eq!(options.assignments[0].1, Path::new(&raw));
    }
}

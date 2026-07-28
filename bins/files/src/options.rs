//! [`Options`] -- the command line.
//!
//! Hand-parsed rather than pulled from a crate: there are four flags, the
//! workspace has no argument-parsing dependency, and adding one for this
//! would be the largest dependency in the binary.

use std::path::PathBuf;

/// How the browser was asked to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// Directory to open.
    pub path: PathBuf,
    pub width: i32,
    pub height: i32,
}

/// Why a command line was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OptionsError {
    /// A flag that takes a value was given none.
    MissingValue(String),
    /// A value that should have been a number was not.
    NotANumber { flag: String, value: String },
    /// A flag we do not know.
    Unknown(String),
}

impl std::fmt::Display for OptionsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingValue(flag) => write!(f, "{flag} needs a value"),
            Self::NotANumber { flag, value } => {
                write!(f, "{flag}: '{value}' is not a number")
            }
            Self::Unknown(arg) => write!(f, "unknown argument '{arg}'"),
        }
    }
}

impl std::error::Error for OptionsError {}

impl Default for Options {
    fn default() -> Self {
        Self {
            // The current directory, falling back to `/` rather than failing:
            // a process whose cwd has been deleted out from under it should
            // still open a usable window.
            path: std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
            width: 900,
            height: 600,
        }
    }
}

impl Options {
    /// Smallest window we will start with, in logical pixels.
    ///
    /// A guard against `--width 0`, which would otherwise produce a surface
    /// the compositor rejects and a startup failure with no useful message.
    const MIN_DIMENSION: i32 = 120;

    /// Parse arguments (already skipping argv[0]).
    pub fn parse(args: impl Iterator<Item = String>) -> Result<Self, OptionsError> {
        let mut options = Self::default();
        let mut args = args;

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--width" => {
                    options.width = Self::number("--width", args.next())?;
                }
                "--height" => {
                    options.height = Self::number("--height", args.next())?;
                }
                // A bare positional is the directory to open. Checked after
                // the flags so a path that happens to start with `-` is still
                // rejected as a flag rather than silently opened.
                other if !other.starts_with('-') => {
                    options.path = PathBuf::from(other);
                }
                other => return Err(OptionsError::Unknown(other.to_owned())),
            }
        }

        options.width = options.width.max(Self::MIN_DIMENSION);
        options.height = options.height.max(Self::MIN_DIMENSION);
        Ok(options)
    }

    /// Parse a numeric flag value.
    fn number(flag: &str, value: Option<String>) -> Result<i32, OptionsError> {
        let value = value.ok_or_else(|| OptionsError::MissingValue(flag.to_owned()))?;
        value.parse().map_err(|_| OptionsError::NotANumber {
            flag: flag.to_owned(),
            value,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Options, OptionsError};
    use std::path::PathBuf;

    fn parse(args: &[&str]) -> Result<Options, OptionsError> {
        Options::parse(args.iter().map(|s| (*s).to_owned()))
    }

    #[test]
    fn no_arguments_opens_the_current_directory() {
        let options = parse(&[]).expect("defaults");
        assert_eq!(options.width, 900);
        assert_eq!(options.height, 600);
        assert!(options.path.is_absolute(), "got {:?}", options.path);
    }

    #[test]
    fn a_bare_path_is_the_directory_to_open() {
        let options = parse(&["/tmp"]).expect("path");
        assert_eq!(options.path, PathBuf::from("/tmp"));
    }

    #[test]
    fn dimensions_are_overridable() {
        let options = parse(&["--width", "1280", "--height", "800"]).expect("size");
        assert_eq!(options.width, 1280);
        assert_eq!(options.height, 800);
    }

    /// A zero-sized surface is a protocol error the compositor reports as an
    /// opaque startup failure, so it is clamped here where the message can
    /// still be useful.
    #[test]
    fn absurd_dimensions_are_clamped_rather_than_passed_through() {
        let options = parse(&["--width", "0", "--height", "-50"]).expect("clamped");
        assert!(options.width >= Options::MIN_DIMENSION);
        assert!(options.height >= Options::MIN_DIMENSION);
    }

    #[test]
    fn a_flag_without_its_value_is_an_error() {
        assert_eq!(
            parse(&["--width"]),
            Err(OptionsError::MissingValue("--width".to_owned()))
        );
    }

    #[test]
    fn a_non_numeric_dimension_is_reported_rather_than_ignored() {
        assert_eq!(
            parse(&["--width", "wide"]),
            Err(OptionsError::NotANumber {
                flag: "--width".to_owned(),
                value: "wide".to_owned(),
            })
        );
    }

    #[test]
    fn an_unknown_flag_is_rejected() {
        assert_eq!(
            parse(&["--colour", "red"]),
            Err(OptionsError::Unknown("--colour".to_owned()))
        );
    }

    /// A typo'd flag must not be silently opened as a directory named
    /// "--heigth", which would show "directory not found" and hide the real
    /// mistake.
    #[test]
    fn a_typoed_flag_is_not_treated_as_a_path() {
        assert!(matches!(
            parse(&["--heigth", "800"]),
            Err(OptionsError::Unknown(_))
        ));
    }

    #[test]
    fn the_last_path_wins() {
        let options = parse(&["/tmp", "/usr"]).expect("two paths");
        assert_eq!(options.path, PathBuf::from("/usr"));
    }

    #[test]
    fn error_messages_name_the_offending_flag() {
        let err = parse(&["--width", "x"]).expect_err("must fail");
        let message = err.to_string();
        assert!(message.contains("--width"), "got {message}");
        assert!(message.contains('x'), "got {message}");
    }
}

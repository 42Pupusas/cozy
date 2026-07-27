//! Command-line parsing for `launcher`.
//!
//! Kept tiny and dependency-free (like `bins/shot`'s `ShotOptions`): the
//! whole surface is "app mode or dmenu mode, plus an optional prompt".

/// Which UI this invocation should open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LauncherMode {
    /// The default: fuzzy-search installed `.desktop` applications.
    Apps,
    /// `--dmenu`: choose among lines read from stdin, print the pick.
    Dmenu {
        /// Text pre-filled into the query box (`-p/--prompt`).
        ///
        /// dmenu's `-p` is conventionally a non-editable *label*; ours
        /// seeds the query instead, because that's the behaviour our single
        /// query line can express honestly rather than pretending to
        /// support a separate prompt widget it doesn't draw.
        prompt: String,
    },
}

/// Parsed arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LauncherOptions {
    pub mode: LauncherMode,
}

impl LauncherOptions {
    /// Parse `args` (the argv tail, excluding argv[0]).
    ///
    /// Unknown flags are an error rather than being ignored: a typo'd flag
    /// in a niri keybind would otherwise silently open the wrong mode, and
    /// the user's only symptom would be a menu that does nothing useful.
    pub fn parse<I, S>(args: I) -> Result<Self, OptionsError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut dmenu = false;
        let mut prompt = String::new();
        let mut iter = args.into_iter();

        while let Some(arg) = iter.next() {
            match arg.as_ref() {
                "--dmenu" | "-d" => dmenu = true,
                "--prompt" | "-p" => {
                    let value = iter.next().ok_or(OptionsError::MissingPromptValue)?;
                    value.as_ref().clone_into(&mut prompt);
                }
                other => return Err(OptionsError::UnknownFlag(other.to_owned())),
            }
        }

        Ok(Self {
            mode: if dmenu {
                LauncherMode::Dmenu { prompt }
            } else {
                LauncherMode::Apps
            },
        })
    }
}

/// Why parsing failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OptionsError {
    UnknownFlag(String),
    MissingPromptValue,
}

impl std::fmt::Display for OptionsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownFlag(flag) => {
                write!(f, "unknown flag `{flag}`; usage: launcher [--dmenu] [--prompt TEXT]")
            }
            Self::MissingPromptValue => write!(f, "`--prompt` needs a value"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LauncherMode, LauncherOptions, OptionsError};

    #[test]
    fn no_args_is_app_mode() {
        let opts = LauncherOptions::parse(Vec::<String>::new()).expect("parse");
        assert_eq!(opts.mode, LauncherMode::Apps);
    }

    #[test]
    fn dmenu_flag_selects_dmenu_mode() {
        for flag in ["--dmenu", "-d"] {
            let opts = LauncherOptions::parse([flag]).expect("parse");
            assert_eq!(
                opts.mode,
                LauncherMode::Dmenu {
                    prompt: String::new()
                },
                "{flag} should select dmenu mode"
            );
        }
    }

    #[test]
    fn prompt_seeds_the_query() {
        let opts = LauncherOptions::parse(["--dmenu", "--prompt", "Power: "]).expect("parse");
        assert_eq!(
            opts.mode,
            LauncherMode::Dmenu {
                prompt: "Power: ".to_owned()
            }
        );
    }

    /// Flag order must not matter -- shell pipelines are written both ways.
    #[test]
    fn flag_order_does_not_matter() {
        let a = LauncherOptions::parse(["-p", "x", "-d"]).expect("parse");
        let b = LauncherOptions::parse(["-d", "-p", "x"]).expect("parse");
        assert_eq!(a, b);
    }

    #[test]
    fn unknown_flag_is_rejected() {
        let err = LauncherOptions::parse(["--nope"]).expect_err("should reject");
        assert_eq!(err, OptionsError::UnknownFlag("--nope".to_owned()));
    }

    #[test]
    fn prompt_without_value_is_rejected() {
        let err = LauncherOptions::parse(["--prompt"]).expect_err("should reject");
        assert_eq!(err, OptionsError::MissingPromptValue);
    }
}

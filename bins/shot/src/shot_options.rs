//! Command-line options, parsed into what the capture actually needs.
//!
//! Modelled on `grim`'s interface so muscle memory transfers, but this binary
//! replaces the whole `slurp | grim -g -` pipe, so region selection is a mode
//! here rather than a separate process's stdout.

use screencopy::CaptureRegion;
use std::ffi::OsString;
use std::path::PathBuf;

/// What area to capture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureTarget {
    /// Every output, or the only one. Chosen when nothing narrower is asked
    /// for.
    AllOutputs,
    /// One output by connector name (`DP-1`, `eDP-1`), matched against
    /// `wl_output.name`.
    Output(String),
    /// A fixed rectangle, skipping the interactive overlay entirely -- the
    /// scriptable path that makes this usable from a keybind.
    Region(CaptureRegion),
    /// Drag a rectangle out interactively, replacing the `slurp` half of the
    /// classic `slurp | grim -g -` pipe.
    Selection,
}

/// Everything the binary needs from its arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShotOptions {
    pub target: CaptureTarget,
    /// Where to write the PNG; `None` means stdout, matching `grim -`.
    pub output_path: Option<PathBuf>,
    /// Composite the cursor into the image (`grim -c`).
    pub overlay_cursor: bool,
}

impl ShotOptions {
    /// Parse `args` (including argv[0]).
    ///
    /// # Errors
    ///
    /// Returns a human-readable message for unknown flags, missing values, and
    /// malformed geometry.
    pub fn parse<I: IntoIterator<Item = OsString>>(args: I) -> Result<Self, String> {
        let mut arguments = args.into_iter().skip(1);
        let mut target = CaptureTarget::AllOutputs;
        let mut output_path = None;
        let mut overlay_cursor = false;

        while let Some(argument) = arguments.next() {
            let text = argument.to_string_lossy().into_owned();
            match text.as_str() {
                "-c" | "--cursor" => overlay_cursor = true,
                "-s" | "--select" => target = CaptureTarget::Selection,
                "-o" | "--output" => {
                    let name = Self::value_for(&mut arguments, &text)?;
                    target = CaptureTarget::Output(name.to_string_lossy().into_owned());
                }
                "-g" | "--geometry" => {
                    let spec = Self::value_for(&mut arguments, &text)?;
                    target = CaptureTarget::Region(Self::parse_geometry(
                        &spec.to_string_lossy(),
                    )?);
                }
                // A bare `-` means stdout, so it must be checked before the
                // generic leading-dash rejection below.
                "-" => output_path = None,
                other if other.starts_with('-') => {
                    return Err(format!("unknown option `{other}`"));
                }
                _ => output_path = Some(PathBuf::from(argument)),
            }
        }

        Ok(Self {
            target,
            output_path,
            overlay_cursor,
        })
    }

    /// Take the value following a flag, or explain what's missing.
    fn value_for<I: Iterator<Item = OsString>>(
        arguments: &mut I,
        flag: &str,
    ) -> Result<OsString, String> {
        arguments
            .next()
            .ok_or_else(|| format!("`{flag}` needs a value"))
    }

    /// Parse `X,Y WxH` -- the format `slurp` prints, so an existing
    /// `slurp`-driven script keeps working when its pipe is replaced by this
    /// binary. `X,Y,WxH` is accepted too, since a comma there is the more
    /// natural thing to type by hand.
    fn parse_geometry(spec: &str) -> Result<CaptureRegion, String> {
        let malformed = || format!("geometry `{spec}` is not in `X,Y WxH` form");
        // Both separators are accepted interchangeably, so splitting on either
        // and taking three fields handles `X,Y WxH` and `X,Y,WxH` uniformly.
        let mut fields = spec.split([' ', ',']).filter(|f| !f.is_empty());
        let x = fields.next().ok_or_else(malformed)?;
        let y = fields.next().ok_or_else(malformed)?;
        let size = fields.next().ok_or_else(malformed)?;
        if fields.next().is_some() {
            return Err(malformed());
        }

        let (width, height) = size.split_once('x').ok_or_else(malformed)?;
        let parse = |value: &str, what: &str| -> Result<i32, String> {
            value
                .trim()
                .parse::<i32>()
                .map_err(|_| format!("geometry {what} `{value}` is not a number"))
        };
        let region = CaptureRegion {
            x: parse(x, "x")?,
            y: parse(y, "y")?,
            width: parse(width, "width")?,
            height: parse(height, "height")?,
        };
        if region.width <= 0 || region.height <= 0 {
            return Err(format!(
                "geometry must have positive width and height, got {}x{}",
                region.width, region.height
            ));
        }
        Ok(region)
    }

    /// The `--help` text.
    #[must_use]
    pub fn usage(program: &std::ffi::OsStr) -> String {
        let name = std::path::Path::new(program)
            .file_name()
            .unwrap_or(program)
            .to_string_lossy();
        format!(
            "usage: {name} [options] [file]\n\
             \n\
             Capture the screen to a PNG. With no file, writes to stdout.\n\
             \n\
             options:\n  \
               -s, --select          drag out a region interactively\n  \
               -o, --output NAME     capture only this output (e.g. DP-1)\n  \
               -g, --geometry GEOM   capture a region, as `X,Y WxH`\n  \
               -c, --cursor          include the cursor in the capture\n  \
               -h, --help            show this help\n  \
               -v, --version         show the version\n"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{CaptureTarget, ShotOptions};
    use screencopy::CaptureRegion;
    use std::ffi::OsString;
    use std::path::PathBuf;

    /// Build an argv, including the program name `parse` skips.
    fn argv(args: &[&str]) -> Vec<OsString> {
        std::iter::once("shot")
            .chain(args.iter().copied())
            .map(OsString::from)
            .collect()
    }

    #[test]
    fn no_arguments_captures_everything_to_stdout() {
        let options = ShotOptions::parse(argv(&[])).expect("valid");
        assert_eq!(options.target, CaptureTarget::AllOutputs);
        assert_eq!(options.output_path, None);
        assert!(!options.overlay_cursor);
    }

    #[test]
    fn a_bare_path_is_the_destination_file() {
        let options = ShotOptions::parse(argv(&["/tmp/shot.png"])).expect("valid");
        assert_eq!(options.output_path, Some(PathBuf::from("/tmp/shot.png")));
    }

    #[test]
    fn output_flag_selects_a_connector_by_name() {
        for flag in ["-o", "--output"] {
            let options = ShotOptions::parse(argv(&[flag, "DP-1"])).expect("valid");
            assert_eq!(options.target, CaptureTarget::Output("DP-1".into()));
        }
    }

    #[test]
    fn geometry_accepts_the_format_slurp_prints() {
        // `slurp` emits `X,Y WxH`; accepting it verbatim is what lets this
        // binary drop into an existing `slurp`-driven keybind.
        let options = ShotOptions::parse(argv(&["-g", "10,20 300x400"])).expect("valid");
        assert_eq!(
            options.target,
            CaptureTarget::Region(CaptureRegion {
                x: 10,
                y: 20,
                width: 300,
                height: 400
            })
        );
    }

    #[test]
    fn geometry_also_accepts_an_all_comma_form() {
        let options = ShotOptions::parse(argv(&["--geometry", "10,20,300x400"])).expect("valid");
        assert_eq!(
            options.target,
            CaptureTarget::Region(CaptureRegion {
                x: 10,
                y: 20,
                width: 300,
                height: 400
            })
        );
    }

    #[test]
    fn negative_origins_are_allowed_because_outputs_can_sit_left_of_zero() {
        // In a multi-output layout the leftmost screen legitimately starts at
        // a negative x, so rejecting these would break the common dual-head
        // arrangement.
        let options = ShotOptions::parse(argv(&["-g", "-1920,0 1920x1080"])).expect("valid");
        assert_eq!(
            options.target,
            CaptureTarget::Region(CaptureRegion {
                x: -1920,
                y: 0,
                width: 1920,
                height: 1080
            })
        );
    }

    #[test]
    fn a_zero_or_negative_size_is_refused() {
        // A zero-area region would otherwise reach the compositor and come
        // back as an opaque protocol failure.
        assert!(ShotOptions::parse(argv(&["-g", "0,0 0x100"])).is_err());
        assert!(ShotOptions::parse(argv(&["-g", "0,0 100x-5"])).is_err());
    }

    #[test]
    fn malformed_geometry_is_reported_not_silently_defaulted() {
        for spec in ["nonsense", "10,20", "10,20 300", "a,b 3x4"] {
            assert!(
                ShotOptions::parse(argv(&["-g", spec])).is_err(),
                "expected `{spec}` to be rejected"
            );
        }
    }

    #[test]
    fn cursor_flag_is_off_unless_asked_for() {
        for flag in ["-c", "--cursor"] {
            let options = ShotOptions::parse(argv(&[flag])).expect("valid");
            assert!(options.overlay_cursor);
        }
    }

    #[test]
    fn a_flag_missing_its_value_is_an_error_not_a_panic() {
        assert!(ShotOptions::parse(argv(&["-o"])).is_err());
        assert!(ShotOptions::parse(argv(&["-g"])).is_err());
    }

    #[test]
    fn unknown_flags_are_rejected_rather_than_treated_as_filenames() {
        // Silently writing to a file called `--outut` would be a nasty way to
        // discover a typo.
        assert!(ShotOptions::parse(argv(&["--outut", "DP-1"])).is_err());
    }

    #[test]
    fn a_bare_dash_means_stdout() {
        let options = ShotOptions::parse(argv(&["-"])).expect("valid");
        assert_eq!(options.output_path, None);
    }

    #[test]
    fn later_targets_override_earlier_ones() {
        let options =
            ShotOptions::parse(argv(&["-o", "DP-1", "-g", "0,0 10x10"])).expect("valid");
        assert!(matches!(options.target, CaptureTarget::Region(_)));
    }
}

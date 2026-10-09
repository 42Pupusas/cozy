//! Command-line controls for the pack preparation tool.

use std::path::PathBuf;

/// What to convert, at what size and rate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrepareOptions {
    pub input: PathBuf,
    pub output: PathBuf,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Luma at or above this value becomes a lit pixel.
    pub threshold: u8,
}

impl Default for PrepareOptions {
    fn default() -> Self {
        Self {
            input: PathBuf::new(),
            output: PathBuf::from("frames.mfp"),
            width: 480,
            height: 360,
            fps: 60,
            threshold: 128,
        }
    }
}

impl PrepareOptions {
    /// Parse arguments that follow the program name.
    pub fn parse<I>(arguments: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = String>,
    {
        let args: Vec<String> = arguments.into_iter().collect();
        let mut options = Self::default();
        let mut index = 0;
        while index < args.len() {
            match args[index].as_str() {
                "--input" => options.input = PathBuf::from(Self::value(&args, &mut index)?),
                "--output" => options.output = PathBuf::from(Self::value(&args, &mut index)?),
                "--width" => options.width = Self::number(&args, &mut index)?,
                "--height" => options.height = Self::number(&args, &mut index)?,
                "--fps" => options.fps = Self::number(&args, &mut index)?,
                "--threshold" => {
                    options.threshold = u8::try_from(Self::number(&args, &mut index)?)
                        .map_err(|_| "--threshold must be 0 to 255".to_owned())?;
                }
                other => return Err(format!("unknown argument: {other}")),
            }
            index += 1;
        }
        options.validate()?;
        Ok(options)
    }

    /// Reject settings that cannot describe a real pack.
    fn validate(&self) -> Result<(), String> {
        if self.input.as_os_str().is_empty() {
            return Err("--input is required".to_owned());
        }
        if self.width == 0 || self.height == 0 {
            return Err("--width and --height must be non-zero".to_owned());
        }
        if self.fps == 0 {
            return Err("--fps must be non-zero".to_owned());
        }
        Ok(())
    }

    /// The arguments handed to ffmpeg to produce raw 8-bit luma frames.
    #[must_use]
    pub fn ffmpeg_arguments(&self) -> Vec<String> {
        vec![
            "-hide_banner".to_owned(),
            "-loglevel".to_owned(),
            "error".to_owned(),
            "-i".to_owned(),
            self.input.display().to_string(),
            "-vf".to_owned(),
            format!(
                "fps={},scale={}:{}:flags=bilinear",
                self.fps, self.width, self.height
            ),
            "-f".to_owned(),
            "rawvideo".to_owned(),
            "-pix_fmt".to_owned(),
            "gray".to_owned(),
            "-".to_owned(),
        ]
    }

    /// Bytes of raw luma in one frame.
    #[must_use]
    pub const fn luma_frame_bytes(&self) -> usize {
        self.width as usize * self.height as usize
    }

    /// The usage text for the tool.
    #[must_use]
    pub fn usage(program: &str) -> String {
        format!(
            "usage: {program} --input FILE [--output FILE] [--width N] [--height N] [--fps N] [--threshold 0-255]\n\
             \n\
             Converts any video ffmpeg can read into a packed 1-bit-per-pixel clip\n\
             for the renderer stress harness. Defaults: 480x360, 60 fps, threshold 128."
        )
    }

    fn value(args: &[String], index: &mut usize) -> Result<String, String> {
        *index += 1;
        args.get(*index)
            .cloned()
            .ok_or_else(|| format!("{} needs a value", args[*index - 1]))
    }

    fn number(args: &[String], index: &mut usize) -> Result<u32, String> {
        let raw = Self::value(args, index)?;
        raw.parse()
            .map_err(|_| format!("{} must be a number, got {raw}", args[*index - 1]))
    }
}

#[cfg(test)]
mod tests {
    use super::PrepareOptions;

    #[test]
    fn parses_every_control() {
        let options = PrepareOptions::parse(
            [
                "--input",
                "a.mp4",
                "--output",
                "b.mfp",
                "--width",
                "64",
                "--height",
                "48",
                "--fps",
                "30",
                "--threshold",
                "200",
            ]
            .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(options.width, 64);
        assert_eq!(options.height, 48);
        assert_eq!(options.fps, 30);
        assert_eq!(options.threshold, 200);
    }

    #[test]
    fn requires_an_input() {
        assert!(PrepareOptions::parse(["--width".to_owned(), "8".to_owned()]).is_err());
    }

    #[test]
    fn rejects_zero_geometry() {
        let error = PrepareOptions::parse(["--input", "a.mp4", "--width", "0"].map(str::to_owned))
            .unwrap_err();
        assert!(error.contains("non-zero"));
    }

    #[test]
    fn rejects_an_unknown_argument() {
        assert!(PrepareOptions::parse(["--nope".to_owned()]).is_err());
    }

    #[test]
    fn defaults_target_sixty_frames_per_second() {
        let options = PrepareOptions::parse(["--input".to_owned(), "a.mp4".to_owned()]).unwrap();
        assert_eq!(options.fps, 60);
        assert_eq!(options.luma_frame_bytes(), 480 * 360);
    }

    #[test]
    fn ffmpeg_arguments_request_raw_luma_at_the_chosen_rate() {
        let options = PrepareOptions::parse(
            [
                "--input", "a.mp4", "--fps", "60", "--width", "8", "--height", "4",
            ]
            .map(str::to_owned),
        )
        .unwrap();
        let arguments = options.ffmpeg_arguments();
        assert!(arguments.contains(&"gray".to_owned()));
        assert!(arguments.contains(&"fps=60,scale=8:4:flags=bilinear".to_owned()));
    }
}

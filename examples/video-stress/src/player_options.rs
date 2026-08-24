//! Command-line controls shared by both playback front ends.

use std::path::PathBuf;
use std::time::Duration;

use crate::subcell_grid::SubcellGrid;
use crate::video_layout::FitMode;

/// What to play, for how long, and at what rate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerOptions {
    pub pack: PathBuf,
    /// Overrides the rate recorded in the pack header when set.
    pub fps: Option<u32>,
    /// Stops after this many frames when set.
    pub frames: Option<u64>,
    /// Stops after this much wall-clock time when set.
    pub duration: Option<Duration>,
    /// Reports progress every this many frames.
    pub report_every: u64,
    /// How each cell divides into independently lit blocks.
    pub subcells: SubcellGrid,
    /// Whether coverage is dithered rather than hard-thresholded.
    pub dithered: bool,
    /// Overrides the cell grid read from the terminal when set.
    pub grid: Option<(u32, u32)>,
    /// How the clip fills a graphical surface.
    pub fit: FitMode,
}

impl Default for PlayerOptions {
    fn default() -> Self {
        Self {
            pack: PathBuf::from("frames.mfp"),
            fps: None,
            frames: None,
            duration: None,
            report_every: 600,
            subcells: SubcellGrid::Sextant,
            dithered: true,
            grid: None,
            fit: FitMode::Fit,
        }
    }
}

impl PlayerOptions {
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
                "--pack" => options.pack = PathBuf::from(Self::value(&args, &mut index)?),
                "--fps" => {
                    options.fps = Some(
                        u32::try_from(Self::number(&args, &mut index)?)
                            .map_err(|_| "--fps is out of range".to_owned())?,
                    );
                }
                "--frames" => options.frames = Some(Self::number(&args, &mut index)?),
                "--seconds" => {
                    options.duration = Some(Duration::from_secs(Self::number(&args, &mut index)?));
                }
                "--report-every" => options.report_every = Self::number(&args, &mut index)?,
                "--cells" => {
                    options.subcells = SubcellGrid::parse(&Self::value(&args, &mut index)?)?;
                }
                "--no-dither" => options.dithered = false,
                "--grid" => options.grid = Some(Self::grid(&Self::value(&args, &mut index)?)?),
                "--fit" => options.fit = FitMode::parse(&Self::value(&args, &mut index)?)?,
                other => return Err(format!("unknown argument: {other}")),
            }
            index += 1;
        }
        Ok(options)
    }

    /// Whether the run must stop after `presented` frames and `elapsed` time.
    #[must_use]
    pub fn is_complete(&self, presented: u64, elapsed: Duration) -> bool {
        self.frames.is_some_and(|limit| presented >= limit)
            || self.duration.is_some_and(|limit| elapsed >= limit)
    }

    /// Whether progress should be reported after `presented` frames.
    #[must_use]
    pub const fn should_report(&self, presented: u64) -> bool {
        self.report_every != 0 && presented.is_multiple_of(self.report_every)
    }

    /// The rate to play at, given the rate stored in the pack.
    #[must_use]
    pub fn rate(&self, pack_fps: u32) -> u32 {
        self.fps.unwrap_or(pack_fps).max(1)
    }

    /// The usage text shared by both players.
    #[must_use]
    pub fn usage(program: &str) -> String {
        format!(
            "usage: {program} --pack FILE [options]\n\
             \n\
             Plays a packed clip produced by video-prepare and reports renderer\n\
             throughput.\n\
             \n\
             options:\n\
               --fps N           override the rate stored in the pack header\n\
               --frames N        stop after N frames\n\
               --seconds N       stop after N seconds\n\
               --report-every N  report progress every N frames (0 disables)\n\
               --cells MODE      half, quadrant, or sextant (default sextant)\n\
               --grid CxR        override the cell grid read from the terminal\n\
               --no-dither       hard-threshold coverage instead of dithering
               --fit MODE        fit, fill, or stretch (graphical players only)"
        )
    }

    /// Parse a `COLUMNSxROWS` grid override.
    fn grid(raw: &str) -> Result<(u32, u32), String> {
        let (columns, rows) = raw
            .split_once(['x', 'X'])
            .ok_or_else(|| format!("--grid must look like 200x60, got {raw}"))?;
        let parse = |value: &str| {
            value
                .parse::<u32>()
                .ok()
                .filter(|parsed| *parsed > 0)
                .ok_or_else(|| format!("--grid extents must be positive numbers, got {raw}"))
        };
        Ok((parse(columns)?, parse(rows)?))
    }

    fn value(args: &[String], index: &mut usize) -> Result<String, String> {
        *index += 1;
        args.get(*index)
            .cloned()
            .ok_or_else(|| format!("{} needs a value", args[*index - 1]))
    }

    fn number(args: &[String], index: &mut usize) -> Result<u64, String> {
        let raw = Self::value(args, index)?;
        raw.parse()
            .map_err(|_| format!("{} must be a number, got {raw}", args[*index - 1]))
    }
}

#[cfg(test)]
mod tests {
    use super::{Duration, FitMode, PlayerOptions, SubcellGrid};

    #[test]
    fn the_fit_mode_can_be_chosen() {
        let options = PlayerOptions::parse(["--fit", "fill"].map(str::to_owned)).unwrap();
        assert_eq!(options.fit, FitMode::Fill);
    }

    #[test]
    fn fitting_is_the_default() {
        assert_eq!(PlayerOptions::default().fit, FitMode::Fit);
    }

    #[test]
    fn an_unknown_fit_mode_is_rejected() {
        assert!(PlayerOptions::parse(["--fit", "cover"].map(str::to_owned)).is_err());
    }

    #[test]
    fn parses_every_control() {
        let options = PlayerOptions::parse(
            ["--pack", "a.mfp", "--fps", "60", "--frames", "100", "--seconds", "5"]
                .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(options.fps, Some(60));
        assert_eq!(options.frames, Some(100));
        assert_eq!(options.duration, Some(Duration::from_secs(5)));
    }

    #[test]
    fn the_pack_rate_applies_when_no_override_is_given() {
        let options = PlayerOptions::parse(["--pack".to_owned(), "a.mfp".to_owned()]).unwrap();
        assert_eq!(options.rate(30), 30);
    }

    #[test]
    fn an_override_replaces_the_pack_rate() {
        let options =
            PlayerOptions::parse(["--fps", "144"].map(str::to_owned)).unwrap();
        assert_eq!(options.rate(30), 144);
    }

    #[test]
    fn an_unlimited_run_never_completes() {
        let options = PlayerOptions::default();
        assert!(!options.is_complete(1_000_000, Duration::from_hours(1)));
    }

    #[test]
    fn a_frame_limit_stops_the_run() {
        let options = PlayerOptions::parse(["--frames", "10"].map(str::to_owned)).unwrap();
        assert!(!options.is_complete(9, Duration::ZERO));
        assert!(options.is_complete(10, Duration::ZERO));
    }

    #[test]
    fn a_time_limit_stops_the_run() {
        let options = PlayerOptions::parse(["--seconds", "5"].map(str::to_owned)).unwrap();
        assert!(!options.is_complete(0, Duration::from_secs(4)));
        assert!(options.is_complete(0, Duration::from_secs(5)));
    }

    #[test]
    fn reporting_can_be_disabled() {
        let options = PlayerOptions::parse(["--report-every", "0"].map(str::to_owned)).unwrap();
        assert!(!options.should_report(100));
    }

    #[test]
    fn rejects_an_unknown_argument() {
        assert!(PlayerOptions::parse(["--nope".to_owned()]).is_err());
    }

    #[test]
    fn the_finest_cell_mode_is_the_default() {
        let options = PlayerOptions::default();
        assert_eq!(options.subcells, SubcellGrid::Sextant);
        assert!(options.dithered);
        assert_eq!(options.grid, None);
    }

    #[test]
    fn the_cell_mode_can_be_chosen() {
        let options = PlayerOptions::parse(["--cells", "quadrant"].map(str::to_owned)).unwrap();
        assert_eq!(options.subcells, SubcellGrid::Quadrant);
        assert!(PlayerOptions::parse(["--cells", "octant"].map(str::to_owned)).is_err());
    }

    #[test]
    fn dithering_can_be_turned_off() {
        let options = PlayerOptions::parse(["--no-dither".to_owned()]).unwrap();
        assert!(!options.dithered);
    }

    #[test]
    fn a_grid_override_is_parsed() {
        let options = PlayerOptions::parse(["--grid", "240x70"].map(str::to_owned)).unwrap();
        assert_eq!(options.grid, Some((240, 70)));
    }

    #[test]
    fn a_malformed_grid_override_is_rejected() {
        assert!(PlayerOptions::parse(["--grid", "240"].map(str::to_owned)).is_err());
        assert!(PlayerOptions::parse(["--grid", "0x70"].map(str::to_owned)).is_err());
        assert!(PlayerOptions::parse(["--grid", "axb"].map(str::to_owned)).is_err());
    }
}

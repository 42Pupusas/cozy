//! Paced playback through a terminal's escape-sequence path.
//!
//! This front end stresses the vt parser and the grid rather than the shm
//! blitter: every frame becomes a cursor-home sequence plus one full screen of
//! block characters. Running it inside the workspace terminal therefore
//! measures parse, grid store, and glyph raster throughput together.

use std::io::Write;
use std::time::Instant;

use crate::ascii_ramp::AsciiRamp;
use crate::frame_pack::FramePack;
use crate::playback_clock::{PlaybackClock, Tick};
use crate::playback_stats::PlaybackStats;
use crate::player_options::PlayerOptions;
use crate::terminal_size::TerminalSize;

/// Writes packed frames to a terminal as block-character screens.
pub struct TerminalPlayer {
    pack: FramePack,
    ramp: AsciiRamp,
    options: PlayerOptions,
    left_margin: u32,
    top_margin: u32,
}

impl TerminalPlayer {
    /// Build a player that fits the clip into `size` without distorting it.
    #[must_use]
    pub fn new(pack: FramePack, size: TerminalSize, options: PlayerOptions) -> Self {
        let header = pack.header();
        let source = Self::subcell_source(header.width, header.height, &options);
        let (columns, rows) = size.fit(source.0, source.1);
        let ramp = AsciiRamp::new(
            header.width,
            header.height,
            columns,
            rows,
            options.subcells,
        )
        .with_dither(options.dithered);
        Self {
            pack,
            ramp,
            left_margin: size.columns().saturating_sub(columns) / 2,
            top_margin: size.rows().saturating_sub(rows) / 2,
            options,
        }
    }

    /// The clip size in subcell units, which is what the cell grid must fit.
    ///
    /// A subcell is not square, so fitting the clip against raw pixels would
    /// stretch the picture by the subcell aspect ratio.
    const fn subcell_source(width: u32, height: u32, options: &PlayerOptions) -> (u32, u32) {
        (
            width * options.subcells.rows(),
            height * options.subcells.columns(),
        )
    }

    /// The cell grid the clip was fitted into.
    #[must_use]
    pub const fn grid(&self) -> (u32, u32) {
        (self.ramp.columns(), self.ramp.rows())
    }

    /// The sample grid the clip is reduced onto.
    #[must_use]
    pub const fn samples(&self) -> (u32, u32) {
        (self.ramp.sample_width(), self.ramp.sample_height())
    }

    /// Play until the configured limit, returning the run statistics.
    pub fn play(&mut self, out: &mut impl Write) -> std::io::Result<PlaybackStats> {
        let rate = self.options.rate(self.pack.header().fps);
        let mut clock = PlaybackClock::start(rate);
        let mut stats = PlaybackStats::start("terminal");
        let started = Instant::now();
        let mut screen = String::new();
        Self::enter_full_screen(out)?;
        while !self.options.is_complete(stats.presented(), started.elapsed()) {
            match clock.tick() {
                Tick::Present { index, skipped } => {
                    let paint_started = Instant::now();
                    self.draw_frame(out, index, &mut screen)?;
                    stats.record(paint_started.elapsed(), skipped);
                }
                Tick::Wait => std::thread::sleep(clock.time_until_next(Instant::now())),
            }
        }
        Self::leave_full_screen(out)?;
        Ok(stats)
    }

    /// Render and emit one frame at its centred position.
    fn draw_frame(
        &mut self,
        out: &mut impl Write,
        index: usize,
        screen: &mut String,
    ) -> std::io::Result<()> {
        let Some(frame) = self.pack.frame(index) else {
            return Ok(());
        };
        self.ramp.render_into(&frame, screen);
        for (row, line) in screen.lines().enumerate() {
            let cursor = format!(
                "\x1b[{};{}H",
                self.top_margin as usize + row + 1,
                self.left_margin + 1
            );
            out.write_all(cursor.as_bytes())?;
            out.write_all(line.as_bytes())?;
        }
        out.flush()
    }

    /// Switch to the alternate screen and hide the cursor.
    fn enter_full_screen(out: &mut impl Write) -> std::io::Result<()> {
        out.write_all(b"\x1b[?1049h\x1b[?25l\x1b[2J")?;
        out.flush()
    }

    /// Restore the original screen and cursor.
    fn leave_full_screen(out: &mut impl Write) -> std::io::Result<()> {
        out.write_all(b"\x1b[?25h\x1b[?1049l")?;
        out.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::{FramePack, PlayerOptions, TerminalPlayer, TerminalSize};
    use crate::pack_header::PackHeader;
    use crate::subcell_grid::SubcellGrid;

    fn pack() -> FramePack {
        let header = PackHeader::new(4, 2, 2, 60).unwrap();
        let mut bytes = header.to_bytes().to_vec();
        bytes.extend_from_slice(&[0xF0, 0x00, 0x00, 0xF0]);
        FramePack::from_bytes(&bytes, 0, 1).unwrap()
    }

    fn options(frames: u64) -> PlayerOptions {
        PlayerOptions::parse(
            ["--frames", &frames.to_string(), "--report-every", "0"].map(str::to_owned),
        )
        .unwrap()
    }

    fn size() -> TerminalSize {
        TerminalSize::default().with_grid(4, 1)
    }

    #[test]
    fn a_run_presents_the_requested_frame_count() {
        let mut player = TerminalPlayer::new(pack(), size(), options(3));
        let mut out = Vec::new();
        let stats = player.play(&mut out).unwrap();
        assert_eq!(stats.presented(), 3);
    }

    #[test]
    fn output_enters_and_leaves_the_alternate_screen() {
        let mut player = TerminalPlayer::new(pack(), size(), options(1));
        let mut out = Vec::new();
        player.play(&mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("\x1b[?1049h"));
        assert!(text.ends_with("\x1b[?1049l"));
    }

    #[test]
    fn every_frame_positions_its_first_row() {
        let mut player = TerminalPlayer::new(pack(), size(), options(2));
        let mut out = Vec::new();
        player.play(&mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.matches("\x1b[1;1H").count(), 2);
    }

    #[test]
    fn the_clip_is_fitted_rather_than_stretched() {
        let size = TerminalSize::default().with_grid(200, 60);
        let player = TerminalPlayer::new(pack(), size, options(1));
        let (columns, rows) = player.grid();
        assert!(columns <= 200);
        assert!(rows <= 60);
        assert!(columns > 1 && rows > 1);
    }

    #[test]
    fn sextants_give_more_samples_than_half_blocks() {
        let size = TerminalSize::default().with_grid(120, 34);
        let mut half_options = options(1);
        half_options.subcells = SubcellGrid::Half;
        let half = TerminalPlayer::new(pack(), size, half_options);
        let sextant = TerminalPlayer::new(pack(), size, options(1));
        let (half_x, half_y) = half.samples();
        let (sextant_x, sextant_y) = sextant.samples();
        assert!(sextant_x * sextant_y > half_x * half_y);
    }
}

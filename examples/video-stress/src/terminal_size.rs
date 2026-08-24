//! The real cell grid of the attached terminal.
//!
//! `COLUMNS` and `LINES` are shell variables, not environment variables, so a
//! child process almost never sees them. Asking the kernel with `TIOCGWINSZ`
//! reports the true grid, and also the window's pixel size, which gives the
//! cell aspect ratio needed to letterbox a clip without distorting it.

use std::os::fd::AsRawFd;

use pty::{PtyIoctl, WinSize};
use ququmatz::RawFd;

/// Grid used when neither the ioctl nor the environment answers.
const FALLBACK_COLUMNS: u32 = 120;
const FALLBACK_ROWS: u32 = 34;

/// Cell shape assumed when the terminal reports no pixel size.
const ASSUMED_CELL_WIDTH: u32 = 1;
const ASSUMED_CELL_HEIGHT: u32 = 2;

/// The terminal's cell grid and the pixel shape of one cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalSize {
    columns: u32,
    rows: u32,
    cell_width: u32,
    cell_height: u32,
}

impl Default for TerminalSize {
    fn default() -> Self {
        Self {
            columns: FALLBACK_COLUMNS,
            rows: FALLBACK_ROWS,
            cell_width: ASSUMED_CELL_WIDTH,
            cell_height: ASSUMED_CELL_HEIGHT,
        }
    }
}

impl TerminalSize {
    /// Read the size of the terminal attached to standard output.
    ///
    /// Falls back to the environment, then to a fixed grid, so a run that is
    /// piped to a file still produces frames.
    #[must_use]
    pub fn query() -> Self {
        usize::try_from(std::io::stdout().as_raw_fd())
            .ok()
            .map(RawFd::from_raw)
            .and_then(|fd| fd.get_win_size().ok())
            .and_then(Self::from_win_size)
            .unwrap_or_else(Self::from_environment)
    }

    /// Build a size from a `TIOCGWINSZ` answer, rejecting an empty grid.
    #[must_use]
    pub fn from_win_size(win_size: WinSize) -> Option<Self> {
        if win_size.cols == 0 || win_size.rows == 0 {
            return None;
        }
        let columns = u32::from(win_size.cols);
        let rows = u32::from(win_size.rows);
        let cell_width = u32::from(win_size.xpixel) / columns;
        let cell_height = u32::from(win_size.ypixel) / rows;
        Some(Self {
            columns,
            rows,
            cell_width: if cell_width == 0 {
                ASSUMED_CELL_WIDTH
            } else {
                cell_width
            },
            cell_height: if cell_height == 0 {
                ASSUMED_CELL_HEIGHT
            } else {
                cell_height
            },
        })
    }

    /// Build a size from `COLUMNS` and `LINES`, or the fallback grid.
    #[must_use]
    pub fn from_environment() -> Self {
        let read = |name: &str, fallback: u32| {
            std::env::var(name)
                .ok()
                .and_then(|value| value.parse().ok())
                .filter(|value| *value > 0)
                .unwrap_or(fallback)
        };
        Self {
            columns: read("COLUMNS", FALLBACK_COLUMNS),
            rows: read("LINES", FALLBACK_ROWS),
            ..Self::default()
        }
    }

    /// Replace the grid while keeping the measured cell shape.
    #[must_use]
    pub const fn with_grid(self, columns: u32, rows: u32) -> Self {
        Self {
            columns,
            rows,
            ..self
        }
    }

    /// The grid width in cells.
    #[must_use]
    pub const fn columns(&self) -> u32 {
        self.columns
    }

    /// The grid height in cells.
    #[must_use]
    pub const fn rows(&self) -> u32 {
        self.rows
    }

    /// The width of one cell in pixels.
    #[must_use]
    pub const fn cell_width(&self) -> u32 {
        self.cell_width
    }

    /// The height of one cell in pixels.
    #[must_use]
    pub const fn cell_height(&self) -> u32 {
        self.cell_height
    }

    /// The largest cell rectangle that shows a `source_width` by
    /// `source_height` clip without distorting it.
    ///
    /// One cell is taller than it is wide, so a grid filled edge to edge
    /// stretches the picture vertically. Reserving the unused cells instead
    /// keeps circles round.
    #[must_use]
    pub fn fit(&self, source_width: u32, source_height: u32) -> (u32, u32) {
        if source_width == 0 || source_height == 0 {
            return (self.columns, self.rows);
        }
        let wide = u64::from(source_width) * u64::from(self.cell_height);
        let tall = u64::from(source_height) * u64::from(self.cell_width);
        let columns = u64::from(self.columns);
        let rows = u64::from(self.rows);
        let (fitted_columns, fitted_rows) = if columns * tall <= rows * wide {
            (columns, columns * tall / wide)
        } else {
            (rows * wide / tall, rows)
        };
        (
            Self::at_least_one(fitted_columns, self.columns),
            Self::at_least_one(fitted_rows, self.rows),
        )
    }

    /// Clamp a fitted extent into the grid, never returning zero cells.
    fn at_least_one(fitted: u64, limit: u32) -> u32 {
        u32::try_from(fitted).unwrap_or(limit).clamp(1, limit.max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::{TerminalSize, WinSize};

    fn win_size(cols: u16, rows: u16, xpixel: u16, ypixel: u16) -> WinSize {
        WinSize {
            rows,
            cols,
            xpixel,
            ypixel,
        }
    }

    #[test]
    fn reads_the_grid_and_cell_shape_from_the_ioctl() {
        let size = TerminalSize::from_win_size(win_size(200, 50, 1600, 1000)).unwrap();
        assert_eq!(size.columns(), 200);
        assert_eq!(size.rows(), 50);
        assert_eq!(size.cell_width(), 8);
        assert_eq!(size.cell_height(), 20);
    }

    #[test]
    fn an_empty_grid_is_rejected() {
        assert!(TerminalSize::from_win_size(win_size(0, 50, 0, 0)).is_none());
        assert!(TerminalSize::from_win_size(win_size(200, 0, 0, 0)).is_none());
    }

    #[test]
    fn a_terminal_without_pixel_size_assumes_tall_cells() {
        let size = TerminalSize::from_win_size(win_size(80, 24, 0, 0)).unwrap();
        assert_eq!(size.cell_width(), 1);
        assert_eq!(size.cell_height(), 2);
    }

    #[test]
    fn a_wide_grid_is_limited_by_its_height() {
        let size = TerminalSize::default().with_grid(400, 50);
        assert_eq!(size.fit(4, 3), (133, 50));
    }

    #[test]
    fn a_tall_grid_is_limited_by_its_width() {
        let size = TerminalSize::default().with_grid(100, 400);
        assert_eq!(size.fit(4, 3), (100, 37));
    }

    #[test]
    fn a_square_clip_on_square_cells_fills_a_square() {
        let size = TerminalSize::from_win_size(win_size(100, 100, 800, 800))
            .unwrap()
            .with_grid(100, 100);
        assert_eq!(size.fit(360, 360), (100, 100));
    }

    #[test]
    fn a_degenerate_clip_falls_back_to_the_whole_grid() {
        let size = TerminalSize::default().with_grid(80, 24);
        assert_eq!(size.fit(0, 0), (80, 24));
    }

    #[test]
    fn the_fitted_extent_never_collapses_to_zero() {
        let size = TerminalSize::default().with_grid(4, 40);
        let (columns, rows) = size.fit(1, 10_000);
        assert!(columns >= 1);
        assert!(rows >= 1);
    }
}

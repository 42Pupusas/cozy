//! Targeted grid workloads for `perf`.
//!
//! These intentionally omit the renderer so a profile answers one question at
//! a time. Run the already-built executable directly, not through Cargo:
//!
//!     cargo build --release -p tty-benches --example grid_hotspots
//!     perf record -g target/release/examples/grid_hotspots print bytes=16777216
//!     perf report --stdio --sort=symbol
//!
//! Modes: `print` (parser + `Grid::apply`), `snapshot` (also `take_dirty`), and
//! `scroll` (the screen's scrollback rotation without parser work).

#![allow(clippy::cast_precision_loss)]

use std::time::Instant;

use grid::Grid;
use tty_benches::{COLS, ROWS};
use vt_parser::Parser;

const DEFAULT_BYTES: usize = 16 * 1024 * 1024;
const LINE: &[u8] = b"the quick brown fox jumps over the lazy dog 0123456789\r\n";

struct GridHotspotWorkload {
    mode: WorkloadMode,
    target_bytes: usize,
}

#[derive(Clone, Copy)]
enum WorkloadMode {
    Print,
    Snapshot,
    Scroll,
}

struct Measurement {
    elapsed: std::time::Duration,
    bytes: usize,
    snapshots: usize,
    scrolls: usize,
    /// Scroll-path work counters from the grid, when built with `--features
    /// stats`; `None` in the default build.
    stats: Option<grid::ScrollStats>,
}

/// Read the grid's scroll counters -- gated wrapper so the workload code
/// stays free of scattered `#[cfg]` blocks.
#[cfg(feature = "stats")]
#[expect(
    clippy::unnecessary_wraps,
    reason = "Option is the interface: the no-stats twin returns None"
)]
fn scroll_stats(grid: &Grid) -> Option<grid::ScrollStats> {
    Some(grid.scroll_stats())
}

#[cfg(not(feature = "stats"))]
#[expect(
    clippy::missing_const_for_fn,
    reason = "signature must match the stats twin, which cannot be const"
)]
fn scroll_stats(_grid: &Grid) -> Option<grid::ScrollStats> {
    None
}

impl WorkloadMode {
    fn parse(value: &str) -> Self {
        match value {
            "snapshot" => Self::Snapshot,
            "scroll" => Self::Scroll,
            _ => Self::Print,
        }
    }
}

impl GridHotspotWorkload {
    fn from_args(args: &[String]) -> Self {
        let mode = args
            .get(1)
            .map_or(WorkloadMode::Print, |arg| WorkloadMode::parse(arg));
        let target_bytes = args
            .iter()
            .find_map(|arg| arg.strip_prefix("bytes=")?.parse().ok())
            .unwrap_or(DEFAULT_BYTES);
        Self { mode, target_bytes }
    }

    fn run(&self) -> Measurement {
        match self.mode {
            WorkloadMode::Scroll => self.run_scroll(),
            WorkloadMode::Print | WorkloadMode::Snapshot => self.run_stream(),
        }
    }

    fn run_stream(&self) -> Measurement {
        let mut parser = Parser::new();
        let mut grid = Grid::new(COLS, ROWS);
        let mut bytes = 0;
        let mut snapshots = 0;
        let start = Instant::now();
        while bytes < self.target_bytes {
            let length = (self.target_bytes - bytes).min(LINE.len());
            parser.feed(&LINE[..length], |event| grid.apply(&event));
            bytes += length;
            if matches!(self.mode, WorkloadMode::Snapshot) && bytes.is_multiple_of(64 * 4096) {
                let _ = grid.take_dirty();
                snapshots += 1;
            }
        }
        Measurement {
            elapsed: start.elapsed(),
            bytes,
            snapshots,
            scrolls: 0,
            stats: scroll_stats(&grid),
        }
    }

    fn run_scroll(&self) -> Measurement {
        let mut grid = Grid::new(COLS, ROWS);
        let mut parser = Parser::new();
        let mut seed = Vec::with_capacity(self.target_bytes.min(DEFAULT_BYTES));
        while seed.len() < self.target_bytes.min(DEFAULT_BYTES) {
            seed.extend_from_slice(LINE);
        }
        parser.feed(&seed, |event| grid.apply(&event));
        let start = Instant::now();
        let scrolls = self.target_bytes / LINE.len();
        for _ in 0..scrolls {
            grid.scroll_viewport(1);
            std::hint::black_box(grid.take_dirty());
            grid.scroll_viewport(-1);
            std::hint::black_box(grid.take_dirty());
        }
        Measurement {
            elapsed: start.elapsed(),
            bytes: 0,
            snapshots: 2 * scrolls,
            scrolls,
            stats: scroll_stats(&grid),
        }
    }

    fn print(&self) {
        let measurement = self.run();
        let seconds = measurement.elapsed.as_secs_f64();
        println!(
            "mode: {}",
            match self.mode {
                WorkloadMode::Print => "print",
                WorkloadMode::Snapshot => "snapshot",
                WorkloadMode::Scroll => "scroll",
            }
        );
        println!("elapsed: {seconds:.6} s");
        if measurement.bytes > 0 {
            println!(
                "throughput: {:.1} MiB/s",
                measurement.bytes as f64 / seconds / 1_048_576.0
            );
        }
        println!(
            "snapshots: {} | scrolls: {}",
            measurement.snapshots, measurement.scrolls
        );
        println!(
            "cell: {} bytes | visible row: {} bytes",
            size_of::<grid::Cell>(),
            size_of::<grid::Cell>() * usize::from(COLS),
        );
        if let Some(stats) = measurement.stats {
            // Convert row counts to bytes so the memmove/memset volume is
            // directly comparable with the input stream size.
            let row_bytes = size_of::<grid::Cell>() as u64 * u64::from(COLS);
            // Compaction counters are gone: the ring buffer never compacts
            // (eviction is a head bump, rows never move), so only the
            // scroll/blank volume remains interesting.
            println!(
                "scroll stats: {} scroll_up calls | {} rows blanked ({:.1} MiB)",
                stats.scroll_up_calls,
                stats.rows_blanked,
                stats.rows_blanked as f64 * row_bytes as f64 / 1_048_576.0,
            );
        }
    }
}

fn main() {
    GridHotspotWorkload::from_args(&std::env::args().collect::<Vec<_>>()).print();
}

//! Draws one tile scene several ways and reports what each costs, on the
//! client, in the compositor and on the GPU.
//!
//!   cargo run --release -p tile-compare -- buffer   [seconds] [tiles] [period] [radius]
//!   cargo run --release -p tile-compare -- damaged  [seconds] [tiles] [period] [radius]
//!   cargo run --release -p tile-compare -- sections [seconds] [tiles] [period] [radius]
//!   cargo run --release -p tile-compare -- idle     [seconds]
//!   cargo run --release -p tile-compare -- sweep    [seconds] [repeats]
//!
//! One tile in `period` changes colour per frame (default 4). `damaged` is
//! `buffer` telling the compositor only the tiles that changed. `idle` opens
//! no window. `sweep` runs a fixed set of configurations round-robin as
//! child processes and prints per-figure medians.

mod buffer_mode;
mod compositor_probe;
mod drm_usage;
mod gpu_probe;
mod process_sample;
mod run_numbers;
mod run_report;
mod section_mode;
mod sweep;
mod tile_diff;
mod tile_scene;

use std::time::Duration;

use platform::toplevel::LogicalSize;

use crate::sweep::{Sweep, SweepConfig};

enum Mode {
    Buffer,
    Damaged,
    Sections,
    Idle,
    Sweep,
}

struct Args {
    mode: Mode,
    seconds: u64,
    tiles: usize,
    period: u64,
    radius: u16,
    repeats: usize,
}

impl Args {
    const SIZE: LogicalSize = LogicalSize {
        width: 1200,
        height: 800,
    };

    fn parse() -> Option<Self> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let mode = match args.first().map(String::as_str) {
            Some("buffer") => Mode::Buffer,
            Some("damaged") => Mode::Damaged,
            Some("sections") => Mode::Sections,
            Some("idle") => Mode::Idle,
            Some("sweep") => Mode::Sweep,
            _ => return None,
        };
        Some(Self {
            mode,
            seconds: args.get(1).and_then(|s| s.parse().ok()).unwrap_or(10),
            tiles: args.get(2).and_then(|s| s.parse().ok()).unwrap_or(40),
            period: args.get(3).and_then(|s| s.parse().ok()).unwrap_or(4),
            radius: args.get(4).and_then(|s| s.parse().ok()).unwrap_or(0),
            repeats: args.get(2).and_then(|s| s.parse().ok()).unwrap_or(5),
        })
    }

    fn run(&self) {
        let scene = tile_scene::TileScene::new(self.tiles, self.period, self.radius);
        match self.mode {
            Mode::Buffer => buffer_mode::BufferMode::run(Self::SIZE, scene, self.seconds, false),
            Mode::Damaged => buffer_mode::BufferMode::run(Self::SIZE, scene, self.seconds, true),
            Mode::Sections => section_mode::SectionMode::run(Self::SIZE, scene, self.seconds),
            Mode::Idle => self.idle(),
            Mode::Sweep => Sweep::new(self.seconds, self.repeats, Self::sweep_configs()).run(),
        }
    }

    fn sweep_configs() -> Vec<SweepConfig> {
        let mut configs = vec![SweepConfig {
            mode: "idle",
            tiles: 0,
            period: 1,
        }];
        for (tiles, period) in [(40, 40), (40, 4), (40, 1), (200, 4)] {
            for mode in ["buffer", "damaged", "sections"] {
                configs.push(SweepConfig {
                    mode,
                    tiles,
                    period,
                });
            }
        }
        configs
    }

    fn idle(&self) {
        let mut report = run_report::RunReport::begin("idle");
        while report.elapsed() < Duration::from_secs(self.seconds) {
            std::thread::sleep(Duration::from_millis(16));
            report.sample_gpu();
        }
        report.print("");
    }
}

fn main() {
    match Args::parse() {
        Some(args) => args.run(),
        None => {
            eprintln!(
                "usage: tile-compare buffer|damaged|sections|idle|sweep [seconds] [tiles] [period]"
            );
        }
    }
}

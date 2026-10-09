//! Draws one tile scene two ways and reports what each costs, on the client,
//! in the compositor and on the GPU. `idle` opens no window and measures the
//! baseline.
//!
//!   cargo run --release -p tile-compare -- buffer   [seconds] [tiles] [period]
//!   cargo run --release -p tile-compare -- sections [seconds] [tiles] [period]
//!
//! One tile in `period` changes colour per frame (default 4).
//!   cargo run --release -p tile-compare -- idle     [seconds]

mod buffer_mode;
mod compositor_probe;
mod drm_usage;
mod gpu_probe;
mod process_sample;
mod run_report;
mod section_mode;
mod tile_scene;

use std::time::Duration;

use platform::toplevel::LogicalSize;

enum Mode {
    Buffer,
    Sections,
    Idle,
}

struct Args {
    mode: Mode,
    seconds: u64,
    tiles: usize,
    period: u64,
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
            Some("sections") => Mode::Sections,
            Some("idle") => Mode::Idle,
            _ => return None,
        };
        Some(Self {
            mode,
            seconds: args.get(1).and_then(|s| s.parse().ok()).unwrap_or(10),
            tiles: args.get(2).and_then(|s| s.parse().ok()).unwrap_or(40),
            period: args.get(3).and_then(|s| s.parse().ok()).unwrap_or(4),
        })
    }

    fn run(&self) {
        let scene = tile_scene::TileScene::new(self.tiles, self.period);
        match self.mode {
            Mode::Buffer => buffer_mode::BufferMode::run(Self::SIZE, scene, self.seconds),
            Mode::Sections => section_mode::SectionMode::run(Self::SIZE, scene, self.seconds),
            Mode::Idle => self.idle(),
        }
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
        None => eprintln!("usage: tile-compare buffer|sections|idle [seconds] [tiles] [period]"),
    }
}

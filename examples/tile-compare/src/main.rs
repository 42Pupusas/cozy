//! Draws one tile scene two ways and reports what each costs.
//!
//!   cargo run --release -p tile-compare -- buffer   [seconds] [tiles]
//!   cargo run --release -p tile-compare -- sections [seconds] [tiles]

mod buffer_mode;
mod process_sample;
mod run_report;
mod section_mode;
mod tile_scene;

use platform::toplevel::LogicalSize;

struct Args {
    sections: bool,
    seconds: u64,
    tiles: usize,
}

impl Args {
    const SIZE: LogicalSize = LogicalSize {
        width: 1200,
        height: 800,
    };

    fn parse() -> Option<Self> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let sections = match args.first().map(String::as_str) {
            Some("buffer") => false,
            Some("sections") => true,
            _ => return None,
        };
        Some(Self {
            sections,
            seconds: args.get(1).and_then(|s| s.parse().ok()).unwrap_or(10),
            tiles: args.get(2).and_then(|s| s.parse().ok()).unwrap_or(40),
        })
    }

    fn run(&self) {
        if self.sections {
            section_mode::SectionMode::run(Self::SIZE, self.tiles, self.seconds);
        } else {
            buffer_mode::BufferMode::run(Self::SIZE, self.tiles, self.seconds);
        }
    }
}

fn main() {
    match Args::parse() {
        Some(args) => args.run(),
        None => eprintln!("usage: tile-compare buffer|sections [seconds] [tiles]"),
    }
}

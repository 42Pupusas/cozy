//! One-shot end-to-end ASCII flood measurement.
//!
//! This models production cadence: 4 KiB PTY chunks, a snapshot every 64
//! chunks, and the real renderer replaying snapshots into a real backbuffer.
//! The Wayland compositor and actual PTY scheduling are not included because
//! they are controlled by the desktop. Pass `font=/path/to/font.ttf` to use
//! the live TTF rendering path.
//!
//!     cargo run --release -p tty-benches --example ascii_512mb
//!     cargo run --release -p tty-benches --example ascii_512mb -- font=/path/to/font.ttf
//!     cargo run --release -p tty-benches --example ascii_512mb -- bytes=1048576

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
)]

use std::time::{Duration, Instant};

use config::FontConfig;
use grid::{DirtySnapshot, Grid};
use platform::draw::Canvas;
use platform::{Renderer, Theme};
use tty_benches::{COLS, ROWS};
use vt_parser::Parser;

const DEFAULT_BYTES: usize = 512 * 1024 * 1024;
const PTY_CHUNK: usize = 4096;
const BATCH_CHUNKS: usize = 64;
const SNAPSHOTS_PER_PRESENT: usize = 6;

struct AsciiFloodBenchmark {
    target_bytes: usize,
    font_path: Option<String>,
}

impl AsciiFloodBenchmark {
    fn from_args(args: &[String]) -> Self {
        let target_bytes = args.iter()
            .find_map(|arg| arg.strip_prefix("bytes=")?.parse().ok())
            .unwrap_or(DEFAULT_BYTES);
        let font_path = args.iter()
            .find_map(|arg| arg.strip_prefix("font=").map(String::from));
        Self { target_bytes, font_path }
    }

    fn input_chunk() -> [u8; PTY_CHUNK] {
        let line = b"the quick brown fox jumps over the lazy dog 0123456789\r\n";
        let mut chunk = [0; PTY_CHUNK];
        for (index, byte) in chunk.iter_mut().enumerate() {
            *byte = line[index % line.len()];
        }
        chunk
    }

    fn renderer(&self) -> Renderer {
        self.font_path.as_ref().map_or_else(
            || Renderer::builtin(Theme::builtin()),
            |path| Renderer::new(Theme::builtin(), &FontConfig {
                path: Some(path.clone()), size: 14, emoji_path: None,
            }),
        )
    }

    fn measure_parser(&self, chunk: &[u8; PTY_CHUNK]) -> Duration {
        let mut parser = Parser::new();
        let mut bytes = 0;
        let start = Instant::now();
        while bytes < self.target_bytes {
            let length = (self.target_bytes - bytes).min(chunk.len());
            parser.feed(&chunk[..length], |event| {
                std::hint::black_box(event);
            });
            bytes += length;
        }
        start.elapsed()
    }

    fn present(renderer: &mut Renderer, backbuffer: &mut [u32], width: usize,
        height: usize, snapshots: &[DirtySnapshot]) -> usize {
        let mut canvas = Canvas::new(backbuffer, width as i32, height as i32);
        for snapshot in snapshots {
            renderer.blit_snapshot(&mut canvas, snapshot);
        }
        snapshots.len()
    }

    fn run(&self) -> Measurement {
        let chunk = Self::input_chunk();
        let parser_only_time = self.measure_parser(&chunk);
        let mut parser = Parser::new();
        let mut grid = Grid::new(COLS, ROWS);
        let mut renderer = self.renderer();
        let cell_width = renderer.cell_w() as usize;
        let cell_height = renderer.cell_h() as usize;
        let width = COLS as usize * cell_width;
        let height = ROWS as usize * cell_height;
        let mut backbuffer = vec![0_u32; width * height];
        let mut pending = Vec::with_capacity(SNAPSHOTS_PER_PRESENT);
        let mut bytes = 0;
        let mut chunks = 0;
        let mut snapshots = 0;
        let mut presents = 0;
        let start = Instant::now();
        let mut parse_snapshot_time = Duration::ZERO;
        let mut render_time = Duration::ZERO;

        while bytes < self.target_bytes {
            let phase_start = Instant::now();
            let length = (self.target_bytes - bytes).min(chunk.len());
            parser.feed(&chunk[..length], |event| grid.apply(&event));
            bytes += length;
            chunks += 1;
            if chunks % BATCH_CHUNKS == 0 || bytes == self.target_bytes {
                let snapshot = grid.take_dirty();
                if !snapshot.is_empty() { pending.push(snapshot); }
            }
            parse_snapshot_time += phase_start.elapsed();
            if pending.len() >= SNAPSHOTS_PER_PRESENT || bytes == self.target_bytes {
                let present_start = Instant::now();
                snapshots += Self::present(&mut renderer, &mut backbuffer, width, height, &pending);
                render_time += present_start.elapsed();
                presents += usize::from(!pending.is_empty());
                pending.clear();
            }
        }
        Measurement {
            bytes, chunks, snapshots, presents, elapsed: start.elapsed(),
            parser_only_time, parse_snapshot_time, render_time, cell_width, cell_height,
        }
    }

    fn print(&self) {
        let measurement = self.run();
        let seconds = measurement.elapsed.as_secs_f64();
        let mib = measurement.bytes as f64 / (1024.0 * 1024.0);
        let throughput = mib / seconds;
        println!("ASCII flood: {mib:.1} MiB");
        println!("renderer: {} ({}x{} cell, {}x{} pixels)",
            self.font_path.as_deref().map_or("bitmap", |_| "ttf"),
            measurement.cell_width, measurement.cell_height,
            COLS as usize * measurement.cell_width, ROWS as usize * measurement.cell_height);
        let parser_seconds = measurement.parser_only_time.as_secs_f64();
        let pipeline_seconds = measurement.parse_snapshot_time.as_secs_f64();
        let grid_seconds = (pipeline_seconds - parser_seconds).max(0.0);
        let render_seconds = measurement.render_time.as_secs_f64();
        println!("elapsed: {seconds:.3} s | throughput: {throughput:.1} MiB/s | {:.1} MB/s",
            throughput * 1.048_576);
        println!("phases: parser-only {parser_seconds:.3} s | grid+snapshot estimate {grid_seconds:.3} s | render/blit {render_seconds:.3} s");
        println!("timed share: parse+grid+snapshot {:.1}% | render/blit {:.1}%",
            100.0 * pipeline_seconds / seconds, 100.0 * render_seconds / seconds);
        println!("work: {} PTY chunks | {} snapshots | {} presents",
            measurement.chunks, measurement.snapshots, measurement.presents);
        println!("note: compositor, Wayland dispatch, and actual PTY scheduling are not timed");
    }
}

struct Measurement {
    bytes: usize,
    chunks: usize,
    snapshots: usize,
    presents: usize,
    elapsed: Duration,
    parser_only_time: Duration,
    parse_snapshot_time: Duration,
    render_time: Duration,
    cell_width: usize,
    cell_height: usize,
}

fn main() {
    AsciiFloodBenchmark::from_args(&std::env::args().collect::<Vec<_>>()).print();
}

#[cfg(test)]
mod tests {
    use super::{AsciiFloodBenchmark, PTY_CHUNK};

    #[test]
    fn chunk_is_full_and_ascii() {
        let chunk = AsciiFloodBenchmark::input_chunk();
        assert_eq!(chunk.len(), PTY_CHUNK);
        assert!(chunk.iter().all(u8::is_ascii));
    }
}

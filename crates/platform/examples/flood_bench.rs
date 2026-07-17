//! Headless reproduction of the **present path** under a sustained flood, so
//! `perf`/wall-clock samples the renderer, not the VT parser.
//!
//! It models the structural fact the interactive loop hides: between two
//! compositor vsync callbacks the parser thread publishes *several*
//! `DirtySnapshot`s, and `TermWindow::maybe_present` replays every one of them
//! in full into the backbuffer before presenting once. Under a full-screen
//! flood each snapshot is a whole-screen dirty set, so a present blits the
//! screen N times over — pure overdraw.
//!
//! Crucially, snapshots are built **once, up front** (parser + grid work), and
//! only the blit loop is timed — the parser thread's cost never touches the
//! present path in the real program, so it must not touch it here either.
//!
//! Run:
//!   cargo run --release -p platform --example `flood_bench` -- scroll
//!   cargo run --release -p platform --example `flood_bench` -- truecolor
//!   cargo run --release -p platform --example `flood_bench` -- truecolor coalesce

// Scratch/manual-run diagnostic tool (superseded for CI-style measurement by
// `benches/tty`'s divan `blit` bench, which this was ported from) -- kept
// standalone for quick one-off `cargo run --example` profiling sessions, so
// its geometry casts (buffer dimensions always small and positive in
// practice) get a blanket allow rather than defensive `try_from` everywhere.
#![allow(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss
)]

use std::time::Instant;

use grid::{DirtyRow, DirtySnapshot, Grid};
use platform::draw::Canvas;
use platform::{Renderer, Theme};
use vt_parser::Parser;

const COLS: u16 = 80;
const ROWS: u16 = 24;
// Snapshots produced per simulated vsync interval. Under a real flood the
// parser batch cap (64 chunks / 5 ms) publishes faster than the ~16 ms vsync,
// so a handful pile up per present. 6 is a representative middle.
const SNAPS_PER_PRESENT: usize = 6;
const PRESENTS: usize = 4000;

fn scroll_line(row: usize) -> Vec<u8> {
    format!("{row}: the quick brown fox jumps over the lazy dog 0123456789\r\n").into_bytes()
}

fn truecolor_line(row: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for c in 0..COLS as usize {
        let fr = c * 255 / COLS as usize;
        let fg = (row * 3) % 255;
        let fb = (c + row) % 255;
        let br = 255 - fr;
        let bg = row % 255;
        let bb = c % 255;
        out.extend_from_slice(format!("\x1b[38;2;{fr};{fg};{fb};48;2;{br};{bg};{bb}m#").as_bytes());
    }
    out.extend_from_slice(b"\x1b[0m\r\n");
    out
}

/// Last-writer-wins coalesce of several snapshots into one effective frame:
/// the newest cells for each row survive, so a present blits at most one
/// screen regardless of how many snapshots queued.
fn coalesce(snaps: &[DirtySnapshot]) -> DirtySnapshot {
    let mut by_row: std::collections::BTreeMap<u16, DirtyRow> = std::collections::BTreeMap::new();
    for snap in snaps {
        for row in &snap.rows {
            by_row.insert(row.row, row.clone());
        }
    }
    DirtySnapshot {
        rows: by_row.into_values().collect(),
        cols: COLS,
        cursor: grid::CursorInfo::default(),
        cursor_changed: false,
        content_delta: 0,
        viewport_offset: None,
        scroll_span: None,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map_or("scroll", String::as_str);
    let do_coalesce = args.iter().any(|a| a == "coalesce");

    let line: fn(usize) -> Vec<u8> = match mode {
        "truecolor" => truecolor_line,
        _ => scroll_line,
    };

    let mut renderer = Renderer::builtin(Theme::builtin());
    let cell_w = renderer.cell_w() as usize;
    let cell_h = renderer.cell_h() as usize;
    let bw = COLS as usize * cell_w;
    let bh = ROWS as usize * cell_h;
    let mut backbuffer = vec![0u32; bw * bh];

    // ── Phase 1: build the snapshot stream up front (NOT timed). This is the
    // parser thread's job in the real program; it never runs on the present
    // path, so it must not be in the timed region.
    let mut grid = Grid::new(COLS, ROWS);
    let mut parser = Parser::new();
    let mut row_counter = 0usize;
    let mut frames: Vec<Vec<DirtySnapshot>> = Vec::with_capacity(PRESENTS);
    for _ in 0..PRESENTS {
        let mut pending: Vec<DirtySnapshot> = Vec::new();
        for _ in 0..SNAPS_PER_PRESENT {
            for _ in 0..(ROWS as usize / 2) {
                let bytes = line(row_counter);
                row_counter += 1;
                parser.feed(&bytes, |ev| grid.apply(&ev));
            }
            let snap = grid.take_dirty();
            if !snap.is_empty() {
                pending.push(snap);
            }
        }
        if do_coalesce {
            frames.push(vec![coalesce(&pending)]);
        } else {
            frames.push(pending);
        }
    }

    // ── Phase 2: the present path only — blit each frame's snapshots into the
    // backbuffer. This is exactly TermWindow::maybe_present's blit loop.
    let mut snaps_blitted = 0usize;
    let mut rows_blitted = 0usize;
    let start = Instant::now();
    for pending in &frames {
        let mut canvas = Canvas::new(&mut backbuffer, bw as i32, bh as i32);
        for snap in pending {
            rows_blitted += snap.rows.len();
            renderer.blit_snapshot(&mut canvas, snap);
            snaps_blitted += 1;
        }
    }
    let elapsed = start.elapsed();

    let per_present_us = elapsed.as_micros() as f64 / PRESENTS as f64;
    eprintln!(
        "mode={mode} coalesce={do_coalesce}: {PRESENTS} presents (blit only) in {:.1} ms\n  \
         {:.1} us/present  |  {snaps_blitted} snapshot-blits  |  {rows_blitted} row-blits\n  \
         implied ceiling: {:.0} fps (blit only, no compositor)",
        elapsed.as_secs_f64() * 1000.0,
        per_present_us,
        1_000_000.0 / per_present_us,
    );
}

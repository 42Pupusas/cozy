//! Deterministic VT-stream corpora shared by every bench in this crate.
//!
//! Each generator mirrors one of the `scripts/stress/*.sh` interactive
//! stress scripts, so the numbers here have a direct interactive analogue:
//! if `blit` regresses on `[Corpus::TRUECOLOR]`, go run
//! `scripts/stress/truecolor_flood.sh` and watch it happen live.
//!
//! Generators are pure functions of `(cols, rows, n)` -> `Vec<u8>` so every
//! bench iteration replays the exact same bytes; no RNG, no wall-clock
//! seeding, no flakiness between runs.

/// Standard bench geometry: matches the default window size the interactive
/// stress scripts assume (`COLUMNS:-80`).
pub const COLS: u16 = 80;
pub const ROWS: u16 = 24;

/// A larger geometry to catch effects that only show up once rows/cols grow
/// past a cache line / SIMD width / small-vec-inline-capacity, etc.
pub const BIG_COLS: u16 = 240;
pub const BIG_ROWS: u16 = 70;

/// One named corpus: a generator plus a label for divan's `args` table.
#[derive(Clone, Copy)]
pub struct Corpus {
    pub name: &'static str,
    pub generate: fn(u16, u16, usize) -> Vec<u8>,
    /// Suggested `n` (line/iteration count) at default geometry — keeps
    /// individual bench bodies from repeating magic numbers.
    pub default_n: usize,
}

impl std::fmt::Display for Corpus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name)
    }
}

pub const CORPORA: &[Corpus] = &[
    Corpus {
        name: "plain_scroll",
        generate: plain_scroll,
        default_n: 2000,
    },
    Corpus {
        name: "truecolor_flood",
        generate: truecolor_flood,
        default_n: 400,
    },
    Corpus {
        name: "dense_cells",
        generate: dense_cells,
        default_n: 400,
    },
    Corpus {
        name: "wide_unicode",
        generate: wide_unicode,
        default_n: 800,
    },
    Corpus {
        name: "alt_screen_thrash",
        generate: alt_screen_thrash,
        default_n: 500,
    },
    Corpus {
        name: "cursor_dance",
        generate: cursor_dance,
        default_n: 2000,
    },
    Corpus {
        name: "osc_titles",
        generate: osc_titles,
        default_n: 2000,
    },
];

/// Raw throughput / scroll stress. Mirrors `scripts/stress/scroll_flood.sh`:
/// plain numbered lines, nothing but Print + CR/LF.
#[must_use]
pub fn plain_scroll(_cols: u16, _rows: u16, n: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for i in 0..n {
        out.extend_from_slice(
            format!("{i}: the quick brown fox jumps over the lazy dog 0123456789\r\n").as_bytes(),
        );
    }
    out
}

/// 24-bit truecolor gradient stress. Mirrors `scripts/stress/truecolor_flood.sh`:
/// every cell gets a distinct RGB SGR pair, forcing per-cell `Attrs` churn.
#[must_use]
pub fn truecolor_flood(cols: u16, _rows: u16, n: usize) -> Vec<u8> {
    let cols = cols as usize;
    let mut out = Vec::new();
    for row in 0..n {
        for c in 0..cols {
            let fr = c * 255 / cols.max(1);
            let fg = (row * 3) % 255;
            let fb = (c + row) % 255;
            let br = 255 - fr;
            let bg = row % 255;
            let bb = c % 255;
            out.extend_from_slice(
                format!("\x1b[38;2;{fr};{fg};{fb};48;2;{br};{bg};{bb}m#").as_bytes(),
            );
        }
        out.extend_from_slice(b"\x1b[0m\r\n");
    }
    out
}

/// Dense full-screen redraw stress. Mirrors `scripts/stress/dense_cells.sh`:
/// every line touches every column with alternating SGR colors.
#[must_use]
pub fn dense_cells(cols: u16, _rows: u16, n: usize) -> Vec<u8> {
    let cols = cols as usize;
    let mut out = Vec::new();
    for i in 0..n {
        let c = (i % 6) + 41;
        out.extend_from_slice(format!("\x1b[{c}m").as_bytes());
        for col in 0..cols {
            // `col % 10` is always in `0..10`, so the truncating cast is exact.
            #[allow(clippy::cast_possible_truncation)]
            let digit = (col % 10) as u8;
            out.push(b'0' + digit);
        }
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"\x1b[0m\n");
    out
}

/// Wide-glyph / CJK stress. Mirrors `scripts/stress/wide_unicode.sh`:
/// double-width characters exercising the `Cell::wide`/spacer path.
#[must_use]
pub fn wide_unicode(_cols: u16, _rows: u16, n: usize) -> Vec<u8> {
    const LINE: &str =
        "国际化测试字符宽度渲染国际化测试字符宽度渲染国际化测试字符宽度渲染国际化测试字符宽度渲染";
    let mut out = String::with_capacity(LINE.len() * n + n);
    for _ in 0..n {
        out.push_str(LINE);
        out.push('\n');
    }
    out.into_bytes()
}

/// Alt-screen thrash. Mirrors `scripts/stress/alt_screen_thrash.sh`: rapid
/// smcup/rmcup toggling, forcing a full-screen dirty snapshot every flip.
#[must_use]
pub fn alt_screen_thrash(_cols: u16, _rows: u16, n: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for i in 0..n {
        out.extend_from_slice(b"\x1b[?1049h");
        out.extend_from_slice(format!("\x1b[H\x1b[2Jframe {i}").as_bytes());
        out.extend_from_slice(b"\x1b[?1049l");
    }
    out
}

/// Cursor-heavy CSI stress modeled on interactive full-screen apps (vim/less
/// style).
///
/// Cursor addressing, relative moves, and clears, with only a little text --
/// the opposite extreme from `plain_scroll`'s pure Print stream.
#[must_use]
pub fn cursor_dance(cols: u16, rows: u16, n: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let rows = rows.max(1);
    let cols = cols.max(1);
    for i in 0..n {
        // `i % rows`/`(i * 7) % cols` are bounded by `rows`/`cols: u16`, so
        // the truncating casts back to u16 are exact.
        #[allow(clippy::cast_possible_truncation)]
        let row = (i % rows as usize) as u16 + 1;
        #[allow(clippy::cast_possible_truncation)]
        let col = ((i * 7) % cols as usize) as u16 + 1;
        out.extend_from_slice(format!("\x1b[{row};{col}H").as_bytes());
        out.extend_from_slice(b"\x1b[K");
        out.extend_from_slice(format!("x{i}").as_bytes());
        if i % 16 == 0 {
            out.extend_from_slice(b"\x1b[2J");
        }
        if i % 4 == 0 {
            out.extend_from_slice(b"\x1b[3A\x1b[5C");
        }
    }
    out
}

/// OSC-heavy stress: window title updates interleaved with a little text.
///
/// Exercises the parser's `OscString` state and `Event::Osc` path (which,
/// unlike most events, borrows text out of the parser's own buffer).
#[must_use]
pub fn osc_titles(_cols: u16, _rows: u16, n: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for i in 0..n {
        out.extend_from_slice(format!("\x1b]0;window title #{i} - some/path/here\x07").as_bytes());
        out.extend_from_slice(format!("line {i}\r\n").as_bytes());
    }
    out
}

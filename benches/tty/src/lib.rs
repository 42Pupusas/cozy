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
        generate: Corpora::plain_scroll,
        default_n: 2000,
    },
    Corpus {
        name: "truecolor_flood",
        generate: Corpora::truecolor_flood,
        default_n: 400,
    },
    Corpus {
        name: "dense_cells",
        generate: Corpora::dense_cells,
        default_n: 400,
    },
    Corpus {
        name: "wide_unicode",
        generate: Corpora::wide_unicode,
        default_n: 800,
    },
    Corpus {
        name: "unicode",
        generate: Corpora::unicode,
        default_n: 1,
    },
    Corpus {
        name: "alt_screen_thrash",
        generate: Corpora::alt_screen_thrash,
        default_n: 500,
    },
    Corpus {
        name: "cursor_dance",
        generate: Corpora::cursor_dance,
        default_n: 2000,
    },
    Corpus {
        name: "osc_titles",
        generate: Corpora::osc_titles,
        default_n: 2000,
    },
    Corpus {
        name: "region_scroll",
        generate: Corpora::region_scroll,
        default_n: 4000,
    },
    Corpus {
        name: "control_heavy",
        generate: Corpora::control_heavy,
        default_n: 500,
    },
];

/// Groups every corpus generator as an associated `fn`, not a free function.
///
/// Each is still a plain `fn(u16, u16, usize) -> Vec<u8>` (so it's usable as
/// the `Corpus::generate` fn-pointer field, same as before), just
/// namespaced under one owner rather than floating at module scope.
pub struct Corpora;

impl Corpora {
    /// Raw throughput / scroll stress. Mirrors `scripts/stress/scroll_flood.sh`:
    /// plain numbered lines, nothing but Print + CR/LF.
    #[must_use]
    pub fn plain_scroll(_cols: u16, _rows: u16, n: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for i in 0..n {
            out.extend_from_slice(
                format!("{i}: the quick brown fox jumps over the lazy dog 0123456789\r\n")
                    .as_bytes(),
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
                // `col % 10` is always in `0..10`, so this always succeeds.
                let digit = u8::try_from(col % 10).expect("col % 10 fits in u8");
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
        const LINE: &str = "国际化测试字符宽度渲染国际化测试字符宽度渲染国际化测试字符宽度渲染国际化测试字符宽度渲染";
        let mut out = String::with_capacity(LINE.len() * n + n);
        for _ in 0..n {
            out.push_str(LINE);
            out.push('\n');
        }
        out.into_bytes()
    }

    /// Every scalar from U+00A1 to U+D7FB on one wrapping line, then the
    /// emoticon block, on the alternate screen: the shape of vtebench's
    /// `unicode` benchmark. Mixes narrow, wide, zero-width and combining
    /// characters, so it exercises width lookup and the cluster path.
    #[must_use]
    pub fn unicode(_cols: u16, _rows: u16, n: usize) -> Vec<u8> {
        let mut out = String::from("\x1b[?1049h");
        for _ in 0..n {
            out.extend((0xA1..=0xD7FB).filter_map(char::from_u32));
            out.push('\n');
            out.extend((0x1F600..=0x1F64F).filter_map(char::from_u32));
            out.push('\n');
        }
        out.push_str("\x1b[?1049l");
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
            // these always succeed.
            let row = u16::try_from(i % rows as usize).expect("i % rows fits in u16") + 1;
            let col =
                u16::try_from((i * 7) % cols as usize).expect("(i * 7) % cols fits in u16") + 1;
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

    /// Scrolling inside a margin on the alternate screen, as vtebench's
    /// `scrolling_*_region` benchmarks: `DECSTBM 2;rows`, then one short line
    /// per scroll. Neither screen pushes these rows into history.
    #[must_use]
    pub fn region_scroll(_cols: u16, rows: u16, n: usize) -> Vec<u8> {
        let mut out = format!("\x1b[?1049h\x1b[2;{rows}r\x1b[{rows};1H").into_bytes();
        for _ in 0..n {
            out.extend_from_slice(b"y\n");
        }
        out.extend_from_slice(b"\x1b[r\x1b[?1049l");
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
            out.extend_from_slice(
                format!("\x1b]0;window title #{i} - some/path/here\x07").as_bytes(),
            );
            out.extend_from_slice(format!("line {i}\r\n").as_bytes());
        }
        out
    }

    /// Mode-negotiation / control-sequence stress.
    ///
    /// Exercises everything wired up in the terminal-emulator audit that the
    /// other corpora never touch, since they're all Print/SGR/scroll-heavy.
    /// Cycles through mouse tracking (1000/1002/1003/1006), DECCKM/DECAWM/
    /// DECOM/focus-events (1004), Kitty keyboard flag push/pop/set, DECSCUSR
    /// cursor styles, HTS/TBC/CHT/CBT tab stops, DEC Special Graphics (SO/SI)
    /// line drawing, combining accents, BEL, a DECRQSS round trip, and REP --
    /// one repeating burst per `i`, with a little Print text between bursts so the
    /// parser also does real grid work. Mirrors no single `scripts/stress/*.sh`
    /// script (none of them exercise this control surface) -- this is the
    /// missing coverage those scripts don't provide.
    #[must_use]
    pub fn control_heavy(_cols: u16, _rows: u16, n: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for i in 0..n {
            // Mouse tracking mode churn.
            out.extend_from_slice(b"\x1b[?1000h\x1b[?1006h\x1b[?1000l\x1b[?1002h\x1b[?1003h");
            // DECCKM / DECAWM / DECOM / focus events.
            out.extend_from_slice(b"\x1b[?1h\x1b[?7l\x1b[?7h\x1b[?6h\x1b[?6l\x1b[?1004h");
            // Kitty keyboard flag stack: push, OR, AND, replace, pop.
            out.extend_from_slice(b"\x1b[>5u\x1b[=2;2u\x1b[=1;3u\x1b[=9;1u\x1b[<1u");
            // DECSCUSR cursor styles.
            out.extend_from_slice(b"\x1b[1 q\x1b[3 q\x1b[5 q\x1b[0 q");
            // Tab stops: set, jump, clear-all, forward/backward tab.
            out.extend_from_slice(b"\x1bH\t\x1b[3g\x1b[2I\x1b[1Z");
            // DEC Special Graphics line drawing via SO/SI.
            out.extend_from_slice(b"\x1b(0lqk\x1b(B\x1b)0\x0eqqq\x0f");
            // Combining accent onto the previous letter.
            out.extend_from_slice("e\u{301}".as_bytes());
            // Bell.
            out.extend_from_slice(b"\x07");
            // DECRQSS round trip for SGR, DECSTBM, and DECSCUSR.
            out.extend_from_slice(b"\x1bP$qm\x1b\\\x1bP$qr\x1b\\\x1bP$q q\x1b\\");
            // REP -- repeat the last printed char a handful of times.
            out.extend_from_slice(b"=\x1b[8b");
            // A little real text + scroll so the corpus does grid work too.
            out.extend_from_slice(format!("line {i} some ordinary text\r\n").as_bytes());
        }
        out
    }
}

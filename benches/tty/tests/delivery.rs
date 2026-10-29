//! Output is neither dropped nor altered on its way into the grid.
//!
//! vtebench times how long a terminal takes to accept bytes and never looks
//! at the screen, so a terminal that lost or mangled output would still score
//! well. These tests look at the screen.
//!
//! * Every benchmark corpus must produce the same grid through the batched
//!   production path (`feed_to`, at any chunk size) as through the
//!   event-per-character path that predates the batching.
//! * A numbered flood of ASCII, wide, combining, emoji and flag text must come
//!   out with every line present, in order, intact.

use grid::{Cell, Grid};
use tty_benches::{COLS, CORPORA, ROWS};
use vt_parser::Parser;

/// Every cell and cluster in the scrollback and the visible rows.
#[derive(Debug, PartialEq, Eq)]
struct GridDump {
    cursor: (u16, u16),
    replies: Vec<u8>,
    rows: Vec<Vec<(Cell, Option<Vec<char>>)>>,
}

impl GridDump {
    fn of(grid: &mut Grid) -> Self {
        let screen = grid.screen();
        let total = screen.scrollback_len() + usize::from(screen.rows());
        let rows = (0..total)
            .map(|row| {
                (0..screen.cols())
                    .map(|col| {
                        (
                            screen.abs_cell(col, row),
                            screen.abs_cluster(col, row).map(<[char]>::to_vec),
                        )
                    })
                    .collect()
            })
            .collect();
        let cursor = grid.cursor();
        Self {
            cursor,
            replies: grid.take_replies(),
            rows,
        }
    }

    /// One string per row, trailing blanks trimmed, wide glyphs and clusters
    /// written once.
    fn text(&self) -> Vec<String> {
        self.rows
            .iter()
            .map(|row| {
                let mut line = String::new();
                for (cell, cluster) in row {
                    if cell.spacer() {
                        continue;
                    }
                    match cluster {
                        Some(chars) => line.extend(chars),
                        None => line.push(cell.ch),
                    }
                }
                line.trim_end().to_owned()
            })
            .collect()
    }
}

/// The same bytes fed through the three kinds of input path.
struct Feed<'a> {
    bytes: &'a [u8],
    scrollback: usize,
}

impl Feed<'_> {
    /// One event per character: the path the batching replaced.
    fn per_event(&self) -> GridDump {
        let mut grid = Grid::with_scrollback_limit(COLS, ROWS, self.scrollback);
        Parser::new().feed(self.bytes, |event| grid.apply(&event));
        GridDump::of(&mut grid)
    }

    /// The production path, handing the parser `chunk` bytes at a time.
    fn direct(&self, chunk: usize) -> GridDump {
        let mut grid = Grid::with_scrollback_limit(COLS, ROWS, self.scrollback);
        let mut parser = Parser::new();
        for piece in self.bytes.chunks(chunk) {
            parser.feed_to(piece, &mut grid);
        }
        GridDump::of(&mut grid)
    }
}

/// First row where two dumps differ, so a failure names a line instead of
/// printing two whole screens.
struct Divergence;

impl Divergence {
    fn assert_same(label: &str, want: &GridDump, got: &GridDump) {
        assert_eq!(want.rows.len(), got.rows.len(), "{label}: row count");
        for (index, (a, b)) in want.rows.iter().zip(&got.rows).enumerate() {
            assert!(a == b, "{label}: row {index} differs");
        }
        assert_eq!(want.cursor, got.cursor, "{label}: cursor");
        assert_eq!(want.replies, got.replies, "{label}: replies to the host");
    }
}

#[test]
fn every_corpus_matches_the_event_per_character_path_at_any_chunk_size() {
    for corpus in CORPORA {
        let bytes = (corpus.generate)(COLS, ROWS, corpus.default_n);
        let feed = Feed {
            bytes: &bytes,
            scrollback: 8 << 20,
        };
        let want = feed.per_event();
        for chunk in [bytes.len().max(1), 4096, 61, 3, 1] {
            Divergence::assert_same(
                &format!("{} at {chunk}-byte chunks", corpus.name),
                &want,
                &feed.direct(chunk),
            );
        }
    }
}

#[test]
fn the_plain_scroll_corpus_ends_with_its_last_line_on_screen() {
    let bytes = (CORPORA[0].generate)(COLS, ROWS, CORPORA[0].default_n);
    let dump = Feed {
        bytes: &bytes,
        scrollback: 8 << 20,
    }
    .direct(4096);
    let text = dump.text();
    let last = CORPORA[0].default_n - 1;
    let want = format!("{last}: the quick brown fox jumps over the lazy dog 0123456789");
    assert!(
        text.contains(&want),
        "the final line never reached the screen"
    );
}

/// Numbered lines whose text cycles through everything the grid treats
/// specially.
struct Flood;

impl Flood {
    const LINES: usize = 40_000;
    const TEXT: [&'static str; 8] = [
        "the quick brown fox",
        "caf\u{e9} cr\u{e8}me br\u{fb}l\u{e9}e",
        "\u{56fd}\u{9645}\u{5316}\u{6d4b}\u{8bd5}",
        "q\u{301} x\u{323} z\u{308}",
        "\u{1f600}\u{1f680}\u{1f9ea}",
        "\u{1f1fa}\u{1f1f8}\u{1f1ef}\u{1f1f5}",
        "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}",
        "\u{1f44b}\u{1f3fd} hi",
    ];

    fn line(index: usize) -> String {
        format!("{index:06} {}", Self::TEXT[index % Self::TEXT.len()])
    }

    fn bytes() -> Vec<u8> {
        let mut out = String::new();
        for index in 0..Self::LINES {
            out.push_str(&Self::line(index));
            out.push_str("\r\n");
        }
        out.into_bytes()
    }
}

#[test]
fn a_numbered_flood_arrives_whole_and_in_order() {
    let bytes = Flood::bytes();
    let feed = Feed {
        bytes: &bytes,
        scrollback: 128 << 20,
    };
    for chunk in [4096, 1021, 7] {
        let text = feed.direct(chunk).text();
        assert!(
            text.len() >= Flood::LINES,
            "only {} rows exist for {} lines at {chunk}-byte chunks",
            text.len(),
            Flood::LINES
        );
        for (index, line) in text.iter().take(Flood::LINES).enumerate() {
            assert_eq!(
                *line,
                Flood::line(index),
                "line {index} at {chunk}-byte chunks"
            );
        }
        assert!(
            text[Flood::LINES..].iter().all(String::is_empty),
            "something extra was written after the flood at {chunk}-byte chunks"
        );
    }
}

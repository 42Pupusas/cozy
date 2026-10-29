//! A real child process, a real PTY and the terminal's own ring wiring, with a
//! parser thread slow enough to keep the ring full. Every line the child
//! prints must reach the grid, in order.
//!
//! The child ends its output with a marker and then sleeps. The parser thread
//! asks the PTY loop to quit once it has parsed the marker, so these tests
//! cover backpressure without depending on what the loop does when a child
//! exits with output still queued (see `output_written_just_before_exit_is_not_lost`).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use grid::Grid;
use pty::{Pty, PtyInput, WinSize};
use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc::{Consumer, Producer, RingBuffer};
use vt_parser::Parser;

/// The terminal's read chunk and parser ring size (`bins/terminal`).
const CHUNK: usize = 4096;
const RING_SLOTS: usize = 64;

const COLS: u16 = 80;
const ROWS: u16 = 24;

const MARKER: &[u8] = b"@@END@@";

struct Chunk {
    buf: [u8; CHUNK],
    len: usize,
}

/// Finds a marker in a stream cut into arbitrary chunks.
struct MarkerScan {
    tail: Vec<u8>,
}

impl MarkerScan {
    const fn new() -> Self {
        Self { tail: Vec::new() }
    }

    fn seen_in(&mut self, chunk: &[u8]) -> bool {
        self.tail.extend_from_slice(chunk);
        let found = self.tail.windows(MARKER.len()).any(|w| w == MARKER);
        let keep = self.tail.len().saturating_sub(MARKER.len() - 1);
        self.tail.drain(..keep);
        found
    }
}

/// Parser thread that sleeps after every `STRIDE` chunks, so the PTY thread
/// finds the ring full and has to hold chunks back.
struct SlowParser {
    rx: Consumer<Chunk>,
    pty_wake: waker::Waker,
    finished: Arc<AtomicBool>,
    grid: Grid,
}

impl SlowParser {
    const STRIDE: usize = 8;
    const NAP: Duration = Duration::from_micros(300);

    fn spawn(self) -> JoinHandle<Grid> {
        std::thread::spawn(move || self.run())
    }

    fn run(mut self) -> Grid {
        let mut parser = Parser::new();
        let mut scan = MarkerScan::new();
        let mut seen = 0usize;
        while let Some(chunk) = self.rx.pop_block() {
            let bytes = &chunk.buf[..chunk.len];
            parser.feed_to(bytes, &mut self.grid);
            if scan.seen_in(bytes) {
                self.finished.store(true, Ordering::Release);
            }
            self.pty_wake.signal();
            if self.finished.load(Ordering::Acquire) {
                break;
            }
            seen += 1;
            if seen.is_multiple_of(Self::STRIDE) {
                std::thread::sleep(Self::NAP);
            }
        }
        self.grid
    }
}

/// What came out of one run of a child through the pipeline.
struct Outcome {
    rejected_chunks: usize,
    lines: Vec<String>,
}

struct Pipeline;

impl Pipeline {
    /// Run `sh -c script` and return the rows of the resulting grid.
    fn run(script: &str, scrollback: usize) -> Outcome {
        let wake = waker::Waker::new().expect("eventfd");
        let shutdown = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        let win = WinSize {
            rows: ROWS,
            cols: COLS,
            xpixel: 0,
            ypixel: 0,
        };
        let pty = Pty::spawn("/bin/sh", &["-c", script], win, wake.raw_fd(), shutdown)
            .expect("Pty::spawn");

        let (tx, rx) = RingBuffer::<Chunk>::new(Capacity::exact(RING_SLOTS)).split();
        let parser = SlowParser {
            rx,
            pty_wake: wake.clone(),
            finished: Arc::clone(&finished),
            grid: Grid::with_scrollback_limit(COLS, ROWS, scrollback),
        }
        .spawn();

        let pty_thread = std::thread::spawn(move || Self::pump(pty, &tx, &finished));
        let rejected_chunks = pty_thread.join().expect("pty thread");
        drop(wake);
        let grid = parser.join().expect("parser thread");
        Outcome {
            rejected_chunks,
            lines: Self::lines(&grid),
        }
    }

    /// Like [`Self::run`], but nothing asks the loop to quit: it ends only
    /// when the child does, as in the terminal.
    fn run_to_exit(script: &str, scrollback: usize) -> Outcome {
        let wake = waker::Waker::new().expect("eventfd");
        let win = WinSize {
            rows: ROWS,
            cols: COLS,
            xpixel: 0,
            ypixel: 0,
        };
        let pty = Pty::spawn(
            "/bin/sh",
            &["-c", script],
            win,
            wake.raw_fd(),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("Pty::spawn");
        let (tx, rx) = RingBuffer::<Chunk>::new(Capacity::exact(RING_SLOTS)).split();
        let parser = SlowParser {
            rx,
            pty_wake: wake.clone(),
            finished: Arc::new(AtomicBool::new(false)),
            grid: Grid::with_scrollback_limit(COLS, ROWS, scrollback),
        }
        .spawn();
        let never = AtomicBool::new(false);
        let rejected_chunks = Self::pump(pty, &tx, &never);
        drop(tx);
        drop(wake);
        let grid = parser.join().expect("parser thread");
        Outcome {
            rejected_chunks,
            lines: Self::lines(&grid),
        }
    }

    /// The terminal's `on_output`: copy into a ring slot, refuse when full.
    /// Returns how many chunks were refused.
    fn pump(pty: Pty, tx: &Producer<Chunk>, finished: &AtomicBool) -> usize {
        let mut rejected = 0usize;
        let mut quit_sent = false;
        pty.run(
            || {
                if quit_sent || !finished.load(Ordering::Acquire) {
                    return None;
                }
                quit_sent = true;
                Some(PtyInput::Quit)
            },
            |bytes: &[u8]| {
                let mut buf = [0u8; CHUNK];
                buf[..bytes.len()].copy_from_slice(bytes);
                let accepted = tx
                    .push(Chunk {
                        buf,
                        len: bytes.len(),
                    })
                    .is_ok();
                if !accepted {
                    rejected += 1;
                }
                accepted
            },
        )
        .expect("pty run");
        rejected
    }

    fn lines(grid: &Grid) -> Vec<String> {
        let screen = grid.screen();
        let total = screen.scrollback_len() + usize::from(screen.rows());
        (0..total)
            .map(|row| {
                let mut line = String::new();
                for col in 0..screen.cols() {
                    let cell = screen.abs_cell(col, row);
                    if cell.spacer() {
                        continue;
                    }
                    match screen.abs_cluster(col, row) {
                        Some(chars) => line.extend(chars),
                        None => line.push(cell.ch),
                    }
                }
                line.trim_end().to_owned()
            })
            .collect()
    }
}

#[test]
fn a_flood_through_a_full_ring_arrives_whole_and_in_order() {
    const LINES: usize = 100_000;
    const TEXT: &str = "caf\u{e9} \u{56fd}\u{9645} \u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467} \u{1f1fa}\u{1f1f8} q\u{301}";
    let script = format!(
        "seq -f '%07g {TEXT}' 1 {LINES}; echo {}; sleep 30",
        String::from_utf8_lossy(MARKER)
    );
    let out = Pipeline::run(&script, 256 << 20);

    assert!(
        out.rejected_chunks > 0,
        "the ring never filled, so backpressure was not exercised"
    );
    for index in 1..=LINES {
        assert_eq!(
            out.lines[index - 1],
            format!("{index:07} {TEXT}"),
            "line {index}"
        );
    }
    assert_eq!(out.lines[LINES], String::from_utf8_lossy(MARKER));
    assert!(
        out.lines[LINES + 1..].iter().all(String::is_empty),
        "extra output after the marker"
    );
}

/// The child exits the instant its last write returns. Everything it wrote
/// must still come out, even though the PTY loop sees SIGCHLD or end-of-file
/// while the parser ring is full and chunks are still waiting.
#[test]
#[ignore = "known loss: Session::event_loop stops at EOF/SIGCHLD with chunks still in `unread`"]
fn output_written_just_before_exit_is_not_lost() {
    const LINES: usize = 60_000;
    let script = format!("seq -f 'row %06g' 1 {LINES}");
    let out = Pipeline::run_to_exit(&script, 64 << 20);

    assert!(out.rejected_chunks > 0, "the ring never filled");
    assert!(
        out.lines.len() > LINES,
        "only {} of {LINES} lines arrived",
        out.lines.len()
    );
    for index in 1..=LINES {
        assert_eq!(
            out.lines[index - 1],
            format!("row {index:06}"),
            "line {index}"
        );
    }
}

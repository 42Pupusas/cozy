//! pty->parser **transport edge** micro-bench: the exact thing Option B
//! changes, isolated from the parser and grid it feeds.
//!
//! Two designs are compared on the same byte stream and geometry:
//!
//! * `old` — the current design. Data crosses on an SPSC ring of
//!   `PtyChunk { Box<[u8; PTY_BUF]>, len }` (heap pointers); a *second* SPSC
//!   ring (`chunk_free`) returns spent `Box`es to the producer so it can
//!   recycle instead of allocating. Two rings, one heap indirection per
//!   chunk, a free-list round-trip per chunk.
//!
//! * `new` — Option B. Data crosses on a single zero-copy SPSC ring whose
//!   slots are inline `[u8; PTY_BUF]` arrays: producer `reserve() -> write()
//!   -> commit()`, consumer `pop_ref()` reads in place and the slot is
//!   released when the reader drops. One ring, no heap indirection, no
//!   free-list: the ring *is* the buffer pool.
//!
//! Two regimes, because they stress different claims:
//!
//! * **steady** — consumer keeps pace, the ring never fills. Here the OLD
//!   design already allocates ~nothing (the free-list stays populated), so
//!   this regime is purely about *cycles*: how much the second ring + the
//!   `Box` indirection cost on the hot path.
//!
//! * **burst** — producer runs ahead of a throttled consumer until the ring
//!   backs up. This is where the OLD free-list empties and it falls back to
//!   fresh `Box` allocation; the divan `AllocProfiler` counts exactly how
//!   many. The NEW design allocates zero in *both* regimes by construction.
//!
//! Run:
//!   cargo bench -p tty-benches --bench transport
//!   cargo bench -p tty-benches --bench transport -- --timer tsc

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use divan::counter::{BytesCount, ItemsCount};
use grid::Grid;
use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc::RingBuffer as MpscRing;
use quetzalcoatl::spsc::RingBuffer;
use vt_parser::Parser;

#[global_allocator]
static ALLOC: divan::AllocProfiler = divan::AllocProfiler::system();

fn main() {
    divan::main();
}

/// PTY read buffer size — mirrors `PTY_BUF` in `bins/terminal/src/main.rs`.
const PTY_BUF: usize = 4096;
/// Data-ring capacity — mirrors `RING_PARSER_IN` (64).
const RING_CAP: usize = 64;
/// Chunks pushed across the edge per benchmarked run.
const CHUNKS: usize = 4096;
/// A representative per-read payload (< `PTY_BUF`, like a real pty read).
const PAYLOAD: usize = 3072;

/// The old data-ring message: a heap buffer plus its filled length.
struct PtyChunk {
    buf: Box<[u8; PTY_BUF]>,
    len: usize,
}

/// When false, the consumer just sums bytes (isolated transport cost). When
/// true, it runs the REAL `parser.feed -> grid.apply` the parser thread does
/// per chunk, so the transport copy is weighed against the work that
/// actually rides on every chunk end-to-end. Set true: the whole point of
/// this bench was proving the transport design is end-to-end noise once the
/// parser+grid work per chunk is present.
const REAL_PARSE: bool = true;

/// Bench geometry for the real-parse consumer.
const PCOLS: u16 = 80;
const PROWS: u16 = 24;

/// A plausible chunk of shell output (plain scroll) to fill each buffer with,
/// so `parser.feed` has real bytes to chew on rather than a flat fill.
fn sample_line() -> Vec<u8> {
    b"the quick brown fox jumps over the lazy dog 0123456789\r\n".to_vec()
}

/// Fill `dst[..PAYLOAD]` with repeated shell-output lines.
fn fill_payload(dst: &mut [u8]) {
    let line = sample_line();
    let mut off = 0;
    while off < PAYLOAD {
        let n = line.len().min(PAYLOAD - off);
        dst[off..off + n].copy_from_slice(&line[..n]);
        off += n;
    }
}

/// The per-chunk consumer work. Either a cheap byte-sum (transport only) or
/// the real parser+grid pipeline (end-to-end weight).
#[inline]
fn consume_with(parser: &mut Parser, grid: &mut Grid, bytes: &[u8]) -> u64 {
    if REAL_PARSE {
        parser.feed(bytes, |ev| grid.apply(&ev));
        0
    } else {
        let mut acc = 0u64;
        for &b in bytes {
            acc = acc.wrapping_add(u64::from(b));
        }
        acc
    }
}

// ── OLD: PtyChunk data ring + chunk_free recycle ring ───────────────────────

fn run_old(throttle: bool) -> u64 {
    let (data_tx, mut data_rx) = RingBuffer::<PtyChunk>::new(Capacity::exact(RING_CAP)).split();
    let (free_tx, mut free_rx) =
        RingBuffer::<Box<[u8; PTY_BUF]>>::new(Capacity::exact(RING_CAP * 2)).split();
    let done = Arc::new(AtomicBool::new(false));
    let done_c = Arc::clone(&done);

    let consumer = std::thread::spawn(move || {
        let mut parser = Parser::new();
        let mut grid = Grid::new(PCOLS, PROWS);
        let mut acc = 0u64;
        loop {
            if let Some(chunk) = data_rx.pop() {
                acc = acc.wrapping_add(consume_with(
                    &mut parser,
                    &mut grid,
                    &chunk.buf[..chunk.len],
                ));
                // Return the spent buffer for the producer to recycle.
                let _ = free_tx.push(chunk.buf);
                if throttle {
                    std::hint::spin_loop();
                }
            } else if done_c.load(Ordering::Acquire) {
                if data_rx.pop().is_none() {
                    break;
                }
            } else {
                std::hint::spin_loop();
            }
        }
        acc
    });

    for i in 0..CHUNKS {
        // Prefer a recycled buffer; allocate a fresh one only when the
        // free-list is momentarily empty (the burst path).
        let mut buf = free_rx.pop().unwrap_or_else(|| Box::new([0u8; PTY_BUF]));
        let _ = i;
        fill_payload(&mut buf[..PAYLOAD]);
        let mut chunk = PtyChunk { buf, len: PAYLOAD };
        // Block-push semantics: retry until the ring accepts it.
        while let Err(returned) = data_tx.push(chunk) {
            chunk = returned;
            std::hint::spin_loop();
        }
    }
    done.store(true, Ordering::Release);
    consumer.join().unwrap()
}

// ── NEW: single zero-copy inline-slot data ring ─────────────────────────────

fn run_new(throttle: bool) -> u64 {
    let (mut data_tx, mut data_rx) =
        RingBuffer::<[u8; PTY_BUF]>::new(Capacity::exact(RING_CAP)).split();
    let done = Arc::new(AtomicBool::new(false));
    let done_c = Arc::clone(&done);

    let consumer = std::thread::spawn(move || {
        let mut parser = Parser::new();
        let mut grid = Grid::new(PCOLS, PROWS);
        let mut acc = 0u64;
        loop {
            if let Some(slot) = data_rx.pop_ref() {
                // Length is implicit here; in the real port a 2-byte len
                // rides in a companion header ring or a small wrapper type.
                acc = acc.wrapping_add(consume_with(&mut parser, &mut grid, &slot[..PAYLOAD]));
                drop(slot); // releases the ring slot for reuse
                if throttle {
                    std::hint::spin_loop();
                }
            } else if done_c.load(Ordering::Acquire) {
                if data_rx.pop_ref().is_none() {
                    break;
                }
            } else {
                std::hint::spin_loop();
            }
        }
        acc
    });

    for i in 0..CHUNKS {
        // reserve -> write directly into the slot -> commit. No Box, no
        // free-list, no allocation ever.
        loop {
            if let Some(mut w) = data_tx.reserve() {
                let _ = i;
                // Write straight into the (possibly uninitialized) slot via
                // its MaybeUninit — don't `assume_init` first, that would be
                // UB on a fresh slot. Fill the whole array so the committed
                // slot is fully initialized.
                let slot: &mut std::mem::MaybeUninit<[u8; PTY_BUF]> = w.slot_mut();
                let ptr = slot.as_mut_ptr().cast::<u8>();
                // SAFETY: ptr points at PTY_BUF contiguous bytes we own
                // exclusively (single producer, reserved slot).
                unsafe {
                    std::ptr::write_bytes(ptr, 0, PTY_BUF);
                    let arr = &mut *slot.as_mut_ptr();
                    fill_payload(&mut arr[..PAYLOAD]);
                    // SAFETY: all PTY_BUF bytes initialized above.
                    w.commit_unchecked();
                }
                break;
            }
            std::hint::spin_loop();
        }
    }
    done.store(true, Ordering::Release);
    consumer.join().unwrap()
}

// ── B2: single MPSC ring, inline [u8; PTY_BUF] in the message ────────────────
//
// Keeps the current single-ring topology (and its free Resize-before-Data
// ordering + unified blocking), but the Data variant carries the bytes
// inline instead of behind a Box, so `chunk_free` disappears. MPSC has no
// zero-copy `reserve`, so `push` memcpys the array into the slot and the
// consumer reads in place via `pop_ref` — one extra ~4 KB copy per chunk vs
// the Box design, which this measures.

// Inline buffer in the large variant is the whole point of B2 (matches the
// production `ParserMsg::Data`); boxing it would defeat the measurement.
#[allow(clippy::large_enum_variant)]
enum Msg {
    Data {
        buf: [u8; PTY_BUF],
        len: usize,
    },
    #[allow(dead_code)]
    Resize {
        cols: u16,
        rows: u16,
    },
}

fn run_inline(throttle: bool) -> u64 {
    let (data_tx, mut data_rx) = MpscRing::<Msg>::new(Capacity::exact(RING_CAP)).split();
    let done = Arc::new(AtomicBool::new(false));
    let done_c = Arc::clone(&done);

    let consumer = std::thread::spawn(move || {
        let mut parser = Parser::new();
        let mut grid = Grid::new(PCOLS, PROWS);
        let mut acc = 0u64;
        loop {
            if let Some(slot) = data_rx.pop_ref() {
                match &*slot {
                    Msg::Data { buf, len } => {
                        acc = acc.wrapping_add(consume_with(&mut parser, &mut grid, &buf[..*len]));
                    }
                    Msg::Resize { .. } => {}
                }
                drop(slot);
                if throttle {
                    std::hint::spin_loop();
                }
            } else if done_c.load(Ordering::Acquire) {
                if data_rx.pop_ref().is_none() {
                    break;
                }
            } else {
                std::hint::spin_loop();
            }
        }
        acc
    });

    for i in 0..CHUNKS {
        let _ = i;
        let mut buf = [0u8; PTY_BUF];
        fill_payload(&mut buf[..PAYLOAD]);
        let mut msg = Msg::Data { buf, len: PAYLOAD };
        while let Err(returned) = data_tx.push(msg) {
            msg = returned;
            std::hint::spin_loop();
        }
    }
    done.store(true, Ordering::Release);
    consumer.join().unwrap()
}

// ── Benches ─────────────────────────────────────────────────────────────────

const BYTES_PER_RUN: usize = CHUNKS * PAYLOAD;

#[divan::bench]
fn old_steady(bencher: divan::Bencher) {
    bencher
        .counter(BytesCount::new(BYTES_PER_RUN))
        .counter(ItemsCount::new(CHUNKS))
        .bench(|| divan::black_box(run_old(false)));
}

#[divan::bench]
fn new_steady(bencher: divan::Bencher) {
    bencher
        .counter(BytesCount::new(BYTES_PER_RUN))
        .counter(ItemsCount::new(CHUNKS))
        .bench(|| divan::black_box(run_new(false)));
}

#[divan::bench]
fn old_burst(bencher: divan::Bencher) {
    bencher
        .counter(BytesCount::new(BYTES_PER_RUN))
        .counter(ItemsCount::new(CHUNKS))
        .bench(|| divan::black_box(run_old(true)));
}

#[divan::bench]
fn new_burst(bencher: divan::Bencher) {
    bencher
        .counter(BytesCount::new(BYTES_PER_RUN))
        .counter(ItemsCount::new(CHUNKS))
        .bench(|| divan::black_box(run_new(true)));
}

#[divan::bench]
fn inline_steady(bencher: divan::Bencher) {
    bencher
        .counter(BytesCount::new(BYTES_PER_RUN))
        .counter(ItemsCount::new(CHUNKS))
        .bench(|| divan::black_box(run_inline(false)));
}

#[divan::bench]
fn inline_burst(bencher: divan::Bencher) {
    bencher
        .counter(BytesCount::new(BYTES_PER_RUN))
        .counter(ItemsCount::new(CHUNKS))
        .bench(|| divan::black_box(run_inline(true)));
}

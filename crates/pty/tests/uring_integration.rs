//! Integration tests driving the real `io_uring` `Pty` event loop against a
//! live shell on the actual kernel.
//!
//! Each test spawns the loop on a dedicated thread (as production does),
//! feeds it input through a ring + eventfd wake, and asserts on the output
//! bytes / exit codes it produces.

use pty::{Pty, PtyInput, WinSize};
use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc::{Consumer, Producer, RingBuffer};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const WIN: WinSize = WinSize {
    rows: 24,
    cols: 80,
    xpixel: 0,
    ypixel: 0,
};

struct Harness {
    wake: waker::Waker,
    inputs: Producer<PtyInput>,
    output: Consumer<Vec<u8>>,
    rejections: Consumer<()>,
    seen: Vec<u8>,
    gate: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    run: JoinHandle<Result<i32, String>>,
}

impl Harness {
    const INPUT_SLOTS: usize = 16;
    const OUTPUT_SLOTS: usize = 4096;
    const REJECTION_SLOTS: usize = 2;

    fn spawn(shell: &str, args: &[&str]) -> Self {
        Self::start(shell, args, true)
    }

    fn spawn_gated(shell: &str, args: &[&str]) -> Self {
        Self::start(shell, args, false)
    }

    fn start(shell: &str, args: &[&str], accepting: bool) -> Self {
        let wake = waker::Waker::new().expect("eventfd");
        let shutdown = Arc::new(AtomicBool::new(false));
        let gate = Arc::new(AtomicBool::new(accepting));
        let pty =
            Pty::spawn(shell, args, WIN, wake.raw_fd(), Arc::clone(&shutdown)).expect("Pty::spawn");

        let (inputs, mut inputs_rx) =
            RingBuffer::<PtyInput>::new(Capacity::exact(Self::INPUT_SLOTS)).split();
        let (output_tx, output) =
            RingBuffer::<Vec<u8>>::new(Capacity::exact(Self::OUTPUT_SLOTS)).split();
        let (rejections_tx, rejections) =
            RingBuffer::<()>::new(Capacity::exact(Self::REJECTION_SLOTS)).split();

        let gate_loop = Arc::clone(&gate);
        let run = std::thread::spawn(move || {
            let next_input = move || inputs_rx.pop();
            let on_output = move |bytes: &[u8]| {
                if !gate_loop.load(Ordering::Acquire) {
                    let _ = rejections_tx.push(());
                    return false;
                }
                output_tx.push(bytes.to_vec()).is_ok()
            };
            pty.run(next_input, on_output).map_err(|e| e.to_string())
        });

        Self {
            wake,
            inputs,
            output,
            rejections,
            seen: Vec::new(),
            gate,
            shutdown,
            run,
        }
    }

    fn send(&self, msg: PtyInput) {
        assert!(self.inputs.push(msg).is_ok(), "input ring full");
        self.wake.signal();
    }

    fn open_gate(&self) {
        self.gate.store(true, Ordering::Release);
        self.wake.signal();
    }

    /// Block until the collected output satisfies `pred`. Panics if the
    /// event loop ends first, since no further output can arrive.
    fn wait_output(&mut self, pred: impl Fn(&[u8]) -> bool) -> Vec<u8> {
        loop {
            if pred(&self.seen) {
                return self.seen.clone();
            }
            let was_full = self.output.is_full();
            let Some(chunk) = self.output.pop_block() else {
                panic!(
                    "event loop ended before the output matched; got: {:?}",
                    String::from_utf8_lossy(&self.seen)
                );
            };
            self.seen.extend_from_slice(&chunk);
            if was_full {
                self.wake.signal();
            }
        }
    }

    fn wait_exit(self) -> Result<i32, String> {
        self.run.join().expect("event loop thread panicked")
    }
}

#[test]
fn uring_captures_child_output() {
    let mut h = Harness::spawn("/bin/sh", &["-c", "echo hello_uring"]);
    let out = h.wait_output(|o| o.windows(11).any(|w| w == b"hello_uring"));
    assert!(!out.is_empty());
    let code = h.wait_exit().expect("run ok");
    assert_eq!(code, 0);
}

#[test]
fn uring_write_roundtrips_through_cat() {
    let mut h = Harness::spawn("/bin/cat", &[]);
    h.send(PtyInput::Bytes(b"ping_pong\n".to_vec().into_boxed_slice()));
    h.wait_output(|o| o.windows(9).any(|w| w == b"ping_pong"));
    h.send(PtyInput::Quit);
    let code = h.wait_exit().expect("run ok");
    assert_eq!(code, 0);
}

#[test]
fn uring_reports_child_exit_code() {
    let h = Harness::spawn("/bin/sh", &["-c", "exit 42"]);
    let code = h.wait_exit().expect("run ok");
    assert_eq!(code, 42);
}

#[test]
fn uring_quit_message_stops_loop_with_live_child() {
    let h = Harness::spawn("/bin/sh", &["-c", "sleep 30"]);
    h.send(PtyInput::Quit);
    let start = Instant::now();
    let _ = h.wait_exit().expect("run ok");
    assert!(start.elapsed() < Duration::from_secs(5));
}

#[test]
fn uring_shutdown_flag_stops_loop() {
    let h = Harness::spawn("/bin/sh", &["-c", "sleep 30"]);
    h.shutdown.store(true, Ordering::Release);
    h.wake.signal();
    let start = Instant::now();
    let _ = h.wait_exit().expect("run ok");
    assert!(start.elapsed() < Duration::from_secs(5));
}

#[test]
fn uring_resize_reaches_child() {
    let mut h = Harness::spawn("/bin/sh", &["-c", "sleep 0.3; stty size"]);
    h.send(PtyInput::Resize(WinSize {
        rows: 31,
        cols: 113,
        xpixel: 0,
        ypixel: 0,
    }));
    h.wait_output(|o| o.windows(6).any(|w| w == b"31 113"));
    let _ = h.wait_exit();
}

#[test]
fn uring_large_write_stress_many_iterations() {
    for iter in 0..40 {
        uring_large_write_is_chunked_and_ordered_once(iter);
    }
}

#[test]
fn uring_large_write_is_chunked_and_ordered() {
    uring_large_write_is_chunked_and_ordered_once(0);
}

/// Real backpressure: when `on_output` rejects a chunk (simulating a full
/// downstream ring), `Pty::run` must retry the exact same bytes once the
/// caller starts accepting again, never dropping them.
#[test]
fn uring_on_output_false_is_retried_not_dropped() {
    let mut h = Harness::spawn_gated("/bin/sh", &["-c", "stty -echo; echo READY; exec cat"]);
    h.send(PtyInput::Bytes(b"stall_me\n".to_vec().into_boxed_slice()));

    h.rejections
        .pop_block()
        .expect("the gated loop rejects the child's output");

    h.open_gate();

    h.wait_output(|o| o.windows(8).any(|w| w == b"stall_me"));
    h.send(PtyInput::Quit);
    let _ = h.wait_exit();
}

/// Stress variant: repeats the single-shot check many times in one process,
/// to shake out any low-probability ordering/requeue race under `io_uring`
/// that a single iteration might not hit. Kept separate from the base test
/// so a flake here doesn't rename/hide the canonical regression test.
#[test]
fn uring_large_write_is_chunked_and_ordered_stress() {
    for iter in 0..60 {
        uring_large_write_is_chunked_and_ordered_once(iter);
    }
}

fn uring_large_write_is_chunked_and_ordered_once(iter: u32) {
    // `stty -echo` first: the line discipline's echo interleaves with cat's
    // output byte-for-byte, which would shred the needles mid-line.
    let mut h = Harness::spawn("/bin/sh", &["-c", "stty -echo; echo READY; exec cat"]);
    h.wait_output(|o| o.windows(5).any(|w| w == b"READY"));
    let mut payload = Vec::with_capacity(20_000);
    for i in 0..2000u32 {
        payload.extend_from_slice(format!("{i:07}\n").as_bytes());
    }
    // Needles use \r\n: the tty's ONLCR output flag rewrites cat's \n on the
    // way back out.
    let needle_first = b"0000000\r\n".to_vec();
    let needle_last = b"0001999\r\n".to_vec();
    h.send(PtyInput::Bytes(payload.into_boxed_slice()));
    let out = h.wait_output(|o| o.windows(needle_last.len()).any(|w| w == &needle_last[..]));
    let first_pos = out
        .windows(needle_first.len())
        .position(|w| w == &needle_first[..])
        .unwrap_or_else(|| {
            panic!(
                "iter {iter}: first line not present; out.len()={} head={:?} tail={:?}",
                out.len(),
                String::from_utf8_lossy(&out[..out.len().min(120)]),
                String::from_utf8_lossy(&out[out.len().saturating_sub(120)..])
            )
        });
    let last_pos = out
        .windows(needle_last.len())
        .position(|w| w == &needle_last[..])
        .expect("last line present");
    assert!(
        first_pos < last_pos,
        "iter {iter}: order preserved across write chunks"
    );
    h.send(PtyInput::Quit);
    let _ = h.wait_exit();
}

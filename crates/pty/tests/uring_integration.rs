//! Integration tests driving the real `io_uring` `Pty` event loop against a
//! live shell on the actual kernel.
//!
//! Each test spawns the loop on a dedicated thread (as production does),
//! feeds it input through closures + an eventfd wake, and asserts on the
//! output bytes / exit codes it produces.

use pty::{Pty, PtyInput, WinSize};
use std::collections::VecDeque;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const WIN: WinSize = WinSize {
    rows: 24,
    cols: 80,
    xpixel: 0,
    ypixel: 0,
};

/// Test harness: an eventfd-backed input queue + collected output.
struct Harness {
    wake: waker::Waker,
    inputs: Arc<Mutex<VecDeque<PtyInput>>>,
    output: Arc<Mutex<Vec<u8>>>,
    exit_rx: std_mpsc::Receiver<Result<i32, String>>,
    shutdown: Arc<AtomicBool>,
}

impl Harness {
    fn spawn(shell: &str, args: &[&str]) -> Self {
        let wake = waker::Waker::new().expect("eventfd");
        let shutdown = Arc::new(AtomicBool::new(false));
        let pty =
            Pty::spawn(shell, args, WIN, wake.raw_fd(), Arc::clone(&shutdown)).expect("Pty::spawn");

        let inputs: Arc<Mutex<VecDeque<PtyInput>>> = Arc::new(Mutex::new(VecDeque::new()));
        let output: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let (exit_tx, exit_rx) = std_mpsc::channel();

        let inputs_loop = Arc::clone(&inputs);
        let output_loop = Arc::clone(&output);
        std::thread::spawn(move || {
            let next_input = move || inputs_loop.lock().unwrap().pop_front();
            let on_output = move |bytes: &[u8]| {
                output_loop.lock().unwrap().extend_from_slice(bytes);
            };
            let _ = exit_tx.send(pty.run(next_input, on_output).map_err(|e| e.to_string()));
        });

        Self {
            wake,
            inputs,
            output,
            exit_rx,
            shutdown,
        }
    }

    /// Queue an input message and rouse the loop.
    fn send(&self, msg: PtyInput) {
        self.inputs.lock().unwrap().push_back(msg);
        self.wake.signal();
    }

    /// Wait until the collected output satisfies `pred`, or panic.
    fn wait_output(&self, deadline: Duration, pred: impl Fn(&[u8]) -> bool) -> Vec<u8> {
        let start = Instant::now();
        loop {
            {
                let out = self.output.lock().unwrap();
                if pred(&out) {
                    return out.clone();
                }
            }
            assert!(
                start.elapsed() < deadline,
                "timed out waiting for output; got: {:?}",
                String::from_utf8_lossy(&self.output.lock().unwrap())
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Wait for the event loop to finish and return its result.
    fn wait_exit(self, deadline: Duration) -> Result<i32, String> {
        self.exit_rx
            .recv_timeout(deadline)
            .expect("event loop did not exit in time")
    }
}

#[test]
fn uring_captures_child_output() {
    let h = Harness::spawn("/bin/sh", &["-c", "echo hello_uring"]);
    let out = h.wait_output(Duration::from_secs(5), |o| {
        o.windows(11).any(|w| w == b"hello_uring")
    });
    assert!(!out.is_empty());
    let code = h.wait_exit(Duration::from_secs(5)).expect("run ok");
    assert_eq!(code, 0);
}

#[test]
fn uring_write_roundtrips_through_cat() {
    let h = Harness::spawn("/bin/cat", &[]);
    h.send(PtyInput::Bytes(b"ping_pong\n".to_vec().into_boxed_slice()));
    // cat echoes; the PTY line discipline echoes too — either way the bytes
    // must come back through the read SQE path.
    h.wait_output(Duration::from_secs(5), |o| {
        o.windows(9).any(|w| w == b"ping_pong")
    });
    h.send(PtyInput::Quit);
    let code = h.wait_exit(Duration::from_secs(5)).expect("run ok");
    // Quit path hangs the child up; exit code is the loop's default 0.
    assert_eq!(code, 0);
}

#[test]
fn uring_reports_child_exit_code() {
    let h = Harness::spawn("/bin/sh", &["-c", "exit 42"]);
    let code = h.wait_exit(Duration::from_secs(5)).expect("run ok");
    assert_eq!(code, 42);
}

#[test]
fn uring_quit_message_stops_loop_with_live_child() {
    let h = Harness::spawn("/bin/sh", &["-c", "sleep 30"]);
    h.send(PtyInput::Quit);
    // Must return promptly (hangup + reap), not wait out the sleep.
    let start = Instant::now();
    let _ = h.wait_exit(Duration::from_secs(5)).expect("run ok");
    assert!(start.elapsed() < Duration::from_secs(5));
}

#[test]
fn uring_shutdown_flag_stops_loop() {
    let h = Harness::spawn("/bin/sh", &["-c", "sleep 30"]);
    h.shutdown.store(true, std::sync::atomic::Ordering::Release);
    h.wake.signal(); // rouse the parked loop so it observes the flag
    let start = Instant::now();
    let _ = h.wait_exit(Duration::from_secs(5)).expect("run ok");
    assert!(start.elapsed() < Duration::from_secs(5));
}

#[test]
fn uring_resize_reaches_child() {
    // `stty size` prints "rows cols" of the tty — spawn a shell that reports
    // its size after a brief pause, and resize before it samples.
    let h = Harness::spawn("/bin/sh", &["-c", "sleep 0.3; stty size"]);
    h.send(PtyInput::Resize(WinSize {
        rows: 31,
        cols: 113,
        xpixel: 0,
        ypixel: 0,
    }));
    h.wait_output(Duration::from_secs(5), |o| {
        o.windows(6).any(|w| w == b"31 113")
    });
    let _ = h.wait_exit(Duration::from_secs(5));
}

#[test]
fn uring_large_write_is_chunked_and_ordered() {
    // 20 KiB of patterned input through cat — crosses the 4 KiB write-buffer
    // boundary several times; ordering must survive the requeue path.
    // `stty -echo` first: the line discipline's echo interleaves with cat's
    // output byte-for-byte, which would shred the needles mid-line.
    let h = Harness::spawn("/bin/sh", &["-c", "stty -echo; echo READY; exec cat"]);
    // Wait for echo to actually be off before sending anything, else bytes
    // landing early are echoed by the line discipline and interleave.
    h.wait_output(Duration::from_secs(5), |o| {
        o.windows(5).any(|w| w == b"READY")
    });
    let mut payload = Vec::with_capacity(20_000);
    for i in 0..2000u32 {
        payload.extend_from_slice(format!("{i:07}\n").as_bytes());
    }
    // Push in one giant Bytes message. Needles use \r\n: the tty's ONLCR
    // output flag rewrites cat's \n on the way back out.
    let needle_first = b"0000000\r\n".to_vec();
    let needle_last = b"0001999\r\n".to_vec();
    h.send(PtyInput::Bytes(payload.into_boxed_slice()));
    let out = h.wait_output(Duration::from_secs(10), |o| {
        o.windows(needle_last.len()).any(|w| w == &needle_last[..])
    });
    let first_pos = out
        .windows(needle_first.len())
        .position(|w| w == &needle_first[..])
        .expect("first line present");
    let last_pos = out
        .windows(needle_last.len())
        .position(|w| w == &needle_last[..])
        .expect("last line present");
    assert!(first_pos < last_pos, "order preserved across write chunks");
    h.send(PtyInput::Quit);
    let _ = h.wait_exit(Duration::from_secs(5));
}

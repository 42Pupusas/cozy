//! Stress probe for the teardown path of the `io_uring` PTY loop.
//!
//! A `cargo test --workspace` run once aborted inside this crate's test
//! binary with a glibc `tcache_thread_shutdown(): unaligned tcache chunk`
//! SIGABRT. That message means the allocator found corrupt heap metadata as
//! a thread exited, so the damage happened earlier and elsewhere.
//!
//! The suspected mechanism is the teardown of [`Pty::run`]: the loop can
//! return while a read SQE is still armed against `read_buf`. `run` takes
//! `self` by value, so the `Pty` (ring first, then the buffers) is dropped
//! immediately afterwards. If the kernel completes that read after the
//! buffer is freed, it writes into memory the allocator has handed back.
//!
//! The original abort needed a loaded machine and did not repeat in four
//! serial runs. These tests recreate the two conditions the ordinary suite
//! does not:
//!
//! - Many spawn/run/drop cycles, so a rare teardown race gets many chances.
//! - Several loops running and tearing down at once, on their own threads,
//!   so the allocator is under concurrent pressure while a buffer is freed.
//!
//! Each test allocates and frees on the loop thread after the `Pty` drops.
//! A kernel write into a freed `read_buf` corrupts a chunk that these later
//! allocations then reuse, which turns a silent stray write into a visible
//! abort. Run under `MALLOC_CHECK_=3` and `MALLOC_PERTURB_=165` to make
//! glibc verify chunk headers instead of trusting them.
//!
//! These tests are `#[ignore]` by default: they take minutes and are a
//! hunting tool, not a regression gate. Run them with
//! `cargo test -p pty --test teardown_stress -- --ignored --nocapture`.

use pty::{Pty, PtyInput, WinSize};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const WIN: WinSize = WinSize {
    rows: 24,
    cols: 80,
    xpixel: 0,
    ypixel: 0,
};

/// One spawn/run/teardown cycle of the real event loop.
///
/// The child writes continuously, so output is in flight (and a read SQE is
/// armed) at the moment the loop is told to stop -- the exact window the
/// abort is suspected to live in.
struct TeardownCycle {
    shell_command: &'static str,
    stop: StopMode,
}

/// How a cycle asks the event loop to stop.
///
/// The two paths leave the ring in different states, so a teardown bug may
/// only show through one of them.
#[derive(Clone, Copy, Debug)]
enum StopMode {
    /// Queue [`PtyInput::Quit`]; the loop hangs the child up and reaps it.
    QuitMessage,
    /// Set the shared shutdown flag and signal the wake eventfd.
    ShutdownFlag,
}

impl TeardownCycle {
    const fn new(shell_command: &'static str, stop: StopMode) -> Self {
        Self {
            shell_command,
            stop,
        }
    }

    /// Run one cycle to completion. Returns the loop's exit result.
    ///
    /// After the loop returns, this thread allocates and frees a burst of
    /// heap blocks. A kernel write into the just-freed `read_buf` lands in
    /// a chunk this burst is likely to claim, so corruption becomes an
    /// abort here rather than an unexplained one much later.
    fn run(&self) -> Result<i32, String> {
        let wake = waker::Waker::new().expect("eventfd");
        let shutdown = Arc::new(AtomicBool::new(false));
        let pty = Pty::spawn(
            "/bin/sh",
            &["-c", self.shell_command],
            WIN,
            wake.raw_fd(),
            Arc::clone(&shutdown),
        )
        .expect("Pty::spawn");

        let inputs: Arc<Mutex<VecDeque<PtyInput>>> = Arc::new(Mutex::new(VecDeque::new()));
        let seen = Arc::new(AtomicUsize::new(0));

        let inputs_loop = Arc::clone(&inputs);
        let seen_loop = Arc::clone(&seen);
        let handle = std::thread::spawn(move || {
            let next_input = move || inputs_loop.lock().unwrap().pop_front();
            let on_output = move |bytes: &[u8]| {
                seen_loop.fetch_add(bytes.len(), Ordering::Relaxed);
                true
            };
            let outcome = pty.run(next_input, on_output).map_err(|e| e.to_string());

            Self::churn_heap();

            outcome
        });

        Self::wait_for_output(&seen);

        match self.stop {
            StopMode::QuitMessage => {
                inputs.lock().unwrap().push_back(PtyInput::Quit);
                wake.signal();
            }
            StopMode::ShutdownFlag => {
                shutdown.store(true, Ordering::Release);
                wake.signal();
            }
        }

        handle.join().expect("event loop thread panicked")
    }

    /// Wait until the child has produced some output, so a read SQE is
    /// certainly armed when the stop request arrives.
    fn wait_for_output(seen: &Arc<AtomicUsize>) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while seen.load(Ordering::Relaxed) == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Allocate and free a burst of blocks sized like the PTY buffers, to
    /// claim any chunk the kernel may still be writing into.
    fn churn_heap() {
        let mut blocks: Vec<Vec<u8>> = Vec::new();
        for round in 0..64u8 {
            blocks.push(vec![round; 4096]);
            blocks.push(vec![round; 128]);
            blocks.push(vec![round; 8]);
        }
        for block in &blocks {
            std::hint::black_box(block.as_ptr());
        }
        drop(blocks);
    }
}

/// Repeated teardown while the child is mid-write, stopped by `Quit`.
#[test]
#[ignore = "stress probe: minutes long, run explicitly when hunting the abort"]
fn repeated_quit_teardown_under_output_does_not_corrupt_the_heap() {
    let cycle = TeardownCycle::new("while :; do echo spew_spew_spew; done", StopMode::QuitMessage);
    for iteration in 0..300 {
        let outcome = cycle.run();
        assert!(
            outcome.is_ok(),
            "iteration {iteration}: loop failed: {outcome:?}"
        );
    }
}

/// The same, stopped by the shutdown flag instead.
#[test]
#[ignore = "stress probe: minutes long, run explicitly when hunting the abort"]
fn repeated_flag_teardown_under_output_does_not_corrupt_the_heap() {
    let cycle = TeardownCycle::new(
        "while :; do echo spew_spew_spew; done",
        StopMode::ShutdownFlag,
    );
    for iteration in 0..300 {
        let outcome = cycle.run();
        assert!(
            outcome.is_ok(),
            "iteration {iteration}: loop failed: {outcome:?}"
        );
    }
}

/// Several loops spawning and tearing down at once.
///
/// The original abort appeared only during a parallel workspace run, so
/// concurrent allocator pressure is part of the reproduction conditions.
#[test]
#[ignore = "stress probe: minutes long, run explicitly when hunting the abort"]
fn concurrent_teardowns_do_not_corrupt_the_heap() {
    let mut threads = Vec::new();
    for worker in 0..8u32 {
        threads.push(std::thread::spawn(move || {
            let stop = if worker % 2 == 0 {
                StopMode::QuitMessage
            } else {
                StopMode::ShutdownFlag
            };
            let cycle = TeardownCycle::new("while :; do echo spew_spew_spew; done", stop);
            for iteration in 0..40 {
                let outcome = cycle.run();
                assert!(
                    outcome.is_ok(),
                    "worker {worker} iteration {iteration}: loop failed: {outcome:?}"
                );
            }
        }));
    }
    for thread in threads {
        thread.join().expect("worker panicked");
    }
}

/// Teardown immediately after spawn, before the child has written anything.
///
/// This is the tightest window: the read SQE is armed and nothing has
/// completed, so the ring is torn down with an operation certainly in
/// flight.
#[test]
#[ignore = "stress probe: minutes long, run explicitly when hunting the abort"]
fn immediate_teardown_with_a_read_in_flight_does_not_corrupt_the_heap() {
    for iteration in 0..300 {
        let wake = waker::Waker::new().expect("eventfd");
        let shutdown = Arc::new(AtomicBool::new(false));
        let pty = Pty::spawn(
            "/bin/sh",
            &["-c", "sleep 30"],
            WIN,
            wake.raw_fd(),
            Arc::clone(&shutdown),
        )
        .expect("Pty::spawn");

        let inputs: Arc<Mutex<VecDeque<PtyInput>>> = Arc::new(Mutex::new(VecDeque::new()));
        inputs.lock().unwrap().push_back(PtyInput::Quit);
        wake.signal();

        let inputs_loop = Arc::clone(&inputs);
        let handle = std::thread::spawn(move || {
            let next_input = move || inputs_loop.lock().unwrap().pop_front();
            let on_output = |_: &[u8]| true;
            let outcome = pty.run(next_input, on_output).map_err(|e| e.to_string());
            TeardownCycle::churn_heap();
            outcome
        });

        let outcome = handle.join().expect("event loop thread panicked");
        assert!(
            outcome.is_ok(),
            "iteration {iteration}: loop failed: {outcome:?}"
        );
    }
}

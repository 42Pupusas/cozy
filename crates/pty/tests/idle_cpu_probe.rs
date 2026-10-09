//! Regression guard: `Pty::run`'s `submit_and_wait` must actually park in
//! the kernel while idle, not busy-spin.
//!
//! This is a regression test for a real bug: `quetzal_uring::UringQueue`'s
//! `enter`/`submit`/`submit_and_wait` used to hardcode `to_submit =
//! u32::MAX` in the raw `io_uring_enter` syscall. Per the kernel's
//! `io_uring_enter` semantics, `to_submit` is not a mere upper bound: if it
//! doesn't match the number of SQEs the kernel actually finds pending, the
//! kernel returns early -- *before* honouring `GETEVENTS` -- so a
//! "blocking" wait never actually blocks. That turned every idle `Pty::run`
//! loop into a tight retry loop pegging a full CPU core, even with zero
//! I/O. Fixed upstream in quetzal-uring by passing the real pending SQE
//! count.
//!
//! This test spawns an idle child and asserts *process* CPU time barely
//! moves over a multi-second idle window (measured via `/proc/self/stat`,
//! since the spinning happens on a thread inside this same process). A
//! correctly parked `io_uring_enter` should consume a tiny fraction of the
//! wall-clock window; a busy-spin regression consumes essentially 100% of
//! one core for the whole window.
use pty::{Pty, WinSize};
use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const WIN: WinSize = WinSize {
    rows: 24,
    cols: 80,
    xpixel: 0,
    ypixel: 0,
};

/// Total process CPU time (utime + stime) in clock ticks, from
/// `/proc/self/stat` fields 14 and 15.
fn process_cpu_ticks() -> u64 {
    let mut buf = String::new();
    std::fs::File::open("/proc/self/stat")
        .expect("open /proc/self/stat")
        .read_to_string(&mut buf)
        .expect("read /proc/self/stat");
    // Field 2 (comm) may contain spaces/parens; skip past the last ')'.
    let after_comm = buf.rsplit_once(')').expect("stat format").1;
    let fields: Vec<&str> = after_comm.split_whitespace().collect();
    // After the comm field, state is index 0 here; the original field 14
    // (utime) is index 11 in this zero-based slice (fields 1..=2 were
    // pid/comm, already consumed by rsplit_once).
    let utime: u64 = fields[11].parse().expect("utime");
    let stime: u64 = fields[12].parse().expect("stime");
    utime + stime
}

/// `sysconf(_SC_CLK_TCK)` is 100 on essentially every Linux system; using
/// the near-universal constant avoids a libc dependency, with a wide
/// tolerance margin in the assertion below to absorb any drift.
const fn clock_ticks_per_sec() -> u64 {
    100
}

#[test]
fn idle_loop_burns_negligible_cpu() {
    let wake = waker::Waker::new().expect("eventfd");
    let shutdown = Arc::new(AtomicBool::new(false));
    let pty =
        Pty::spawn("/bin/cat", &[], WIN, wake.raw_fd(), Arc::clone(&shutdown)).expect("Pty::spawn");

    let handle = std::thread::spawn(move || {
        let _ = pty.run(|| None, |_output: &[u8]| true);
    });

    // Let the loop settle into (what should be) a parked wait.
    std::thread::sleep(Duration::from_millis(200));

    let ticks_before = process_cpu_ticks();
    let wall_start = Instant::now();
    std::thread::sleep(Duration::from_secs(3));
    let wall_elapsed = wall_start.elapsed();
    let ticks_after = process_cpu_ticks();

    shutdown.store(true, Ordering::Relaxed);
    wake.signal();
    handle
        .join()
        .expect("Pty::run exits after the shutdown flag is set and the loop woken");

    // Tick counts here are small (a few hundred over a 5s probe window), so
    // this narrowing to u32 (then losslessly to f64) is exact -- nowhere
    // near either type's precision/range cliff.
    let tick_delta = u32::try_from(ticks_after - ticks_before).expect("tick delta fits in u32");
    let ticks_per_sec = u32::try_from(clock_ticks_per_sec()).expect("ticks/sec fits in u32");
    let cpu_secs = f64::from(tick_delta) / f64::from(ticks_per_sec);
    let wall_secs = wall_elapsed.as_secs_f64();

    // A correctly parked loop burns a small fraction of the idle window.
    // A busy-spin regression burns essentially the whole window (per the
    // `perf stat` measurements that diagnosed this bug: >100% of one
    // core). Use 50% as a generous, non-flaky threshold.
    assert!(
        cpu_secs < wall_secs * 0.5,
        "Pty::run appears to be busy-spinning while idle: {cpu_secs:.3}s CPU \
         over {wall_secs:.3}s wall-clock (>= 50% of one core). This is the \
         quetzal-uring `to_submit = u32::MAX` regression -- see module docs."
    );
}

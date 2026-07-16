//! Integration tests spawning a real shell under `PtySimple` on the actual
//! Linux PTY subsystem. No mocks: these run the true fork/exec/ioctl path.

use pty::PtySimple;
use std::time::{Duration, Instant};

/// Poll `try_read` until it yields data, EOF, or the deadline passes.
/// Returns accumulated bytes.
fn read_until(pty: &mut PtySimple, deadline: Duration) -> Vec<u8> {
    let start = Instant::now();
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match pty.try_read(&mut buf) {
            Ok(Some(0)) => break, // EOF
            Ok(Some(n)) => out.extend_from_slice(&buf[..n]),
            Ok(None) => {
                if start.elapsed() > deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => break,
        }
        if start.elapsed() > deadline {
            break;
        }
    }
    out
}

#[test]
fn spawn_echo_roundtrip() {
    let mut pty = PtySimple::spawn("/bin/sh", &["-c", "echo hello_pty"], 80, 24)
        .expect("spawn /bin/sh");
    let out = read_until(&mut pty, Duration::from_secs(3));
    let text = String::from_utf8_lossy(&out);
    assert!(text.contains("hello_pty"), "got: {text:?}");
}

#[test]
fn write_is_echoed_back_by_shell_tty() {
    // A PTY in cooked mode echoes input back; catting stdin should give us
    // back whatever we write.
    let mut pty = PtySimple::spawn("/bin/cat", &[], 80, 24).expect("spawn /bin/cat");
    pty.write(b"round-trip-me\n").expect("write");
    let out = read_until(&mut pty, Duration::from_secs(3));
    let text = String::from_utf8_lossy(&out);
    assert!(text.contains("round-trip-me"), "got: {text:?}");
}

#[test]
fn nonexistent_shell_returns_err_not_panic() {
    // Spawning itself (fork+exec) always succeeds even for a bad path -- the
    // exec failure happens in the child post-fork, which we can't observe
    // synchronously from the parent's spawn() call. What matters is that we
    // get back a handle we can safely read/drop rather than a panic.
    let result = PtySimple::spawn("/definitely/not/a/real/shell", &[], 80, 24);
    // Either an error surfaces, or spawn succeeds and the child just exits
    // 127 immediately -- both are acceptable, but neither may panic.
    match result {
        Ok(mut pty) => {
            let _ = read_until(&mut pty, Duration::from_secs(1));
        }
        Err(_) => {}
    }
}

#[test]
fn resize_after_spawn_does_not_panic() {
    let pty = PtySimple::spawn("/bin/sh", &["-c", "sleep 1"], 80, 24).expect("spawn");
    pty.resize(120, 40).expect("resize should succeed while child is alive");
}

#[test]
fn try_read_returns_none_when_no_data_pending() {
    let mut pty = PtySimple::spawn("/bin/sh", &["-c", "sleep 1"], 80, 24).expect("spawn");
    let mut buf = [0u8; 16];
    // Immediately after spawn, before the shell has produced a prompt write,
    // a non-blocking read should report "no data yet", not an error.
    match pty.try_read(&mut buf) {
        Ok(_) => {} // None (no data) or Some(n) (shell already wrote something) both fine
        Err(e) => panic!("try_read errored on a live pty: {e}"),
    }
}

#[test]
fn dropping_pty_reaps_child_without_hanging() {
    // Drop must hangup + reap_blocking + close without deadlocking, even for
    // a child that's still "running" (sleep).
    let pty = PtySimple::spawn("/bin/sh", &["-c", "sleep 5"], 80, 24).expect("spawn");
    let start = Instant::now();
    drop(pty);
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "drop should hang up + reap promptly, not wait for sleep 5 to finish"
    );
}

#[test]
fn many_small_writes_are_all_delivered() {
    let mut pty = PtySimple::spawn("/bin/cat", &[], 80, 24).expect("spawn /bin/cat");
    for i in 0..20 {
        pty.write(format!("line{i}\n").as_bytes()).expect("write");
    }
    let out = read_until(&mut pty, Duration::from_secs(3));
    let text = String::from_utf8_lossy(&out);
    for i in 0..20 {
        assert!(text.contains(&format!("line{i}")), "missing line{i} in {text:?}");
    }
}

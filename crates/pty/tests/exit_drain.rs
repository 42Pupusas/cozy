//! What the event loop does when the child exits with output unread.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use pty::{Pty, WinSize};

const WIN: WinSize = WinSize {
    rows: 24,
    cols: 80,
    xpixel: 0,
    ypixel: 0,
};

struct Run {
    output: Vec<u8>,
    code: i32,
    elapsed: Duration,
}

impl Run {
    fn of(script: &str) -> Self {
        let wake = waker::Waker::new().expect("eventfd");
        let pty = Pty::spawn(
            "/bin/sh",
            &["-c", script],
            WIN,
            wake.raw_fd(),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("Pty::spawn");
        let mut output = Vec::new();
        let start = Instant::now();
        let code = pty
            .run(
                || None,
                |bytes: &[u8]| {
                    output.extend_from_slice(bytes);
                    true
                },
            )
            .expect("pty run");
        Self {
            output,
            code,
            elapsed: start.elapsed(),
        }
    }
}

/// A background process inherits the slave, so the master never reports
/// end-of-file after the shell exits. The loop must still deliver what the
/// shell wrote and then stop.
#[test]
fn a_surviving_background_process_does_not_hang_the_loop() {
    let run = Run::of("sleep 30 & echo last_words");
    assert!(
        String::from_utf8_lossy(&run.output).contains("last_words"),
        "got {:?}",
        String::from_utf8_lossy(&run.output)
    );
    assert_eq!(run.code, 0);
    assert!(run.elapsed < Duration::from_secs(5), "took {:?}", run.elapsed);
}

#[test]
fn every_line_written_before_a_fast_exit_arrives() {
    const LINES: usize = 20_000;
    let run = Run::of(&format!("seq 1 {LINES}"));
    let text = String::from_utf8_lossy(&run.output);
    let seen: Vec<&str> = text.lines().collect();
    assert_eq!(seen.len(), LINES);
    assert_eq!(seen.last().copied(), Some(LINES.to_string().as_str()));
}

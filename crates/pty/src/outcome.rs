//! How the event loop ended, and the child's exit code.

use crate::sys::{ChildPid, WaitStatus};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// The child's exit code; 0 when the loop stopped before it exited.
    pub code: i32,
    /// The child has been reaped, so teardown must not hang it up.
    pub reaped: bool,
    /// The master fd hit EOF/EIO: the child closed the slave side.
    eof: bool,
    /// No more output will come from the master: it hit EOF/EIO.
    pub done: bool,
}

impl Outcome {
    /// A SIGCHLD arrived. Returns `false` when the child is still running
    /// (stop/continue), so the signal read has to be re-armed. This does not
    /// end the loop: the output the child left behind still has to be read.
    /// When the session leader exits the kernel hangs up the pty, so the
    /// master reaches EOF/EIO after that output even if a background process
    /// still holds the slave.
    pub fn child_signalled(&mut self, child: ChildPid) -> bool {
        match child.wait_nohang() {
            Ok(status) => match Self::exit_code(status) {
                Some(code) => {
                    self.code = code;
                    self.reaped = true;
                }
                None => return false,
            },
            // wait4 failed: don't re-arm a signalfd that can't tell us
            // anything, and don't fabricate a success code.
            Err(_) => self.code = -1,
        }
        true
    }

    /// EOF or EIO on the master fd. The SIGCHLD completion — possibly in
    /// this same batch, possibly not yet delivered — carries the exit code;
    /// [`reap_after_eof`](Self::reap_after_eof) covers the case where it
    /// never arrives.
    pub const fn hit_eof(&mut self) {
        self.eof = true;
        self.done = true;
    }

    /// The master fd can report EOF before the SIGCHLD completion is seen.
    /// The child is exiting either way, so reap it synchronously and recover
    /// the real exit code instead of returning a bogus 0.
    pub fn reap_after_eof(&mut self, child: ChildPid) {
        if !self.eof || self.reaped {
            return;
        }
        // None means wait4 failed (e.g. already reaped by a racing SIGCHLD
        // handler) — keep whatever code we have.
        if let Some(code) = child.reap_blocking_status().and_then(Self::exit_code) {
            self.code = code;
        }
        self.reaped = true;
    }

    /// The exit code for a finished child, using the shell convention
    /// (128 + signum) for one killed by a signal.
    const fn exit_code(status: WaitStatus) -> Option<i32> {
        match status {
            WaitStatus::Exited(code) => Some(code),
            WaitStatus::Signaled(sig) => Some(128 + sig),
            WaitStatus::StillRunning => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Outcome;
    use crate::sys::{ChildPid, WaitStatus};

    #[test]
    fn a_normal_exit_reports_its_code() {
        assert_eq!(Outcome::exit_code(WaitStatus::Exited(3)), Some(3));
    }

    #[test]
    fn a_signal_death_reports_128_plus_the_signal() {
        assert_eq!(Outcome::exit_code(WaitStatus::Signaled(9)), Some(137));
    }

    #[test]
    fn a_running_child_has_no_exit_code() {
        assert_eq!(Outcome::exit_code(WaitStatus::StillRunning), None);
    }

    #[test]
    fn eof_stops_the_loop() {
        let mut outcome = Outcome::default();
        outcome.hit_eof();
        assert!(outcome.done);
    }

    #[test]
    fn without_eof_there_is_nothing_to_reap() {
        let mut outcome = Outcome::default();
        outcome.reap_after_eof(ChildPid(0));
        assert_eq!(outcome, Outcome::default());
    }

    /// pid 0 is invalid, so wait4 is never reached and the code stays put.
    #[test]
    fn a_failed_reap_after_eof_keeps_the_code() {
        let mut outcome = Outcome {
            code: 7,
            ..Outcome::default()
        };
        outcome.hit_eof();
        outcome.reap_after_eof(ChildPid(0));
        assert_eq!(outcome.code, 7);
        assert!(outcome.reaped);
    }
}

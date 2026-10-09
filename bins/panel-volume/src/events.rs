//! The audio server's change notifications, read from `pactl subscribe`.
//!
//! The subprocess prints one line per change (`Event 'change' on sink #3`)
//! and nothing otherwise, so its stdout descriptor is readable exactly when
//! something happened. The worker polls that descriptor instead of timing
//! its own refreshes.

use std::io::{self, Read};
use std::os::fd::{AsRawFd, RawFd};
use std::process::{Child, ChildStdout, Command, Stdio};

/// Line splitter and filter for the `pactl subscribe` text stream.
#[derive(Debug, Default)]
pub struct EventLines {
    partial: Vec<u8>,
}

impl EventLines {
    /// Facilities whose events change what the panel shows. `client`,
    /// `card`, `module` and `sample_cache` are excluded: every `pactl`
    /// invocation, including the panel's own snapshots, connects as a
    /// client, so reacting to those would feed back on itself.
    const WATCHED_FACILITIES: [&'static str; 5] =
        ["sink", "source", "sink-input", "source-output", "server"];

    /// Feed raw bytes; returns whether any completed line was a change to
    /// a watched facility.
    pub fn feed(&mut self, bytes: &[u8]) -> bool {
        self.partial.extend_from_slice(bytes);
        let mut changed = false;
        while let Some(end) = self.partial.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=end).collect();
            changed |= Self::is_mixer_change(&String::from_utf8_lossy(&line));
        }
        changed
    }

    fn is_mixer_change(line: &str) -> bool {
        let mut words = line.split_whitespace();
        words.next() == Some("Event")
            && words
                .nth(2)
                .is_some_and(|facility| Self::WATCHED_FACILITIES.contains(&facility))
    }
}

/// What one read from the event stream produced.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Absorbed {
    pub changed: bool,
    /// The subprocess ended; no further events will arrive.
    pub closed: bool,
}

/// A running `pactl subscribe`.
pub struct MixerEvents {
    child: Child,
    stdout: ChildStdout,
    lines: EventLines,
}

impl MixerEvents {
    /// Start `pactl subscribe`.
    ///
    /// # Errors
    /// Returns the OS error if `pactl` cannot be started.
    pub fn spawn() -> io::Result<Self> {
        let mut child = Command::new("pactl")
            .arg("subscribe")
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("pactl subscribe has no stdout"))?;
        Ok(Self {
            child,
            stdout,
            lines: EventLines::default(),
        })
    }

    /// The descriptor that becomes readable when events are waiting.
    #[must_use]
    pub fn raw_fd(&self) -> RawFd {
        self.stdout.as_raw_fd()
    }

    /// Read what is available. Call only after the descriptor polled
    /// readable; the read then returns at once.
    pub fn absorb(&mut self) -> Absorbed {
        let mut buf = [0u8; 4096];
        match self.stdout.read(&mut buf) {
            Ok(0) => Absorbed {
                changed: false,
                closed: true,
            },
            Ok(n) => Absorbed {
                changed: self.lines.feed(&buf[..n]),
                closed: false,
            },
            Err(e) if e.kind() == io::ErrorKind::Interrupted => Absorbed::default(),
            Err(_) => Absorbed {
                changed: false,
                closed: true,
            },
        }
    }
}

impl Drop for MixerEvents {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sink_change_is_a_mixer_change() {
        assert!(EventLines::default().feed(b"Event 'change' on sink #3\n"));
    }

    #[test]
    fn stream_arrival_and_removal_are_mixer_changes() {
        let mut lines = EventLines::default();
        assert!(lines.feed(b"Event 'new' on sink-input #12\n"));
        assert!(lines.feed(b"Event 'remove' on source-output #4\n"));
    }

    #[test]
    fn a_default_device_switch_is_a_mixer_change() {
        assert!(EventLines::default().feed(b"Event 'change' on server #4294967295\n"));
    }

    #[test]
    fn client_events_caused_by_pactl_itself_are_ignored() {
        let mut lines = EventLines::default();
        assert!(!lines.feed(b"Event 'new' on client #90\nEvent 'remove' on client #90\n"));
    }

    #[test]
    fn a_line_split_across_reads_is_judged_once_complete() {
        let mut lines = EventLines::default();
        assert!(!lines.feed(b"Event 'change' on si"));
        assert!(lines.feed(b"nk #0\n"));
    }

    #[test]
    fn one_relevant_line_among_ignored_ones_is_enough() {
        let mut lines = EventLines::default();
        assert!(lines.feed(b"Event 'new' on client #7\nEvent 'change' on source #1\n"));
    }

    #[test]
    fn unrelated_text_is_ignored() {
        assert!(!EventLines::default().feed(b"Connection failure: Connection refused\n"));
    }
}

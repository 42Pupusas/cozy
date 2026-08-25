//! A stand-in for polkit's helper, so the conversation is testable.
//!
//! The real helper runs as root and only accepts a cookie polkit issued, so
//! a test cannot drive it end to end. This speaks the same line protocol over
//! a socket pair, which lets [`crate::HelperSession::converse`] be exercised
//! against a real socket rather than a mock -- including the parts that only
//! appear over a wire, such as a peer hanging up mid-conversation.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::thread::JoinHandle;

/// What the fake helper should do when it receives an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FakeVerdict {
    /// Reply `SUCCESS`.
    Accept,
    /// Reply `FAILURE`.
    Reject,
    /// Close the connection without a verdict.
    HangUp,
}

/// A scripted helper running on the far end of a socket pair.
pub struct FakeHelper {
    handle: Option<JoinHandle<Vec<String>>>,
}

impl FakeHelper {
    /// Start a helper that sends `script` and then applies `verdict` to the
    /// first answer it receives. Returns the stream the agent should use.
    #[must_use]
    pub fn start(script: Vec<String>, verdict: FakeVerdict) -> (Self, UnixStream) {
        let (ours, theirs) = UnixStream::pair().expect("a socket pair");
        let handle = std::thread::spawn(move || Self::run(&theirs, &script, verdict));
        (
            Self {
                handle: Some(handle),
            },
            ours,
        )
    }

    /// Everything the agent sent, in order, once the helper has finished.
    pub fn received(&mut self) -> Vec<String> {
        self.handle
            .take()
            .map(|h| h.join().expect("the helper thread"))
            .unwrap_or_default()
    }

    /// The helper's side of the conversation.
    fn run(stream: &UnixStream, script: &[String], verdict: FakeVerdict) -> Vec<String> {
        let mut writer = stream.try_clone().expect("a writable clone");
        let mut reader = BufReader::new(stream);
        let mut received = Vec::new();

        for line in script {
            if writeln!(writer, "{line}").is_err() {
                return received;
            }
            let _ = writer.flush();

            if !Self::expects_answer(line) {
                continue;
            }
            let mut answer = String::new();
            match reader.read_line(&mut answer) {
                Ok(0) | Err(_) => return received,
                Ok(_) => received.push(answer.trim_end().to_owned()),
            }
        }

        match verdict {
            FakeVerdict::Accept => {
                let _ = writeln!(writer, "SUCCESS");
            }
            FakeVerdict::Reject => {
                let _ = writeln!(writer, "FAILURE");
            }
            FakeVerdict::HangUp => return received,
        }
        let _ = writer.flush();
        received
    }

    /// Whether a scripted line is a prompt the agent must answer.
    fn expects_answer(line: &str) -> bool {
        line.starts_with("PAM_PROMPT_ECHO_OFF ") || line.starts_with("PAM_PROMPT_ECHO_ON ")
    }
}

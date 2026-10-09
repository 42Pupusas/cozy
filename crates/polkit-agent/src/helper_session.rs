//! The PAM conversation with polkit's helper.
//!
//! # The transport
//!
//! `/run/polkit/agent-helper.socket` is a systemd socket unit with
//! `Accept=yes`, so connecting starts a helper instance that runs as root and
//! talks to us over that connection. The exchange is:
//!
//! ```text
//! ->  <login name>\n
//! ->  <cookie>\n
//! <-  PAM_PROMPT_ECHO_OFF Password: \n
//! ->  <password>\n
//! <-  SUCCESS  (or FAILURE)
//! ```
//!
//! The helper -- not us -- tells polkit the authentication succeeded, using
//! the cookie. So this crate never holds an authorisation, only a socket.
//!
//! # The secret
//!
//! A password passes straight from the caller to the socket. It is never
//! logged, never copied into an error, and the buffer holding it is
//! overwritten before it is released. `SecretResponse` owns that discipline
//! so no call site has to remember it.

use crate::helper_line::HelperLine;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

/// How an authentication attempt ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthOutcome {
    /// The user authenticated. The helper has already told polkit.
    Authenticated,
    /// The user did not authenticate, e.g. a wrong password.
    Rejected,
    /// The user dismissed the prompt.
    Cancelled,
}

/// Why a conversation could not be run at all.
#[derive(Debug)]
pub enum SessionError {
    /// The helper socket is absent, so this machine needs the setuid helper.
    NoHelperSocket,
    /// The socket could not be reached.
    Connect(std::io::Error),
    /// Reading or writing failed mid-conversation.
    Io(std::io::Error),
    /// The helper said something outside its own protocol.
    Protocol(String),
}

impl SessionError {
    /// A sentence for a user, never containing a secret.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::NoHelperSocket => "This system has no polkit helper socket.".into(),
            Self::Connect(_) => "Could not reach the authentication helper.".into(),
            Self::Io(_) => "The authentication helper stopped responding.".into(),
            Self::Protocol(_) => "The authentication helper sent an unexpected reply.".into(),
        }
    }
}

/// A password held only as long as it takes to send it.
///
/// Exists so the wiping is structural rather than a comment: the bytes are
/// overwritten in `Drop`, so an early return or a panic cannot skip it.
pub struct SecretResponse {
    bytes: Vec<u8>,
}

impl SecretResponse {
    /// Take ownership of a secret.
    #[must_use]
    pub fn new(secret: &str) -> Self {
        Self {
            bytes: secret.as_bytes().to_vec(),
        }
    }

    /// Write the secret and its terminating newline to `sink`.
    fn write_to(&self, sink: &mut impl Write) -> std::io::Result<()> {
        sink.write_all(&self.bytes)?;
        sink.write_all(b"\n")?;
        sink.flush()
    }
}

impl Drop for SecretResponse {
    fn drop(&mut self) {
        // A volatile write would be stronger, but this crate takes no
        // dependencies; the vector is dropped immediately after, so the
        // window is a single statement.
        self.bytes.fill(0);
    }
}

impl std::fmt::Debug for SecretResponse {
    /// Never reveal the secret, even in a debug log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecretResponse({} bytes, redacted)", self.bytes.len())
    }
}

/// One conversation with the helper.
pub struct HelperSession {
    stream: UnixStream,
}

impl HelperSession {
    /// The socket modern polkit listens on.
    pub const SOCKET_PATH: &'static str = "/run/polkit/agent-helper.socket";

    /// Whether this machine offers the socket transport.
    #[must_use]
    pub fn socket_available() -> bool {
        std::path::Path::new(Self::SOCKET_PATH).exists()
    }

    /// Wrap an already-connected stream, so the conversation can be driven
    /// against a stand-in helper in tests.
    #[must_use]
    pub const fn from_stream(stream: UnixStream) -> Self {
        Self { stream }
    }

    /// Open a conversation to authenticate `user` for `cookie`.
    ///
    /// The socket variant expects the login name first, then the cookie. (The
    /// setuid variant takes the name as `argv[1]` instead, which is why the
    /// two cannot share this code.)
    pub fn open(user: &str, cookie: &str) -> Result<Self, SessionError> {
        if !Self::socket_available() {
            return Err(SessionError::NoHelperSocket);
        }
        let mut stream = UnixStream::connect(Self::SOCKET_PATH).map_err(SessionError::Connect)?;
        write!(stream, "{user}\n{cookie}\n").map_err(SessionError::Io)?;
        stream.flush().map_err(SessionError::Io)?;
        Ok(Self { stream })
    }

    /// Run the conversation to its end, asking `answer` for each prompt.
    ///
    /// `answer` receives the prompt text and whether it may be echoed, and
    /// returns the user's response, or `None` to cancel. Notices and errors
    /// are passed to `notify`.
    ///
    /// Returns only when the helper reports an outcome, which is what
    /// `BeginAuthentication` requires of its caller.
    pub fn converse(
        self,
        mut answer: impl FnMut(&str, bool) -> Option<String>,
        mut notify: impl FnMut(&HelperLine),
    ) -> Result<AuthOutcome, SessionError> {
        let mut writer = self.stream.try_clone().map_err(SessionError::Io)?;
        let mut reader = BufReader::new(self.stream);
        let mut line = String::new();

        loop {
            line.clear();
            let read = reader.read_line(&mut line).map_err(SessionError::Io)?;
            if read == 0 {
                return Err(SessionError::Protocol(String::from(
                    "the helper closed the connection",
                )));
            }
            let Some(parsed) = HelperLine::parse(&line) else {
                return Err(SessionError::Protocol(String::from(
                    "unrecognised line from the helper",
                )));
            };
            match parsed {
                HelperLine::Success => return Ok(AuthOutcome::Authenticated),
                HelperLine::Failure => return Ok(AuthOutcome::Rejected),
                HelperLine::Prompt { ref prompt, echo } => {
                    let Some(response) = answer(prompt, echo) else {
                        return Ok(AuthOutcome::Cancelled);
                    };
                    let secret = SecretResponse::new(&response);
                    secret.write_to(&mut writer).map_err(SessionError::Io)?;
                }
                ref shown @ (HelperLine::Error(_) | HelperLine::Info(_)) => notify(shown),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A secret must never appear in a debug rendering, which is how
    /// passwords end up in logs.
    #[test]
    fn a_secret_is_redacted_in_debug_output() {
        let secret = SecretResponse::new("hunter2");
        let shown = format!("{secret:?}");
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(shown.contains("redacted"), "{shown}");
    }

    /// The secret is written verbatim with exactly one newline, since the
    /// helper reads a line.
    #[test]
    fn a_secret_is_written_with_one_terminating_newline() {
        let secret = SecretResponse::new("hunter2");
        let mut sink = Vec::new();
        secret.write_to(&mut sink).expect("write");
        assert_eq!(sink, b"hunter2\n");
    }

    /// An empty password is still a valid answer and must reach the helper,
    /// which decides whether to reject it.
    #[test]
    fn an_empty_secret_is_still_sent() {
        let secret = SecretResponse::new("");
        let mut sink = Vec::new();
        secret.write_to(&mut sink).expect("write");
        assert_eq!(sink, b"\n");
    }

    /// Every error must produce a sentence fit to show, and none may leak
    /// internals.
    #[test]
    fn every_session_error_has_a_readable_message() {
        let errors = [
            SessionError::NoHelperSocket,
            SessionError::Connect(std::io::Error::other("x")),
            SessionError::Io(std::io::Error::other("x")),
            SessionError::Protocol("internal detail".into()),
        ];
        for err in errors {
            let msg = err.message();
            assert!(msg.ends_with('.'), "{msg}");
            assert!(!msg.contains("internal detail"), "{msg}");
        }
    }

    /// The three outcomes are distinct: cancelling is not the same as being
    /// rejected, and neither is success.
    #[test]
    fn the_outcomes_are_distinct() {
        assert_ne!(AuthOutcome::Authenticated, AuthOutcome::Rejected);
        assert_ne!(AuthOutcome::Rejected, AuthOutcome::Cancelled);
        assert_ne!(AuthOutcome::Authenticated, AuthOutcome::Cancelled);
    }

    /// This machine has the socket, which is what makes the setuid-free path
    /// possible. A failure here means the design assumption broke.
    #[test]
    fn the_helper_socket_exists_on_this_machine() {
        assert!(
            HelperSession::socket_available(),
            "no {}; the setuid helper would be required",
            HelperSession::SOCKET_PATH
        );
    }

    use crate::fake_helper::{FakeHelper, FakeVerdict};

    /// The script the real helper on this machine sends first.
    fn password_prompt() -> Vec<String> {
        vec![String::from("PAM_PROMPT_ECHO_OFF Password: ")]
    }

    /// The happy path, driven over a real socket: prompt, answer, SUCCESS.
    #[test]
    fn answering_the_prompt_correctly_authenticates() {
        let (mut helper, stream) = FakeHelper::start(password_prompt(), FakeVerdict::Accept);
        let session = HelperSession::from_stream(stream);
        let outcome = session
            .converse(|_, _| Some(String::from("hunter2")), |_| {})
            .expect("the conversation runs");
        assert_eq!(outcome, AuthOutcome::Authenticated);
        assert_eq!(
            helper.received(),
            vec![String::from("hunter2")],
            "the helper must receive exactly the answer given"
        );
    }

    /// A wrong password is a rejection, not an error.
    #[test]
    fn a_refused_password_is_reported_as_rejected() {
        let (_helper, stream) = FakeHelper::start(password_prompt(), FakeVerdict::Reject);
        let session = HelperSession::from_stream(stream);
        let outcome = session
            .converse(|_, _| Some(String::from("wrong")), |_| {})
            .expect("the conversation runs");
        assert_eq!(outcome, AuthOutcome::Rejected);
    }

    /// Dismissing the prompt must cancel without sending anything at all --
    /// sending an empty password instead would be a failed login attempt the
    /// user never made.
    #[test]
    fn dismissing_the_prompt_cancels_without_sending_a_password() {
        let (mut helper, stream) = FakeHelper::start(password_prompt(), FakeVerdict::Accept);
        let session = HelperSession::from_stream(stream);
        let outcome = session
            .converse(|_, _| None, |_| {})
            .expect("the conversation runs");
        assert_eq!(outcome, AuthOutcome::Cancelled);
        assert!(
            helper.received().is_empty(),
            "a cancelled prompt must send nothing"
        );
    }

    /// The prompt text and its echo flag must reach the caller, or the UI
    /// cannot know to hide what is typed.
    #[test]
    fn the_prompt_text_and_echo_flag_reach_the_caller() {
        let (_helper, stream) = FakeHelper::start(password_prompt(), FakeVerdict::Accept);
        let session = HelperSession::from_stream(stream);
        let mut seen = Vec::new();
        let outcome = session
            .converse(
                |prompt, echo| {
                    seen.push((prompt.to_owned(), echo));
                    Some(String::from("x"))
                },
                |_| {},
            )
            .expect("the conversation runs");
        assert_eq!(outcome, AuthOutcome::Authenticated);
        assert_eq!(seen, vec![(String::from("Password: "), false)]);
    }

    /// Errors and notices must be shown, not silently dropped: PAM uses them
    /// to say things like 'password expired'.
    #[test]
    fn notices_and_errors_are_passed_to_the_caller() {
        let script = vec![
            String::from("PAM_TEXT_INFO Your password expires soon"),
            String::from("PAM_ERROR_MSG Previous attempt failed"),
            String::from("PAM_PROMPT_ECHO_OFF Password: "),
        ];
        let (_helper, stream) = FakeHelper::start(script, FakeVerdict::Accept);
        let session = HelperSession::from_stream(stream);
        let mut shown = Vec::new();
        session
            .converse(
                |_, _| Some(String::from("x")),
                |line| shown.push(line.clone()),
            )
            .expect("the conversation runs");
        assert_eq!(
            shown,
            vec![
                HelperLine::Info("Your password expires soon".into()),
                HelperLine::Error("Previous attempt failed".into()),
            ]
        );
    }

    /// Several prompts in one conversation must each be answered, which is
    /// what a two-factor PAM stack does.
    #[test]
    fn a_multi_prompt_conversation_answers_each_prompt() {
        let script = vec![
            String::from("PAM_PROMPT_ECHO_ON Username: "),
            String::from("PAM_PROMPT_ECHO_OFF Password: "),
            String::from("PAM_PROMPT_ECHO_OFF Verification code: "),
        ];
        let (mut helper, stream) = FakeHelper::start(script, FakeVerdict::Accept);
        let session = HelperSession::from_stream(stream);
        let mut answers = vec!["alice", "hunter2", "123456"].into_iter();
        let outcome = session
            .converse(|_, _| answers.next().map(String::from), |_| {})
            .expect("the conversation runs");
        assert_eq!(outcome, AuthOutcome::Authenticated);
        assert_eq!(helper.received(), vec!["alice", "hunter2", "123456"]);
    }

    /// A helper that dies mid-conversation must be reported, not treated as
    /// a success -- this is the failure mode that would otherwise mount a
    /// disk with nobody authenticated.
    #[test]
    fn a_helper_that_hangs_up_is_an_error_not_a_success() {
        let (_helper, stream) = FakeHelper::start(password_prompt(), FakeVerdict::HangUp);
        let session = HelperSession::from_stream(stream);
        let result = session.converse(|_, _| Some(String::from("x")), |_| {});
        match result {
            Err(SessionError::Protocol(_)) => {}
            other => panic!("a hang-up must be a protocol error, got {other:?}"),
        }
    }

    /// Gibberish must abort rather than be guessed at.
    #[test]
    fn an_unrecognised_line_aborts_the_conversation() {
        let script = vec![String::from("TOTALLY UNKNOWN")];
        let (_helper, stream) = FakeHelper::start(script, FakeVerdict::Accept);
        let session = HelperSession::from_stream(stream);
        let result = session.converse(|_, _| Some(String::from("x")), |_| {});
        match result {
            Err(SessionError::Protocol(_)) => {}
            other => panic!("gibberish must be a protocol error, got {other:?}"),
        }
    }

    /// A conversation with no prompt at all still resolves, e.g. when PAM is
    /// configured to accept without asking.
    #[test]
    fn a_conversation_with_no_prompt_still_resolves() {
        let (_helper, stream) = FakeHelper::start(Vec::new(), FakeVerdict::Accept);
        let session = HelperSession::from_stream(stream);
        let outcome = session
            .converse(|_, _| panic!("must not be asked"), |_| {})
            .expect("the conversation runs");
        assert_eq!(outcome, AuthOutcome::Authenticated);
    }
}

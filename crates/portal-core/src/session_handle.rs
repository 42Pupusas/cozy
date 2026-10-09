//! The `Session` object path, and the lifecycle rules that decide when a
//! `Closed` signal may be emitted.
//!
//! A session's path is shaped like a request's, but its lifetime is not:
//! a request ends once, when its interaction ends, while a session stays
//! alive across many calls and can be ended from either side. The spec
//! gives each side its own verb -- the *caller* calls `Close`, the
//! *backend* emits `Closed` -- and they are not interchangeable:
//!
//! - The caller called `Close`. The session ends and the backend emits no
//!   `Closed` signal; the caller already knows.
//! - The backend ended it, or the caller fell off the bus. The session
//!   ends and `Closed` is emitted.
//!
//! Encoding that as a state machine here keeps the "did we already close
//! this, and does this ending owe a signal" question in tested domain
//! code. Answered ad hoc at each call site, it produces either a lost
//! signal (the caller waits forever) or a doubled one (the caller sees a
//! session end twice).

use crate::caller_id::CallerId;
use crate::request_handle::RequestHandle;
use dbus::value::Value;

/// The prefix every session path this backend exports begins with.
const SESSION_PREFIX: &str = "/org/freedesktop/portal/desktop/session";

/// Who ended a session, which decides whether a `Closed` signal is owed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseCause {
    /// The caller invoked `Session.Close`.
    CallerRequested,
    /// This backend ended the session itself.
    BackendEnded,
    /// The caller lost its bus name.
    CallerDisappeared,
}

impl CloseCause {
    /// Whether a session ended this way owes the caller a `Closed` signal.
    #[must_use]
    pub const fn owes_closed_signal(self) -> bool {
        match self {
            Self::CallerRequested | Self::CallerDisappeared => false,
            Self::BackendEnded => true,
        }
    }
}

/// A live or closed portal session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionHandle {
    path: String,
    owner: String,
    closed: bool,
}

impl SessionHandle {
    /// Build a session for `caller` carrying `token`.
    ///
    /// The token rules are a request's: this is the same kind of object
    /// path element, so it gets the same validation rather than a second
    /// copy that could drift.
    #[must_use]
    pub fn new(caller: &CallerId, token: &str) -> Option<Self> {
        if !RequestHandle::is_valid_token(token) {
            return None;
        }
        Some(Self {
            path: format!("{SESSION_PREFIX}/{}/{token}", caller.as_segment()),
            owner: caller.as_segment().to_string(),
            closed: false,
        })
    }

    /// Build the session named by a call's sender and `session_handle_token`.
    #[must_use]
    pub fn from_call(sender: &str, options: &[(String, Value)]) -> Option<Self> {
        let caller = CallerId::from_unique_name(sender)?;
        let token = Self::token_from_options(options)?;
        Self::new(&caller, token)
    }

    /// Read `session_handle_token` out of an options vardict.
    ///
    /// Unlike `handle_token` this has no documented default: a session the
    /// caller never named is a session it cannot address, so an absent
    /// token is an error rather than a guess.
    #[must_use]
    pub fn token_from_options(options: &[(String, Value)]) -> Option<&str> {
        let (_, value) = options
            .iter()
            .find(|(key, _)| key == "session_handle_token")?;
        Self::text_of(value).filter(|token| RequestHandle::is_valid_token(token))
    }

    fn text_of(value: &Value) -> Option<&str> {
        match value {
            Value::Str(text) | Value::ObjectPath(text) => Some(text),
            Value::Variant(inner) => Self::text_of(inner),
            _ => None,
        }
    }

    /// The path as it goes on the wire.
    #[must_use]
    pub fn as_path(&self) -> &str {
        &self.path
    }

    /// The path as an `o` value.
    #[must_use]
    pub fn as_value(&self) -> Value {
        Value::ObjectPath(self.path.clone())
    }

    /// Whether this session is still live.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        !self.closed
    }

    /// Whether `sender` is the caller that owns this session.
    ///
    /// Another caller must not be able to close a session it does not own
    /// merely by naming its path, which is public the moment it is
    /// returned.
    #[must_use]
    pub fn is_owned_by(&self, sender: &str) -> bool {
        CallerId::from_unique_name(sender).is_some_and(|caller| caller.as_segment() == self.owner)
    }

    /// End the session, reporting whether a `Closed` signal is owed.
    ///
    /// Closing an already-closed session owes nothing and is not an error:
    /// a caller's `Close` racing this backend's own teardown is ordinary,
    /// and the second one must stay silent rather than emit a duplicate.
    pub const fn close(&mut self, cause: CloseCause) -> bool {
        if self.closed {
            return false;
        }
        self.closed = true;
        cause.owes_closed_signal()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caller() -> CallerId {
        CallerId::from_unique_name(":1.42").expect("valid unique name")
    }

    fn session() -> SessionHandle {
        SessionHandle::new(&caller(), "sess1").expect("valid token")
    }

    fn option(key: &str, token: &str) -> Vec<(String, Value)> {
        vec![(
            key.to_string(),
            Value::Variant(Box::new(Value::Str(token.into()))),
        )]
    }

    #[test]
    fn a_session_path_has_the_documented_shape() {
        assert_eq!(
            session().as_path(),
            "/org/freedesktop/portal/desktop/session/1_42/sess1"
        );
    }

    #[test]
    fn a_session_path_is_not_a_request_path() {
        let request = RequestHandle::new(&caller(), "sess1").expect("valid token");
        assert_ne!(session().as_path(), request.as_path());
    }

    #[test]
    fn a_token_with_a_path_separator_is_refused() {
        assert_eq!(SessionHandle::new(&caller(), "a/b"), None);
    }

    #[test]
    fn a_new_session_is_open() {
        assert!(session().is_open());
    }

    #[test]
    fn an_absent_session_token_is_an_error_with_no_default() {
        assert_eq!(SessionHandle::token_from_options(&[]), None);
    }

    #[test]
    fn a_session_token_arrives_wrapped_in_a_variant() {
        let options = option("session_handle_token", "sess9");
        assert_eq!(SessionHandle::token_from_options(&options), Some("sess9"));
    }

    #[test]
    fn a_request_token_does_not_name_a_session() {
        let options = option("handle_token", "tok");
        assert_eq!(SessionHandle::token_from_options(&options), None);
    }

    #[test]
    fn the_caller_closing_its_own_session_owes_no_signal() {
        let mut session = session();
        assert!(!session.close(CloseCause::CallerRequested));
        assert!(!session.is_open());
    }

    #[test]
    fn the_backend_ending_a_session_owes_a_closed_signal() {
        let mut session = session();
        assert!(session.close(CloseCause::BackendEnded));
        assert!(!session.is_open());
    }

    #[test]
    fn a_caller_falling_off_the_bus_owes_no_signal() {
        // Nobody is left to receive it.
        let mut session = session();
        assert!(!session.close(CloseCause::CallerDisappeared));
    }

    #[test]
    fn closing_twice_emits_at_most_one_signal() {
        let mut session = session();
        assert!(session.close(CloseCause::BackendEnded));
        assert!(!session.close(CloseCause::BackendEnded));
    }

    #[test]
    fn a_caller_close_racing_a_backend_end_still_emits_one_signal() {
        let mut session = session();
        assert!(session.close(CloseCause::BackendEnded));
        assert!(!session.close(CloseCause::CallerRequested));
    }

    #[test]
    fn a_session_is_owned_by_the_caller_that_created_it() {
        assert!(session().is_owned_by(":1.42"));
    }

    #[test]
    fn another_caller_does_not_own_this_session() {
        assert!(!session().is_owned_by(":1.43"));
        assert!(!session().is_owned_by("org.example.App"));
    }

    #[test]
    fn from_call_builds_a_session_the_caller_named() {
        let options = option("session_handle_token", "s1");
        let session = SessionHandle::from_call(":1.7", &options).expect("valid call");
        assert_eq!(
            session.as_path(),
            "/org/freedesktop/portal/desktop/session/1_7/s1"
        );
    }
}

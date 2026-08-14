//! The `Request` object path a portal call exports, and the
//! `handle_token` option it is built from.
//!
//! Since xdg-desktop-portal 0.9 the path is predictable --
//! `/org/freedesktop/portal/desktop/request/SENDER/TOKEN` -- precisely so
//! a caller can subscribe to `Response` *before* making the call and not
//! race the reply. That makes this string a protocol commitment, not an
//! internal detail: a backend that mints a different path breaks the race
//! avoidance the caller is relying on.
//!
//! The token is caller-supplied and lands in an object path, so it is
//! validated here rather than trusted. D-Bus permits only `[A-Za-z0-9_]`
//! in a path element, and a token carrying `/` would silently extend the
//! path into an object nobody exported.

use crate::caller_id::CallerId;
use dbus::value::Value;

/// The prefix every request path this backend exports begins with.
const REQUEST_PREFIX: &str = "/org/freedesktop/portal/desktop/request";

/// The token the spec tells a backend to assume when the caller sent no
/// `handle_token`.
const DEFAULT_TOKEN: &str = "t";

/// A validated `Request` object path, built from a caller's unique name
/// and its `handle_token`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestHandle {
    path: String,
}

impl RequestHandle {
    /// Build the handle for `caller`'s request carrying `token`.
    ///
    /// Returns `None` if the token is not a legal object-path element.
    #[must_use]
    pub fn new(caller: &CallerId, token: &str) -> Option<Self> {
        if !Self::is_valid_token(token) {
            return None;
        }
        Some(Self {
            path: format!("{REQUEST_PREFIX}/{}/{token}", caller.as_segment()),
        })
    }

    /// Build the handle straight from a call's sender and options vardict.
    #[must_use]
    pub fn from_call(sender: &str, options: &[(String, Value)]) -> Option<Self> {
        let caller = CallerId::from_unique_name(sender)?;
        let token = Self::token_from_options(options)?;
        Self::new(&caller, token)
    }

    /// The token rules: non-empty, and only the characters D-Bus allows in
    /// a single path element.
    #[must_use]
    pub fn is_valid_token(token: &str) -> bool {
        !token.is_empty()
            && token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    }

    /// Read `handle_token` out of a call's options vardict, falling back to
    /// the spec's default when the caller omitted it.
    ///
    /// A present-but-invalid token is an error rather than a fallback: the
    /// caller has told us which path it is listening on, and quietly
    /// choosing a different one would leave it waiting forever.
    #[must_use]
    pub fn token_from_options(options: &[(String, Value)]) -> Option<&str> {
        let Some((_, value)) = options.iter().find(|(key, _)| key == "handle_token") else {
            return Some(DEFAULT_TOKEN);
        };
        Self::text_of(value).filter(|token| Self::is_valid_token(token))
    }

    /// Borrow a value's text, following through a variant wrapper -- which
    /// is how every value in an `a{sv}` options dictionary arrives.
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caller() -> CallerId {
        CallerId::from_unique_name(":1.42").expect("valid unique name")
    }

    fn option(key: &str, value: Value) -> Vec<(String, Value)> {
        vec![(key.to_string(), value)]
    }

    #[test]
    fn a_handle_has_the_documented_shape() {
        let handle = RequestHandle::new(&caller(), "portal1234").expect("valid token");
        assert_eq!(
            handle.as_path(),
            "/org/freedesktop/portal/desktop/request/1_42/portal1234"
        );
    }

    #[test]
    fn the_handle_goes_on_the_wire_as_an_object_path() {
        let handle = RequestHandle::new(&caller(), "t").expect("valid token");
        assert_eq!(
            handle.as_value(),
            Value::ObjectPath("/org/freedesktop/portal/desktop/request/1_42/t".into())
        );
    }

    #[test]
    fn a_token_with_a_path_separator_is_refused() {
        assert_eq!(RequestHandle::new(&caller(), "a/b"), None);
        assert_eq!(RequestHandle::new(&caller(), "../../session/1_42/x"), None);
    }

    #[test]
    fn a_token_with_a_hyphen_or_dot_is_refused() {
        // D-Bus path elements permit neither, so these would produce a
        // path the bus itself rejects.
        assert_eq!(RequestHandle::new(&caller(), "a-b"), None);
        assert_eq!(RequestHandle::new(&caller(), "a.b"), None);
    }

    #[test]
    fn an_empty_token_is_refused() {
        assert_eq!(RequestHandle::new(&caller(), ""), None);
    }

    #[test]
    fn absent_options_yield_the_spec_default_token() {
        assert_eq!(RequestHandle::token_from_options(&[]), Some("t"));
    }

    #[test]
    fn an_unrelated_option_still_yields_the_default_token() {
        let options = option("modal", Value::Bool(true));
        assert_eq!(RequestHandle::token_from_options(&options), Some("t"));
    }

    #[test]
    fn a_token_arrives_wrapped_in_a_variant() {
        let options = option(
            "handle_token",
            Value::Variant(Box::new(Value::Str("tok1".into()))),
        );
        assert_eq!(RequestHandle::token_from_options(&options), Some("tok1"));
    }

    #[test]
    fn a_present_but_invalid_token_is_an_error_not_a_fallback() {
        let options = option(
            "handle_token",
            Value::Variant(Box::new(Value::Str("bad/token".into()))),
        );
        assert_eq!(RequestHandle::token_from_options(&options), None);
    }

    #[test]
    fn a_token_of_the_wrong_type_is_an_error() {
        let options = option("handle_token", Value::Variant(Box::new(Value::U32(7))));
        assert_eq!(RequestHandle::token_from_options(&options), None);
    }

    #[test]
    fn from_call_builds_the_path_a_caller_can_predict() {
        let options = option(
            "handle_token",
            Value::Variant(Box::new(Value::Str("abc".into()))),
        );
        let handle = RequestHandle::from_call(":1.7", &options).expect("valid call");
        assert_eq!(
            handle.as_path(),
            "/org/freedesktop/portal/desktop/request/1_7/abc"
        );
    }

    #[test]
    fn from_call_refuses_a_sender_that_is_not_a_unique_name() {
        assert_eq!(RequestHandle::from_call("org.example.App", &[]), None);
    }

    #[test]
    fn two_callers_with_the_same_token_get_different_paths() {
        let mine = RequestHandle::from_call(":1.1", &[]).expect("valid call");
        let yours = RequestHandle::from_call(":1.2", &[]).expect("valid call");
        assert_ne!(mine, yours);
    }
}

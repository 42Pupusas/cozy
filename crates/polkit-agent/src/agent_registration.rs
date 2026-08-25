//! Registering as the session's authentication agent.
//!
//! Polkit will not call an agent it does not know about. Registration says
//! "for this subject, send `BeginAuthentication` to this object path".
//!
//! # The subject must be the session, not the process
//!
//! `RegisterAuthenticationAgent` takes the same `(sa{sv})` subject shape as
//! an authorisation check, but here it must be a `unix-session`. A
//! `unix-process` subject registers an agent for one process, which is not
//! what polkit asks about when some *other* process needs a password -- the
//! registration would appear to succeed and then never be called.
//!
//! # The object path is sent as a plain string
//!
//! Polkit's own introspection declares `object_path` as `s`, not `o`, on both
//! `RegisterAuthenticationAgent` and `UnregisterAuthenticationAgent`. Sending
//! a real object path is rejected with `InvalidArgs`, which is exactly what
//! happened the first time this ran against the live daemon.
//!
//! # Unregistering matters
//!
//! An agent that exits without unregistering leaves polkit believing the
//! session has one. The next authorisation attempt is then sent to an object
//! that no longer exists, and the user sees no prompt at all. [`Drop`] is not
//! enough on its own (a killed process never runs it), but it covers the
//! ordinary case, and polkit also drops the registration when our bus name
//! vanishes.

use dbus::conn::{DbusConn, DbusError};
use dbus::value::Value;

/// The agent's registration with polkit.
pub struct AgentRegistration {
    session_id: String,
    object_path: String,
    locale: String,
}

impl AgentRegistration {
    const DEST: &'static str = "org.freedesktop.PolicyKit1";
    const AUTHORITY_PATH: &'static str = "/org/freedesktop/PolicyKit1/Authority";
    const AUTHORITY_IFACE: &'static str = "org.freedesktop.PolicyKit1.Authority";

    /// The object path our agent listens on.
    pub const OBJECT_PATH: &'static str = "/org/freedesktop/PolicyKit1/AuthenticationAgent";

    /// Register for `session_id`, which comes from logind.
    #[must_use]
    pub fn new(session_id: &str) -> Self {
        Self {
            session_id: session_id.to_owned(),
            object_path: Self::OBJECT_PATH.to_owned(),
            locale: Self::current_locale(),
        }
    }

    /// The locale polkit should translate its messages into.
    fn current_locale() -> String {
        for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
            if let Ok(value) = std::env::var(key)
                && !value.is_empty()
            {
                return value;
            }
        }
        String::from("en_US.UTF-8")
    }

    /// The `(sa{sv})` subject naming our login session.
    fn subject(&self) -> Value {
        Value::Struct(vec![
            Value::Str("unix-session".into()),
            Value::Array(vec![Value::DictEntry(
                Box::new(Value::Str("session-id".into())),
                Box::new(Value::Variant(Box::new(Value::Str(
                    self.session_id.clone(),
                )))),
            )]),
        ])
    }

    /// The argument body of a `RegisterAuthenticationAgent` call.
    fn register_body(&self) -> [Value; 3] {
        [
            self.subject(),
            Value::Str(self.locale.clone()),
            Value::Str(self.object_path.clone()),
        ]
    }

    /// The argument body of an `UnregisterAuthenticationAgent` call.
    fn unregister_body(&self) -> [Value; 2] {
        [self.subject(), Value::Str(self.object_path.clone())]
    }

    /// Tell polkit we will answer authentication requests.
    pub fn register(&self, conn: &mut DbusConn) -> Result<(), DbusError> {
        let values = self.register_body();
        conn.call_with_values(
            Self::DEST,
            Self::AUTHORITY_PATH,
            Self::AUTHORITY_IFACE,
            "RegisterAuthenticationAgent",
            &values,
            "(sa{sv})ss",
        )?;
        Ok(())
    }

    /// Withdraw the registration, so polkit stops routing prompts to an
    /// object that is about to disappear.
    pub fn unregister(&self, conn: &mut DbusConn) -> Result<(), DbusError> {
        let values = self.unregister_body();
        conn.call_with_values(
            Self::DEST,
            Self::AUTHORITY_PATH,
            Self::AUTHORITY_IFACE,
            "UnregisterAuthenticationAgent",
            &values,
            "(sa{sv})s",
        )?;
        Ok(())
    }

    /// The session this registration is for.
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// The object path polkit will call.
    #[must_use]
    pub fn object_path(&self) -> &str {
        &self.object_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbus::marshal::Marshaller;

    /// The subject must name a *session*; a process subject registers an
    /// agent polkit will never call for other processes' requests.
    #[test]
    fn the_subject_is_a_session_not_a_process() {
        let registration = AgentRegistration::new("c2");
        let Value::Struct(fields) = registration.subject() else {
            panic!("a subject is a struct");
        };
        let kind = fields[0]
            .clone()
            .as_display_string()
            .expect("a kind string");
        assert_eq!(kind, "unix-session", "must not be unix-process");
    }

    /// The session id must reach the wire, or polkit registers the agent for
    /// the wrong session.
    #[test]
    fn the_session_id_is_carried_in_the_subject() {
        let registration = AgentRegistration::new("c2");
        let Value::Struct(fields) = registration.subject() else {
            panic!("a subject is a struct");
        };
        let entries = fields[1].clone();
        let items = entries.as_items().expect("a dictionary");
        let Value::DictEntry(key, value) = &items[0] else {
            panic!("a dict entry");
        };
        assert_eq!(key.clone().as_display_string().expect("key"), "session-id");
        assert_eq!(
            value.clone().into_inner().as_display_string().expect("id"),
            "c2"
        );
    }

    /// The register call must marshal against polkit's declared signature,
    /// or it fails on the wire rather than at the policy check.
    #[test]
    fn the_registration_body_marshals_against_polkits_signature() {
        let registration = AgentRegistration::new("c2");
        let body = Marshaller::marshal_body(&registration.register_body(), "(sa{sv})ss");
        assert!(body.is_some_and(|bytes| !bytes.is_empty()));
    }

    /// Unregistering has a different signature and must marshal too.
    #[test]
    fn the_unregistration_body_marshals_against_polkits_signature() {
        let registration = AgentRegistration::new("c2");
        let body = Marshaller::marshal_body(&registration.unregister_body(), "(sa{sv})s");
        assert!(body.is_some_and(|bytes| !bytes.is_empty()));
    }

    /// The path travels as a plain string, never as a D-Bus object path:
    /// polkit declares the argument as `s` and rejects `o` with `InvalidArgs`.
    /// This is a live-daemon fact, so it is pinned rather than assumed.
    #[test]
    fn the_object_path_is_sent_as_a_string_not_an_object_path() {
        let registration = AgentRegistration::new("c2");
        assert!(
            Marshaller::marshal_body(&registration.unregister_body(), "(sa{sv})s").is_some(),
            "the string form must marshal"
        );
        let as_path = [
            registration.subject(),
            Value::ObjectPath(String::from(AgentRegistration::OBJECT_PATH)),
        ];
        assert!(
            Marshaller::marshal_body(&as_path, "(sa{sv})s").is_none(),
            "an object path must not satisfy the declared string argument"
        );
    }

    /// The path must be a valid object path, since polkit calls it back.
    #[test]
    fn the_object_path_is_absolute() {
        let registration = AgentRegistration::new("c2");
        assert!(registration.object_path().starts_with('/'));
        assert!(!registration.object_path().ends_with('/'));
    }

    /// A locale is always supplied, so polkit never has to guess.
    #[test]
    fn a_locale_is_always_present() {
        let registration = AgentRegistration::new("c2");
        assert!(!registration.locale.is_empty());
    }
}

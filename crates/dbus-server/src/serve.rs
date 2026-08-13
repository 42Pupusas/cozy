//! Bridging one incoming wire message to a dispatch call and its reply.
//!
//! The last piece between "a [`crate::dispatcher::Dispatcher`] can decide
//! what a call should do" and "a real server answers a real caller".
//!
//! Two small, independently-testable pieces:
//!
//! - [`IncomingCall`] extracts exactly what [`Dispatcher::dispatch`]
//!   needs from a [`dbus::message::Reply`] that turned out to be a
//!   `MethodCall` (this crate's other pieces work in terms of loose
//!   `&str`/`&[Value]` arguments, not a whole parsed wire message, so
//!   this is where that translation happens, once).
//! - [`ReplySink`] is how a dispatch outcome becomes wire bytes, kept as
//!   a trait rather than a hard dependency on [`dbus::conn::DbusConn`]:
//!   a caller can unit-test its serving logic against a fake sink that
//!   just records what it was asked to send, without a real socket.
//!   [`dbus::conn::DbusConn`] implements it directly (see the `impl`
//!   below), so a real server passes `&mut conn` anywhere a `ReplySink`
//!   is expected.

use dbus::conn::{DbusConn, DbusError};
use dbus::message::{MessageType, Reply};
use dbus::value::{Unmarshaller, Value};

use crate::dispatcher::Dispatcher;

/// Everything [`Dispatcher::dispatch`] needs, extracted from one incoming
/// `MethodCall` [`Reply`].
pub struct IncomingCall {
    pub sender: String,
    pub serial: u32,
    pub path: String,
    pub interface: String,
    pub member: String,
    pub signature: String,
    pub args: Vec<Value>,
}

impl IncomingCall {
    /// Extract a method call's routing fields and decoded arguments from
    /// `reply`. Returns `None` if `reply` isn't actually a `MethodCall`,
    /// or lacks a field a well-formed one must have (`sender`, `path`,
    /// `interface`, `member` -- the bus itself guarantees these on
    /// anything it routes to us, but a malformed or adversarial peer
    /// isn't ruled out at the transport layer, so this stays a
    /// `None`-returning check rather than a panic).
    #[must_use]
    pub fn from_reply(reply: &Reply) -> Option<Self> {
        if reply.kind != MessageType::MethodCall {
            return None;
        }
        Some(Self {
            sender: reply.sender.clone()?,
            serial: reply.serial,
            path: reply.path.clone()?,
            interface: reply.interface.clone()?,
            member: reply.member.clone()?,
            signature: reply.signature.clone(),
            args: Unmarshaller::unmarshal_body(&reply.body, &reply.signature).unwrap_or_default(),
        })
    }

    /// Run this call through `dispatcher` and send the resulting reply
    /// (a method return or an `Error`) via `sink`. This is the one
    /// function a server's receive loop calls per incoming `MethodCall`.
    pub fn serve<S: ReplySink>(
        &self,
        dispatcher: &Dispatcher,
        sink: &mut S,
    ) -> Result<(), S::Error> {
        match dispatcher.dispatch(
            &self.path,
            &self.interface,
            &self.member,
            &self.signature,
            &self.args,
        ) {
            Ok(outcome) => sink.send_return(
                &self.sender,
                self.serial,
                &outcome.values,
                &outcome.reply_signature,
            ),
            Err(err) => {
                sink.send_error(&self.sender, self.serial, err.error_name(), &err.message())
            }
        }
    }
}

/// How a dispatch outcome reaches the wire. See the module doc comment
/// for why this is a trait rather than a direct [`DbusConn`] dependency
/// in [`IncomingCall::serve`].
pub trait ReplySink {
    type Error;

    /// Send a successful method return.
    fn send_return(
        &mut self,
        destination: &str,
        call_serial: u32,
        values: &[Value],
        reply_signature: &str,
    ) -> Result<(), Self::Error>;

    /// Send an `Error` reply.
    fn send_error(
        &mut self,
        destination: &str,
        call_serial: u32,
        error_name: &str,
        message: &str,
    ) -> Result<(), Self::Error>;
}

impl ReplySink for DbusConn {
    type Error = DbusError;

    fn send_return(
        &mut self,
        destination: &str,
        call_serial: u32,
        values: &[Value],
        reply_signature: &str,
    ) -> Result<(), DbusError> {
        self.send_method_return_with_values(destination, call_serial, values, reply_signature)
    }

    fn send_error(
        &mut self,
        destination: &str,
        call_serial: u32,
        error_name: &str,
        message: &str,
    ) -> Result<(), DbusError> {
        // Resolves to DbusConn's own inherent `send_error` (Rust prefers
        // inherent methods over trait methods, so this is not infinite
        // recursion) -- named identically on purpose so the trait impl is
        // a transparent pass-through, not a translation layer.
        Self::send_error(self, destination, call_serial, error_name, message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::introspection::InterfaceDescriptor;
    use crate::router::DispatchError;

    const SETTINGS_IFACE: &str = "org.freedesktop.impl.portal.Settings";
    const SETTINGS_PATH: &str = "/org/freedesktop/portal/desktop";
    const SENDER: &str = ":1.50";

    /// Records exactly what it was asked to send, so tests can assert on
    /// `IncomingCall::serve`'s behavior without a real connection.
    #[derive(Default)]
    struct RecordingSink {
        returns: Vec<(String, u32, Vec<Value>, String)>,
        errors: Vec<(String, u32, String, String)>,
    }

    impl ReplySink for RecordingSink {
        type Error = std::convert::Infallible;

        fn send_return(
            &mut self,
            destination: &str,
            call_serial: u32,
            values: &[Value],
            reply_signature: &str,
        ) -> Result<(), Self::Error> {
            self.returns.push((
                destination.to_string(),
                call_serial,
                values.to_vec(),
                reply_signature.to_string(),
            ));
            Ok(())
        }

        fn send_error(
            &mut self,
            destination: &str,
            call_serial: u32,
            error_name: &str,
            message: &str,
        ) -> Result<(), Self::Error> {
            self.errors.push((
                destination.to_string(),
                call_serial,
                error_name.to_string(),
                message.to_string(),
            ));
            Ok(())
        }
    }

    fn method_call_reply(
        sender: &str,
        serial: u32,
        path: &str,
        interface: &str,
        member: &str,
        signature: &str,
        body: Vec<u8>,
    ) -> Reply {
        Reply {
            kind: MessageType::MethodCall,
            sender: Some(sender.to_string()),
            path: Some(path.to_string()),
            interface: Some(interface.to_string()),
            member: Some(member.to_string()),
            serial,
            error_name: None,
            reply_serial: None,
            signature: signature.to_string(),
            body,
            unix_fds: 0,
        }
    }

    #[test]
    fn from_reply_rejects_non_method_call_kinds() {
        let mut reply =
            method_call_reply(SENDER, 1, SETTINGS_PATH, SETTINGS_IFACE, "X", "", vec![]);
        reply.kind = MessageType::Signal;
        assert!(IncomingCall::from_reply(&reply).is_none());
    }

    #[test]
    fn from_reply_rejects_a_method_call_missing_required_fields() {
        let mut reply =
            method_call_reply(SENDER, 1, SETTINGS_PATH, SETTINGS_IFACE, "X", "", vec![]);
        reply.sender = None;
        assert!(IncomingCall::from_reply(&reply).is_none());
    }

    #[test]
    fn from_reply_extracts_routing_fields_and_decodes_the_body() {
        let mut body = Vec::new();
        dbus::marshal::Marshaller::marshal(&mut body, &Value::Str("hi".into()), "s").unwrap();
        let reply = method_call_reply(SENDER, 7, SETTINGS_PATH, SETTINGS_IFACE, "Echo", "s", body);

        let call = IncomingCall::from_reply(&reply).expect("well-formed method call");
        assert_eq!(call.sender, SENDER);
        assert_eq!(call.serial, 7);
        assert_eq!(call.path, SETTINGS_PATH);
        assert_eq!(call.interface, SETTINGS_IFACE);
        assert_eq!(call.member, "Echo");
        assert_eq!(call.args, vec![Value::Str("hi".into())]);
    }

    fn dispatcher_with_read_all() -> Dispatcher {
        let mut dispatcher = Dispatcher::new();
        dispatcher.register_object(SETTINGS_PATH, [SETTINGS_IFACE]);
        dispatcher.register_interface(InterfaceDescriptor::new(SETTINGS_IFACE));
        dispatcher.register_method(SETTINGS_IFACE, "ReadAll", "as", "a{sa{sv}}", |_p, _a| {
            Ok(vec![Value::Array(vec![])])
        });
        dispatcher
    }

    #[test]
    fn serve_sends_a_method_return_for_a_successful_dispatch() {
        let dispatcher = dispatcher_with_read_all();
        let mut body = Vec::new();
        dbus::marshal::Marshaller::marshal(&mut body, &Value::Array(vec![]), "as").unwrap();
        let reply = method_call_reply(
            SENDER,
            3,
            SETTINGS_PATH,
            SETTINGS_IFACE,
            "ReadAll",
            "as",
            body,
        );
        let call = IncomingCall::from_reply(&reply).unwrap();

        let mut sink = RecordingSink::default();
        call.serve(&dispatcher, &mut sink).unwrap();

        assert_eq!(sink.returns.len(), 1);
        assert!(sink.errors.is_empty());
        let (destination, serial, values, signature) = &sink.returns[0];
        assert_eq!(destination, SENDER);
        assert_eq!(*serial, 3);
        assert_eq!(*values, vec![Value::Array(vec![])]);
        assert_eq!(signature, "a{sa{sv}}");
    }

    #[test]
    fn serve_sends_an_error_for_a_failed_dispatch() {
        let dispatcher = dispatcher_with_read_all();
        let reply = method_call_reply(
            SENDER,
            4,
            SETTINGS_PATH,
            SETTINGS_IFACE,
            "NoSuchMethod",
            "",
            vec![],
        );
        let call = IncomingCall::from_reply(&reply).unwrap();

        let mut sink = RecordingSink::default();
        call.serve(&dispatcher, &mut sink).unwrap();

        assert!(sink.returns.is_empty());
        assert_eq!(sink.errors.len(), 1);
        let (destination, serial, error_name, _message) = &sink.errors[0];
        assert_eq!(destination, SENDER);
        assert_eq!(*serial, 4);
        assert_eq!(
            error_name,
            DispatchError::UnknownMethod {
                interface: SETTINGS_IFACE.into(),
                member: "NoSuchMethod".into(),
            }
            .error_name()
        );
    }
}

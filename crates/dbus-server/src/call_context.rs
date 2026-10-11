//! Who is calling, and where the answer goes.

use dbus::value::Value;

use crate::serve::ReplySink;

/// The routing facts of one incoming method call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallContext {
    path: String,
    sender: String,
    serial: u32,
}

impl CallContext {
    #[must_use]
    pub fn new(path: impl Into<String>, sender: impl Into<String>, serial: u32) -> Self {
        Self {
            path: path.into(),
            sender: sender.into(),
            serial,
        }
    }

    /// A context for a call with no caller to answer, as in direct dispatch.
    #[must_use]
    pub fn anonymous(path: impl Into<String>) -> Self {
        Self::new(path, "", 0)
    }

    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    #[must_use]
    pub fn sender(&self) -> &str {
        &self.sender
    }

    #[must_use]
    pub const fn serial(&self) -> u32 {
        self.serial
    }

    pub fn reply<S: ReplySink>(
        &self,
        sink: &mut S,
        values: &[Value],
        reply_signature: &str,
    ) -> Result<(), S::Error> {
        sink.send_return(&self.sender, self.serial, values, reply_signature)
    }

    pub fn fail<S: ReplySink>(
        &self,
        sink: &mut S,
        error_name: &str,
        message: &str,
    ) -> Result<(), S::Error> {
        sink.send_error(&self.sender, self.serial, error_name, message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
                destination.to_owned(),
                call_serial,
                values.to_vec(),
                reply_signature.to_owned(),
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
                destination.to_owned(),
                call_serial,
                error_name.to_owned(),
                message.to_owned(),
            ));
            Ok(())
        }
    }

    #[test]
    fn a_reply_goes_to_the_caller_that_asked() {
        let context = CallContext::new("/p", ":1.9", 42);
        let mut sink = RecordingSink::default();
        context
            .reply(&mut sink, &[Value::U32(0)], "u")
            .expect("infallible");
        assert_eq!(
            sink.returns,
            vec![(":1.9".to_owned(), 42, vec![Value::U32(0)], "u".to_owned())]
        );
        assert!(sink.errors.is_empty());
    }

    #[test]
    fn a_failure_goes_to_the_caller_that_asked() {
        let context = CallContext::new("/p", ":1.9", 42);
        let mut sink = RecordingSink::default();
        context
            .fail(&mut sink, "org.example.Error", "nope")
            .expect("infallible");
        assert_eq!(
            sink.errors,
            vec![(
                ":1.9".to_owned(),
                42,
                "org.example.Error".to_owned(),
                "nope".to_owned()
            )]
        );
        assert!(sink.returns.is_empty());
    }

    #[test]
    fn an_anonymous_context_has_no_sender() {
        let context = CallContext::anonymous("/p");
        assert_eq!(context.path(), "/p");
        assert_eq!(context.sender(), "");
        assert_eq!(context.serial(), 0);
    }
}

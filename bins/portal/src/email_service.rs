//! `org.freedesktop.impl.portal.Email`.

use std::sync::Arc;

use dbus::value::Value;
use dbus_server::{Argument, Dispatcher, InterfaceDescriptor, MethodDescriptor};
use portal_core::{EmailMessage, ResponseCode};

use crate::mail_launcher::MailLauncher;

pub struct EmailService;

impl EmailService {
    pub const IFACE: &'static str = "org.freedesktop.impl.portal.Email";

    const METHOD: &'static str = "ComposeEmail";
    const OPTIONS_INDEX: usize = 3;

    pub fn install(dispatcher: &mut Dispatcher, launcher: Arc<dyn MailLauncher>) {
        dispatcher.register_interface(Self::interface_descriptor());
        dispatcher.register_method(
            Self::IFACE,
            Self::METHOD,
            "ossa{sv}",
            "ua{sv}",
            move |_path, args| {
                let options = args
                    .get(Self::OPTIONS_INDEX)
                    .ok_or_else(|| "ComposeEmail expects an options dictionary".to_string())?;
                let code = Self::compose(launcher.as_ref(), options);
                Ok(vec![code.as_value(), Value::Array(vec![])])
            },
        );
    }

    fn compose(launcher: &dyn MailLauncher, options: &Value) -> ResponseCode {
        let message = EmailMessage::from_options(options);
        match launcher.launch(&message.mailto_url(), message.activation_token()) {
            Ok(()) => ResponseCode::Success,
            Err(error) => {
                eprintln!("portal: ComposeEmail: {error}");
                ResponseCode::Other
            }
        }
    }

    fn interface_descriptor() -> InterfaceDescriptor {
        InterfaceDescriptor::new(Self::IFACE).method(MethodDescriptor::new(
            Self::METHOD,
            vec![
                Argument::new("handle", "o"),
                Argument::new("app_id", "s"),
                Argument::new("parent_window", "s"),
                Argument::new("options", "a{sv}"),
            ],
            vec![
                Argument::new("response", "u"),
                Argument::new("results", "a{sv}"),
            ],
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opener::OpenError;
    use std::sync::Mutex;

    struct RecordingLauncher {
        calls: Mutex<Vec<(String, Option<String>)>>,
        outcome: Result<(), OpenError>,
    }

    impl RecordingLauncher {
        fn replying(outcome: Result<(), OpenError>) -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
                outcome,
            })
        }

        fn calls(&self) -> Vec<(String, Option<String>)> {
            self.calls.lock().expect("calls lock").clone()
        }
    }

    impl MailLauncher for RecordingLauncher {
        fn launch(&self, url: &str, token: Option<&str>) -> Result<(), OpenError> {
            self.calls
                .lock()
                .expect("calls lock")
                .push((url.to_owned(), token.map(str::to_owned)));
            self.outcome.clone()
        }
    }

    struct Fixture {
        dispatcher: Dispatcher,
    }

    impl Fixture {
        const PATH: &'static str = "/org/freedesktop/portal/desktop";

        fn with(launcher: Arc<RecordingLauncher>) -> Self {
            let mut dispatcher = Dispatcher::new();
            dispatcher.register_object(Self::PATH, [EmailService::IFACE]);
            EmailService::install(&mut dispatcher, launcher);
            Self { dispatcher }
        }

        fn compose(&self, options: Vec<(&str, Value)>) -> (u32, Value) {
            let options = Value::Array(
                options
                    .into_iter()
                    .map(|(key, value)| {
                        Value::DictEntry(
                            Box::new(Value::Str(key.into())),
                            Box::new(Value::Variant(Box::new(value))),
                        )
                    })
                    .collect(),
            );
            let outcome = self
                .dispatcher
                .dispatch(
                    Self::PATH,
                    EmailService::IFACE,
                    "ComposeEmail",
                    "ossa{sv}",
                    &[
                        Value::ObjectPath("/org/freedesktop/portal/desktop/request/1_2/t".into()),
                        Value::Str("org.example.App".into()),
                        Value::Str(String::new()),
                        options,
                    ],
                )
                .expect("dispatch succeeds");
            assert_eq!(outcome.reply_signature, "ua{sv}");
            let Value::U32(code) = outcome.values[0] else {
                panic!("response must be a u32");
            };
            (code, outcome.values[1].clone())
        }
    }

    #[test]
    fn a_launched_client_answers_success_with_empty_results() {
        let launcher = RecordingLauncher::replying(Ok(()));
        let fixture = Fixture::with(launcher.clone());
        let (code, results) =
            fixture.compose(vec![("address", Value::Str("a@example.com".into()))]);
        assert_eq!(code, 0);
        assert_eq!(results, Value::Array(vec![]));
        assert_eq!(
            launcher.calls(),
            vec![("mailto:a@example.com".to_owned(), None)]
        );
    }

    #[test]
    fn the_activation_token_reaches_the_launcher() {
        let launcher = RecordingLauncher::replying(Ok(()));
        let fixture = Fixture::with(launcher.clone());
        fixture.compose(vec![("activation_token", Value::Str("tok".into()))]);
        assert_eq!(
            launcher.calls(),
            vec![("mailto:".to_owned(), Some("tok".to_owned()))]
        );
    }

    #[test]
    fn a_missing_mail_client_answers_other() {
        let launcher = RecordingLauncher::replying(Err(OpenError::NoHandler));
        let fixture = Fixture::with(launcher);
        let (code, results) = fixture.compose(vec![]);
        assert_eq!(code, 2);
        assert_eq!(results, Value::Array(vec![]));
    }

    #[test]
    fn a_spawn_failure_answers_other() {
        let launcher = RecordingLauncher::replying(Err(OpenError::SpawnFailed {
            program: "thunderbird".into(),
            reason: "not found".into(),
        }));
        let fixture = Fixture::with(launcher);
        assert_eq!(fixture.compose(vec![]).0, 2);
    }

    #[test]
    fn a_wrong_signature_is_rejected_before_the_launcher_runs() {
        let launcher = RecordingLauncher::replying(Ok(()));
        let fixture = Fixture::with(launcher.clone());
        let error = fixture
            .dispatcher
            .dispatch(
                Fixture::PATH,
                EmailService::IFACE,
                "ComposeEmail",
                "a{sv}",
                &[Value::Array(vec![])],
            )
            .unwrap_err();
        assert!(matches!(
            error,
            dbus_server::DispatchError::InvalidArgs { .. }
        ));
        assert!(launcher.calls().is_empty());
    }

    #[test]
    fn introspection_describes_compose_email() {
        let fixture = Fixture::with(RecordingLauncher::replying(Ok(())));
        let outcome = fixture
            .dispatcher
            .dispatch(
                Fixture::PATH,
                "org.freedesktop.DBus.Introspectable",
                "Introspect",
                "",
                &[],
            )
            .expect("introspect");
        let Value::Str(xml) = &outcome.values[0] else {
            panic!("Introspect must reply with a string");
        };
        assert!(xml.contains(&format!("<interface name=\"{}\">", EmailService::IFACE)));
        assert!(xml.contains("<method name=\"ComposeEmail\">"));
    }
}

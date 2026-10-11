//! Assembles every interface this daemon serves onto one `Dispatcher`.

use std::sync::Arc;

use dbus_server::Dispatcher;

use crate::email_service::EmailService;
use crate::mail_launcher::{AssociatedMailClient, MailLauncher};
use crate::settings_service::SettingsService;
use crate::settings_state::SettingsState;

pub struct PortalDispatcher;

impl PortalDispatcher {
    pub const PATH: &'static str = SettingsService::PATH;

    #[must_use]
    pub fn build(state: SettingsState) -> Dispatcher {
        Self::build_with(state, Arc::new(AssociatedMailClient))
    }

    #[must_use]
    pub fn build_with(state: SettingsState, mail: Arc<dyn MailLauncher>) -> Dispatcher {
        let mut dispatcher = Dispatcher::new();
        dispatcher.register_object(Self::PATH, [SettingsService::IFACE, EmailService::IFACE]);
        SettingsService::install(&mut dispatcher, state);
        EmailService::install(&mut dispatcher, mail);
        dispatcher
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbus::value::Value;
    use opener::OpenError;

    struct NoMailClient;

    impl MailLauncher for NoMailClient {
        fn launch(&self, _url: &str, _token: Option<&str>) -> Result<(), OpenError> {
            Err(OpenError::NoHandler)
        }
    }

    fn dispatcher() -> Dispatcher {
        let state = SettingsState::new(&config::Config::default());
        PortalDispatcher::build_with(state, Arc::new(NoMailClient))
    }

    #[test]
    fn one_object_answers_for_both_interfaces() {
        let dispatcher = dispatcher();
        let read = dispatcher.dispatch(
            PortalDispatcher::PATH,
            SettingsService::IFACE,
            "ReadAll",
            "as",
            &[Value::Array(vec![])],
        );
        assert!(read.is_ok());
        let compose = dispatcher.dispatch(
            PortalDispatcher::PATH,
            EmailService::IFACE,
            "ComposeEmail",
            "ossa{sv}",
            &[
                Value::ObjectPath("/org/freedesktop/portal/desktop/request/1_2/t".into()),
                Value::Str("app".into()),
                Value::Str(String::new()),
                Value::Array(vec![]),
            ],
        );
        assert!(compose.is_ok());
    }

    #[test]
    fn introspection_lists_both_interfaces() {
        let outcome = dispatcher()
            .dispatch(
                PortalDispatcher::PATH,
                "org.freedesktop.DBus.Introspectable",
                "Introspect",
                "",
                &[],
            )
            .expect("introspect");
        let Value::Str(xml) = &outcome.values[0] else {
            panic!("Introspect must reply with a string");
        };
        assert!(xml.contains(SettingsService::IFACE));
        assert!(xml.contains(EmailService::IFACE));
    }
}

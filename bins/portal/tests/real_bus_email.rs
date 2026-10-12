use std::sync::{Arc, Mutex};

use dbus::conn::DbusConn;
use dbus::value::Value;
use dbus_server::ServerLoop;
use opener::OpenError;
use portal_bin::{EmailService, MailLauncher, PortalDispatcher, SettingsState};
use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc::RingBuffer;

type Calls = Arc<Mutex<Vec<(String, Option<String>)>>>;

struct RecordingLauncher {
    calls: Calls,
    outcome: Result<(), OpenError>,
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

struct EmailProbe {
    name: &'static str,
    calls: Calls,
}

impl EmailProbe {
    const PATH: &'static str = "/org/freedesktop/portal/desktop";
    const SIGNATURE: &'static str = "ossa{sv}";

    fn session_bus_reachable() -> bool {
        match DbusConn::session() {
            Ok(_) => true,
            Err(error) => {
                eprintln!("no session D-Bus available ({error}); skipping real_bus_email test");
                false
            }
        }
    }

    fn serve(
        name: &'static str,
        outcome: Result<(), OpenError>,
        count: usize,
    ) -> (Self, std::thread::JoinHandle<()>) {
        let calls: Calls = Arc::new(Mutex::new(Vec::new()));
        let launcher = Arc::new(RecordingLauncher {
            calls: Arc::clone(&calls),
            outcome,
        });
        let (ready_tx, mut ready_rx) = RingBuffer::new(Capacity::exact(2)).split();
        let handle = std::thread::spawn(move || {
            let conn = DbusConn::session().expect("session bus was reachable a moment ago");
            let state = SettingsState::new(&config::Config::default());
            let mut server = ServerLoop::new(conn, PortalDispatcher::build_with(state, launcher));
            match server.conn_mut().request_name(name) {
                Ok(true) => {}
                Ok(false) => panic!("another connection already owns {name}"),
                Err(error) => panic!("could not claim {name}: {error}"),
            }
            let _ = ready_tx.push(());
            let mut answered = 0;
            while answered < count {
                match server.serve_one() {
                    Ok(true) => answered += 1,
                    Ok(false) => {}
                    Err(error) => panic!("email test server stopped early: {error}"),
                }
            }
        });
        ready_rx
            .pop_block()
            .expect("server thread should signal readiness before panicking");
        (Self { name, calls }, handle)
    }

    fn compose(&self, client: &mut DbusConn, options: Vec<(&str, Value)>) -> (u32, Value) {
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
        let reply = client
            .call_with_values(
                self.name,
                Self::PATH,
                EmailService::IFACE,
                "ComposeEmail",
                &[
                    Value::ObjectPath("/org/freedesktop/portal/desktop/request/1_2/t".into()),
                    Value::Str("org.example.App".into()),
                    Value::Str(String::new()),
                    options,
                ],
                Self::SIGNATURE,
            )
            .expect("ComposeEmail should succeed");
        let values = reply.values();
        let Value::U32(code) = values[0] else {
            panic!("response must be a u32");
        };
        (code, values[1].clone())
    }

    fn calls(&self) -> Vec<(String, Option<String>)> {
        self.calls.lock().expect("calls lock").clone()
    }
}

#[test]
fn a_real_caller_composes_an_email_and_the_launcher_gets_the_mailto_url() {
    if !EmailProbe::session_bus_reachable() {
        return;
    }
    let (probe, handle) = EmailProbe::serve(
        "org.freedesktop.impl.portal.desktop.cozy.testemail",
        Ok(()),
        1,
    );
    let mut client = DbusConn::session().expect("second connection");
    let (code, results) = probe.compose(
        &mut client,
        vec![
            ("address", Value::Str("a@example.com".into())),
            ("subject", Value::Str("Hello & bye".into())),
            ("activation_token", Value::Str("tok".into())),
        ],
    );
    handle.join().expect("email test server should not panic");
    assert_eq!(code, 0);
    assert_eq!(results, Value::Array(vec![]));
    assert_eq!(
        probe.calls(),
        vec![(
            "mailto:a@example.com?subject=Hello%20%26%20bye".to_owned(),
            Some("tok".to_owned())
        )]
    );
}

#[test]
fn a_real_caller_gets_response_two_when_no_mail_client_exists() {
    if !EmailProbe::session_bus_reachable() {
        return;
    }
    let (probe, handle) = EmailProbe::serve(
        "org.freedesktop.impl.portal.desktop.cozy.testemailfail",
        Err(OpenError::NoHandler),
        1,
    );
    let mut client = DbusConn::session().expect("second connection");
    let (code, _) = probe.compose(&mut client, vec![]);
    handle.join().expect("email test server should not panic");
    assert_eq!(code, 2);
}

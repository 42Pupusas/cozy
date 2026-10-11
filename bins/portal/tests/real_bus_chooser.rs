use std::os::fd::RawFd;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use dbus::conn::DbusConn;
use dbus::value::Value;
use portal_bin::{ConfigWatcher, PortalLoop, SettingsState};
use portal_core::{
    ChooserDialog, ChooserOutcome, ChooserRequest, DialogFactory, Selection, VardictBuilder,
    VardictReader,
};
use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc::RingBuffer;
use waker::Waker;

struct ScriptedDialog {
    waker: Waker,
    outcome: Option<ChooserOutcome>,
}

impl ChooserDialog for ScriptedDialog {
    fn wake_fd(&self) -> RawFd {
        self.waker.raw_fd()
    }

    fn advance(&mut self) -> Option<ChooserOutcome> {
        self.waker.drain();
        self.outcome.take()
    }

    fn close(&mut self) {}
}

struct ScriptedDialogs {
    opened: Arc<AtomicUsize>,
    outcome: Option<ChooserOutcome>,
}

impl DialogFactory for ScriptedDialogs {
    fn open(&mut self, _request: ChooserRequest) -> Result<Box<dyn ChooserDialog>, String> {
        let waker = Waker::new().map_err(|error| error.to_string())?;
        if self.outcome.is_some() {
            waker.signal();
        }
        self.opened.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(ScriptedDialog {
            waker,
            outcome: self.outcome.take(),
        }))
    }
}

struct ChooserProbe;

impl ChooserProbe {
    const PICK_NAME: &'static str = "org.freedesktop.impl.portal.desktop.cozy.testchooserpick";
    const CLOSE_NAME: &'static str = "org.freedesktop.impl.portal.desktop.cozy.testchooserclose";
    const CLOSE_HANDLE: &'static str = "/org/freedesktop/portal/desktop/request/1_3/close";
    const PATH: &'static str = "/org/freedesktop/portal/desktop";
    const IFACE: &'static str = "org.freedesktop.impl.portal.FileChooser";
    const REQUEST_IFACE: &'static str = "org.freedesktop.impl.portal.Request";
    const SIGNATURE: &'static str = "osssa{sv}";

    fn session_bus_reachable() -> bool {
        match DbusConn::session() {
            Ok(_) => true,
            Err(error) => {
                eprintln!("no session D-Bus available ({error}); skipping real_bus_chooser test");
                false
            }
        }
    }

    fn serve(
        name: &'static str,
        outcome: Option<ChooserOutcome>,
    ) -> (Arc<AtomicUsize>, std::thread::JoinHandle<()>) {
        let opened = Arc::new(AtomicUsize::new(0));
        let factory = ScriptedDialogs {
            opened: Arc::clone(&opened),
            outcome,
        };
        let (ready_tx, mut ready_rx) = RingBuffer::new(Capacity::exact(2)).split();
        let watched = Arc::clone(&opened);
        let handle = std::thread::spawn(move || {
            let conn = DbusConn::session().expect("session bus was reachable a moment ago");
            let state = SettingsState::new(&config::Config::default());
            let mut portal = PortalLoop::with_dialogs(
                conn,
                state,
                ConfigWatcher::for_path(std::env::temp_dir().join("portal-chooser-none.toml")),
                Box::new(factory),
            );
            match portal.conn_mut().request_name(name) {
                Ok(true) => {}
                Ok(false) => panic!("another connection already owns {name}"),
                Err(error) => panic!("could not claim {name}: {error}"),
            }
            let _ = ready_tx.push(());
            while watched.load(Ordering::SeqCst) == 0 || portal.open_dialogs() > 0 {
                portal.pump_n(1).expect("pump");
            }
        });
        ready_rx
            .pop_block()
            .expect("server thread should signal readiness before panicking");
        (opened, handle)
    }

    fn open_file(client: &mut DbusConn, name: &str, handle: &str) -> (u32, Value) {
        let reply = client
            .call_with_values(
                name,
                Self::PATH,
                Self::IFACE,
                "OpenFile",
                &[
                    Value::ObjectPath(handle.into()),
                    Value::Str("org.example.App".into()),
                    Value::Str(String::new()),
                    Value::Str("Pick".into()),
                    VardictBuilder::new().build(),
                ],
                Self::SIGNATURE,
            )
            .expect("OpenFile should be answered");
        let values = reply.values();
        let Value::U32(code) = values[0] else {
            panic!("response must be a u32");
        };
        (code, values[1].clone())
    }

    fn close_when_exported(client: &mut DbusConn, name: &str, handle: &str) {
        for _ in 0..200 {
            if client
                .call(name, handle, Self::REQUEST_IFACE, "Close", &[])
                .is_ok()
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the request object was never exported at {handle}");
    }
}

#[test]
fn a_real_caller_blocks_on_open_file_until_the_dialog_picks() {
    if !ChooserProbe::session_bus_reachable() {
        return;
    }
    let name = ChooserProbe::PICK_NAME;
    let picked = ChooserOutcome::Picked(Selection {
        paths: vec![PathBuf::from("/home/me/a b.txt")],
        ..Selection::default()
    });
    let (_opened, handle) = ChooserProbe::serve(name, Some(picked));
    let mut client = DbusConn::session().expect("second connection");

    let (code, results) = ChooserProbe::open_file(
        &mut client,
        name,
        "/org/freedesktop/portal/desktop/request/1_2/pick",
    );

    handle.join().expect("chooser test server should not panic");
    assert_eq!(code, 0);
    assert_eq!(
        VardictReader::new(&results).text_list("uris"),
        ["file:///home/me/a%20b.txt"]
    );
}

#[test]
fn request_close_from_another_connection_ends_the_call_with_response_two() {
    if !ChooserProbe::session_bus_reachable() {
        return;
    }
    let (name, handle_path) = (ChooserProbe::CLOSE_NAME, ChooserProbe::CLOSE_HANDLE);
    let (_opened, handle) = ChooserProbe::serve(name, None);

    let caller = std::thread::spawn(move || {
        let mut client = DbusConn::session().expect("caller connection");
        ChooserProbe::open_file(&mut client, name, handle_path)
    });
    let mut closer = DbusConn::session().expect("closer connection");
    ChooserProbe::close_when_exported(&mut closer, name, handle_path);

    let (code, _) = caller.join().expect("caller thread");
    handle.join().expect("chooser test server should not panic");
    assert_eq!(code, 2);
}

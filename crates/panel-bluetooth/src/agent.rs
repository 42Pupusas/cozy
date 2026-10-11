//! The panel's auto-accept `org.bluez.Agent1`, served on its own
//! dedicated bus connection.
//!
//! A D-Bus server loop blocks in `recv_message`, so the agent cannot
//! share the worker's call connection — it gets its own thread and its
//! own connection instead. Registered only for the panel's lifetime:
//! [`PairingAgent::spawn`] registers with bluez and [`Drop`] unregisters,
//! so closing the panel restores whatever agent (if any) owned pairing
//! before.
//!
//! Every bluez agent method is answered affirmatively: `JustWorks`
//! headphones and mice pair with zero interaction, and a keyboard's
//! PIN request is refused with the spec's `Rejected` error rather than
//! hanging bluez's pairing queue (v1 scope — a PIN entry UI can layer
//! on later).

use dbus_server::dispatcher::Dispatcher;
use dbus_server::introspection::{Argument, InterfaceDescriptor, MethodDescriptor};
use dbus_server::serve::IncomingCall;

const AGENT_IFACE: &str = "org.bluez.Agent1";
const AGENT_PATH: &str = "/panel/agent";
const BLUEZ_DEST: &str = "org.bluez";
/// bluez's agent-manager capability vocabulary: `DisplayYesNo` accepts
/// confirmations, exactly what an auto-accept agent declares.
const AGENT_CAPABILITY: &str = "DisplayYesNo";
/// `poll(2)` timeout meaning "wait until a descriptor is ready".
const BLOCK_FOREVER: i32 = -1;

/// The bluez-spec error name for "the agent refused", which bluez
/// surfaces as a failed pairing rather than a retry loop.
const REJECTED: &str = "org.bluez.Error.Rejected";

/// Owns the agent thread's lifecycle: a waker the dropper signals and
/// the thread handle, so [`Drop`] unregisters cleanly when the panel
/// closes.
pub struct PairingAgent {
    stop: waker::Waker,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl PairingAgent {
    /// Connect, build the dispatcher, register with bluez's agent
    /// manager, and enter the serve loop on a new thread. Failure to
    /// register is non-fatal for the panel (pairing just won't work;
    /// power/connect/disconnect still do) and is reported on stderr.
    #[must_use]
    pub fn spawn() -> Self {
        let stop = waker::Waker::new().expect("create agent stop waker");
        let thread = std::thread::Builder::new()
            .name("panel-bluetooth-agent".into())
            .spawn({
                let stop = stop.clone();
                move || {
                    if let Err(e) = Self::serve(&stop) {
                        eprintln!("panel-bluetooth: pairing agent ended: {e}");
                    }
                }
            })
            .expect("spawn pairing agent thread");
        Self {
            stop,
            thread: Some(thread),
        }
    }

    fn serve(stop: &waker::Waker) -> Result<(), dbus::DbusError> {
        let mut conn = dbus::DbusConn::system()?;
        let mut dispatcher = Dispatcher::new();
        Self::register_methods(&mut dispatcher);
        dispatcher.register_object(AGENT_PATH, [AGENT_IFACE]);
        dispatcher.register_interface(Self::interface_descriptor());

        if let Err(e) = Self::register_with_bluez(&mut conn) {
            eprintln!("panel-bluetooth: could not register pairing agent: {e}");
            return Err(e);
        }
        let result = Self::serve_until_stop(&mut conn, &dispatcher, stop);
        // Always unregister, even after a transport error: bluez keeps a
        // registered path around and a stale one breaks the *next*
        // panel's registration until bluez restarts.
        let _ = Self::unregister_from_bluez(&mut conn);
        result
    }

    fn serve_until_stop(
        conn: &mut dbus::DbusConn,
        dispatcher: &Dispatcher,
        stop: &waker::Waker,
    ) -> Result<(), dbus::DbusError> {
        let mut poller = waker::Poller::two(conn.raw_fd(), stop.raw_fd());
        loop {
            let queued = conn.has_pending();
            let (bus_ready, stop_ready) = poller.poll_two(if queued { 0 } else { BLOCK_FOREVER });
            if stop_ready {
                stop.drain();
                return Ok(());
            }
            if !bus_ready && !queued {
                continue;
            }
            let message = conn.recv_message()?;
            if let Some(call) = IncomingCall::from_reply(&message) {
                // A failed send (caller gone) is that one caller's
                // problem, not a reason to stop serving.
                let _ = call.serve(dispatcher, conn);
            }
        }
    }

    fn register_with_bluez(conn: &mut dbus::DbusConn) -> Result<(), dbus::DbusError> {
        conn.call(
            BLUEZ_DEST,
            "/org/bluez",
            "org.bluez.AgentManager1",
            "RegisterAgent",
            &[
                dbus::message::BodyArg::ObjectPath(AGENT_PATH.into()),
                dbus::message::BodyArg::Str(AGENT_CAPABILITY.into()),
            ],
        )?;
        conn.call(
            BLUEZ_DEST,
            "/org/bluez",
            "org.bluez.AgentManager1",
            "RequestDefaultAgent",
            &[dbus::message::BodyArg::ObjectPath(AGENT_PATH.into())],
        )?;
        Ok(())
    }

    fn unregister_from_bluez(conn: &mut dbus::DbusConn) -> Result<(), dbus::DbusError> {
        conn.call(
            BLUEZ_DEST,
            "/org/bluez",
            "org.bluez.AgentManager1",
            "UnregisterAgent",
            &[dbus::message::BodyArg::ObjectPath(AGENT_PATH.into())],
        )?;
        Ok(())
    }

    /// Every agent method, wired: `Release` and `Cancel` are no-ops with
    /// empty replies, `RequestConfirmation`/`RequestAuthorization`/
    /// `AuthorizeService` accept, `RequestPinCode`/`RequestPasskey` are
    /// refused (v1 has no PIN UI), and `DisplayPinCode`/`DisplayPasskey`
    /// acknowledge silently.
    fn register_methods(dispatcher: &mut Dispatcher) {
        dispatcher.register_method(AGENT_IFACE, "Release", "", "", |_, _| Ok(Vec::new()));
        dispatcher.register_method(AGENT_IFACE, "Cancel", "", "", |_, _| Ok(Vec::new()));
        dispatcher.register_method(AGENT_IFACE, "RequestConfirmation", "ou", "", |_, _| {
            Ok(Vec::new())
        });
        dispatcher.register_method(AGENT_IFACE, "RequestAuthorization", "o", "", |_, _| {
            Ok(Vec::new())
        });
        dispatcher.register_method(AGENT_IFACE, "AuthorizeService", "os", "", |_, _| {
            Ok(Vec::new())
        });
        dispatcher.register_method(AGENT_IFACE, "RequestPinCode", "o", "s", |_, _| {
            Err(REJECTED.to_owned())
        });
        dispatcher.register_method(AGENT_IFACE, "RequestPasskey", "o", "u", |_, _| {
            Err(REJECTED.to_owned())
        });
        dispatcher.register_method(AGENT_IFACE, "DisplayPinCode", "os", "", |_, _| {
            Ok(Vec::new())
        });
        dispatcher.register_method(AGENT_IFACE, "DisplayPasskey", "ouq", "", |_, _| {
            Ok(Vec::new())
        });
    }

    fn interface_descriptor() -> InterfaceDescriptor {
        let arg = |name: &str, sig: &str| Argument::new(name, sig);
        let method = |name: &str, ins: &[(&str, &str)], outs: &[(&str, &str)]| {
            MethodDescriptor::new(
                name,
                ins.iter().map(|(n, s)| arg(n, s)).collect(),
                outs.iter().map(|(n, s)| arg(n, s)).collect(),
            )
        };
        InterfaceDescriptor::new(AGENT_IFACE)
            .method(method("Release", &[], &[]))
            .method(method("Cancel", &[], &[]))
            .method(method(
                "RequestPinCode",
                &[("device", "o")],
                &[("pincode", "s")],
            ))
            .method(method(
                "DisplayPinCode",
                &[("device", "o"), ("pincode", "s")],
                &[],
            ))
            .method(method(
                "RequestPasskey",
                &[("device", "o")],
                &[("passkey", "u")],
            ))
            .method(method(
                "DisplayPasskey",
                &[("device", "o"), ("passkey", "u"), ("entered", "q")],
                &[],
            ))
            .method(method(
                "RequestConfirmation",
                &[("device", "o"), ("passkey", "u")],
                &[],
            ))
            .method(method("RequestAuthorization", &[("device", "o")], &[]))
            .method(method(
                "AuthorizeService",
                &[("device", "o"), ("uuid", "s")],
                &[],
            ))
    }
}

impl Drop for PairingAgent {
    fn drop(&mut self) {
        self.stop.signal();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

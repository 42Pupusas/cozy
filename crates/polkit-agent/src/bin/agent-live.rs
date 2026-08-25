//! Prove the agent works against the real polkit, end to end.
//!
//! Registers as this session's authentication agent, then triggers an action
//! that needs a password, and answers the prompt from the terminal.
//!
//! This is the honest test of the pieces a unit test cannot reach: whether
//! polkit accepts the registration, whether it calls `BeginAuthentication`
//! back on our object, and whether the helper's verdict actually unlocks the
//! action. Run it manually:
//!
//! ```text
//! cargo run -p polkit-agent --bin agent-live
//! ```
//!
//! It mounts nothing that can succeed: the device it names does not exist, so
//! the only thing being proven is the authentication path.

use dbus::conn::DbusConn;
use dbus::message::MessageType;
use dbus_server::serve::IncomingCall;
use polkit_agent::agent_registration::AgentRegistration;
use polkit_agent::auth_request::AuthRequest;
use polkit_agent::helper_session::{AuthOutcome, HelperSession};
use std::io::{BufRead, Write};

/// Reads a password from the terminal. It echoes, because turning echo off
/// needs a termios call; this is a diagnostic, not the real UI.
///
/// Pass `--cancel` to decline every prompt instead. That still proves the
/// whole path -- registration, callback, helper handshake -- without needing
/// anyone to type, which is what makes this runnable unattended.
struct TerminalPrompt;

impl TerminalPrompt {
    /// Whether the run was told to decline every prompt.
    fn cancels() -> bool {
        std::env::args().any(|arg| arg == "--cancel")
    }

    fn ask(prompt: &str, _echo: bool) -> Option<String> {
        if Self::cancels() {
            println!("\n  PROMPT RECEIVED: {prompt:?}");
            println!("  (--cancel given, so declining without sending a password)");
            return None;
        }
        print!("\n  {prompt}[probe echoes; the real UI will not] ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        let read = std::io::stdin().lock().read_line(&mut line).ok()?;
        if read == 0 {
            return None;
        }
        let trimmed = line.trim_end_matches(['\n', '\r']).to_owned();
        if trimmed.is_empty() { None } else { Some(trimmed) }
    }
}

/// The live agent run.
struct LiveAgent;

impl LiveAgent {
    /// Answer one `BeginAuthentication` call.
    fn handle(call: &IncomingCall, conn: &mut DbusConn) {
        let Some(request) = AuthRequest::from_args(&call.args) else {
            println!("  could not decode the request");
            let _ = conn.send_method_return_with_values(&call.sender, call.serial, &[], "");
            return;
        };
        println!("\npolkit asked us to authenticate:");
        println!("  action:  {}", request.action_id());
        println!("  message: {}", request.message());

        let own_uid = dbus::sys::DbusSys::effective_uid();
        let Some(identity) = request.choose_identity(own_uid) else {
            println!("  no identity we can authenticate");
            let _ = conn.send_method_return_with_values(&call.sender, call.serial, &[], "");
            return;
        };
        println!("  as user: {} (uid {})", identity.name(), identity.uid());

        match HelperSession::open(identity.name(), request.cookie()) {
            Ok(session) => {
                let outcome = session.converse(TerminalPrompt::ask, |line| {
                    println!("  helper says: {line:?}");
                });
                match outcome {
                    Ok(AuthOutcome::Authenticated) => println!("\n  -> AUTHENTICATED"),
                    Ok(AuthOutcome::Rejected) => println!("\n  -> rejected"),
                    Ok(AuthOutcome::Cancelled) => println!("\n  -> cancelled"),
                    Err(err) => println!("\n  -> error: {}", err.message()),
                }
            }
            Err(err) => println!("  helper unreachable: {}", err.message()),
        }

        let _ = conn.send_method_return_with_values(&call.sender, call.serial, &[], "");
    }

    /// Make the request that needs a password, on its own connection: the
    /// agent's connection must stay free to receive `BeginAuthentication`
    /// while the request is outstanding.
    fn trigger() -> std::thread::JoinHandle<String> {
        std::thread::spawn(|| {
            let Ok(mut conn) = DbusConn::system() else {
                return String::from("no second bus connection");
            };
            let mount = dbus::systemd1::TransientMount::new(
                "/dev/does-not-exist",
                "/run/media/cozy-agent-probe",
                "vfat",
                1000,
                1000,
            );
            match mount.start(&mut conn) {
                Ok(job) => format!("systemd queued job {job}"),
                Err(err) => format!("{err:?} -- {}", err.message()),
            }
        })
    }

    fn run() {
        let mut conn = match DbusConn::system() {
            Ok(conn) => conn,
            Err(err) => {
                println!("no system bus: {err:?}");
                return;
            }
        };

        let session_id = match dbus::logind::LogindSession::own_session_id(&mut conn) {
            Ok(id) => id,
            Err(err) => {
                println!("no logind session: {err:?}");
                return;
            }
        };
        println!("session id: {session_id}");

        let registration = AgentRegistration::new(&session_id);
        if let Err(err) = registration.register(&mut conn) {
            println!("polkit refused the registration: {err:?}");
            return;
        }
        println!("registered as the session's authentication agent");
        println!("object path: {}", registration.object_path());

        println!("\ntriggering an action that requires a password...");
        let trigger = Self::trigger();

        loop {
            let Ok(message) = conn.recv_message() else {
                println!("\nthe bus connection ended");
                break;
            };
            if message.kind != MessageType::MethodCall {
                continue;
            }
            let Some(call) = IncomingCall::from_reply(&message) else {
                continue;
            };
            println!("\nincoming call: {}.{}", call.interface, call.member);
            if call.member == "BeginAuthentication" {
                Self::handle(&call, &mut conn);
                break;
            }
            if call.member == "CancelAuthentication" {
                println!("  polkit cancelled the request");
                let _ = conn.send_method_return_with_values(&call.sender, call.serial, &[], "");
                break;
            }
            let _ = conn.send_error(
                &call.sender,
                call.serial,
                "org.freedesktop.DBus.Error.UnknownMethod",
                "not implemented by this probe",
            );
        }

        println!("\nthe triggering call reported:");
        match trigger.join() {
            Ok(result) => println!("  {result}"),
            Err(_) => println!("  the triggering thread panicked"),
        }

        if let Err(err) = registration.unregister(&mut conn) {
            println!("\nwarning: could not unregister: {err:?}");
        } else {
            println!("\nunregistered cleanly");
        }
    }
}

fn main() {
    LiveAgent::run();
}

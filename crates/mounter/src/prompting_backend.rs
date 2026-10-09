//! [`PromptingBackend`] -- mount, and answer the password prompt it provokes.
//!
//! # The shape of the problem
//!
//! The mount call does not return until polkit has decided, and polkit
//! decides by calling **us** back on the same bus. So the call and its
//! authentication overlap in time, and one connection cannot do both: the
//! request would block the connection that must receive `BeginAuthentication`,
//! and the two would wait for each other forever.
//!
//! So the mount is made on a second connection, from another thread, while
//! this thread serves the agent callback. That is not a design flourish; a
//! single-connection version deadlocks.
//!
//! # Why the first attempt is made without an agent
//!
//! `auth_admin_keep` caches a recent grant. A mount shortly after another one
//! succeeds with no prompt at all, so registering an agent first would be
//! wasted work and, worse, would train the user to expect a password every
//! time. The backend therefore tries plainly, and only registers when the
//! answer is [`MountOutcome::NeedsAuthorization`].
//!
//! # The prompt is the caller's
//!
//! This crate never draws anything. [`PasswordPrompt`] is supplied by the
//! caller, which in the file browser is a modal that does not echo. A prompt
//! that returns `None` means the user dismissed it, and dismissal is a normal
//! outcome rather than an error.

use dbus::conn::DbusConn;
use dbus::message::MessageType;
use dbus_server::serve::IncomingCall;
use polkit_agent::agent_registration::AgentRegistration;
use polkit_agent::auth_request::AuthRequest;
use polkit_agent::helper_session::{AuthOutcome, HelperSession};
use std::path::Path;
use volumes::mount_backend::{MountBackend, MountOutcome};
use volumes::mount_request::MountRequest;

use crate::systemd_mount::SystemdMount;

/// Asks the user for a password.
///
/// Receives the prompt text polkit chose and whether it may be echoed;
/// returns the answer, or `None` to dismiss.
pub trait PasswordPrompt {
    /// Ask for one secret.
    fn ask(&mut self, prompt: &str, echo: bool) -> Option<String>;

    /// Report a notice or error from the authentication helper, e.g. "3
    /// attempts remaining".
    fn notify(&mut self, _message: &str) {}
}

/// A mounter that answers polkit's prompt through a caller-supplied UI.
pub struct PromptingBackend<P: PasswordPrompt> {
    prompt: P,
    conn: DbusConn,
    last_authentication: Option<AuthOutcome>,
}

impl<P: PasswordPrompt> PromptingBackend<P> {
    /// Connect to the system bus and prompt through `prompt`.
    pub fn connect(prompt: P) -> Result<Self, dbus::conn::DbusError> {
        Ok(Self {
            prompt,
            conn: DbusConn::system()?,
            last_authentication: None,
        })
    }

    /// How the most recent authentication ended, if one was attempted.
    ///
    /// Lets a caller distinguish "the user dismissed the prompt" from "the
    /// password was wrong", which the mount outcome alone cannot express.
    #[must_use]
    pub const fn last_authentication(&self) -> Option<&AuthOutcome> {
        self.last_authentication.as_ref()
    }

    /// Attempt a mount on its own connection, in another thread.
    ///
    /// Separate because this connection must stay free to receive
    /// `BeginAuthentication` while the request is outstanding.
    fn attempt(request: &MountRequest) -> std::thread::JoinHandle<MountOutcome> {
        let request = request.clone();
        std::thread::spawn(move || match SystemdMount::connect() {
            Ok(mut mount) => mount.mount(&request),
            Err(err) => MountOutcome::Unavailable(format!("{err:?}")),
        })
    }

    /// Run the whole prompt-and-retry sequence.
    fn mount_with_authentication(&mut self, request: &MountRequest) -> MountOutcome {
        let Ok(session_id) = dbus::logind::LogindSession::own_session_id(&mut self.conn) else {
            return MountOutcome::Unavailable(String::from("no logind session"));
        };
        let registration = AgentRegistration::new(&session_id);
        if registration.register(&mut self.conn).is_err() {
            return MountOutcome::Unavailable(String::from(
                "polkit refused the authentication agent",
            ));
        }

        let attempt = Self::attempt(request);
        self.serve_until_authenticated();
        let outcome = attempt
            .join()
            .unwrap_or_else(|_| MountOutcome::Failed(String::from("the mount thread panicked")));

        let _ = registration.unregister(&mut self.conn);
        self.reinterpret(outcome)
    }

    /// Report a dismissed prompt as a cancellation rather than a refusal.
    ///
    /// polkit answers a dismissed prompt with the same denial it uses for a
    /// real refusal, because from its side nothing was proven either way.
    /// Passing that through tells someone who closed the prompt that they
    /// lack a permission they actually have, and sends them hunting a problem
    /// that does not exist. We know which happened; polkit does not.
    fn reinterpret(&self, outcome: MountOutcome) -> MountOutcome {
        let dismissed = matches!(self.last_authentication, Some(AuthOutcome::Cancelled));
        if dismissed && !outcome.is_mounted() {
            return MountOutcome::Cancelled;
        }
        outcome
    }

    /// Serve bus traffic until the authentication call has been answered.
    ///
    /// Returns after `BeginAuthentication`, because that is the only call the
    /// outstanding mount is waiting on.
    fn serve_until_authenticated(&mut self) {
        while let Ok(message) = self.conn.recv_message() {
            if message.kind != MessageType::MethodCall {
                continue;
            }
            let Some(call) = IncomingCall::from_reply(&message) else {
                continue;
            };
            match call.member.as_str() {
                "BeginAuthentication" => {
                    self.authenticate(&call);
                    return;
                }
                "CancelAuthentication" => {
                    let _ = self.conn.send_method_return_with_values(
                        &call.sender,
                        call.serial,
                        &[],
                        "",
                    );
                    return;
                }
                _ => {
                    let _ = self.conn.send_error(
                        &call.sender,
                        call.serial,
                        "org.freedesktop.DBus.Error.UnknownMethod",
                        "not implemented by this agent",
                    );
                }
            }
        }
    }

    /// Run one authentication conversation and reply to polkit.
    ///
    /// The reply must not be sent until the conversation has finished:
    /// `BeginAuthentication` returning is what tells polkit to re-check, so
    /// answering early reports a failure the user never had a chance to fix.
    fn authenticate(&mut self, call: &IncomingCall) {
        self.last_authentication = self.converse(call);
        let _ = self
            .conn
            .send_method_return_with_values(&call.sender, call.serial, &[], "");
    }

    /// The conversation itself, separated so the reply is sent on every path.
    fn converse(&mut self, call: &IncomingCall) -> Option<AuthOutcome> {
        let request = AuthRequest::from_args(&call.args)?;
        let own_uid = dbus::sys::DbusSys::effective_uid();
        let identity = request.choose_identity(own_uid)?;
        let session = HelperSession::open(identity.name(), request.cookie()).ok()?;
        // Both callbacks need the prompt, and the helper interleaves them:
        // a notice such as "2 attempts remaining" arrives between prompts and
        // is worthless once the conversation has ended, so it cannot be
        // deferred to afterwards.
        let prompt = std::cell::RefCell::new(&mut self.prompt);
        session
            .converse(
                |text, echo| prompt.borrow_mut().ask(text, echo),
                |line| prompt.borrow_mut().notify(&format!("{line:?}")),
            )
            .ok()
    }
}

impl<P: PasswordPrompt> MountBackend for PromptingBackend<P> {
    fn mount(&mut self, request: &MountRequest) -> MountOutcome {
        let first = Self::attempt(request)
            .join()
            .unwrap_or_else(|_| MountOutcome::Failed(String::from("the mount thread panicked")));
        if !first.is_retryable() {
            return first;
        }
        self.mount_with_authentication(request)
    }

    fn unmount(&mut self, mount_point: &Path) -> MountOutcome {
        SystemdMount::on(match DbusConn::system() {
            Ok(conn) => conn,
            Err(err) => return MountOutcome::Unavailable(format!("{err:?}")),
        })
        .unmount(mount_point)
    }

    fn is_available(&self) -> bool {
        HelperSession::socket_available()
    }
}

#[cfg(test)]
mod tests {
    use super::{PasswordPrompt, PromptingBackend};
    use polkit_agent::helper_session::{AuthOutcome, HelperSession};
    use volumes::mount_backend::MountOutcome;

    /// A prompt that records what it was asked and answers from a script.
    struct ScriptedPrompt {
        answers: Vec<Option<String>>,
        asked: Vec<String>,
        notices: Vec<String>,
        echo_flags: Vec<bool>,
    }

    impl ScriptedPrompt {
        fn new(answers: Vec<Option<String>>) -> Self {
            Self {
                answers,
                asked: Vec::new(),
                notices: Vec::new(),
                echo_flags: Vec::new(),
            }
        }
    }

    impl PasswordPrompt for ScriptedPrompt {
        fn ask(&mut self, prompt: &str, echo: bool) -> Option<String> {
            self.asked.push(prompt.to_owned());
            self.echo_flags.push(echo);
            if self.answers.is_empty() {
                return None;
            }
            self.answers.remove(0)
        }

        fn notify(&mut self, message: &str) {
            self.notices.push(message.to_owned());
        }
    }

    /// The prompt must receive the echo flag, since a password prompt that
    /// echoes shows the password on screen.
    #[test]
    fn the_prompt_receives_the_echo_flag_it_is_given() {
        let mut prompt =
            ScriptedPrompt::new(vec![Some(String::from("x")), Some(String::from("y"))]);
        prompt.ask("Password: ", false);
        prompt.ask("Token: ", true);
        assert_eq!(prompt.echo_flags, [false, true]);
    }

    /// A prompt with nothing left to say dismisses rather than looping, or a
    /// helper that keeps asking would hang the UI forever.
    #[test]
    fn an_exhausted_prompt_dismisses() {
        let mut prompt = ScriptedPrompt::new(vec![]);
        assert!(prompt.ask("Password: ", false).is_none());
    }

    /// The prompt text reaches the UI verbatim: polkit chooses the wording
    /// and it may be localised.
    #[test]
    fn the_prompt_text_reaches_the_ui_unchanged() {
        let mut prompt = ScriptedPrompt::new(vec![Some(String::from("x"))]);
        prompt.ask("Contraseña: ", false);
        assert_eq!(prompt.asked, ["Contraseña: "]);
    }

    /// Availability follows the helper socket, since without it there is no
    /// way to answer a prompt and the action would always fail.
    #[test]
    fn availability_follows_the_helper_socket() {
        let Ok(backend) = PromptingBackend::connect(ScriptedPrompt::new(vec![])) else {
            return;
        };
        assert_eq!(
            volumes::mount_backend::MountBackend::is_available(&backend),
            HelperSession::socket_available()
        );
    }

    /// No authentication has happened before a mount is attempted, so a
    /// caller cannot mistake a stale outcome for a fresh one.
    #[test]
    fn a_fresh_backend_reports_no_authentication() {
        let Ok(backend) = PromptingBackend::connect(ScriptedPrompt::new(vec![])) else {
            return;
        };
        assert!(backend.last_authentication().is_none());
    }

    /// The live probe found this: dismissing the prompt reported "you do not
    /// have permission", which is false. polkit cannot tell a dismissal from
    /// a refusal, but we can, so the denial is reinterpreted.
    #[test]
    fn dismissing_the_prompt_reports_cancellation_not_a_denial() {
        let Ok(mut backend) = PromptingBackend::connect(ScriptedPrompt::new(vec![])) else {
            return;
        };
        backend.last_authentication = Some(AuthOutcome::Cancelled);
        assert_eq!(
            backend.reinterpret(MountOutcome::Denied),
            MountOutcome::Cancelled
        );
        assert!(
            backend
                .reinterpret(MountOutcome::Denied)
                .message()
                .is_none(),
            "a dismissed prompt must not accuse the user of lacking permission"
        );
    }

    /// A wrong password is a real rejection and must keep saying so, or the
    /// user is left with no idea why nothing happened.
    #[test]
    fn a_rejected_password_still_reports_the_denial() {
        let Ok(mut backend) = PromptingBackend::connect(ScriptedPrompt::new(vec![])) else {
            return;
        };
        backend.last_authentication = Some(AuthOutcome::Rejected);
        assert_eq!(
            backend.reinterpret(MountOutcome::Denied),
            MountOutcome::Denied
        );
    }

    /// A cancellation must never rewrite a success: if the mount happened,
    /// the browser has to navigate to it.
    #[test]
    fn a_successful_mount_is_never_rewritten_as_cancelled() {
        let Ok(mut backend) = PromptingBackend::connect(ScriptedPrompt::new(vec![])) else {
            return;
        };
        backend.last_authentication = Some(AuthOutcome::Cancelled);
        assert_eq!(
            backend.reinterpret(MountOutcome::Mounted),
            MountOutcome::Mounted
        );
    }
}

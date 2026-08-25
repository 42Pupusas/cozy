//! [`MountSession`] -- at most one mount in flight, and the identity it runs
//! as.
//!
//! [`MountService`] owns the threading. This owns the *policy* around it:
//!
//! * **One at a time.** A second mount started while the first is waiting for
//!   a password would raise a second prompt over the first, and the user
//!   could not tell which device either belonged to. A request made while one
//!   is running is refused rather than queued -- queueing would raise the
//!   second prompt after the first, at a moment the user has stopped
//!   expecting one.
//! * **Who the files belong to.** `vfat` and `exfat` store no ownership, so
//!   the mounter is told which uid and gid to present every file as. Get it
//!   wrong and the stick mounts read-only-in-practice: owned by root, with
//!   the user unable to write to their own device.
//! * **Where the backend comes from.** The real one needs a bus connection
//!   and must be built on the worker thread; the tests need a fake. Both go
//!   through [`Self::begin_with`].
//!
//! The session is deliberately not the modal. It reports what the mount asks
//! for and knows nothing about how the question is drawn.

use crate::mount_service::{MountProgress, MountService};
use volumes::mount_backend::MountBackend;
use volumes::mount_request::MountRequest;
use volumes::AttachedDevice;

/// The user a mount runs on behalf of.
///
/// A struct rather than three loose arguments because they must agree: the
/// name picks the directory under `/run/media`, and the ids decide who owns
/// the files inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountIdentity {
    name: String,
    uid: u32,
    gid: u32,
}

impl MountIdentity {
    /// Describe a user explicitly.
    ///
    /// Production builds the identity from the running process (see
    /// [`Self::current`]); this is how a test states one, since the ids that
    /// end up owning the files are the thing worth pinning.
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn new(name: impl Into<String>, uid: u32, gid: u32) -> Self {
        Self {
            name: name.into(),
            uid,
            gid,
        }
    }

    /// The user this process is running as.
    ///
    /// `$USER` names the media directory, and the *real* uid decides file
    /// ownership. The two are read from different places on purpose: `$USER`
    /// is inherited and can be stale or absent, so it is only ever used for a
    /// directory name -- which [`volumes::MountPoint`] sanitises anyway --
    /// while ownership comes from the kernel, which cannot be lied to.
    #[must_use]
    pub fn current() -> Self {
        let uid = dbus::sys::DbusSys::effective_uid();
        let name = std::env::var("USER").unwrap_or_else(|_| uid.to_string());
        Self { name, uid, gid: uid }
    }

    /// The user name, for the media directory.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// The mount currently in flight, if any.
pub struct MountSession {
    /// Woken so a prompt from the worker reaches the loop at once.
    wake: waker::Waker,
    /// Who mounts run as.
    identity: MountIdentity,
    /// The mount in progress. `None` when nothing is being attached.
    running: Option<MountService>,
}

impl MountSession {
    /// A session that runs mounts as `identity`, waking `wake`.
    #[must_use]
    pub const fn new(wake: waker::Waker, identity: MountIdentity) -> Self {
        Self {
            wake,
            identity,
            running: None,
        }
    }

    /// Whether a mount is in flight.
    #[must_use]
    pub const fn is_running(&self) -> bool {
        self.running.is_some()
    }

    /// Start attaching `device`, with a backend built by `build` on the
    /// worker thread.
    ///
    /// Returns `false` when the device cannot be mounted -- it is already
    /// attached, or its label yields no usable destination -- and when a
    /// mount is already running.
    pub fn begin_with<B, F>(&mut self, device: &AttachedDevice, build: F) -> bool
    where
        B: MountBackend,
        F: FnOnce(crate::mount_service::ChannelPrompt) -> B + Send + 'static,
    {
        if self.is_running() {
            return false;
        }
        let Some(request) = self.request_for(device) else {
            return false;
        };
        self.running = Some(MountService::start(&request, &self.wake, build));
        true
    }

    /// Start attaching `device` through the real system mounter.
    ///
    /// The backend is built inside the worker: a `DbusConn` belongs to the
    /// thread that uses it, and connecting can block.
    pub fn begin(&mut self, device: &AttachedDevice) -> bool {
        self.begin_with(device, BusBackend::connect)
    }

    /// The request this device implies, as this session's user.
    fn request_for(&self, device: &AttachedDevice) -> Option<MountRequest> {
        device.mount_request(self.identity.name(), self.identity.uid, self.identity.gid)
    }

    /// Take the next message from the running mount, if any.
    ///
    /// Clears the session when the mount finishes, so the next request is
    /// accepted rather than refused by a session holding a corpse.
    pub fn poll(&mut self) -> Option<MountProgress> {
        let progress = self.running.as_mut()?.poll()?;
        if matches!(progress, MountProgress::Finished(_)) {
            self.running = None;
        }
        Some(progress)
    }

    /// Deliver the user's reply to the waiting conversation.
    pub fn answer(&self, reply: Option<String>) -> bool {
        self.running
            .as_ref()
            .is_some_and(|service| service.answer(reply))
    }

    /// Abandon the mount, releasing a worker blocked on a prompt.
    pub fn abandon(&mut self) -> bool {
        self.running.take().is_some()
    }
}

/// The real mounter, or a stand-in that reports why it is unusable.
///
/// `PromptingBackend::connect` fails on a machine with no system bus. That is
/// not a crash: it is an answer the user should see, so the failure is
/// carried as a backend that reports it rather than unwrapped on a worker
/// thread where a panic would be invisible.
enum BusBackend {
    /// A live connection to the system bus.
    Live(Box<mounter::PromptingBackend<crate::mount_service::ChannelPrompt>>),
    /// No bus; every mount reports this.
    Unreachable(String),
}

impl BusBackend {
    fn connect(prompt: crate::mount_service::ChannelPrompt) -> Self {
        match mounter::PromptingBackend::connect(prompt) {
            Ok(backend) => Self::Live(Box::new(backend)),
            Err(error) => Self::Unreachable(format!("{error:?}")),
        }
    }
}

impl MountBackend for BusBackend {
    fn mount(&mut self, request: &MountRequest) -> volumes::mount_backend::MountOutcome {
        match self {
            Self::Live(backend) => backend.mount(request),
            Self::Unreachable(why) => {
                volumes::mount_backend::MountOutcome::Unavailable(why.clone())
            }
        }
    }

    fn unmount(&mut self, mount_point: &std::path::Path) -> volumes::mount_backend::MountOutcome {
        match self {
            Self::Live(backend) => backend.unmount(mount_point),
            Self::Unreachable(why) => {
                volumes::mount_backend::MountOutcome::Unavailable(why.clone())
            }
        }
    }

    fn is_available(&self) -> bool {
        match self {
            Self::Live(backend) => backend.is_available(),
            Self::Unreachable(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MountIdentity, MountSession};
    use crate::mount_service::MountProgress;
    use std::path::Path;
    use volumes::mount_backend::MountOutcome;
    use volumes::{AttachedDevice, ScriptedBackend};

    /// A removable stick holding a labelled vfat filesystem.
    fn stick(label: &str) -> AttachedDevice {
        AttachedDevice::new("/dev/sdb1", Some(label), "vfat", 1_000_000_000, 8, 17)
    }

    fn session() -> MountSession {
        MountSession::new(
            waker::Waker::new().expect("an eventfd"),
            MountIdentity::new("ada", 1000, 1000),
        )
    }

    /// Wait for a message rather than hanging the suite forever.
    fn next(session: &mut MountSession) -> MountProgress {
        for _ in 0..2000 {
            if let Some(progress) = session.poll() {
                return progress;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("the mount produced no message");
    }

    #[test]
    fn a_fresh_session_is_idle() {
        let session = session();
        assert!(!session.is_running());
    }

    #[test]
    fn beginning_a_mount_runs_it_and_reports_the_outcome() {
        let device = stick("KINGSTON");
        let mut session = session();
        assert!(session.begin_with(&device, |_| ScriptedBackend::always_succeeding()));
        assert!(session.is_running());
        assert_eq!(
            next(&mut session),
            MountProgress::Finished(MountOutcome::Mounted)
        );
        assert!(
            !session.is_running(),
            "a finished mount must not block the next one"
        );
    }

    /// The user's identity must reach the request, or the stick mounts owned
    /// by root and the user cannot write to their own device.
    #[test]
    fn the_request_carries_the_user_and_their_ids() {
        let device = stick("KINGSTON");
        let session = session();
        let request = session.request_for(&device).expect("a request");
        assert_eq!(request.uid(), 1000);
        assert_eq!(request.gid(), 1000);
        assert_eq!(
            request.mount_point().path(),
            Path::new("/run/media/ada/KINGSTON")
        );
    }

    /// A second prompt raised over the first is one the user cannot attribute
    /// to a device, so a concurrent request is refused.
    #[test]
    fn a_second_mount_is_refused_while_one_is_running() {
        let device = stick("KINGSTON");
        let mut session = session();
        assert!(session.begin_with(&device, |_| ScriptedBackend::always_succeeding()));
        assert!(
            !session.begin_with(&device, |_| ScriptedBackend::always_succeeding()),
            "one mount at a time"
        );
    }

    /// Mounting a mounted device would attach a second copy over the first.
    #[test]
    fn an_already_mounted_device_is_refused() {
        let device = stick("KINGSTON").mounted_at("/run/media/ada/KINGSTON");
        let mut session = session();
        assert!(!session.begin_with(&device, |_| ScriptedBackend::always_succeeding()));
        assert!(!session.is_running());
    }

    #[test]
    fn abandoning_clears_the_session() {
        let device = stick("KINGSTON");
        let mut session = session();
        session.begin_with(&device, |_| ScriptedBackend::always_succeeding());
        assert!(session.abandon());
        assert!(!session.is_running());
        assert!(!session.abandon());
    }

    /// `$USER` may be absent or stale, and it must never be the thing that
    /// stops a mount working.
    #[test]
    fn the_current_identity_always_yields_a_usable_name() {
        let identity = MountIdentity::current();
        assert!(!identity.name().is_empty());
    }
}

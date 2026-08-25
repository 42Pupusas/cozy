//! [`MountBackend`] -- the seam between "which device" and "how to mount it".
//!
//! # Why a trait rather than a function
//!
//! Mounting needs D-Bus, systemd and a polkit agent. Detection needs none of
//! them, and this crate has no dependencies at all -- deliberately, so that
//! listing storage works on a machine with no daemon installed. Putting the
//! privileged act behind a trait keeps that true: the implementation lives in
//! the binary that already has a bus connection, and `crates/volumes` never
//! learns the word "D-Bus".
//!
//! # Why the outcome is not `io::Error`
//!
//! The interesting failure is "a password is required", which is not an I/O
//! condition and is not final. A caller that receives it must show a prompt
//! and try again, so it has to be distinguishable from a refusal that no
//! amount of retrying will fix. [`MountOutcome`] therefore separates
//! *retryable* from *final*, and [`MountOutcome::is_retryable`] is the
//! question the UI actually asks.
//!
//! # Unmounting is offered, not assumed
//!
//! [`MountBackend::unmount`] has a default implementation that refuses, so a
//! backend which can only mount is a legal backend. The alternative -- making
//! every implementation write a stub -- invites a stub that lies.

use crate::mount_request::MountRequest;
use std::path::Path;

/// What happened when a mount was attempted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MountOutcome {
    /// The filesystem is now attached at the requested point.
    Mounted,
    /// A password is required and no agent answered. Prompt, then retry.
    NeedsAuthorization,
    /// The user dismissed the password prompt.
    ///
    /// Distinct from [`Self::Denied`], which is the machine refusing. Telling
    /// someone who closed a prompt that they lack permission is a lie, and it
    /// sends them looking for a problem that does not exist.
    Cancelled,
    /// Authorization was refused. Retrying will not help.
    Denied,
    /// The mounter was reached but the mount failed.
    Failed(String),
    /// The mounter could not be reached at all.
    Unavailable(String),
}

impl MountOutcome {
    /// Whether the filesystem is attached.
    #[must_use]
    pub const fn is_mounted(&self) -> bool {
        matches!(self, Self::Mounted)
    }

    /// Whether trying again, after asking the user for a password, could
    /// succeed.
    ///
    /// The single question the UI asks: it decides whether to raise a prompt
    /// or to show an error and stop.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        matches!(self, Self::NeedsAuthorization)
    }

    /// A sentence to show the user, or `None` when there is nothing to
    /// report because it worked.
    #[must_use]
    pub fn message(&self) -> Option<String> {
        match self {
            Self::Mounted | Self::Cancelled => None,
            Self::NeedsAuthorization => {
                Some(String::from("Authentication is required to mount this device."))
            }
            Self::Denied => Some(String::from(
                "You do not have permission to mount this device.",
            )),
            Self::Failed(what) => Some(format!("The device could not be mounted: {what}")),
            Self::Unavailable(_) => Some(String::from(
                "Could not reach the system service that mounts devices.",
            )),
        }
    }
}

/// Something that can attach a filesystem.
pub trait MountBackend {
    /// Attach the device described by `request`.
    fn mount(&mut self, request: &MountRequest) -> MountOutcome;

    /// Detach whatever is mounted at `mount_point`.
    ///
    /// Refused by default, so a mount-only backend needs no stub.
    fn unmount(&mut self, mount_point: &Path) -> MountOutcome {
        let _ = mount_point;
        MountOutcome::Unavailable(String::from("this backend cannot unmount"))
    }

    /// Whether this backend can do anything at all.
    ///
    /// Lets the UI hide a Mount action entirely rather than offering one that
    /// always fails.
    fn is_available(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{MountBackend, MountOutcome};
    use crate::mount_point::MountPoint;
    use crate::mount_request::MountRequest;
    use std::path::Path;

    /// A backend that refuses everything, standing in for a machine with no
    /// system mounter present.
    struct RefusingBackend;

    impl MountBackend for RefusingBackend {
        fn mount(&mut self, _request: &MountRequest) -> MountOutcome {
            MountOutcome::Unavailable(String::from("no mounter"))
        }

        fn is_available(&self) -> bool {
            false
        }
    }

    fn request() -> MountRequest {
        let point = MountPoint::under(Path::new("/run/media/bob"), Some("K"), "/dev/sdb1")
            .expect("an absolute root");
        MountRequest::new(
            String::from("/dev/sdb1"),
            String::from("vfat"),
            point,
            1000,
            1000,
        )
    }

    /// The distinction the whole enum exists for: a password prompt is worth
    /// retrying, a refusal is not.
    #[test]
    fn only_a_missing_password_is_retryable() {
        assert!(MountOutcome::NeedsAuthorization.is_retryable());
        assert!(!MountOutcome::Denied.is_retryable());
        assert!(!MountOutcome::Mounted.is_retryable());
        assert!(!MountOutcome::Cancelled.is_retryable());
        assert!(!MountOutcome::Failed(String::from("x")).is_retryable());
        assert!(!MountOutcome::Unavailable(String::from("x")).is_retryable());
    }

    /// Dismissing a prompt is a deliberate act, so it needs no error: the
    /// user knows what they did and an alert would be noise.
    #[test]
    fn a_dismissed_prompt_says_nothing_and_is_not_a_denial() {
        assert!(MountOutcome::Cancelled.message().is_none());
        assert_ne!(
            MountOutcome::Cancelled,
            MountOutcome::Denied,
            "closing a prompt is not the machine refusing"
        );
        assert!(!MountOutcome::Cancelled.is_mounted());
    }

    /// The wording of a refusal must never be shown for a cancellation: it
    /// tells someone who declined that they lack a permission they have.
    #[test]
    fn only_a_real_refusal_talks_about_permission() {
        let denied = MountOutcome::Denied.message().expect("a message");
        assert!(denied.contains("permission"), "{denied}");
        assert!(MountOutcome::Cancelled.message().is_none());
    }

    #[test]
    fn only_a_mounted_outcome_reports_success() {
        assert!(MountOutcome::Mounted.is_mounted());
        assert!(!MountOutcome::Denied.is_mounted());
    }

    /// Success and cancellation need no message; every real failure needs
    /// one, or the UI has nothing to show.
    #[test]
    fn every_failure_has_a_message_and_success_has_none() {
        assert!(MountOutcome::Mounted.message().is_none());
        for outcome in [
            MountOutcome::NeedsAuthorization,
            MountOutcome::Denied,
            MountOutcome::Failed(String::from("bad superblock")),
            MountOutcome::Unavailable(String::from("no bus")),
        ] {
            let message = outcome.message().expect("a failure must explain itself");
            assert!(!message.is_empty(), "{outcome:?}");
        }
    }

    /// A backend that cannot unmount must say so rather than claiming
    /// success, which would leave the UI showing a device as gone.
    #[test]
    fn the_default_unmount_refuses_rather_than_pretending() {
        let outcome = RefusingBackend.unmount(Path::new("/run/media/bob/K"));
        assert!(!outcome.is_mounted());
        assert!(!outcome.is_retryable(), "a missing feature is not retryable");
    }

    /// The trait must be usable as a trait object, since the UI holds one
    /// without knowing which backend it is.
    #[test]
    fn a_backend_is_object_safe() {
        let mut backend: Box<dyn MountBackend> = Box::new(RefusingBackend);
        assert!(!backend.is_available());
        assert_eq!(
            backend.mount(&request()),
            MountOutcome::Unavailable(String::from("no mounter"))
        );
    }
}

//! [`SystemdMount`] -- turn a [`MountRequest`] into a transient mount unit.
//!
//! The translation half of the backend, kept apart from the authentication
//! half so each can be tested alone. This one needs a bus but no password;
//! [`crate::prompting_backend`] needs both.
//!
//! # Why the outcome is not the transport error
//!
//! [`MountOutcome`] separates *retryable* from *final*, and the whole point
//! of the mapping here is that polkit's "interactive authorization required"
//! is retryable while "access denied" is not. Collapsing them would make the
//! UI either prompt forever or never prompt at all.

use dbus::conn::DbusConn;
use dbus::systemd1::{MountError, TransientMount, TransientUnmount};
use volumes::mount_backend::{MountBackend, MountOutcome};
use volumes::mount_request::MountRequest;
use std::path::Path;

/// Mounts through systemd's `StartTransientUnit`.
pub struct SystemdMount {
    conn: DbusConn,
}

impl SystemdMount {
    /// Connect to the system bus.
    pub fn connect() -> Result<Self, dbus::conn::DbusError> {
        Ok(Self {
            conn: DbusConn::system()?,
        })
    }

    /// Use an existing connection.
    #[must_use]
    pub const fn on(conn: DbusConn) -> Self {
        Self { conn }
    }

    /// The bus connection, for a caller that must also run an agent on it.
    pub const fn conn_mut(&mut self) -> &mut DbusConn {
        &mut self.conn
    }

    /// Build the systemd request that corresponds to `request`.
    fn unit_for(request: &MountRequest) -> TransientMount {
        let point = request.mount_point().as_str();
        if request.read_only() {
            return TransientMount::read_only(
                request.device(),
                &point,
                request.fs_type(),
                request.uid(),
                request.gid(),
            );
        }
        TransientMount::new(
            request.device(),
            &point,
            request.fs_type(),
            request.uid(),
            request.gid(),
        )
    }

    /// Classify a systemd failure into the seam's vocabulary.
    fn outcome_of(error: &MountError) -> MountOutcome {
        match error {
            MountError::NeedsAuthorization => MountOutcome::NeedsAuthorization,
            MountError::Denied => MountOutcome::Denied,
            MountError::BadMountPoint => {
                MountOutcome::Failed(String::from("the mount point is not a valid absolute path"))
            }
            MountError::Failed(what) => MountOutcome::Failed(what.clone()),
            MountError::Transport(what) => MountOutcome::Unavailable(what.clone()),
        }
    }
}

impl MountBackend for SystemdMount {
    fn mount(&mut self, request: &MountRequest) -> MountOutcome {
        match Self::unit_for(request).start(&mut self.conn) {
            Ok(_job) => MountOutcome::Mounted,
            Err(error) => Self::outcome_of(&error),
        }
    }

    fn unmount(&mut self, mount_point: &Path) -> MountOutcome {
        let point = mount_point.to_string_lossy();
        match TransientUnmount::new(&point).stop(&mut self.conn) {
            Ok(_job) => MountOutcome::Mounted,
            Err(error) => Self::outcome_of(&error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SystemdMount;
    use dbus::systemd1::MountError;
    use volumes::mount_backend::MountOutcome;
    use volumes::mount_point::MountPoint;
    use volumes::mount_request::MountRequest;
    use std::path::Path;

    fn request(fs: &str) -> MountRequest {
        let point = MountPoint::under(Path::new("/run/media/bob"), Some("KINGSTON"), "/dev/sdb1")
            .expect("an absolute root");
        MountRequest::new(
            String::from("/dev/sdb1"),
            String::from(fs),
            point,
            1000,
            985,
        )
    }

    /// The distinction the seam exists for must survive the translation: a
    /// password prompt stays retryable, a refusal does not.
    #[test]
    fn a_password_requirement_stays_retryable_and_a_denial_does_not() {
        assert!(
            SystemdMount::outcome_of(&MountError::NeedsAuthorization).is_retryable(),
            "a prompt must remain retryable across the translation"
        );
        assert!(!SystemdMount::outcome_of(&MountError::Denied).is_retryable());
    }

    /// A transport failure is not a mount failure: the difference is whether
    /// the service was reachable at all, and the UI wording differs.
    #[test]
    fn a_transport_failure_reports_the_service_as_unavailable() {
        let outcome = SystemdMount::outcome_of(&MountError::Transport(String::from("no bus")));
        assert_eq!(outcome, MountOutcome::Unavailable(String::from("no bus")));
    }

    /// A systemd failure keeps its detail, which is the only diagnosis the
    /// user gets for an unreadable disk.
    #[test]
    fn a_mount_failure_keeps_its_detail() {
        let outcome = SystemdMount::outcome_of(&MountError::Failed(String::from("bad superblock")));
        assert_eq!(outcome, MountOutcome::Failed(String::from("bad superblock")));
    }

    /// No error maps to success, or a failed mount would leave the browser
    /// navigating into a directory that is not there.
    #[test]
    fn no_failure_is_ever_translated_into_success() {
        for error in [
            MountError::NeedsAuthorization,
            MountError::Denied,
            MountError::BadMountPoint,
            MountError::Failed(String::new()),
            MountError::Transport(String::new()),
        ] {
            assert!(
                !SystemdMount::outcome_of(&error).is_mounted(),
                "{error:?} must not read as mounted"
            );
        }
    }

    /// The request's destination must be what systemd is told to mount on,
    /// since that path was validated by `MountPoint` and nothing else is.
    #[test]
    fn the_unit_is_named_after_the_validated_mount_point() {
        let unit = SystemdMount::unit_for(&request("vfat"));
        assert_eq!(
            unit.unit_name().expect("absolute").as_str(),
            "run-media-bob-KINGSTON.mount"
        );
    }

    /// A read-only request must not silently become writable, which would
    /// let a write reach a disk the user asked to protect.
    #[test]
    fn a_read_only_request_stays_read_only() {
        let unit = SystemdMount::unit_for(&request("vfat").without_write_access());
        let debug = format!("{unit:?}");
        assert!(debug.contains("ro"), "{debug}");
    }

    /// The hardening lives in `MountOptions`, but it must actually reach the
    /// unit built from a request -- a stick is attacker-supplied data.
    #[test]
    fn the_request_reaches_the_unit_hardened() {
        let unit = SystemdMount::unit_for(&request("vfat"));
        let debug = format!("{unit:?}");
        assert!(debug.contains("nosuid"), "{debug}");
        assert!(debug.contains("nodev"), "{debug}");
    }
}

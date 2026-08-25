//! [`MountRequest`] -- everything a mounter needs, and nothing about how.
//!
//! The request is the vocabulary shared by this crate and whatever performs
//! the privileged act. It names a device, a filesystem, a destination and an
//! owner. It says nothing about D-Bus, systemd, `UDisks2` or polkit, which is
//! what lets `crates/volumes` stay free of dependencies while a backend that
//! needs all four lives elsewhere.

use crate::mount_point::MountPoint;

/// A device to mount, and where it should go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MountRequest {
    /// The block device, e.g. `/dev/sdb1`.
    device: String,
    /// The filesystem type, e.g. `vfat`.
    fs_type: String,
    /// The validated destination directory.
    mount_point: MountPoint,
    /// The user who will own the files on a filesystem that stores no
    /// ownership of its own.
    uid: u32,
    /// That user's primary group.
    gid: u32,
    /// Whether to mount without write access.
    read_only: bool,
}

impl MountRequest {
    /// Describe a mount.
    #[must_use]
    pub const fn new(
        device: String,
        fs_type: String,
        mount_point: MountPoint,
        uid: u32,
        gid: u32,
    ) -> Self {
        Self {
            device,
            fs_type,
            mount_point,
            uid,
            gid,
            read_only: false,
        }
    }

    /// The same request, mounted read-only.
    #[must_use]
    pub const fn without_write_access(mut self) -> Self {
        self.read_only = true;
        self
    }

    /// The block device to attach.
    #[must_use]
    pub fn device(&self) -> &str {
        &self.device
    }

    /// The filesystem type.
    #[must_use]
    pub fn fs_type(&self) -> &str {
        &self.fs_type
    }

    /// Where it will be attached.
    #[must_use]
    pub const fn mount_point(&self) -> &MountPoint {
        &self.mount_point
    }

    /// The owning user.
    #[must_use]
    pub const fn uid(&self) -> u32 {
        self.uid
    }

    /// The owning group.
    #[must_use]
    pub const fn gid(&self) -> u32 {
        self.gid
    }

    /// Whether write access is withheld.
    #[must_use]
    pub const fn read_only(&self) -> bool {
        self.read_only
    }
}

#[cfg(test)]
mod tests {
    use super::MountRequest;
    use crate::mount_point::MountPoint;
    use std::path::Path;

    fn request() -> MountRequest {
        let point =
            MountPoint::under(Path::new("/run/media/bob"), Some("KINGSTON"), "/dev/sdb1")
                .expect("an absolute root");
        MountRequest::new(
            String::from("/dev/sdb1"),
            String::from("vfat"),
            point,
            1000,
            985,
        )
    }

    #[test]
    fn a_request_carries_what_a_mounter_needs() {
        let req = request();
        assert_eq!(req.device(), "/dev/sdb1");
        assert_eq!(req.fs_type(), "vfat");
        assert_eq!(
            req.mount_point().path(),
            Path::new("/run/media/bob/KINGSTON")
        );
        assert_eq!(req.uid(), 1000);
        assert_eq!(req.gid(), 985);
    }

    /// Write access is the default, since a stick the user plugged in is
    /// normally theirs to write to.
    #[test]
    fn a_request_is_writable_unless_asked_otherwise() {
        assert!(!request().read_only());
        assert!(request().without_write_access().read_only());
    }

    /// Withholding write access must change nothing else about the request.
    #[test]
    fn withholding_write_access_changes_only_that() {
        let writable = request();
        let read_only = request().without_write_access();
        assert_eq!(writable.device(), read_only.device());
        assert_eq!(writable.mount_point(), read_only.mount_point());
        assert_eq!(writable.uid(), read_only.uid());
    }
}

//! `org.freedesktop.systemd1` transient mount units.
//!
//! # Why systemd and not `mount(2)`
//!
//! `mount(2)` needs `CAP_SYS_ADMIN`, which a desktop application does not
//! have and should not want. The routes to a mounted stick are therefore an
//! fstab `user` entry (which must list the device in advance), a setuid
//! helper of our own (a local-root attack surface we refuse to ship), or
//! asking a privileged service that already exists. This is the third.
//!
//! `UDisks2` is the usual such service, but it is not installed everywhere --
//! notably not on the machine this was written on. `systemd` always is, and
//! `StartTransientUnit` can create a `.mount` unit at runtime, so it is the
//! dependency-free choice.
//!
//! # The authorisation is real
//!
//! `org.freedesktop.systemd1.manage-units` is `auth_admin_keep` by default,
//! so this call fails with an interactive-authorization error unless a polkit
//! agent answers a password prompt. That refusal is
//! [`MountError::NeedsAuthorization`], and it is a normal outcome rather than
//! a bug: the caller is expected to run an agent and retry.

use crate::call_flags::CallFlags;
use crate::call_target::CallTarget;
use crate::conn::{DbusConn, DbusError};
use crate::mount_options::MountOptions;
use crate::unit_name::UnitName;
use crate::value::Value;

const DEST: &str = "org.freedesktop.systemd1";
const MANAGER_PATH: &str = "/org/freedesktop/systemd1";
const MANAGER_IFACE: &str = "org.freedesktop.systemd1.Manager";

/// Why a mount attempt did not produce a mounted filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MountError {
    /// Polkit requires a password that no agent answered. Run an agent and
    /// retry; this is not a permanent refusal.
    NeedsAuthorization,
    /// Polkit refused outright, so retrying will not help.
    Denied,
    /// The mount point is not an absolute path.
    BadMountPoint,
    /// systemd accepted the request but the mount failed, e.g. an unknown
    /// filesystem or an unreadable disk.
    Failed(String),
    /// The bus itself failed. Held as a description rather than the
    /// `DbusError`, which wraps `io::Error` and so is neither `Clone` nor
    /// comparable -- this value lives in UI state, which needs both.
    Transport(String),
}

impl MountError {
    /// Classify a bus error, so a password prompt is distinguished from a
    /// real refusal.
    fn from_dbus(err: &DbusError) -> Self {
        let DbusError::Reply { ref name } = *err else {
            return Self::Transport(format!("{err:?}"));
        };
        match name.as_str() {
            "org.freedesktop.PolicyKit1.Error.NotAuthorized"
            | "org.freedesktop.DBus.Error.InteractiveAuthorizationRequired" => {
                Self::NeedsAuthorization
            }
            "org.freedesktop.DBus.Error.AccessDenied" => Self::Denied,
            other => Self::Failed(other.to_owned()),
        }
    }

    /// A short sentence for a user, not a log line.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::NeedsAuthorization => "Authentication is required to mount this device.".into(),
            Self::Denied => "You do not have permission to mount this device.".into(),
            Self::BadMountPoint => "That mount point is not a valid absolute path.".into(),
            Self::Failed(what) => format!("The device could not be mounted: {what}"),
            Self::Transport(_) => "Could not reach the system service to mount this device.".into(),
        }
    }
}

/// A mount request: what to mount, where, and how.
#[derive(Debug, Clone)]
pub struct TransientMount {
    device: String,
    mount_point: String,
    fs_type: String,
    options: MountOptions,
}

impl TransientMount {
    /// Mount `device` at `mount_point`, hardened for removable media.
    #[must_use]
    pub fn new(device: &str, mount_point: &str, fs_type: &str, uid: u32, gid: u32) -> Self {
        Self {
            device: device.to_owned(),
            mount_point: mount_point.to_owned(),
            fs_type: fs_type.to_owned(),
            options: MountOptions::for_filesystem(fs_type, uid, gid),
        }
    }

    /// The same, mounted read-only.
    #[must_use]
    pub fn read_only(device: &str, mount_point: &str, fs_type: &str, uid: u32, gid: u32) -> Self {
        Self {
            device: device.to_owned(),
            mount_point: mount_point.to_owned(),
            fs_type: fs_type.to_owned(),
            options: MountOptions::read_only(fs_type, uid, gid),
        }
    }

    /// The unit systemd will be asked to create.
    #[must_use]
    pub fn unit_name(&self) -> Option<UnitName> {
        UnitName::for_mount_point(&self.mount_point)
    }

    /// The `a(sv)` property list describing this mount to systemd.
    ///
    /// `What`, `Where`, `Type` and `Options` are the mount unit's own
    /// properties. `Description` is cosmetic but appears in `systemctl`
    /// output, so it names the device rather than leaving a blank unit.
    fn properties(&self) -> Value {
        let prop = |key: &str, value: &str| {
            Value::Struct(vec![
                Value::Str(key.to_owned()),
                Value::Variant(Box::new(Value::Str(value.to_owned()))),
            ])
        };
        Value::Array(vec![
            prop("Description", &format!("Mount {}", self.device)),
            prop("What", &self.device),
            prop("Where", &self.mount_point),
            prop("Type", &self.fs_type),
            prop("Options", &self.options.as_string()),
        ])
    }

    /// Ask systemd to create and start the unit.
    ///
    /// `replace` is the standard job mode: if a job for this unit is already
    /// queued, supersede it rather than failing.
    ///
    /// Returns the job object path on success.
    pub fn start(&self, conn: &mut DbusConn) -> Result<String, MountError> {
        let Some(unit) = self.unit_name() else {
            return Err(MountError::BadMountPoint);
        };
        let values = [
            Value::Str(unit.into_string()),
            Value::Str(String::from("replace")),
            self.properties(),
            Value::Array(vec![]),
        ];
        let reply = conn
            .call_with_flags(
                CallTarget::new(DEST, MANAGER_PATH, MANAGER_IFACE, "StartTransientUnit"),
                &values,
                "ssa(sv)a(sa(sv))",
                CallFlags::allow_interactive_authorization(),
            )
            .map_err(|err| MountError::from_dbus(&err))?;
        reply
            .values()
            .first()
            .and_then(Value::as_display_string)
            .ok_or_else(|| MountError::Failed(String::from("no job path returned")))
    }
}

/// Stopping a mount unit, which unmounts it.
#[derive(Debug, Clone)]
pub struct TransientUnmount {
    mount_point: String,
}

impl TransientUnmount {
    /// Unmount whatever is mounted at `mount_point`.
    #[must_use]
    pub fn new(mount_point: &str) -> Self {
        Self {
            mount_point: mount_point.to_owned(),
        }
    }

    /// Stop the unit named by the mount point.
    pub fn stop(&self, conn: &mut DbusConn) -> Result<String, MountError> {
        let Some(unit) = UnitName::for_mount_point(&self.mount_point) else {
            return Err(MountError::BadMountPoint);
        };
        let values = [
            Value::Str(unit.into_string()),
            Value::Str(String::from("replace")),
        ];
        let reply = conn
            .call_with_flags(
                CallTarget::new(DEST, MANAGER_PATH, MANAGER_IFACE, "StopUnit"),
                &values,
                "ss",
                CallFlags::allow_interactive_authorization(),
            )
            .map_err(|err| MountError::from_dbus(&err))?;
        reply
            .values()
            .first()
            .and_then(Value::as_display_string)
            .ok_or_else(|| MountError::Failed(String::from("no job path returned")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A password prompt is a retryable outcome and must not be reported as
    /// a permanent refusal, or the caller will never run an agent.
    #[test]
    fn an_interactive_authorization_error_is_distinguished_from_a_denial() {
        let needs = MountError::from_dbus(&DbusError::Reply {
            name: "org.freedesktop.DBus.Error.InteractiveAuthorizationRequired".into(),
        });
        assert_eq!(needs, MountError::NeedsAuthorization);

        let denied = MountError::from_dbus(&DbusError::Reply {
            name: "org.freedesktop.DBus.Error.AccessDenied".into(),
        });
        assert_eq!(denied, MountError::Denied);
        assert_ne!(needs, denied, "a prompt is not a refusal");
    }

    /// Polkit's own not-authorized name means the same thing.
    #[test]
    fn polkits_not_authorized_also_asks_for_a_password() {
        let err = MountError::from_dbus(&DbusError::Reply {
            name: "org.freedesktop.PolicyKit1.Error.NotAuthorized".into(),
        });
        assert_eq!(err, MountError::NeedsAuthorization);
    }

    /// An unrecognised bus error must survive with its name intact for
    /// diagnosis rather than collapsing to a generic failure.
    #[test]
    fn an_unknown_error_keeps_its_name() {
        let err = MountError::from_dbus(&DbusError::Reply {
            name: "org.freedesktop.systemd1.NoSuchUnit".into(),
        });
        assert_eq!(
            err,
            MountError::Failed("org.freedesktop.systemd1.NoSuchUnit".into())
        );
    }

    /// Every error must produce a sentence fit to show a user.
    #[test]
    fn every_error_has_a_readable_message() {
        let errors = [
            MountError::NeedsAuthorization,
            MountError::Denied,
            MountError::BadMountPoint,
            MountError::Failed("oh no".into()),
            MountError::Transport("Malformed".into()),
        ];
        for err in errors {
            let msg = err.message();
            assert!(!msg.is_empty(), "{err:?}");
            assert!(msg.ends_with('.') || msg.contains(": "), "{err:?}: {msg}");
        }
    }

    /// The unit named must be the mount point's, since that is what makes
    /// the mount land where the caller asked.
    #[test]
    fn the_unit_is_named_after_the_mount_point() {
        let mount = TransientMount::new("/dev/sdb1", "/run/media/bob/KINGSTON", "vfat", 1000, 1000);
        assert_eq!(
            mount.unit_name().expect("absolute").as_str(),
            "run-media-bob-KINGSTON.mount"
        );
    }

    /// A relative mount point must be refused before any bus traffic.
    #[test]
    fn a_relative_mount_point_has_no_unit() {
        let mount = TransientMount::new("/dev/sdb1", "relative/path", "vfat", 1000, 1000);
        assert!(mount.unit_name().is_none());
    }

    /// The properties must carry exactly what a mount unit needs, and the
    /// hardening must be present in what is actually sent -- not merely in
    /// `MountOptions` in isolation.
    #[test]
    fn the_properties_describe_the_mount_and_stay_hardened() {
        let mount = TransientMount::new("/dev/sdb1", "/run/media/bob/K", "vfat", 1000, 985);
        let Value::Array(props) = mount.properties() else {
            panic!("properties must be an array");
        };
        let mut seen = Vec::new();
        for prop in &props {
            let Value::Struct(fields) = prop else {
                panic!("each property is a struct");
            };
            let key = fields[0].clone().as_display_string().expect("a key");
            let value = fields[1].clone().into_inner();
            let text = value.as_display_string().expect("a string value");
            seen.push(key.clone());
            match key.as_str() {
                "What" => assert_eq!(text, "/dev/sdb1"),
                "Where" => assert_eq!(text, "/run/media/bob/K"),
                "Type" => assert_eq!(text, "vfat"),
                "Options" => {
                    assert!(text.contains("nosuid"), "{text}");
                    assert!(text.contains("nodev"), "{text}");
                    assert!(text.contains("uid=1000"), "{text}");
                }
                _ => {}
            }
        }
        for required in ["What", "Where", "Type", "Options"] {
            assert!(seen.contains(&required.to_owned()), "missing {required}");
        }
    }

    /// A read-only request must reach the wire as read-only.
    #[test]
    fn a_read_only_mount_sends_ro() {
        let mount = TransientMount::read_only("/dev/sdb1", "/run/media/bob/K", "vfat", 1000, 1000);
        let Value::Array(props) = mount.properties() else {
            panic!("properties must be an array");
        };
        let options = props
            .iter()
            .find_map(|p| {
                let Value::Struct(fields) = p else { return None };
                let key = fields[0].clone().as_display_string()?;
                (key == "Options").then(|| fields[1].clone().into_inner().as_display_string())?
            })
            .expect("an Options property");
        assert!(options.contains("ro"), "{options}");
    }

    /// The message body must marshal against the signature systemd declares,
    /// or the call fails on the wire rather than at the policy check.
    #[test]
    fn the_request_body_marshals_against_systemds_signature() {
        let mount = TransientMount::new("/dev/sdb1", "/run/media/bob/K", "vfat", 1000, 1000);
        let unit = mount.unit_name().expect("absolute").into_string();
        let values = [
            Value::Str(unit),
            Value::Str(String::from("replace")),
            mount.properties(),
            Value::Array(vec![]),
        ];
        let body = crate::marshal::Marshaller::marshal_body(&values, "ssa(sv)a(sa(sv))");
        assert!(body.is_some(), "the body must marshal");
    }

    /// The flag that makes a prompt possible. Without it polkit refuses
    /// instead of asking, so a perfectly correct authentication agent is
    /// never consulted -- which is exactly what happened on the first live
    /// run. Pinned on the wire, since the failure is silent.
    #[test]
    fn the_mount_call_permits_an_interactive_prompt() {
        let flags = CallFlags::allow_interactive_authorization();
        let frame = crate::message::MessageBuilder::build_method_call_with_flags(
            1,
            CallTarget::new(DEST, MANAGER_PATH, MANAGER_IFACE, "StartTransientUnit"),
            &[Value::Str(String::from("x")), Value::Str(String::new())],
            "ss",
            flags,
        )
        .expect("the frame builds");
        assert_eq!(
            frame[2] & 0x4,
            0x4,
            "the flags byte must carry ALLOW_INTERACTIVE_AUTHORIZATION"
        );
    }

    /// And the default stays off, so no existing caller starts prompting.
    #[test]
    fn an_ordinary_call_does_not_permit_a_prompt() {
        let frame = crate::message::MessageBuilder::build_method_call_with_values(
            1,
            DEST,
            MANAGER_PATH,
            MANAGER_IFACE,
            "ListUnits",
            &[],
            "",
        )
        .expect("the frame builds");
        assert_eq!(frame[2], 0, "an ordinary call carries no flags");
    }

    /// Unmounting names the same unit as mounting, so the two cannot drift.
    #[test]
    fn unmounting_targets_the_same_unit_as_mounting() {
        let point = "/run/media/bob/KINGSTON";
        let mount = TransientMount::new("/dev/sdb1", point, "vfat", 1000, 1000);
        let unmount = TransientUnmount::new(point);
        assert_eq!(
            mount.unit_name().expect("absolute"),
            UnitName::for_mount_point(&unmount.mount_point).expect("absolute")
        );
    }
}

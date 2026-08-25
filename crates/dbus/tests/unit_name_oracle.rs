//! Check [`UnitName`] against systemd's own escaping, rather than against my
//! reading of it.
//!
//! `systemd-escape --path --suffix=mount` is the same code systemd uses
//! internally, so any disagreement here is a real bug in ours. The test skips
//! itself if the binary is absent, so it stays valid on a machine without
//! systemd.

use dbus::unit_name::UnitName;
use std::process::Command;

/// The oracle: systemd's own escaper.
struct SystemdEscape;

impl SystemdEscape {
    /// Whether the binary exists to compare against.
    fn available() -> bool {
        Command::new("systemd-escape")
            .arg("--version")
            .output()
            .is_ok()
    }

    /// What systemd calls the mount unit for `path`.
    fn mount_unit(path: &str) -> Option<String> {
        let out = Command::new("systemd-escape")
            .args(["--path", "--suffix=mount", path])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        Some(String::from_utf8(out.stdout).ok()?.trim_end().to_owned())
    }
}

/// Every path we could plausibly meet must escape identically to systemd.
#[test]
fn our_escaping_matches_systemds_own() {
    if !SystemdEscape::available() {
        eprintln!("systemd-escape absent; skipping oracle comparison");
        return;
    }

    let paths = [
        "/",
        "/mnt",
        "/run/media/bob/KINGSTON",
        "/run/media/bob/My Disk",
        "/mnt/my-disk",
        "/mnt/my/disk",
        "/mnt/disk.backup",
        "/.hidden/disk",
        "/mnt/a:b_c",
        "/mnt/a\\b",
        "/media/Ünïcode",
        "/media/emoji-💾-stick",
        "/mnt/tab\there",
        "/mnt/percent%20literal",
        "/mnt/quote'and\"quote",
        "/mnt/UPPER/lower/123",
        "/run/media/bob/SANDISK 64GB (backup)",
    ];

    for path in paths {
        let Some(expected) = SystemdEscape::mount_unit(path) else {
            continue;
        };
        let ours = UnitName::for_mount_point(path)
            .unwrap_or_else(|| panic!("{path} must produce a unit name"))
            .into_string();
        assert_eq!(ours, expected, "escaping disagreed for {path}");
    }
}

/// Redundant separators must resolve to the same unit systemd names.
#[test]
fn messy_separators_agree_with_systemd_too() {
    if !SystemdEscape::available() {
        eprintln!("systemd-escape absent; skipping oracle comparison");
        return;
    }

    for path in ["/run//media/bob", "/run/media/bob/", "//run/media/bob//"] {
        let Some(expected) = SystemdEscape::mount_unit(path) else {
            continue;
        };
        let ours = UnitName::for_mount_point(path)
            .unwrap_or_else(|| panic!("{path} must produce a unit name"))
            .into_string();
        assert_eq!(ours, expected, "escaping disagreed for {path}");
    }
}

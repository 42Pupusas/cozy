//! `org.freedesktop.login1` convenience calls: setting backlight brightness
//! through the session, not a direct sysfs write.
//!
//! `/sys/class/backlight/*/brightness` is root-owned by default; the usual
//! way an unprivileged desktop session gets write access is a udev rule
//! granting the `video` group (or an `ACL`/`uaccess` tag) permission on it.
//! This system has neither set up, which is exactly why a direct sysfs
//! write silently failed here (see `statusbar::brightness::Brightness::adjust`'s history).
//!
//! `logind` exposes `SetBrightness` on the session object specifically so a
//! desktop session doesn't need either of those: it's polkit-gated instead,
//! and (for an active, local session) the default polkit policy allows it
//! without a password prompt.

use crate::conn::{DbusConn, DbusError};
use crate::message::BodyArg;
use crate::sys::DbusSys;
use crate::value::Value;

const DEST: &str = "org.freedesktop.login1";
const MANAGER_PATH: &str = "/org/freedesktop/login1";
const MANAGER_IFACE: &str = "org.freedesktop.login1.Manager";
const SESSION_IFACE: &str = "org.freedesktop.login1.Session";

/// `org.freedesktop.login1` calls scoped to "our own session".
///
/// A zero-sized marker type (not a value with fields) since every call
/// takes the live `&mut DbusConn` fresh rather than owning one, matching
/// the posture of every other module in this crate (short-lived polls, not
/// a long-held session handle).
pub struct LogindSession;

impl LogindSession {
    /// Find the object path of the session this process belongs to.
    ///
    /// Tries `Manager.GetSessionByPID(our own pid)` first -- the direct,
    /// correct answer for the common case (the statusbar running directly
    /// inside the graphical session's process tree). Falls back to picking
    /// the seat0 session out of `Manager.ListSessions` if that fails with
    /// `NoSessionForPID` (e.g. this process's session scope got detached
    /// from its pid's cgroup somehow -- `systemd-run`, certain sandboxing,
    /// etc.) -- there's normally exactly one graphical session on a
    /// single-seat desktop, so "the session on seat0" is a reasonable,
    /// unambiguous fallback target.
    fn own_path(conn: &mut DbusConn) -> Result<String, DbusError> {
        let by_pid = conn.call(
            DEST,
            MANAGER_PATH,
            MANAGER_IFACE,
            "GetSessionByPID",
            &[BodyArg::U32(DbusSys::own_pid())],
        );
        if let Ok(reply) = by_pid
            && let Some(path) = reply
                .values()
                .into_iter()
                .next()
                .and_then(|v| v.as_display_string())
        {
            return Ok(path);
        }

        let reply = conn.call(DEST, MANAGER_PATH, MANAGER_IFACE, "ListSessions", &[])?;
        let values = reply.values();
        let sessions = values
            .first()
            .and_then(Value::as_items)
            .ok_or(DbusError::Malformed)?;
        for session in sessions {
            let Some(fields) = session.as_items() else {
                continue;
            };
            // ListSessions element: (s session_id, u uid, s user_name, s seat_id, o object_path)
            let Some(seat_id) = fields.get(3).and_then(Value::as_display_string) else {
                continue;
            };
            if seat_id == "seat0"
                && let Some(path) = fields.get(4).and_then(Value::as_display_string)
            {
                return Ok(path);
            }
        }
        Err(DbusError::Malformed)
    }

    /// Set `/sys/class/backlight/<name>/brightness` to `value`.
    ///
    /// Calls `Session.SetBrightness("backlight", name, value)` on our own
    /// session object -- `subsystem` is always `"backlight"` (the other
    /// logind-known subsystem, `"leds"`, is for keyboard backlights, not
    /// display ones).
    pub fn set_backlight_brightness(
        conn: &mut DbusConn,
        name: &str,
        value: u32,
    ) -> Result<(), DbusError> {
        let session_path = Self::own_path(conn)?;
        conn.call(
            DEST,
            &session_path,
            SESSION_IFACE,
            "SetBrightness",
            &[
                BodyArg::Str("backlight".to_string()),
                BodyArg::Str(name.to_string()),
                BodyArg::U32(value),
            ],
        )?;
        Ok(())
    }
}

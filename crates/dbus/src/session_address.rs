//! Resolution of the session bus's socket address.
//!
//! Split out of [`crate::conn::DbusConn`] because address resolution is a
//! pure, string-level decision with several fallbacks and awkward failure
//! modes, while `DbusConn` is all socket/`io_uring` machinery. Keeping them
//! fused meant the interesting logic could only be exercised by actually
//! connecting to a live bus -- so the branch that matters most in practice
//! (no `DBUS_SESSION_BUS_ADDRESS` at all) had no test coverage.

use std::path::PathBuf;

/// Where the session bus socket lives, as a connectable endpoint.
///
/// D-Bus addresses distinguish filesystem sockets from Linux abstract
/// sockets, and they are connected with different `sockaddr_un` encodings,
/// so this preserves the distinction rather than flattening to a string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionAddress {
    /// A filesystem path (`unix:path=/run/user/1000/bus`).
    Path(String),
    /// A Linux abstract socket name (`unix:abstract=/tmp/dbus-XXXX`).
    Abstract(String),
}

impl SessionAddress {
    /// The conventional session bus socket beneath `XDG_RUNTIME_DIR`.
    const RUNTIME_SOCKET: &'static str = "bus";

    /// Resolve the session bus address the way the reference implementations
    /// do: honour `DBUS_SESSION_BUS_ADDRESS` when it is set, and otherwise
    /// fall back to `$XDG_RUNTIME_DIR/bus`.
    ///
    /// The fallback is not a nicety -- it is the case that matters on a
    /// niri/Wayland session. Compositor children inherit the compositor's
    /// environment, and when niri is started from a TTY or from a display
    /// manager that does not wrap the session in `dbus-run-session`, the
    /// variable is simply absent even though the user bus is running and
    /// reachable at its well-known path. Requiring the variable meant the
    /// status bar's tray watcher gave up at startup and never claimed
    /// `org.kde.StatusNotifierWatcher`, so tray clients launched alongside
    /// it (Mattermost) found no host and never showed an icon -- while a
    /// manual relaunch from a terminal, which *does* export the variable,
    /// always worked and made the bug look like a spawn-ordering race.
    #[must_use]
    pub fn resolve() -> Option<Self> {
        if let Some(address) = std::env::var("DBUS_SESSION_BUS_ADDRESS")
            .ok()
            .and_then(|value| Self::parse(&value))
        {
            return Some(address);
        }
        Self::from_runtime_dir(std::env::var("XDG_RUNTIME_DIR").ok().as_deref())
    }

    /// Where the session bus socket file is, or will appear, in the
    /// current environment.
    ///
    /// Unlike [`Self::resolve`], the answer does not depend on the socket
    /// existing, so a caller can start watching for its creation before the
    /// bus is up. `None` for an abstract address, which has no file to
    /// watch.
    #[must_use]
    pub fn expected_socket_path() -> Option<PathBuf> {
        Self::expected_socket_path_in(
            std::env::var("DBUS_SESSION_BUS_ADDRESS").ok().as_deref(),
            std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
        )
    }

    /// [`Self::expected_socket_path`] with the environment passed in.
    #[must_use]
    pub fn expected_socket_path_in(
        address_variable: Option<&str>,
        runtime_dir: Option<&str>,
    ) -> Option<PathBuf> {
        match address_variable.and_then(Self::parse) {
            Some(Self::Path(path)) => Some(PathBuf::from(path)),
            Some(Self::Abstract(_)) => None,
            None => {
                let dir = runtime_dir.filter(|dir| !dir.is_empty())?;
                Some(PathBuf::from(dir).join(Self::RUNTIME_SOCKET))
            }
        }
    }

    /// Build the well-known `$XDG_RUNTIME_DIR/bus` endpoint.
    ///
    /// Taken as a parameter rather than read from the environment so the
    /// fallback is testable without mutating process-global state (which
    /// would race other tests in the same binary).
    #[must_use]
    pub fn from_runtime_dir(runtime_dir: Option<&str>) -> Option<Self> {
        let dir = runtime_dir?;
        if dir.is_empty() {
            return None;
        }
        let dir = dir.strip_suffix('/').unwrap_or(dir);
        let path = format!("{dir}/{}", Self::RUNTIME_SOCKET);
        // Only offer the fallback when the socket is really there. Handing a
        // nonexistent path to `connect(2)` would turn a clean "no bus" into
        // a confusing ENOENT from deep inside the connection code.
        std::fs::metadata(&path).ok()?;
        Some(Self::Path(path))
    }

    /// Parse a `DBUS_SESSION_BUS_ADDRESS` value.
    ///
    /// The variable may hold several `;`-separated candidate addresses, each
    /// with `,`-separated key/value pairs. Only the `unix:` transport is
    /// supported; anything else (e.g. `tcp:`) yields `None` rather than a
    /// guess, letting the caller fall back to the runtime directory.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        for part in value.split(';') {
            let Some(rest) = part.strip_prefix("unix:") else {
                continue;
            };
            for kv in rest.split(',') {
                if let Some(path) = kv.strip_prefix("path=")
                    && !path.is_empty()
                {
                    return Some(Self::Path(path.to_owned()));
                }
                if let Some(name) = kv.strip_prefix("abstract=")
                    && !name.is_empty()
                {
                    return Some(Self::Abstract(name.to_owned()));
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_plain_unix_path_address() {
        assert_eq!(
            SessionAddress::parse("unix:path=/run/user/1000/bus"),
            Some(SessionAddress::Path("/run/user/1000/bus".to_owned()))
        );
    }

    #[test]
    fn parses_an_abstract_address() {
        assert_eq!(
            SessionAddress::parse("unix:abstract=/tmp/dbus-AbCdEf"),
            Some(SessionAddress::Abstract("/tmp/dbus-AbCdEf".to_owned()))
        );
    }

    #[test]
    fn ignores_guid_fields_alongside_the_path() {
        assert_eq!(
            SessionAddress::parse("unix:path=/run/user/1000/bus,guid=deadbeef"),
            Some(SessionAddress::Path("/run/user/1000/bus".to_owned()))
        );
    }

    #[test]
    fn skips_unsupported_transports_and_takes_the_unix_candidate() {
        assert_eq!(
            SessionAddress::parse("tcp:host=localhost,port=1234;unix:path=/run/user/1000/bus"),
            Some(SessionAddress::Path("/run/user/1000/bus".to_owned()))
        );
    }

    #[test]
    fn rejects_an_address_with_no_unix_transport() {
        assert_eq!(SessionAddress::parse("tcp:host=localhost,port=1234"), None);
    }

    #[test]
    fn rejects_an_empty_path_rather_than_connecting_to_nothing() {
        assert_eq!(SessionAddress::parse("unix:path="), None);
    }

    #[test]
    fn expected_path_follows_the_address_variable() {
        assert_eq!(
            SessionAddress::expected_socket_path_in(
                Some("unix:path=/run/user/7/custom-bus,guid=ab"),
                Some("/run/user/1000"),
            ),
            Some(PathBuf::from("/run/user/7/custom-bus"))
        );
    }

    #[test]
    fn expected_path_falls_back_to_the_runtime_dir_without_requiring_the_socket() {
        assert_eq!(
            SessionAddress::expected_socket_path_in(None, Some("/nonexistent-xdg-runtime-dir")),
            Some(PathBuf::from("/nonexistent-xdg-runtime-dir/bus"))
        );
    }

    #[test]
    fn expected_path_is_absent_for_an_abstract_address() {
        assert_eq!(
            SessionAddress::expected_socket_path_in(
                Some("unix:abstract=/tmp/dbus-AbCdEf"),
                Some("/run/user/1000"),
            ),
            None
        );
    }

    #[test]
    fn expected_path_needs_some_location_to_name() {
        assert_eq!(SessionAddress::expected_socket_path_in(None, None), None);
        assert_eq!(
            SessionAddress::expected_socket_path_in(None, Some("")),
            None
        );
    }

    #[test]
    fn runtime_fallback_needs_a_runtime_dir() {
        assert_eq!(SessionAddress::from_runtime_dir(None), None);
        assert_eq!(SessionAddress::from_runtime_dir(Some("")), None);
    }

    #[test]
    fn runtime_fallback_declines_when_no_socket_exists_there() {
        // A directory that certainly holds no `bus` socket: the fallback must
        // decline rather than hand a bogus path to connect(2).
        assert_eq!(
            SessionAddress::from_runtime_dir(Some("/nonexistent-xdg-runtime-dir")),
            None
        );
    }

    #[test]
    fn runtime_fallback_tolerates_a_trailing_slash() {
        // Both spellings must agree; on a machine with a real user bus this
        // finds it, and on one without, both decline together.
        assert_eq!(
            SessionAddress::from_runtime_dir(Some("/run/user/1000")),
            SessionAddress::from_runtime_dir(Some("/run/user/1000/"))
        );
    }
}

//! Ask polkit what this session may actually do, before any mounting code
//! is designed around an assumption.
//!
//! Run with `cargo run -p dbus --bin mount-probe`.

use dbus::conn::DbusConn;
use dbus::value::Value;

/// One polkit question and its answer.
struct AuthProbe {
    action: &'static str,
}

impl AuthProbe {
    const DEST: &'static str = "org.freedesktop.PolicyKit1";
    const PATH: &'static str = "/org/freedesktop/PolicyKit1/Authority";
    const IFACE: &'static str = "org.freedesktop.PolicyKit1.Authority";

    const fn new(action: &'static str) -> Self {
        Self { action }
    }

    /// The `(sa{sv})` subject naming this process by pid and start time.
    fn subject() -> Value {
        let pid = std::process::id();
        Value::Struct(vec![
            Value::Str("unix-process".into()),
            Value::Array(vec![
                Value::DictEntry(
                    Box::new(Value::Str("pid".into())),
                    Box::new(Value::Variant(Box::new(Value::U32(pid)))),
                ),
                Value::DictEntry(
                    Box::new(Value::Str("start-time".into())),
                    Box::new(Value::Variant(Box::new(Value::U64(Self::start_time())))),
                ),
            ]),
        ])
    }

    /// Field 22 of `/proc/self/stat`, the process start time in clock ticks.
    /// The comm field can contain spaces, so scan after its final `)`.
    fn start_time() -> u64 {
        let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
        stat.rfind(')')
            .and_then(|close| stat.get(close + 2..))
            .and_then(|rest| rest.split_whitespace().nth(19))
            .and_then(|field| field.parse().ok())
            .unwrap_or(0)
    }

    /// Ask, without allowing an interactive prompt, so the answer reports
    /// what happens silently rather than what a password could unlock.
    fn ask(&self, conn: &mut DbusConn) -> String {
        let values = [
            Self::subject(),
            Value::Str(self.action.into()),
            Value::Array(vec![]),
            Value::U32(0),
            Value::Str(String::new()),
        ];
        match conn.call_with_values(
            Self::DEST,
            Self::PATH,
            Self::IFACE,
            "CheckAuthorization",
            &values,
            "(sa{sv})sa{ss}us",
        ) {
            Ok(reply) => Self::describe(&reply.values()),
            Err(err) => format!("call failed: {err:?}"),
        }
    }

    /// The reply is `(bba{ss})`: authorized, challenge, details.
    fn describe(values: &[Value]) -> String {
        let Some(Value::Struct(fields)) = values.first() else {
            return format!("unexpected reply shape: {values:?}");
        };
        let flag = |i: usize| matches!(fields.get(i), Some(Value::Bool(true)));
        match (flag(0), flag(1)) {
            (true, _) => "ALLOWED silently".into(),
            (false, true) => "needs a password prompt (challenge)".into(),
            (false, false) => "DENIED outright".into(),
        }
    }
}

/// Whether a bus name can be started at all, which tells a missing daemon
/// apart from a refused permission.
struct ServiceProbe {
    name: &'static str,
}

impl ServiceProbe {
    const fn new(name: &'static str) -> Self {
        Self { name }
    }

    fn present(&self, conn: &mut DbusConn) -> String {
        match conn.call(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "NameHasOwner",
            &[dbus::message::BodyArg::Str(self.name.into())],
        ) {
            Ok(reply) => match reply.values().first() {
                Some(Value::Bool(true)) => "running".into(),
                Some(Value::Bool(false)) => "not running (may be activatable)".into(),
                other => format!("odd reply: {other:?}"),
            },
            Err(err) => format!("query failed: {err:?}"),
        }
    }
}

/// The probe run as a whole.
struct MountProbe;

impl MountProbe {
    const ACTIONS: [&'static str; 5] = [
        "org.freedesktop.systemd1.manage-units",
        "org.freedesktop.udisks2.filesystem-mount",
        "org.freedesktop.udisks2.filesystem-mount-system",
        "org.freedesktop.login1.power-off",
        "org.freedesktop.login1.set-brightness",
    ];

    const SERVICES: [&'static str; 3] = [
        "org.freedesktop.systemd1",
        "org.freedesktop.UDisks2",
        "org.freedesktop.PolicyKit1",
    ];

    fn run() {
        let mut conn = match DbusConn::system() {
            Ok(conn) => conn,
            Err(err) => {
                println!("no system bus: {err:?}");
                return;
            }
        };

        println!("bus names:\n");
        for name in Self::SERVICES {
            let state = ServiceProbe::new(name).present(&mut conn);
            println!("  {name}\n    -> {state}");
        }

        println!("\npolkit verdicts for this session:\n");
        for action in Self::ACTIONS {
            let verdict = AuthProbe::new(action).ask(&mut conn);
            println!("  {action}\n    -> {verdict}");
        }

        Self::probe_session_id(&mut conn);
        Self::probe_real_mount(&mut conn);
    }

    /// The session id polkit needs to register an agent.
    fn probe_session_id(conn: &mut DbusConn) {
        println!("\nlogind session:\n");
        match dbus::logind::LogindSession::own_session_id(conn) {
            Ok(id) => println!("  our session id -> {id:?}"),
            Err(err) => println!("  could not resolve a session id: {err:?}"),
        }
    }

    /// Attempt a mount that cannot succeed, to see which error the bus
    /// really returns. The device does not exist, so the only possible
    /// outcomes are an authorisation refusal (proving the classification) or
    /// a mount failure (proving we got past the policy check).
    fn probe_real_mount(conn: &mut DbusConn) {
        use dbus::systemd1::{MountError, TransientMount};

        let point = "/run/media/cozy-probe-nonexistent";
        let mount = TransientMount::new("/dev/does-not-exist", point, "vfat", 1000, 1000);
        println!("\nattempting a mount that cannot succeed:\n");
        println!("  unit: {}", mount.unit_name().expect("absolute").as_str());
        match mount.start(conn) {
            Ok(job) => println!("  -> systemd queued job {job} (policy check PASSED)"),
            Err(MountError::NeedsAuthorization) => {
                println!("  -> NeedsAuthorization: a polkit agent is required");
                println!("     (classification confirmed against the live bus)");
            }
            Err(other) => println!("  -> {other:?}\n     {}", other.message()),
        }
    }
}

fn main() {
    MountProbe::run();
}

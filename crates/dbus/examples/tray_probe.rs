//! Live-bus diagnostic: register a fake `StatusNotifierItem` with whatever
//! watcher currently owns the name, then report which protocol hops respond.
//!
//! Run: `cargo run -p dbus --example tray_probe`

use std::io::Write as _;
use std::time::{Duration, Instant};

use dbus::message::{BodyArg, MessageType, Reply};
use dbus::{DbusConn, Value};

const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const ITEM_IFACE: &str = "org.kde.StatusNotifierItem";
const ITEM_PATH: &str = "/StatusNotifierItem";
const BUS: &str = "org.freedesktop.DBus";
const BUS_PATH: &str = "/org/freedesktop/DBus";
const ICON_EDGE: i32 = 22;

struct ProcScan;

impl ProcScan {
    fn read(path: &str) -> String {
        std::fs::read_to_string(path).unwrap_or_default()
    }

    fn state_of(stat: &str) -> String {
        stat.rfind(')')
            .and_then(|i| stat.get(i + 2..i + 3))
            .unwrap_or("?")
            .to_owned()
    }

    fn dump_threads_of(pid: u64) {
        let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
            println!("proc: pid {pid} has no readable task dir");
            return;
        };
        for task in tasks.flatten() {
            let tid = task.file_name().to_string_lossy().into_owned();
            let base = format!("/proc/{pid}/task/{tid}");
            let comm = Self::read(&format!("{base}/comm")).trim().to_owned();
            let wchan = Self::read(&format!("{base}/wchan")).trim().to_owned();
            let state = Self::state_of(&Self::read(&format!("{base}/stat")));
            println!("proc:   tid {tid} [{state}] {comm} wchan={wchan}");
        }
    }
}

struct TrayProbe {
    conn: DbusConn,
    attention: bool,
    started: Instant,
}

impl TrayProbe {
    fn new() -> Self {
        let conn = match DbusConn::session() {
            Ok(conn) => conn,
            Err(e) => {
                println!("FATAL: session bus unreachable: {e}");
                std::process::exit(2);
            }
        };
        Self {
            conn,
            attention: false,
            started: Instant::now(),
        }
    }

    fn log(&self, msg: &str) {
        println!("[{:7.3}s] {msg}", self.started.elapsed().as_secs_f64());
        let _ = std::io::stdout().flush();
    }

    fn report_watcher_owner(&mut self) -> Option<u64> {
        let owner = match self.conn.call(
            BUS,
            BUS_PATH,
            BUS,
            "GetNameOwner",
            &[BodyArg::Str(WATCHER.into())],
        ) {
            Ok(reply) => reply
                .values()
                .first()
                .and_then(Value::as_display_string)
                .unwrap_or_default(),
            Err(e) => {
                self.log(&format!("FATAL: no watcher owns {WATCHER}: {e:?}"));
                std::process::exit(2);
            }
        };
        let pid = self
            .conn
            .call(
                BUS,
                BUS_PATH,
                BUS,
                "GetConnectionUnixProcessID",
                &[BodyArg::Str(owner.clone())],
            )
            .ok()
            .and_then(|r| r.values().first().and_then(Value::as_u64));
        let comm = pid.map(|p| ProcScan::read(&format!("/proc/{p}/comm")).trim().to_owned());
        self.log(&format!("watcher owner: {owner} pid={pid:?} comm={comm:?}"));
        pid
    }

    fn register(&mut self) {
        self.log("register: calling RegisterStatusNotifierItem (blocks until watcher replies)");
        match self.conn.call(
            WATCHER,
            WATCHER_PATH,
            WATCHER,
            "RegisterStatusNotifierItem",
            &[BodyArg::Str(ITEM_PATH.into())],
        ) {
            Ok(_) => self.log("register: PASS -- watcher loop is alive and replied"),
            Err(e) => {
                self.log(&format!("register: FAIL: {e:?}"));
                std::process::exit(2);
            }
        }
    }

    fn icon(argb: [u8; 4]) -> Value {
        let pixels = usize::try_from(ICON_EDGE * ICON_EDGE).unwrap_or(0);
        let mut bytes = Vec::with_capacity(pixels * 4);
        for _ in 0..pixels {
            bytes.extend_from_slice(&argb);
        }
        Value::Array(vec![Value::Struct(vec![
            Value::I32(ICON_EDGE),
            Value::I32(ICON_EDGE),
            Value::Array(bytes.into_iter().map(Value::Byte).collect()),
        ])])
    }

    fn property_value(&self, property: &str) -> Option<Value> {
        match property {
            "Status" => {
                let status = if self.attention {
                    "NeedsAttention"
                } else {
                    "Active"
                };
                Some(Value::Variant(Box::new(Value::Str(status.into()))))
            }
            "IconPixmap" => Some(Value::Variant(Box::new(Self::icon([0xff, 0xff, 0, 0])))),
            "AttentionIconPixmap" => Some(Value::Variant(Box::new(Self::icon([0xff, 0, 0, 0xff])))),
            _ => None,
        }
    }

    fn answer(&mut self, msg: &Reply) -> bool {
        let sender = msg.sender.clone().unwrap_or_default();
        if msg.kind != MessageType::MethodCall {
            self.log(&format!(
                "recv non-call: kind={:?} iface={:?} member={:?}",
                msg.kind, msg.interface, msg.member
            ));
            return false;
        }
        if msg.member.as_deref() != Some("Get") {
            self.log(&format!(
                "recv unexpected call {:?}.{:?} -- answering UnknownMethod",
                msg.interface, msg.member
            ));
            let _ = self.conn.send_error(
                &sender,
                msg.serial,
                "org.freedesktop.DBus.Error.UnknownMethod",
                "tray probe",
            );
            return false;
        }
        let values = msg.values();
        let property = values
            .get(1)
            .and_then(Value::as_display_string)
            .unwrap_or_default();
        let Some(value) = self.property_value(&property) else {
            self.log(&format!(
                "recv Get {property:?} -- answering UnknownProperty"
            ));
            let _ = self.conn.send_error(
                &sender,
                msg.serial,
                "org.freedesktop.DBus.Error.UnknownProperty",
                "tray probe",
            );
            return false;
        };
        let sent = self
            .conn
            .send_method_return_with_values(&sender, msg.serial, &[value], "v");
        self.log(&format!("answered Get {property} ok={}", sent.is_ok()));
        true
    }

    fn expect_gets(&mut self, label: &str, want: usize) {
        self.log(&format!("{label}: waiting for {want} property reads..."));
        let mut got = 0;
        while got < want {
            match self.conn.recv_message() {
                Ok(msg) => {
                    if self.answer(&msg) {
                        got += 1;
                    }
                }
                Err(e) => {
                    self.log(&format!("{label}: FAIL, bus recv error: {e:?}"));
                    std::process::exit(2);
                }
            }
        }
        self.log(&format!("{label}: PASS"));
    }

    fn emit(&mut self, member: &str) {
        self.log(&format!("emit {member}"));
        if let Err(e) = self.conn.send_signal(ITEM_PATH, ITEM_IFACE, member, &[]) {
            self.log(&format!("emit {member} failed: {e:?}"));
            std::process::exit(2);
        }
    }

    fn emit_properties_changed(&mut self) {
        self.log("emit PropertiesChanged");
        let values = [
            Value::Str(ITEM_IFACE.into()),
            Value::Array(Vec::new()),
            Value::Array(Vec::new()),
        ];
        if let Err(e) = self.conn.send_signal_with_values(
            ITEM_PATH,
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
            &values,
            "sa{sv}as",
        ) {
            self.log(&format!("emit PropertiesChanged failed: {e:?}"));
            std::process::exit(2);
        }
    }

    fn run(mut self) {
        if let Some(pid) = self.report_watcher_owner() {
            ProcScan::dump_threads_of(pid);
        }
        self.register();
        self.expect_gets("initial read", 2);
        std::thread::sleep(Duration::from_secs(1));

        self.attention = true;
        self.emit("NewStatus");
        self.expect_gets("NewStatus refresh", 2);

        self.emit("NewIcon");
        self.expect_gets("NewIcon refresh", 2);

        self.emit_properties_changed();
        self.expect_gets("PropertiesChanged refresh", 2);

        self.log("all hops PASS -- holding attention state 5s for visual check");
        std::thread::sleep(Duration::from_secs(5));
        self.log("probe done");
    }
}

fn main() {
    TrayProbe::new().run();
}

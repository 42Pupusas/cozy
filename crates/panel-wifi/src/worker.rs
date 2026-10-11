//! The Wi-Fi panel's worker thread.
//!
//! Owns the system-bus D-Bus connection and the `NetworkManager` calls.
//! It sleeps in `poll(2)` on two descriptors: the bus socket, which becomes
//! readable when `NetworkManager` announces a change, and the command
//! eventfd, which becomes readable when the UI thread sends an action. It
//! holds no timer: a scan result, a state transition and a profile change
//! each arrive as a signal and each triggers one fresh snapshot.

use std::thread::JoinHandle;

use dbus::DbusConn;
use dbus::nm_wifi::{NmWifi, WifiPanelSnapshot};
use panel_ui::CommandInbox;
use quetzalcoatl::mpsc::Producer;
use waker::{Poller, Waker};

use crate::model::WifiCommand;

/// The worker thread's state.
pub struct WifiWorker {
    conn: DbusConn,
    snapshots: Producer<WifiPanelSnapshot>,
    inbox: CommandInbox<WifiCommand>,
    wake: Waker,
}

impl WifiWorker {
    /// Spawn the worker; `snapshot_tx` delivers fresh snapshots (and
    /// wakes the main loop), `inbox` carries user actions. Dropping the
    /// matching sender stops it.
    ///
    /// # Panics
    /// Panics if the thread cannot be spawned.
    #[must_use]
    pub fn spawn(
        snapshot_tx: Producer<WifiPanelSnapshot>,
        inbox: CommandInbox<WifiCommand>,
        wake: Waker,
    ) -> JoinHandle<()> {
        std::thread::Builder::new()
            .name("panel-wifi-worker".into())
            .spawn(move || {
                let Ok(conn) = DbusConn::system() else {
                    eprintln!("panel-wifi: system bus unavailable");
                    return;
                };
                Self {
                    conn,
                    snapshots: snapshot_tx,
                    inbox,
                    wake,
                }
                .run();
            })
            .expect("spawn panel-wifi worker")
    }

    fn run(mut self) {
        if let Err(e) = NmWifi::subscribe(&mut self.conn) {
            eprintln!("panel-wifi: cannot subscribe to NetworkManager signals: {e}");
        }
        self.scan();
        self.publish();
        loop {
            self.sleep_until_work();
            let batch = self.inbox.take();
            for command in batch.commands {
                self.execute(command);
            }
            if batch.closed {
                return;
            }
            match self.absorb_signals() {
                Ok(true) => self.publish(),
                Ok(false) => {}
                Err(e) => {
                    eprintln!("panel-wifi: system bus connection lost: {e}");
                    return;
                }
            }
        }
    }

    fn sleep_until_work(&self) {
        if self.conn.has_pending() {
            return;
        }
        let _ = Poller::two(self.conn.raw_fd(), self.inbox.raw_fd()).poll_two(-1);
    }

    /// Read every signal that is already available, without blocking for
    /// more. Returns whether any arrived, so a burst of signals costs one
    /// snapshot rather than one each.
    fn absorb_signals(&mut self) -> Result<bool, dbus::DbusError> {
        let mut arrived = false;
        while self.conn.has_pending() || Poller::one(self.conn.raw_fd()).poll(0) {
            self.conn.recv_message()?;
            arrived = true;
        }
        Ok(arrived)
    }

    fn publish(&mut self) {
        let snapshot = Self::collect(&mut self.conn);
        if self.snapshots.push_block(snapshot).is_ok() {
            self.wake.signal();
        }
    }

    /// One full model read.
    fn collect(conn: &mut DbusConn) -> WifiPanelSnapshot {
        let device = NmWifi::device(conn).ok().flatten();
        let mut snapshot = WifiPanelSnapshot {
            device: device.clone(),
            access_points: Vec::new(),
            saved: Vec::new(),
            active: None,
        };
        let Some(dev) = device else {
            return snapshot;
        };
        snapshot.access_points = NmWifi::access_points(conn, &dev.path).unwrap_or_default();
        snapshot.saved = NmWifi::saved_connections(conn).unwrap_or_default();
        snapshot.active = NmWifi::active_connection(conn, &dev.path).ok().flatten();
        snapshot
    }

    fn scan(&mut self) {
        if let Some(dev) = NmWifi::device(&mut self.conn).ok().flatten() {
            let _ = NmWifi::scan(&mut self.conn, &dev.path);
        }
    }

    fn execute(&mut self, command: WifiCommand) {
        let conn = &mut self.conn;
        match command {
            WifiCommand::Connect { ap, password } => {
                if let Some(device) = NmWifi::device(conn).ok().flatten() {
                    let _ = NmWifi::connect(conn, &device, &ap, password.as_deref());
                }
            }
            WifiCommand::Disconnect => {
                if let Some(dev) = NmWifi::device(conn).ok().flatten() {
                    let _ = NmWifi::disconnect(conn, &dev.path);
                }
            }
            WifiCommand::ActivateSaved { path } => {
                if let Some(device) = NmWifi::device(conn).ok().flatten() {
                    let _ = NmWifi::activate_saved(conn, &device, &path);
                }
            }
            WifiCommand::Scan => self.scan(),
            WifiCommand::Forget { path } => {
                let _ = NmWifi::forget(conn, &path);
            }
        }
    }
}

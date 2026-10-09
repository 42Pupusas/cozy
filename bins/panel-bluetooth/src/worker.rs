//! The Bluetooth panel's worker thread.
//!
//! Owns the system-bus connection and the bluez calls. It sleeps in
//! `poll(2)` on two descriptors: the bus socket, which becomes readable when
//! bluez announces a change, and the command eventfd, which becomes readable
//! when the UI thread sends an action or drops its sender. Each relevant
//! signal triggers one fresh read of the adapter and devices. When the
//! sender is dropped the worker stops any discovery it started and returns.

use std::thread::JoinHandle;

use dbus::DbusConn;
use dbus::bluetooth::{BluetoothAdapter, BluetoothDevice, BluetoothPanel};
use panel_ui::CommandInbox;
use quetzalcoatl::mpsc::Producer;
use waker::{Poller, Waker};

use crate::model::BluetoothCommand;

/// `poll(2)` timeout meaning "wait until a descriptor is ready".
const BLOCK_FOREVER: i32 = -1;

/// One published reading of the adapter and its devices.
pub type BluetoothState = (Option<BluetoothAdapter>, Vec<BluetoothDevice>);

/// The worker thread's state.
pub struct BluetoothWorker {
    conn: DbusConn,
    states: Producer<BluetoothState>,
    inbox: CommandInbox<BluetoothCommand>,
    wake: Waker,
    scanning: bool,
}

impl BluetoothWorker {
    /// Spawn the worker; `state_tx` delivers (adapter, devices) pairs and
    /// wakes the main loop, `inbox` carries user actions. Dropping the
    /// matching sender and joining the returned handle shuts the worker
    /// down after it has stopped discovery.
    ///
    /// # Panics
    /// Panics if the thread cannot be spawned.
    #[must_use]
    pub fn spawn(
        state_tx: Producer<BluetoothState>,
        inbox: CommandInbox<BluetoothCommand>,
        wake: Waker,
    ) -> JoinHandle<()> {
        std::thread::Builder::new()
            .name("panel-bluetooth-worker".into())
            .spawn(move || {
                let Ok(conn) = DbusConn::system() else {
                    eprintln!("panel-bluetooth: system bus unavailable");
                    return;
                };
                Self {
                    conn,
                    states: state_tx,
                    inbox,
                    wake,
                    scanning: false,
                }
                .run();
            })
            .expect("spawn panel-bluetooth worker")
    }

    fn run(mut self) {
        if let Err(e) = BluetoothPanel::subscribe(&mut self.conn) {
            eprintln!("panel-bluetooth: cannot subscribe to bluez signals: {e}");
        }
        self.power_on_and_scan();
        self.publish();
        loop {
            self.sleep_until_work();
            let batch = self.inbox.take();
            for command in batch.commands {
                self.execute(command);
            }
            if batch.closed {
                self.stop_scanning();
                return;
            }
            match self.absorb_signals() {
                Ok(true) => self.publish(),
                Ok(false) => {}
                Err(e) => {
                    eprintln!("panel-bluetooth: system bus connection lost: {e}");
                    return;
                }
            }
        }
    }

    /// Power the adapter on open when it is off (nothing else in the panel
    /// works otherwise) and start discovery immediately.
    fn power_on_and_scan(&mut self) {
        let Ok(Some(mut adapter)) = BluetoothPanel::adapter(&mut self.conn) else {
            return;
        };
        if !adapter.powered {
            adapter.powered =
                BluetoothPanel::set_powered(&mut self.conn, &adapter.path, true).is_ok();
        }
        if adapter.powered {
            self.set_scanning(&adapter.path, true);
        }
    }

    fn sleep_until_work(&self) {
        if self.conn.has_pending() {
            return;
        }
        let _ = Poller::two(self.conn.raw_fd(), self.inbox.raw_fd()).poll_two(BLOCK_FOREVER);
    }

    fn set_scanning(&mut self, adapter_path: &str, on: bool) {
        if BluetoothPanel::set_discovering(&mut self.conn, adapter_path, on).is_ok() {
            self.scanning = on;
        }
    }

    fn stop_scanning(&mut self) {
        if !self.scanning {
            return;
        }
        if let Some(adapter) = BluetoothPanel::adapter(&mut self.conn).ok().flatten() {
            self.set_scanning(&adapter.path, false);
        }
    }

    /// Read every message that is already available, without blocking for
    /// more. Returns whether any of them changed what the panel shows, so a
    /// burst of signals costs one read rather than one each.
    fn absorb_signals(&mut self) -> Result<bool, dbus::DbusError> {
        let mut changed = false;
        while self.conn.has_pending() || Poller::one(self.conn.raw_fd()).poll(0) {
            changed |= BluetoothPanel::changes_panel_state(&self.conn.recv_message()?);
        }
        Ok(changed)
    }

    fn publish(&mut self) {
        let adapter = BluetoothPanel::adapter(&mut self.conn).ok().flatten();
        let devices = BluetoothPanel::devices(&mut self.conn).unwrap_or_default();
        let _ = self.states.push_block((adapter, devices));
        self.wake.signal();
    }

    fn execute(&mut self, command: BluetoothCommand) {
        let conn = &mut self.conn;
        match command {
            BluetoothCommand::SetPowered { on } => {
                if let Some(a) = BluetoothPanel::adapter(conn).ok().flatten() {
                    let _ = BluetoothPanel::set_powered(conn, &a.path, on);
                }
            }
            BluetoothCommand::SetScanning { on } => {
                if let Some(a) = BluetoothPanel::adapter(conn).ok().flatten() {
                    self.set_scanning(&a.path, on);
                }
            }
            BluetoothCommand::Connect { path } => {
                let _ = BluetoothPanel::connect(conn, &path);
            }
            BluetoothCommand::Disconnect { path } => {
                let _ = BluetoothPanel::disconnect(conn, &path);
            }
            BluetoothCommand::Pair { path } => {
                if BluetoothPanel::pair(conn, &path).is_ok() {
                    let _ = BluetoothPanel::connect(conn, &path);
                }
            }
            BluetoothCommand::Forget { path } => {
                if let Some(a) = BluetoothPanel::adapter(conn).ok().flatten() {
                    let _ = BluetoothPanel::remove(conn, &a.path, &path);
                }
            }
        }
    }
}

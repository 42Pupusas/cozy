//! `panel-bluetooth` — the Bluetooth statusbar panel: adapter power,
//! discovery, and a pair/connect/disconnect device list, driven by bluez
//! over the workspace's own D-Bus client, with an auto-accept
//! `org.bluez.Agent1` served while the panel is open (headphones and
//! mice pair `JustWorks`; the agent answers everything so no pairing
//! flow can hang).
//!
//! Same two-thread split as the bar and `panel-wifi`: the main thread
//! owns the [`PopupWindow`](panel_ui::PopupWindow) Wayland pump; the
//! worker thread owns the blocking bluez round trips. The agent runs on
//! a third, dedicated connection (a D-Bus server loop blocks on
//! `recv_message`, so it can't share the worker's call connection).

pub mod agent;
pub mod model;
pub mod view;
pub mod worker;

pub use model::{BluetoothModel, PANEL_HEIGHT, PANEL_WIDTH};

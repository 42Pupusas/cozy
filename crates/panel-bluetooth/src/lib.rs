//! `panel-bluetooth` — the Bluetooth statusbar panel: adapter power,
//! discovery, and a pair/connect/disconnect device list, driven by bluez
//! over the workspace's own D-Bus client, with an auto-accept
//! `org.bluez.Agent1` served while the panel is open (headphones and
//! mice pair `JustWorks`; the agent answers everything so no pairing
//! flow can hang).
//!
//! The bar hosts [`BluetoothPopupPanel`] in a popup. The worker thread owns the
//! blocking bluez round trips; the agent runs on a third, dedicated
//! connection (a D-Bus server loop blocks on `recv_message`, so it can't
//! share the worker's call connection).

pub mod agent;
pub mod input;
pub mod model;
pub mod panel;
pub mod view;
pub mod worker;

pub use model::{BluetoothModel, PANEL_HEIGHT, PANEL_WIDTH};
pub use panel::BluetoothPopupPanel;

//! `panel-wifi` — the Wi-Fi statusbar panel: a scan-and-connect popup
//! driven by `NetworkManager` over the workspace's own D-Bus client.
//!
//! The bar hosts [`WifiPanel`] in a popup on its own thread; the worker
//! thread owns the D-Bus connection, sleeps until `NetworkManager` signals
//! a change or the panel sends a command, and ships snapshots back.

pub mod input;
pub mod model;
pub mod panel;
pub mod view;
pub mod worker;

pub use model::{PANEL_HEIGHT, PANEL_WIDTH, WifiModel};
pub use panel::WifiPanel;

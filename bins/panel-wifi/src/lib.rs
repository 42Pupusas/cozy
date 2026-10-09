//! `panel-wifi` — the Wi-Fi statusbar panel: a scan-and-connect popup
//! driven by `NetworkManager` over the workspace's own D-Bus client.
//!
//! Layout mirrors the bar's own two-thread split (see
//! `bins/statusbar/src/main.rs`'s module doc): the main thread owns the
//! [`PopupWindow`](panel_ui::PopupWindow) Wayland pump and never blocks;
//! the worker thread owns the D-Bus connection, sleeps until
//! `NetworkManager` signals a change or the UI sends a command, and ships
//! [`WifiPanelSnapshot`]s back.

pub mod model;
pub mod view;
pub mod worker;

pub use model::{PANEL_HEIGHT, PANEL_WIDTH, WifiModel};

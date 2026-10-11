//! `panel-ui` — what the statusbar's panels (Wi-Fi, Bluetooth, volume)
//! share: the [`Panel`] trait the bar hosts them through, the widget set
//! (rows, sliders, toggles) they paint with, and the command channel to
//! their worker threads.
//!
//! Pure presentation: this crate knows nothing about D-Bus,
//! `NetworkManager`, bluez, or `pactl`, and nothing about Wayland surfaces;
//! the bar owns the popup each panel paints into.

pub mod command_channel;
pub mod panel;
pub mod widgets;
pub mod worker_link;

pub use command_channel::{CommandBatch, CommandChannel, CommandInbox, CommandSender};
pub use panel::{Panel, PanelFlow};
pub use statusbar::PanelKey;
pub use widgets::{Metrics, Row, RowContent, Slider, Theme, Toggle};
pub use worker_link::WorkerLink;

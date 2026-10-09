//! `panel-ui` — the shared popup backend for the statusbar's clickable
//! widgets (Wi-Fi, Bluetooth, volume): a [`PopupWindow`] layer-shell
//! surface opening below the clicked bar module, plus the widget set
//! (rows, sliders, toggles) every panel paints it with.
//!
//! Pure presentation: this crate knows nothing about D-Bus,
//! `NetworkManager`, bluez, or `pactl`. Each panel binary owns its model,
//! drives a [`PopupWindow`] through the [`Panel`] trait, and paints with
//! the widgets here.

pub mod args;
pub mod command_channel;
pub mod placement;
pub mod popup_window;
pub mod widgets;

pub use args::{PanelArgs, ResolvedPanel};
pub use command_channel::{CommandBatch, CommandChannel, CommandInbox, CommandSender};
pub use placement::{Anchored, ModuleAnchor};
pub use popup_window::{PanelKey, PopupWindow};
pub use widgets::{Metrics, Row, RowContent, Slider, Theme, Toggle};

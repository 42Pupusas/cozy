//! `statusbar` — a Wayland status bar built on the same `io_uring` Wayland
//! connection, wire encoder and software-canvas primitives as the
//! `terminal` binary's `platform` crate, docked via `zwlr_layer_shell_v1`
//! instead of `xdg_wm_base`.
//!
//! Module layout:
//!   * [`bar_window`] — the layer-shell window: connection setup, resize/
//!     configure handling, double-buffered shm present loop.
//!   * the individual status widgets (each a marker-type owner like
//!     [`volume::Volume`], not a free function -- see each module's doc for
//!     why): [`clock`], [`battery`], [`cpu`], [`memory`], [`temperature`],
//!     [`wifi`], [`bluetooth`], [`brightness`], [`volume`]. Each produces a
//!     plain string (or a few) the bar lays out left/center/right and draws
//!     with the embedded bitmap font.

pub mod bar_window;
pub mod battery;
pub mod bluetooth;
pub mod brightness;
pub mod click;
pub mod clock;
pub mod colors;
pub mod cpu;
pub mod localtime;
pub mod memory;
pub mod monitor;
pub mod notifications;
mod pointer_state;
pub mod scroll;
mod sys_file;
pub mod temperature;
pub mod text;
pub mod tray;
pub mod tray_icon;
pub mod volume;
pub mod wifi;

pub use bar_window::{BarWindow, Edge};
pub use battery::Battery;
pub use bluetooth::Bluetooth;
pub use brightness::Brightness;
pub use clock::Clock;
pub use colors::ModuleKind;
pub use cpu::Cpu;
pub use memory::Memory;
pub use monitor::{MonitorReader, MonitorSample, ProcessInfo};
pub use notifications::{NotificationSegment, Notifications};
pub use scroll::{ScrollAccumulator, steps_to_delta_pct};
pub use temperature::Temperature;
pub use text::TextRenderer;
pub use tray::{TrayItem, TraySnapshot, TrayWatcher};
pub use tray_icon::{TrayIcon, TrayPixmap};
pub use volume::Volume;
pub use wifi::Wifi;

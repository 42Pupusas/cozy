#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::doc_markdown,
    clippy::option_if_let_else,
    clippy::too_many_lines,
    clippy::needless_pass_by_value,
    clippy::collapsible_if,
    clippy::unnecessary_wraps,
    clippy::missing_const_for_fn,
    clippy::use_self,
    clippy::struct_excessive_bools,
    clippy::or_fun_call,
    clippy::map_unwrap_or,
    clippy::too_long_first_doc_paragraph
)]
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
pub mod notifications;
mod pointer_state;
pub mod scroll;
mod sys_file;
pub mod temperature;
pub mod text;
pub mod volume;
pub mod wifi;

pub use bar_window::{BarWindow, Edge};
pub use battery::Battery;
pub use bluetooth::Bluetooth;
pub use brightness::Brightness;
pub use click::settings_page;
pub use clock::Clock;
pub use colors::{ModuleKind, module_color};
pub use cpu::Cpu;
pub use memory::Memory;
pub use notifications::NotificationSegment;
pub use scroll::{ScrollAccumulator, steps_to_delta_pct};
pub use temperature::Temperature;
pub use text::TextRenderer;
pub use volume::Volume;
pub use wifi::Wifi;

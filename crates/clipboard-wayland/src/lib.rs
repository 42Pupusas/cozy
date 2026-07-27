//! Wayland transport for clipboard ownership and observation.
//!
//! `crates/clipboard` owns the rules — which MIME types matter, what history
//! retains, when an offer is a duplicate — and knows nothing about Wayland.
//! This crate is the other half: the `zwlr_data_control_unstable_v1` plumbing
//! those rules are driven by. The split keeps every decision headlessly
//! testable while confining "needs a live compositor" to one place.
//!
//! Data-control rather than core `wl_data_device` throughout, because both
//! consumers here (a screenshot keybind, a background history daemon) act
//! without keyboard focus and therefore have no input serial to present.

mod clipboard_session;
mod mime_reader;
mod selection_watcher;

pub use clipboard_session::ClipboardSession;
pub use mime_reader::MimeReader;
pub use selection_watcher::{SelectionWatcher, WatcherLoop};

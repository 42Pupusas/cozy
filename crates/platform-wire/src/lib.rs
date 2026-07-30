//! The Wayland wire protocol, decoupled from the connection that speaks it.
//!
//! Audit finding #3 (god-crate), second cut: after `sys.rs` (raw
//! syscalls) came out as `platform-sys`, this is the next self-contained
//! seam in `platform` -- typed object ids, the interface/opcode tables, and
//! the byte-level request/event codec. All three (`id`, `protocol`, `wire`)
//! reference only each other and `std`: no I/O, no `crate::conn`, no
//! rendering. They were already a closed dependency subgraph inside
//! `platform`, just not drawn as a crate boundary.
//!
//! Re-exported from `platform` as `platform::{id, protocol, wire}` (see that
//! crate's `lib.rs`), so every existing call site -- inside `platform`
//! itself and in the half-dozen external crates that speak Wayland directly
//! (`statusbar`, `clipboard-wayland`, `screencopy`, `bins/shot`,
//! `bins/wallpaper`, `bins/launcher`, `bins/terminal`) -- keeps compiling
//! unchanged.

pub mod id;
pub mod protocol;
pub mod wire;

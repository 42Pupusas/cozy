//! `crates/platform` — Wayland window + software terminal renderer.
//!
//! Adapted from the `paint` scaffold:
//!   * Wayland wire protocol via `ququmatz` `io_uring`
//!   * Lock-free SPSC event ring via `quetzalcoatl`
//!   * Software framebuffer over `wl_shm`
//!
//! Terminal-specific additions:
//!   * Consumes [`DirtySnapshot`]s from `crates/grid` and blits them onto the
//!     framebuffer using the embedded 8×16 bitmap font from `paint`.
//!   * Key events produce `InputEvent`s destined for `crates/input` (M5).

mod clipboard;
pub mod conn;
pub mod cursor;
pub mod cursor_anim;
pub mod cursor_gate;
mod debug_overlay;
pub mod draw;
pub mod error;
pub mod event;
pub mod file_uri;
pub mod font;
pub mod id;
pub mod key_repeat;
pub mod keymap;
pub mod layer;
pub mod link_scan;
pub mod pixel_buffers;
pub mod premultiply;
pub mod present;
pub mod progress_guard;
pub mod protocol;
pub mod reader_health;
pub mod round_rect;
pub mod surface_format;
pub mod wire;
pub mod xcursor;

/// Raw syscalls and libc FFI live in their own zero-`crate::`-dependency
/// crate (an audit-driven split off this one, the first cut along the
/// sys/wayland/render seam -- see `platform_sys`'s doc comment). Re-exported
/// under the name every existing `crate::sys::X` / `platform::sys::X` call
/// site already uses, so the split is invisible from the outside.
pub use platform_sys as sys;

mod render;
pub mod selection;
pub mod surface;
pub mod text;
pub mod toplevel;
pub mod window;

pub use cursor_anim::{CursorAnimConfig, CursorAnimator};
pub use error::{Error, Result};
pub use premultiply::Premultiplier;
pub use progress_guard::{Progress, ProgressGuard};
pub use reader_health::{ReaderHealth, ReaderState};
pub use render::{Renderer, Theme};
pub use surface::{Frame, Routed};
pub use surface_format::SurfaceFormat;
pub use waker::Waker;
pub use window::{InputEvent, TermWindow};

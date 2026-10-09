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
pub mod error;
pub mod event;
pub mod file_uri;
pub mod frame_carry;
pub mod input_serial;
pub mod key_repeat;
pub mod keyboard_decoder;
pub mod keymap;
pub mod layer;
pub mod link_scan;
pub mod multi_click;
pub mod output_probe;
pub mod premultiply;
pub mod present;
pub mod progress_guard;
pub mod reader_health;
pub mod row_band;
pub mod xcursor;

/// Raw syscalls and libc FFI live in their own zero-`crate::`-dependency
/// crate (an audit-driven split off this one, the first cut along the
/// sys/wayland/render seam -- see `platform_sys`'s doc comment). Re-exported
/// under the name every existing `crate::sys::X` / `platform::sys::X` call
/// site already uses, so the split is invisible from the outside.
pub use platform_sys as sys;

/// Typed Wayland object ids, the interface/opcode tables, and the wire
/// codec -- the second cut along the sys/wayland/render seam (see
/// `platform_wire`'s doc comment). Re-exported as three names,
/// `platform::id` / `platform::protocol` / `platform::wire`, matching what
/// every existing call site (inside this crate and out) already writes.
pub use platform_wire::id;
pub use platform_wire::protocol;
pub use platform_wire::wire;

/// The software rasterizer -- pixels, shapes, and text -- the third cut
/// along the sys/wayland/render seam (see `platform_draw`'s doc comment).
/// Re-exported as `platform::draw` / `platform::round_rect` /
/// `platform::surface_format` / `platform::font`, matching what every
/// existing call site (inside this crate and out) already writes.
pub use platform_draw::draw;
pub use platform_draw::font;
pub use platform_draw::round_rect;
pub use platform_draw::surface_format;
pub use platform_draw::text;

mod render;
pub mod scene;
pub mod selection;
pub mod surface;
pub mod toplevel;
pub mod window;
pub mod word_select;

pub use cursor_anim::{CursorAnimConfig, CursorAnimator};
pub use error::{PlatformError, Result};
pub use premultiply::Premultiplier;
pub use progress_guard::{Progress, ProgressGuard};
pub use reader_health::{ReaderHealth, ReaderState};
pub use render::{Renderer, Theme};
pub use surface::{Frame, Routed};
pub use surface_format::SurfaceFormat;
pub use text::GlyphBlit;
pub use waker::Waker;
pub use window::{InputEvent, TermWindow};

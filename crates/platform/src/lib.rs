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
mod debug_overlay;
pub mod draw;
pub mod error;
pub mod event;
pub mod font;
pub mod id;
mod key_repeat;
pub mod keymap;
pub mod present;
pub mod protocol;
pub mod sys;
pub mod wire;
pub mod xcursor;

mod render;
pub mod selection;
pub mod surface;
pub mod text;
pub mod window;

pub use cursor_anim::{CursorAnimConfig, CursorAnimator};
pub use error::{Error, Result};
pub use render::{Renderer, Theme};
pub use surface::{Frame, Routed};
pub use waker::Waker;
pub use window::{InputEvent, TermWindow};

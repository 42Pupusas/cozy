// Platform code is largely vendored from `paint` — suppress lint noise en-masse.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::cast_lossless,
    clippy::option_if_let_else,
    clippy::too_many_lines,
    clippy::needless_pass_by_value,
    clippy::collapsible_if,
    clippy::unnecessary_wraps,
    clippy::doc_markdown,
    clippy::many_single_char_names,
    clippy::unreadable_literal,
    clippy::missing_const_for_fn,
    clippy::must_use_candidate,
    clippy::use_self,
    clippy::return_self_not_must_use,
    clippy::ptr_as_ptr,
    clippy::too_long_first_doc_paragraph,
    clippy::match_same_arms,
    clippy::items_after_statements,
    clippy::single_match_else,
    clippy::redundant_closure,
    clippy::needless_return,
    clippy::unused_self,
    clippy::needless_pass_by_ref_mut,
    clippy::trivially_copy_pass_by_ref,
    clippy::similar_names,
    clippy::items_after_test_module,
    clippy::non_send_fields_in_send_ty,
    clippy::used_underscore_binding,
    clippy::borrow_as_ptr,
    clippy::no_effect_underscore_binding,
    clippy::struct_field_names,
    clippy::ptr_cast_constness,
    clippy::as_ptr_cast_mut,
    clippy::cast_ptr_alignment
)]
//! `crates/platform` — Wayland window + software terminal renderer.
//!
//! Adapted from the `paint` scaffold:
//!   * Wayland wire protocol via `ququmatz` io_uring
//!   * Lock-free SPSC event ring via `quetzalcoatl`
//!   * Software framebuffer over `wl_shm`
//!
//! Terminal-specific additions:
//!   * Consumes [`DirtySnapshot`]s from `crates/grid` and blits them onto the
//!     framebuffer using the embedded 8×16 bitmap font from `paint`.
//!   * Key events produce `InputEvent`s destined for `crates/input` (M5).

pub mod conn;
pub mod cursor;
pub mod cursor_anim;
pub mod draw;
pub mod error;
pub mod event;
pub mod font;
pub mod id;
pub mod keymap;
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

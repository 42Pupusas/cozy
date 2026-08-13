//! A generic xdg-shell toplevel window: a real application window with a
//! titlebar, resizable by the user, that draws whatever its owner paints.
//!
//! # Why this exists alongside `TermWindow` and `LauncherWindow`
//!
//! The crate already has two windows, and neither is reusable:
//!
//! * [`crate::window::TermWindow`] *is* an xdg-shell toplevel, but it is
//!   fused to the terminal. Its configure path converts pixels into rows and
//!   columns, it owns a grid renderer, a cell mirror, selection tracking, and
//!   a cursor animator. A file browser wants none of that.
//! * `statusbar::BarWindow` is `wlr-layer-shell` -- an *overlay* role for
//!   bars and panels. It has no titlebar, cannot be resized by the user, and
//!   on some compositors cannot be focused normally. Wrong role for an app.
//!
//! So this module owns the third thing: the toplevel role with **no opinion
//! about content**. The owner supplies a painter; we supply a correctly
//! mapped, correctly resized surface to paint into.
//!
//! # Relationship to `TermWindow` (deliberate, temporary duplication)
//!
//! `TermWindow` is not being refactored onto this yet, and that is a
//! considered choice rather than an oversight: it is the most load-bearing
//! code in the workspace, and rebuilding its foundation to serve a window
//! that does not exist yet risks a working terminal for a speculative gain.
//! The convergence is planned once this type has proven itself -- at which
//! point `TermWindow` keeps its grid/cursor/selection logic and delegates
//! surface, geometry, and lifecycle here.
//!
//! What is *not* duplicated: everything already role-agnostic in
//! [`crate::surface`] (connection bootstrap, shm swap chain, registry
//! discovery, the common event routing) is reused as-is.

pub mod activation;
pub mod drag;
pub mod geometry;
pub mod globals;
pub mod keyboard;
pub mod keysym;
pub mod lifecycle;
pub mod modifiers;
pub mod pointer;
pub mod shell;
pub mod window;

pub use activation::{Activation, ActivationToken, TOKEN_ENV};
pub use drag::{DragController, Dropped};
pub use geometry::{Geometry, LogicalSize, PhysicalSize};
pub use globals::{ToplevelGlobals, ToplevelGlobalsBuilder};
pub use keyboard::{KeyPress, KeyboardState, RepeatInfo};
pub use keysym::Key;
pub use lifecycle::{Lifecycle, Phase};
pub use modifiers::ModifierState;
pub use pointer::{PointerEventKind, PointerState, Position};
pub use shell::ShellSurface;
pub use window::{Painter, Toplevel, WindowEvent};

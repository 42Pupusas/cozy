//! A small widget set for this workspace's Wayland clients.
//!
//! Explicitly **not** a general GUI toolkit. `platform::draw::Canvas` gives us
//! `fill_rect` and `draw_text`; everything above that -- a scrollable list, a
//! text input, a scrollbar -- did not exist, and `bins/launcher` and
//! `bins/statusbar` each hand-rolled their own fixed-size, non-scrolling
//! version. This crate is the shared answer to that duplication, scoped to the
//! handful of widgets `bins/files` actually needs.
//!
//! ### Structure
//! Each widget is split into a headless *geometry* type and (later) a thin
//! painting seam:
//!
//! - [`ListViewport`] -- scroll offsets, visible ranges, hit-testing.
//! - [`ListSelection`] -- cursor, anchor, and range selection rules.
//! - [`Scrollbar`] -- thumb geometry and drag, driven by a `ListViewport`.
//! - [`TextInput`] -- a single-line editable field with a caret.
//! - [`ListPane`] -- the three list widgets composed, owning the rules that
//!   span them (a moved cursor must stay on screen; a shrunken list must not
//!   leave a stale highlight).
//!
//! The geometry types have no Wayland or rendering dependencies at all, which
//! is what lets the fiddly cases (empty lists, a resize that strands the
//! scroll offset, 100k-row overflow) be tested in microseconds without a
//! compositor.

mod command;
mod cursor_request;
mod cursor_shape;
mod double_click;
mod drag_arm;
mod monotonic_clock;
mod pane;
mod scrollbar;
mod selection;
mod text_input;
mod viewport;

pub use command::{Direction, Edge, ListCommand, Mods, PaneResponse};
pub use cursor_request::CursorRequest;
pub use cursor_shape::CursorShape;
pub use double_click::DoubleClick;
pub use drag_arm::DragArm;
pub use monotonic_clock::MonotonicClock;
pub use pane::ListPane;
pub use scrollbar::{Scrollbar, Thumb};
pub use selection::ListSelection;
pub use text_input::TextInput;
pub use viewport::ListViewport;

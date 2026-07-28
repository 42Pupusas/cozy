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
//!
//! The geometry types have no Wayland or rendering dependencies at all, which
//! is what lets the fiddly cases (empty lists, a resize that strands the
//! scroll offset, 100k-row overflow) be tested in microseconds without a
//! compositor.

mod selection;
mod viewport;

pub use selection::ListSelection;
pub use viewport::ListViewport;

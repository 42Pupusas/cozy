//! What the bar needs from a panel it hosts in a popup.
//!
//! A panel owns its model, its worker thread and its hit-test rects. The
//! bar owns the surface, the font, the theme and the input routing.
//! Dropping a panel closes its command channel, which stops its worker.

use platform::draw::{Canvas, Point};
use statusbar::{PanelKey, TextRenderer};

use crate::widgets::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelFlow {
    Stay,
    Close,
}

/// `Send` so the bar can drop a closed panel, and join its worker, off
/// the render thread.
pub trait Panel: Send {
    /// Popup size in logical pixels; fixed for the panel's lifetime.
    fn size(&self) -> (i32, i32);

    /// Apply every update the worker has sent; `true` when a repaint is owed.
    fn drain(&mut self) -> bool;

    fn paint(&mut self, canvas: &mut Canvas, text: &mut TextRenderer, theme: &Theme);

    fn key(&mut self, key: PanelKey) -> PanelFlow;

    /// A left click at `at`, popup-local.
    fn click(&mut self, at: Point);

    /// Accumulated wheel pixels at `at`, popup-local; positive is down.
    fn scroll(&mut self, _at: Point, _delta: i32) {}
}

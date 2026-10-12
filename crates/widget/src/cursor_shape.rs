//! [`CursorShape`] -- the pointer image a region asks for.
//!
//! Names follow the CSS/Xcursor vocabulary so a window can map them onto
//! theme cursor files with `Toplevel::set_cursor`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorShape {
    /// Nothing special: the plain arrow.
    Default,
    /// Over a movable body.
    Move,
    /// Over a text region.
    Text,
    /// Panning the canvas itself.
    Grab,
    /// Mid-pan, button held.
    Grabbing,
    /// Diagonal resize, top-left <-> bottom-right.
    ResizeNwse,
    /// Diagonal resize, top-right <-> bottom-left.
    ResizeNesw,
    /// Horizontal resize.
    ResizeEw,
    /// Vertical resize.
    ResizeNs,
    /// Crosshair, for drawing a new shape.
    Crosshair,
}

impl CursorShape {
    pub const ALL: [Self; 10] = [
        Self::Default,
        Self::Move,
        Self::Text,
        Self::Grab,
        Self::Grabbing,
        Self::ResizeNwse,
        Self::ResizeNesw,
        Self::ResizeEw,
        Self::ResizeNs,
        Self::Crosshair,
    ];

    /// A stable identifier for this shape, used as the cursor cache key.
    ///
    /// Distinct from the theme names: those vary by theme, this never does.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Move => "move",
            Self::Text => "text",
            Self::Grab => "grab",
            Self::Grabbing => "grabbing",
            Self::ResizeNwse => "nwse",
            Self::ResizeNesw => "nesw",
            Self::ResizeEw => "ew",
            Self::ResizeNs => "ns",
            Self::Crosshair => "crosshair",
        }
    }

    /// The Xcursor theme names this shape may be published under, in
    /// preference order. Themes disagree about which alias they ship, so
    /// the loader tries each in turn.
    #[must_use]
    pub const fn theme_names(self) -> &'static [&'static str] {
        match self {
            Self::Default => &["left_ptr", "default", "arrow"],
            Self::Move => &["fleur", "move", "all-scroll"],
            Self::Text => &["xterm", "text", "ibeam"],
            Self::Grab => &["openhand", "grab", "hand1"],
            Self::Grabbing => &["closedhand", "grabbing", "hand2"],
            Self::ResizeNwse => &["nwse-resize", "size_fdiag", "bottom_right_corner"],
            Self::ResizeNesw => &["nesw-resize", "size_bdiag", "bottom_left_corner"],
            Self::ResizeEw => &["ew-resize", "sb_h_double_arrow", "right_side"],
            Self::ResizeNs => &["ns-resize", "sb_v_double_arrow", "bottom_side"],
            Self::Crosshair => &["crosshair", "cross", "tcross"],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CursorShape;

    #[test]
    fn every_shape_has_a_distinct_cache_key() {
        let mut keys: Vec<&str> = CursorShape::ALL.iter().map(|s| s.key()).collect();
        keys.sort_unstable();
        let before = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), before, "two cursor shapes share a cache key");
    }

    #[test]
    fn every_cursor_shape_offers_at_least_one_theme_name() {
        for shape in CursorShape::ALL {
            assert!(!shape.theme_names().is_empty(), "{shape:?} has no names");
        }
    }
}

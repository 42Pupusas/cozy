//! [`Frame`] -- the window's top-level geometry: where the two main columns
//! and every band inside them begin and end.
//!
//! The guide's shell is `grid-template-columns: 210px 1fr` with a full-height
//! sidebar, and the toolbar, content and status strip nested *inside* the
//! right column. That is the whole reason this module exists: the obvious
//! reading of a file manager is three horizontal bands -- toolbar across the
//! top, status across the bottom, panes in between -- and painting it that
//! way boxes the sidebar in on both ends, which is exactly the layout bug
//! this type was introduced to kill. Encoding the nesting once, in a tested
//! type, means the painter cannot quietly drift back to bands: every band it
//! draws asks the frame for its rectangle, and the frame is what the tests
//! assert against.
//!
//! Everything here is pure arithmetic over a window size, so the entire
//! layout is testable without a compositor.

use platform::draw::Rect;

use crate::spacing::Spacing;

/// The window's resolved layout: two columns, and the bands within them.
///
/// Built per paint from the window size. Cheap enough to rebuild (a handful
/// of additions) that caching it would only create a chance for it to go
/// stale against a resize.
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    /// The full window.
    size: Rect,
    /// Width of the left column, already collapsed to a rail if narrow, or
    /// zero when the sidebar is hidden entirely.
    sidebar_width: i32,
    /// Height of the sidebar's storage footer, or zero when there is no room
    /// for it.
    storage_height: i32,
    /// Height of the toolbar band at the top of the right column.
    toolbar_height: i32,
    /// Height of the column-header band under the toolbar.
    header_height: i32,
    /// Height of the status band at the bottom of the right column.
    status_height: i32,
    /// Horizontal inset applied to content inside the right column.
    inset: i32,
}

impl Frame {
    /// Resolve the layout for a window of `width` x `height`.
    ///
    /// `sidebar_visible` is the user's toggle; the *rail* collapse below the
    /// guide's threshold is decided here rather than by the caller, so the
    /// two ways a sidebar can be narrow stay in one place.
    #[must_use]
    pub fn new(
        width: i32,
        height: i32,
        sidebar_visible: bool,
        spacing: Spacing,
        toolbar_height: i32,
        status_height: i32,
        column_header_height: i32,
    ) -> Self {
        let width = width.max(0);
        let height = height.max(0);
        let compact = width < spacing.collapse_below();
        let sidebar_width = if !sidebar_visible {
            0
        } else if compact {
            spacing.rail_width()
        } else {
            spacing.sidebar_width()
        }
        // A sidebar wider than the window would leave the list at a negative
        // width; the list wins the tie because a file manager with no file
        // list is useless, while one with no sidebar still works.
        .min(width);

        // The storage footer is the first thing sacrificed when the window is
        // short: it is ambient information, whereas the places above it are
        // how the user navigates. It is also hidden on the rail, where there
        // is no width for either the device name or its percentage.
        let storage = spacing.storage_height();
        let sidebar_body = height.saturating_sub(storage);
        let storage_height =
            if compact || sidebar_width == 0 || sidebar_body < spacing.sidebar_row() {
                0
            } else {
                storage
            };

        // Chrome that does not fit is dropped from the bottom up, so a window
        // squeezed to nothing still shows a toolbar telling you where you are
        // rather than a slice of table with no context.
        let toolbar_height = toolbar_height.min(height);
        let remaining = height - toolbar_height;
        let status_height = status_height.min(remaining);
        let remaining = remaining - status_height;
        // Passed in rather than read off `spacing`, for the same reason the
        // toolbar and status heights are: those bands are density-scaled by
        // `Theme`, and a column-header strip that took the guide's raw height
        // would be the one band out of step with the rows beneath it.
        let header_height = column_header_height.min(remaining);

        Self {
            size: Rect::new(0, 0, width, height),
            sidebar_width,
            storage_height,
            toolbar_height,
            header_height,
            status_height,
            inset: if compact { spacing.m() } else { spacing.l() },
        }
    }

    /// Whether the sidebar occupies any width at all.
    #[must_use]
    pub const fn has_sidebar(&self) -> bool {
        self.sidebar_width > 0
    }

    /// The left column, full window height.
    ///
    /// Full height is the point: it runs past the toolbar's top and the
    /// status strip's bottom, because in the guide those bands belong to the
    /// right column only.
    #[must_use]
    pub const fn sidebar(&self) -> Rect {
        Rect::new(0, 0, self.sidebar_width, self.size.h)
    }

    /// The sidebar's scrolling list of places, above the storage footer.
    #[must_use]
    pub const fn sidebar_body(&self) -> Rect {
        Rect::new(0, 0, self.sidebar_width, self.size.h - self.storage_height)
    }

    /// The sidebar's disk-usage footer, pinned to the bottom of the column.
    ///
    /// `None` when it was dropped for want of space, so a caller cannot
    /// accidentally paint a zero-height widget's border.
    #[must_use]
    pub const fn storage(&self) -> Option<Rect> {
        if self.storage_height == 0 {
            return None;
        }
        Some(Rect::new(
            0,
            self.size.h - self.storage_height,
            self.sidebar_width,
            self.storage_height,
        ))
    }

    /// The right column: everything beside the sidebar.
    #[must_use]
    pub const fn main(&self) -> Rect {
        Rect::new(
            self.sidebar_width,
            0,
            self.size.w - self.sidebar_width,
            self.size.h,
        )
    }

    /// The toolbar band, at the top of the right column only.
    #[must_use]
    pub const fn toolbar(&self) -> Rect {
        Rect::new(
            self.sidebar_width,
            0,
            self.size.w - self.sidebar_width,
            self.toolbar_height,
        )
    }

    /// The column-header band, under the toolbar.
    #[must_use]
    pub const fn column_headers(&self) -> Rect {
        Rect::new(
            self.sidebar_width,
            self.toolbar_height,
            self.size.w - self.sidebar_width,
            self.header_height,
        )
    }

    /// The scrolling file rows, between the headers and the status strip.
    #[must_use]
    pub const fn rows(&self) -> Rect {
        let top = self.toolbar_height + self.header_height;
        let bottom = self.size.h - self.status_height;
        Rect::new(
            self.sidebar_width,
            top,
            self.size.w - self.sidebar_width,
            if bottom > top { bottom - top } else { 0 },
        )
    }

    /// The status band, at the bottom of the right column only.
    #[must_use]
    pub const fn status(&self) -> Rect {
        Rect::new(
            self.sidebar_width,
            self.size.h - self.status_height,
            self.size.w - self.sidebar_width,
            self.status_height,
        )
    }

    /// The horizontal inset for content inside the right column.
    #[must_use]
    pub const fn inset(&self) -> i32 {
        self.inset
    }

    /// The x coordinate where inset content in the right column begins.
    #[must_use]
    pub const fn content_x(&self) -> i32 {
        self.sidebar_width + self.inset
    }

    /// The width available to inset content in the right column.
    #[must_use]
    pub const fn content_width(&self) -> i32 {
        let width = self.size.w - self.sidebar_width - self.inset * 2;
        if width > 0 { width } else { 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::Frame;
    use crate::spacing::Spacing;

    /// The guide's own baseline: 13px type on a 1x output.
    fn spacing() -> Spacing {
        Spacing::new(13.0, 1.0)
    }

    fn frame(width: i32, height: i32) -> Frame {
        let s = spacing();
        Frame::new(
            width,
            height,
            true,
            s,
            s.toolbar_height(),
            s.status_height(),
            s.column_header(),
        )
    }

    /// The bug this module exists to prevent: the sidebar must run the whole
    /// height of the window, not be boxed between a full-width toolbar and a
    /// full-width status strip.
    #[test]
    fn the_sidebar_runs_the_full_window_height() {
        let f = frame(1280, 790);
        assert_eq!(f.sidebar().y, 0);
        assert_eq!(f.sidebar().h, 790);
    }

    /// The mirror of the above: the toolbar and status belong to the right
    /// column, so neither may start at x = 0 while a sidebar is shown.
    #[test]
    fn the_toolbar_and_status_stay_out_of_the_sidebar() {
        let f = frame(1280, 790);
        assert_eq!(f.toolbar().x, f.sidebar().w);
        assert_eq!(f.status().x, f.sidebar().w);
        assert_eq!(f.toolbar().w, 1280 - f.sidebar().w);
        assert_eq!(f.status().w, 1280 - f.sidebar().w);
    }

    #[test]
    fn the_sidebar_takes_the_guide_width() {
        let f = frame(1280, 790);
        assert_eq!(f.sidebar().w, spacing().sidebar_width());
    }

    /// The two columns tile the window exactly, with no seam and no overlap.
    #[test]
    fn the_two_columns_fill_the_window() {
        let f = frame(1280, 790);
        assert_eq!(f.sidebar().w + f.main().w, 1280);
        assert_eq!(f.main().x, f.sidebar().w);
    }

    /// Inside the right column the bands tile it exactly, top to bottom.
    #[test]
    fn the_bands_tile_the_right_column() {
        let f = frame(1280, 790);
        assert_eq!(f.toolbar().y, 0);
        assert_eq!(f.column_headers().y, f.toolbar().h);
        assert_eq!(f.rows().y, f.column_headers().y + f.column_headers().h);
        assert_eq!(f.rows().y + f.rows().h, f.status().y);
        assert_eq!(f.status().y + f.status().h, 790);
    }

    /// Every band in the right column shares its left edge and width.
    #[test]
    fn the_right_column_bands_are_aligned() {
        let f = frame(1280, 790);
        for band in [f.toolbar(), f.column_headers(), f.rows(), f.status()] {
            assert_eq!(band.x, f.main().x, "band {band:?} is not in the column");
            assert_eq!(band.w, f.main().w, "band {band:?} is the wrong width");
        }
    }

    /// The storage footer is pinned to the bottom of the sidebar, and the
    /// places list stops exactly where it starts.
    #[test]
    fn the_storage_footer_sits_at_the_bottom_of_the_sidebar() {
        let f = frame(1280, 790);
        let storage = f.storage().expect("a tall window has room for storage");
        assert_eq!(storage.y + storage.h, 790);
        assert_eq!(storage.w, f.sidebar().w);
        assert_eq!(f.sidebar_body().h, storage.y);
    }

    #[test]
    fn hiding_the_sidebar_gives_the_whole_window_to_the_list() {
        let s = spacing();
        let f = Frame::new(
            1280,
            790,
            false,
            s,
            s.toolbar_height(),
            s.status_height(),
            s.column_header(),
        );
        assert!(!f.has_sidebar());
        assert_eq!(f.sidebar().w, 0);
        assert_eq!(f.main().x, 0);
        assert_eq!(f.main().w, 1280);
        assert_eq!(f.toolbar().x, 0);
        // With no sidebar there is nothing to put a footer in.
        assert!(f.storage().is_none());
    }

    /// Below the guide's threshold the sidebar becomes a rail -- still full
    /// height, just narrow.
    #[test]
    fn a_narrow_window_collapses_the_sidebar_to_a_rail() {
        let f = frame(700, 600);
        assert_eq!(f.sidebar().w, spacing().rail_width());
        assert_eq!(f.sidebar().h, 600);
        // A rail has no room for a device name or a percentage.
        assert!(f.storage().is_none());
    }

    /// A window shorter than its own chrome must still produce a sane frame:
    /// no negative heights anywhere for a downstream calculation to inherit.
    #[test]
    fn a_tiny_window_yields_no_negative_geometry() {
        for height in [0, 1, 20, 60, 90] {
            for width in [0, 1, 40, 300] {
                let f = frame(width, height);
                for band in [
                    f.sidebar(),
                    f.sidebar_body(),
                    f.main(),
                    f.toolbar(),
                    f.column_headers(),
                    f.rows(),
                    f.status(),
                ] {
                    assert!(
                        band.w >= 0 && band.h >= 0,
                        "{width}x{height} produced {band:?}"
                    );
                }
                assert!(f.content_width() >= 0);
            }
        }
    }

    /// Chrome is dropped from the bottom up, so the toolbar -- which says
    /// where you are -- is the last thing to go.
    #[test]
    fn a_short_window_keeps_the_toolbar_over_the_status_strip() {
        let s = spacing();
        let f = frame(1280, s.toolbar_height());
        assert_eq!(f.toolbar().h, s.toolbar_height());
        assert_eq!(f.status().h, 0);
        assert_eq!(f.rows().h, 0);
    }

    /// A sidebar can never be wider than the window it is inside.
    #[test]
    fn the_sidebar_cannot_exceed_the_window() {
        let f = frame(40, 600);
        assert!(f.sidebar().w <= 40);
        assert!(f.main().w >= 0);
    }

    #[test]
    fn content_is_inset_within_the_right_column() {
        let f = frame(1280, 790);
        assert_eq!(f.content_x(), f.main().x + f.inset());
        assert_eq!(f.content_width(), f.main().w - f.inset() * 2);
    }

    /// The rows band is where the file list scrolls, so it must be the tallest
    /// thing in the window -- if chrome ever outgrew it the design would be
    /// upside down.
    #[test]
    fn the_rows_get_most_of_the_height() {
        let f = frame(1280, 790);
        let chrome = f.toolbar().h + f.column_headers().h + f.status().h;
        assert!(f.rows().h > chrome, "rows lost to chrome");
    }
}

//! [`Theme`] -- every colour and measurement the browser paints with.
//!
//! Gathered into one struct rather than scattered as consts at their use
//! sites, because the alternative is what the other binaries in this
//! workspace ended up with: a `Color::rgb(0x1e, 0x1e, 0x2e)` literal repeated
//! in six places, one of which is subtly a different shade and nobody knows
//! whether that was deliberate.
//!
//! Hard-coded for now rather than read from `crates/config`. Wiring the
//! config crate in is a real task -- it means a `[files]` table, mirrored
//! defaults, and validation tests -- and doing it before the window exists
//! would be building a settings system for a program that cannot yet draw.

use platform::draw::Color;

/// Colours and metrics for the file browser.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub background: Color,
    /// Fill behind the selected rows.
    pub selection: Color,
    /// Outline on the cursor row, drawn even when the row is not selected so
    /// keyboard focus is always locatable.
    pub cursor: Color,
    pub text: Color,
    /// Directory names, coloured distinctly so structure is scannable
    /// without reading.
    pub dir: Color,
    pub symlink: Color,
    /// A symlink whose target is gone -- worth shouting about, since every
    /// operation on it will behave surprisingly.
    pub broken: Color,
    /// Dimmed text: sizes, counts, the status line.
    pub muted: Color,
    /// The header strip showing the current path.
    pub header_bg: Color,
    pub status_bg: Color,
    pub error: Color,
    pub scrollbar_track: Color,
    pub scrollbar_thumb: Color,
    /// Fill behind the filter box while it has focus.
    pub filter_bg: Color,
    pub caret: Color,

    pub row_height: i32,
    pub header_height: i32,
    pub status_height: i32,
    /// Left/right padding inside a row.
    pub padding: i32,
}

impl Default for Theme {
    /// A dark theme in the same family as the rest of the workspace's
    /// clients, so the browser does not look like a stranger next to the
    /// terminal and the bar.
    fn default() -> Self {
        Self {
            background: Color::rgb(0x1e, 0x1e, 0x2e),
            selection: Color::rgb(0x45, 0x47, 0x5a),
            cursor: Color::rgb(0x89, 0xb4, 0xfa),
            text: Color::rgb(0xcd, 0xd6, 0xf4),
            dir: Color::rgb(0x89, 0xb4, 0xfa),
            symlink: Color::rgb(0x94, 0xe2, 0xd5),
            broken: Color::rgb(0xf3, 0x8b, 0xa8),
            muted: Color::rgb(0x6c, 0x70, 0x86),
            header_bg: Color::rgb(0x18, 0x18, 0x25),
            status_bg: Color::rgb(0x18, 0x18, 0x25),
            error: Color::rgb(0xf3, 0x8b, 0xa8),
            scrollbar_track: Color::rgb(0x18, 0x18, 0x25),
            scrollbar_thumb: Color::rgb(0x58, 0x5b, 0x70),
            filter_bg: Color::rgb(0x31, 0x32, 0x44),
            caret: Color::rgb(0xf9, 0xe2, 0xaf),

            // The bitmap font is 16px tall; 20 leaves 2px of breathing room
            // above and below without making the list feel sparse.
            row_height: 20,
            header_height: 24,
            status_height: 20,
            padding: 8,
        }
    }
}

impl Theme {
    /// Vertical space the list gets in a window of `height` pixels.
    ///
    /// Clamped at zero so a window dragged smaller than its own chrome
    /// yields an empty list rather than a negative height that would make
    /// every downstream calculation nonsense.
    #[must_use]
    pub const fn list_height(&self, height: i32) -> i32 {
        let used = self.header_height + self.status_height;
        if height > used { height - used } else { 0 }
    }

    /// The y coordinate where the list starts.
    #[must_use]
    pub const fn list_top(&self) -> i32 {
        self.header_height
    }
}

#[cfg(test)]
mod tests {
    use super::Theme;

    #[test]
    fn the_list_gets_what_the_chrome_does_not_use() {
        let theme = Theme::default();
        let height = 600;
        assert_eq!(
            theme.list_height(height),
            height - theme.header_height - theme.status_height
        );
    }

    /// A window dragged smaller than its own chrome must not produce a
    /// negative list height -- every scroll calculation downstream would
    /// silently produce nonsense.
    #[test]
    fn a_window_smaller_than_its_chrome_yields_an_empty_list() {
        let theme = Theme::default();
        assert_eq!(theme.list_height(10), 0);
        assert_eq!(theme.list_height(0), 0);
    }

    #[test]
    fn the_list_starts_below_the_header() {
        let theme = Theme::default();
        assert_eq!(theme.list_top(), theme.header_height);
    }
}

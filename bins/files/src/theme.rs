//! [`Theme`] -- every colour and measurement the browser paints with.
//!
//! Gathered into one struct rather than scattered as consts at their use
//! sites, because the alternative is what the other binaries in this
//! workspace ended up with: a `Color::rgb(0x1e, 0x1e, 0x2e)` literal repeated
//! in six places, one of which is subtly a different shade and nobody knows
//! whether that was deliberate.
//!
//! Read from `crates/config`'s `[files]` table via [`Theme::from_config`].
//! [`Theme::default`] remains the fallback used when no config file exists
//! and the values the tests below pin.

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
    /// Derive every colour and measurement from `cfg`, mirroring
    /// `bins/launcher`'s and `bins/monitor`'s `Theme::from_config`.
    ///
    /// All the actual policy -- which keys mirror which palette entry, what
    /// the derived chrome shades are -- lives in `config`'s `files_*`
    /// accessors, where it can be tested without a window. This is only the
    /// `config::Color` -> `platform::draw::Color` hop and the `u32` -> `i32`
    /// one, both of which the config layer has already range-checked.
    #[must_use]
    pub fn from_config(cfg: &config::Config) -> Self {
        // Metrics are validated to 0..=512 on the way in, so every one of
        // these conversions is trivially in range for i32.
        let px = |n: u32| i32::try_from(n).unwrap_or(0);
        Self {
            background: cfg.files_background().into(),
            selection: cfg.files_selection().into(),
            cursor: cfg.files_cursor().into(),
            text: cfg.files_text().into(),
            dir: cfg.files_dir().into(),
            symlink: cfg.files_symlink().into(),
            broken: cfg.files_broken().into(),
            muted: cfg.files_muted().into(),
            header_bg: cfg.files_header_bg().into(),
            status_bg: cfg.files_status_bg().into(),
            error: cfg.files_error().into(),
            scrollbar_track: cfg.files_scrollbar_track().into(),
            scrollbar_thumb: cfg.files_scrollbar_thumb().into(),
            filter_bg: cfg.files_filter_bg().into(),
            caret: cfg.files_caret().into(),

            row_height: px(cfg.files_row_height()),
            header_height: px(cfg.files_header_height()),
            status_height: px(cfg.files_status_height()),
            padding: px(cfg.files_padding()),
        }
    }

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
    use super::{Color, Theme};

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

    /// With no config file at all, the wired-up theme must match the
    /// hardcoded one it replaced in *metrics* -- a silent change to row
    /// height or padding would reflow every window on upgrade.
    #[test]
    fn an_empty_config_reproduces_the_default_metrics() {
        let theme = Theme::from_config(&config::Config::default());
        let fallback = Theme::default();
        assert_eq!(theme.row_height, fallback.row_height);
        assert_eq!(theme.header_height, fallback.header_height);
        assert_eq!(theme.status_height, fallback.status_height);
        assert_eq!(theme.padding, fallback.padding);
    }

    /// A `[files]` override must actually arrive in the painted theme --
    /// this is the wire the whole task exists to connect, so it gets a test
    /// that would fail if `from_config` read the wrong accessor.
    #[test]
    fn a_files_override_reaches_the_theme() {
        let cfg = config::Config::from_bytes(
            b"[files]\ndir = \"#ff0000\"\nrow_height = 30\npadding = 3\n",
        )
        .expect("parse");
        let theme = Theme::from_config(&cfg);
        assert_eq!(theme.dir, Color::rgb(0xff, 0x00, 0x00));
        assert_eq!(theme.row_height, 30);
        assert_eq!(theme.padding, 3);
    }

    /// Re-theming the *terminal* must carry into the browser with no
    /// `[files]` table present -- the mirroring rule, end to end.
    #[test]
    fn the_browser_follows_a_retheme_of_the_terminal() {
        let cfg = config::Config::from_bytes(b"[colors]\nbackground = \"#fdf6e3\"\n")
            .expect("parse");
        let theme = Theme::from_config(&cfg);
        assert_eq!(theme.background, Color::rgb(0xfd, 0xf6, 0xe3));
    }

    /// Every colour role must be distinguishable from what it is drawn on,
    /// for a light palette as well as the dark default -- the browser is
    /// unusable if the cursor outline or the scrollbar thumb is invisible.
    #[test]
    fn no_role_is_invisible_against_its_own_background() {
        for doc in [
            &b""[..],
            b"[colors]\nbackground = \"#ffffff\"\nforeground = \"#000000\"\n",
            b"[colors]\nbackground = \"#000000\"\nforeground = \"#ffffff\"\n",
        ] {
            let cfg = config::Config::from_bytes(doc).expect("parse");
            let t = Theme::from_config(&cfg);
            assert_ne!(t.text, t.background, "text on background");
            assert_ne!(t.selection, t.background, "selection on background");
            assert_ne!(t.header_bg, t.background, "header on background");
            assert_ne!(t.filter_bg, t.background, "filter box on background");
            assert_ne!(
                t.scrollbar_thumb, t.scrollbar_track,
                "scrollbar thumb on its track"
            );
        }
    }
}

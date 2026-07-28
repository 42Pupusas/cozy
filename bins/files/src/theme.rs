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

use crate::spacing::Spacing;

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
    ///
    /// Retained as a distinct role from [`Self::toolbar_bg`] because the
    /// config exposes it and a user may have set it; the toolbar mirrors it
    /// by default, so they only diverge if someone asks them to.
    #[allow(dead_code)]
    pub header_bg: Color,
    pub status_bg: Color,
    pub error: Color,
    pub scrollbar_track: Color,
    pub scrollbar_thumb: Color,
    /// Fill behind the filter box while it has focus.
    pub filter_bg: Color,
    pub caret: Color,

    /// Fill behind the places sidebar. Slightly off the list background so
    /// the two panes read as separate regions without needing a hard border.
    pub sidebar_bg: Color,
    /// The hairline between the sidebar and the list.
    pub sidebar_border: Color,
    /// Fill behind the sidebar row for the directory currently open.
    pub sidebar_active: Color,

    /// Fill under a hovered row or control.
    ///
    /// The guide asks every interactive element for a distinct hover state.
    /// Unused until the window tracks pointer position per row -- motion is
    /// currently only forwarded for scrollbar drags -- but defined here so
    /// the colour is chosen once, with the palette, rather than invented at
    /// the call site later.
    #[allow(dead_code)]
    pub hover: Color,
    /// Fill behind the toolbar strip.
    pub toolbar_bg: Color,
    /// Outline around toolbar controls (the filter box, the view switch).
    pub control_border: Color,
    /// Outline around a control while it has keyboard focus.
    pub focus_ring: Color,
    /// The accent: active location, selection, and feedback. Used sparingly
    /// -- per the guide it indicates state rather than decorating.
    pub accent: Color,
    /// A quiet wash of the accent, for the fill *behind* accented content:
    /// the active sidebar row and the selected half of the view toggle.
    ///
    /// Distinct from [`Self::accent`] because the guide uses the accent for
    /// two different jobs -- a 2px rule at full strength, and a background at
    /// a fraction of it. Painting a fill with the full accent would drown the
    /// text sitting on top of it.
    pub accent_surface: Color,
    /// Text and glyphs for a control that exists but cannot be used yet.
    ///
    /// A distinct role from [`Self::muted`], which means "subordinate but
    /// live" (a file size, a timestamp). Disabled means "not available", and
    /// conflating the two would make working metadata look broken -- or, worse,
    /// make a dead button look clickable.
    pub disabled: Color,

    pub row_height: i32,
    pub header_height: i32,
    pub status_height: i32,
    /// Left/right padding inside a row.
    pub padding: i32,
    /// Width of the places sidebar.
    pub sidebar_width: i32,

    /// The 4px spacing rhythm and the geometry derived from it.
    ///
    /// Held by the theme rather than passed alongside it because every call
    /// site that wants a colour also wants a gap, and threading two values
    /// through the painter would guarantee they drift apart.
    pub spacing: Spacing,
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

            // A touch darker than the list, matching the header/status strips
            // so the chrome reads as one family.
            sidebar_bg: Color::rgb(0x18, 0x18, 0x25),
            sidebar_border: Color::rgb(0x31, 0x32, 0x44),
            sidebar_active: Color::rgb(0x31, 0x32, 0x44),

            hover: Color::rgb(0x28, 0x28, 0x3c),
            toolbar_bg: Color::rgb(0x18, 0x18, 0x25),
            control_border: Color::rgb(0x31, 0x32, 0x44),
            focus_ring: Color::rgb(0x89, 0xb4, 0xfa),
            accent: Color::rgb(0x89, 0xb4, 0xfa),
            accent_surface: Color::rgb(0x31, 0x32, 0x44),
            // Between the background and muted: readable as a shape, clearly
            // not as live text.
            disabled: Color::rgb(0x4a, 0x4d, 0x60),

            // The bitmap font is 16px tall; 20 leaves 2px of breathing room
            // above and below without making the list feel sparse.
            row_height: 20,
            header_height: 24,
            status_height: 20,
            padding: 8,
            // Wide enough for "Downloads" plus padding at the default size,
            // narrow enough to leave the list the majority of a small window.
            sidebar_width: 160,
            spacing: Spacing::new(13.0, 1.0),
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

            // Derived from existing roles rather than given their own config
            // keys: the sidebar is chrome, and chrome that tracks the header
            // automatically stays coherent when the palette is re-themed.
            sidebar_bg: cfg.files_header_bg().into(),
            sidebar_border: cfg.files_filter_bg().into(),
            sidebar_active: cfg.files_filter_bg().into(),

            // Hover sits between the background and the selection: present
            // enough to track the pointer, quiet enough not to compete with
            // an actual selection.
            hover: cfg.files_filter_bg().into(),
            toolbar_bg: cfg.files_header_bg().into(),
            control_border: cfg.files_filter_bg().into(),
            // The cursor colour already means "keyboard is here", so the
            // focus ring reuses it rather than introducing a second
            // convention the user would have to learn.
            focus_ring: cfg.files_cursor().into(),
            accent: cfg.files_cursor().into(),
            // Derived rather than configured, for the same reason as the
            // sidebar roles: a re-themed palette should carry the accent's
            // backing wash with it instead of stranding a stale colour.
            accent_surface: cfg.files_filter_bg().into(),
            // Halfway between the muted text and the background it sits on,
            // so it recedes under a light or a dark palette alike.
            disabled: cfg.files_muted().mix(cfg.files_background(), 0.5).into(),

            row_height: px(cfg.files_row_height()),
            header_height: px(cfg.files_header_height()),
            status_height: px(cfg.files_status_height()),
            padding: px(cfg.files_padding()),
            sidebar_width: Self::default().sidebar_width,
            spacing: Spacing::new(13.0, 1.0),
        }
    }

    /// Adopt the design guide's geometry for a given font size and output
    /// scale.
    ///
    /// Applied after [`Self::from_config`] so an explicit `[files]` metric
    /// still wins: the user asked for that number. Everything they did *not*
    /// state follows the guide's proportions, which is what makes the layout
    /// hold together at any font size.
    #[must_use]
    pub fn with_spacing(mut self, spacing: Spacing) -> Self {
        let fallback = Self::default();
        self.spacing = spacing;
        if self.row_height == fallback.row_height {
            self.row_height = spacing.list_row();
        }
        if self.header_height == fallback.header_height {
            self.header_height = spacing.toolbar_height();
        }
        if self.status_height == fallback.status_height {
            self.status_height = spacing.status_height();
        }
        if self.padding == fallback.padding {
            self.padding = spacing.m();
        }
        if self.sidebar_width == fallback.sidebar_width {
            self.sidebar_width = spacing.sidebar_width();
        }
        self
    }

    /// Whether the window is narrow enough for the guide's compact rules:
    /// a collapsed sidebar rail and a dropped `modified` column.
    #[must_use]
    pub fn is_compact(&self, window_width: i32) -> bool {
        window_width < self.spacing.collapse_below()
    }

    /// Adopt the metrics derived from the loaded type scale.
    ///
    /// Config still wins where the user stated a value; this only replaces
    /// the defaults, so a bigger font grows the chrome instead of clipping
    /// its own glyphs.
    #[must_use]
    pub fn with_metrics(mut self, row: i32, header: i32, status: i32) -> Self {
        let fallback = Self::default();
        if self.row_height == fallback.row_height {
            self.row_height = row;
        }
        if self.header_height == fallback.header_height {
            self.header_height = header;
        }
        if self.status_height == fallback.status_height {
            self.status_height = status;
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{Color, Spacing, Theme};

    /// The guide's responsive rule: a narrow window collapses the sidebar to
    /// an icon rail so filenames keep their room.
    #[test]
    fn a_narrow_window_collapses_the_sidebar_to_a_rail() {
        let theme = Theme::default().with_spacing(Spacing::new(13.0, 1.0));
        assert!(theme.is_compact(700));
        assert!(!theme.is_compact(1280));
    }

    /// The guide's geometry must actually arrive when nothing is configured.
    #[test]
    fn the_guide_geometry_replaces_the_defaults() {
        let spacing = Spacing::new(13.0, 1.0);
        let theme = Theme::default().with_spacing(spacing);
        assert_eq!(theme.row_height, spacing.list_row());
        assert_eq!(theme.header_height, spacing.toolbar_height());
        assert_eq!(theme.status_height, spacing.status_height());
        assert_eq!(theme.sidebar_width, spacing.sidebar_width());
        assert_eq!(theme.padding, spacing.m());
    }

    /// …but an explicit config value still outranks the guide's number.
    #[test]
    fn a_configured_metric_outranks_the_guide_geometry() {
        let cfg = config::Config::from_bytes(b"[files]\nrow_height = 44\n").expect("parse");
        let theme = Theme::from_config(&cfg).with_spacing(Spacing::new(13.0, 1.0));
        assert_eq!(theme.row_height, 44, "config must win");
        assert_ne!(theme.header_height, 44, "unset metrics still derive");
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
        let cfg =
            config::Config::from_bytes(b"[colors]\nbackground = \"#fdf6e3\"\n").expect("parse");
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
            assert_ne!(t.sidebar_active, t.sidebar_bg, "active place on sidebar");
            assert_ne!(t.sidebar_border, t.background, "the pane divider");
            assert_ne!(t.hover, t.background, "a hovered row");
            assert_ne!(t.toolbar_bg, t.background, "the toolbar strip");
            assert_ne!(t.control_border, t.toolbar_bg, "a control outline");
            assert_ne!(t.focus_ring, t.toolbar_bg, "the focus ring");
            assert_ne!(t.accent, t.background, "the accent");
            assert_ne!(t.accent_surface, t.background, "the accent wash");
            // Disabled must be distinguishable from live muted text, or a
            // working timestamp reads as a dead control.
            assert_ne!(t.disabled, t.muted, "disabled vs muted");
            assert_ne!(t.disabled, t.toolbar_bg, "a disabled glyph");
        }
    }

    /// Derived metrics replace the defaults, so a larger font grows the
    /// chrome rather than clipping its glyphs.
    #[test]
    fn derived_metrics_replace_the_defaults() {
        let theme = Theme::default().with_metrics(30, 40, 26);
        assert_eq!(theme.row_height, 30);
        assert_eq!(theme.header_height, 40);
        assert_eq!(theme.status_height, 26);
    }

    /// …but an explicit config value outranks the derived one: the user asked
    /// for that number and must get it.
    #[test]
    fn a_configured_metric_outranks_the_derived_one() {
        let cfg = config::Config::from_bytes(b"[files]\nrow_height = 44\n").expect("parse");
        let theme = Theme::from_config(&cfg).with_metrics(30, 40, 26);
        assert_eq!(theme.row_height, 44, "config must win");
        assert_eq!(theme.header_height, 40, "unset metrics still derive");
    }
}

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

use crate::density::Density;
use crate::metric_overrides::MetricOverrides;
use crate::spacing::Spacing;

/// Colours and metrics for the file browser.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub background: Color,
    /// Fill behind a selected row, for surfaces with no entry type to express.
    ///
    /// The *list* uses [`Self::selection_dir`] / [`Self::selection_file`]
    /// instead. Retained because `[files].selection` is a documented config
    /// key and those two derive from the palette, not from it -- so this stays
    /// the honest home for a user's literal value, and the sidebar and toolbar
    /// keep a type-free selection colour to reach for.
    #[allow(dead_code)]
    pub selection: Color,
    /// The keyboard-cursor colour.
    ///
    /// No longer painted as a row outline -- the cursor row now takes the
    /// hover wash, since a border on the highlight is precisely what the
    /// design asks against -- but still the source of [`Self::focus_ring`]
    /// and [`Self::accent`], and still the documented `[files].cursor` key.
    #[allow(dead_code)]
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

    /// Fill under a hovered control (the filter box, a toolbar button).
    ///
    /// List *rows* use [`Self::hover_dir`] / [`Self::hover_file`] instead,
    /// which carry the entry's type; this stays the neutral wash for chrome
    /// that has no type to express. Not yet reached for -- the toolbar's
    /// controls do not track the pointer -- but the colour belongs with the
    /// palette rather than being invented at that call site later.
    #[allow(dead_code)]
    pub hover: Color,
    /// Fill under a hovered directory row, and behind a selected one.
    ///
    /// Tinted toward [`Self::dir`] so the highlight itself says what kind of
    /// thing is under the pointer. A single neutral highlight for both kinds
    /// throws away information the row is already carrying in its text
    /// colour -- and the text colour is the thing the highlight covers up.
    pub hover_dir: Color,
    /// Fill under a hovered file row, and behind a selected one.
    pub hover_file: Color,
    /// Selection fill for a directory row -- the same hue as
    /// [`Self::hover_dir`], carried further from the background so selection
    /// still outranks hover when both apply to the same row.
    pub selection_dir: Color,
    /// Selection fill for a file row.
    pub selection_file: Color,
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

    /// How tightly this theme packs the guide's dimensions.
    ///
    /// Kept so the *controls* inside a band scale with the band itself. The
    /// guide's 31px control in a 58px toolbar is a ratio, not two independent
    /// numbers: tightening only the band would leave a full-size control in a
    /// smaller strip, which is precisely the vertical crowding that
    /// `the_toolbar_always_contains_its_controls` exists to catch.
    density: Density,

    /// Which measurements the user stated explicitly.
    ///
    /// Carried so the derivation stages below can tell "the user asked for
    /// this number" apart from "this is whatever the last stage left here".
    /// See [`MetricOverrides`] for why inferring that by comparing against
    /// the defaults was actively wrong.
    overrides: MetricOverrides,
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
            // Washes of the type colours over the list background: the same
            // hue the filename already uses, at a fraction that reads as a
            // surface rather than as ink. Hover is the quieter pair, selection
            // the stronger, so a selected row still wins under the pointer.
            hover_dir: Color::rgb(0x25, 0x2c, 0x3d),
            hover_file: Color::rgb(0x28, 0x2a, 0x3a),
            selection_dir: Color::rgb(0x33, 0x40, 0x5c),
            selection_file: Color::rgb(0x3a, 0x3d, 0x50),
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
            density: Density::default(),
            overrides: MetricOverrides::default(),
        }
    }
}

impl Theme {
    /// How far a hover highlight carries its type colour over the background.
    ///
    /// Low on purpose: hover is transient feedback that follows the pointer,
    /// and a strong fill flashing under the cursor as it crosses a list is
    /// noise. It only has to be *findable*, not loud.
    const HOVER_WASH: f32 = 0.14;

    /// The same for a selection, which is a state the user chose and expects
    /// to stay put. Comfortably ahead of [`Self::HOVER_WASH`] so a selected
    /// row still reads as selected while the pointer is over it.
    const SELECTION_WASH: f32 = 0.30;

    /// Mix `ink` into `surface` by `t`, as a paint colour.
    ///
    /// Named rather than inlined because it is applied four times with two
    /// strengths, and the thing that must hold is that all four use the *same*
    /// derivation -- otherwise the directory and file highlights end up on
    /// subtly different curves and only one of them looks right.
    fn wash(surface: config::Color, ink: config::Color, t: f32) -> Color {
        surface.mix(ink, t).into()
    }

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

            // Row highlights are washes of the *type* colour over the list
            // background, so the highlight carries the same information the
            // filename's colour does -- which matters most exactly when the
            // highlight is covering that filename up.
            //
            // Derived rather than configured: these follow `[files].dir` and
            // `[files].text`, so re-theming those moves the highlights with
            // them instead of stranding four hand-picked shades. The two
            // strengths keep selection ahead of hover on the same row.
            hover_dir: Self::wash(cfg.files_background(), cfg.files_dir(), Self::HOVER_WASH),
            hover_file: Self::wash(cfg.files_background(), cfg.files_text(), Self::HOVER_WASH),
            selection_dir: Self::wash(
                cfg.files_background(),
                cfg.files_dir(),
                Self::SELECTION_WASH,
            ),
            selection_file: Self::wash(
                cfg.files_background(),
                cfg.files_text(),
                Self::SELECTION_WASH,
            ),
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
            density: Density::default(),
            overrides: MetricOverrides::from_config(cfg),
        }
    }

    /// Adopt the design guide's geometry for a given font size and output
    /// scale.
    ///
    /// NOTE: this stage owns the *chrome band* heights, because the guide
    /// sizes the controls inside those bands (31px filter box, 31px view
    /// toggle) against the same table. A band sized from anything else leaves
    /// its own controls too tall for it -- which is exactly the vertical
    /// crowding this replaced.
    ///
    /// Applied after [`Self::from_config`] so an explicit `[files]` metric
    /// still wins: the user asked for that number. Everything they did *not*
    /// state follows the guide's proportions, which is what makes the layout
    /// hold together at any font size.
    #[must_use]
    pub fn with_spacing(self, spacing: Spacing) -> Self {
        self.with_spacing_at(spacing, Density::default())
    }

    /// [`Self::with_spacing`] with the packing policy stated explicitly.
    ///
    /// `density` tightens the guide's dimensions for a list that draws no
    /// icons -- see [`Density`]. It is applied here rather than inside
    /// [`Spacing`] so `Spacing` keeps encoding the guide faithfully and stays
    /// testable against its published table; this stage is where the browser's
    /// own deviation from it is expressed.
    ///
    /// Note the ordering: density scales the *guide's* number, and an explicit
    /// `[files]` override replaces it outright rather than being scaled. A
    /// user who writes `row_height = 44` means 44 pixels, not 44 tightened to
    /// 27.
    #[must_use]
    pub fn with_spacing_at(mut self, spacing: Spacing, density: Density) -> Self {
        self.spacing = spacing;
        self.density = density;
        // Only an explicit `[files]` value outranks the guide. Anything the
        // user left unset takes the guide's number even if an earlier stage
        // already wrote something there.
        self.row_height = self
            .overrides
            .row_height()
            .unwrap_or_else(|| density.row(spacing.list_row()));
        self.header_height = self
            .overrides
            .header_height()
            .unwrap_or_else(|| density.row(spacing.toolbar_height()));
        self.status_height = self
            .overrides
            .status_height()
            .unwrap_or_else(|| density.row(spacing.status_height()));
        self.padding = self
            .overrides
            .padding()
            .unwrap_or_else(|| density.pad(spacing.m()));
        self.sidebar_width = spacing.sidebar_width();
        self
    }

    /// Whether the window is narrow enough for the guide's compact rules:
    /// a collapsed sidebar rail and a dropped `modified` column.
    #[must_use]
    pub fn is_compact(&self, window_width: i32) -> bool {
        window_width < self.spacing.collapse_below()
    }

    /// Height of a toolbar control (the filter box, the view toggle), packed
    /// to this theme's density.
    ///
    /// Exists so call sites cannot reach for the raw `spacing.control_height()`
    /// and land a guide-sized control in a density-tightened band. Reading the
    /// height through the theme keeps the band and its contents on one policy.
    #[must_use]
    pub fn control_height(&self) -> i32 {
        self.density.row(self.spacing.control_height())
    }

    /// A sidebar navigation row, packed to this theme's density.
    #[must_use]
    pub fn sidebar_row(&self) -> i32 {
        self.density.row(self.spacing.sidebar_row())
    }

    /// The column-header strip above the list, packed to this theme's density.
    #[must_use]
    pub fn column_header(&self) -> i32 {
        self.density.row(self.spacing.column_header())
    }

    /// Adopt the metrics derived from the loaded type scale.
    ///
    /// Config still wins where the user stated a value; this only replaces
    /// the defaults, so a bigger font grows the chrome instead of clipping
    /// its own glyphs.
    ///
    /// A *floor*, not an assignment: the guide's proportions already scale
    /// with the configured font, so this only rescues the case where a tier's
    /// glyphs are genuinely taller than the band the guide asks for. Taking
    /// the larger of the two keeps text from clipping without letting a
    /// font-derived number quietly replace the guide's geometry.
    #[must_use]
    pub fn with_metrics(mut self, row: i32, header: i32, status: i32) -> Self {
        if self.overrides.row_height().is_none() {
            self.row_height = self.row_height.max(row);
        }
        if self.overrides.header_height().is_none() {
            self.header_height = self.header_height.max(header);
        }
        if self.overrides.status_height().is_none() {
            self.status_height = self.status_height.max(status);
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{Color, Density, Spacing, Theme};

    /// The guide's responsive rule: a narrow window collapses the sidebar to
    /// an icon rail so filenames keep their room.
    #[test]
    fn a_narrow_window_collapses_the_sidebar_to_a_rail() {
        let theme = Theme::default().with_spacing(Spacing::new(13.0, 1.0));
        assert!(theme.is_compact(700));
        assert!(!theme.is_compact(1280));
    }

    /// The guide's geometry must actually arrive when nothing is configured --
    /// stated at [`Density::GUIDE`], since the shipped policy deliberately
    /// tightens it (see the test below).
    #[test]
    fn the_guide_geometry_replaces_the_defaults() {
        let spacing = Spacing::new(13.0, 1.0);
        let theme = Theme::default().with_spacing_at(spacing, Density::GUIDE);
        assert_eq!(theme.row_height, spacing.list_row());
        assert_eq!(theme.header_height, spacing.toolbar_height());
        assert_eq!(theme.status_height, spacing.status_height());
        assert_eq!(theme.sidebar_width, spacing.sidebar_width());
        assert_eq!(theme.padding, spacing.m());
    }

    /// The shipped density must actually tighten the guide's list geometry --
    /// this is the user-visible request ("smaller text, tighter spacing") that
    /// [`Density`] exists to serve, so it is pinned rather than left implicit.
    ///
    /// Sidebar width is excluded on purpose: density is about vertical packing
    /// and internal padding, and narrowing the sidebar would start truncating
    /// place names, which is a different decision with a different tradeoff.
    #[test]
    fn the_shipped_density_tightens_the_guide_geometry() {
        let spacing = Spacing::new(13.0, 1.0);
        let guide = Theme::default().with_spacing_at(spacing, Density::GUIDE);
        let shipped = Theme::default().with_spacing(spacing);

        assert!(
            shipped.row_height < guide.row_height,
            "rows must be tighter than the guide's icon-sized ones"
        );
        assert!(shipped.padding < guide.padding, "padding must be tighter");
        assert!(
            shipped.header_height < guide.header_height,
            "the toolbar must be tighter"
        );
        assert_eq!(
            shipped.sidebar_width, guide.sidebar_width,
            "density must not narrow the sidebar and truncate place names"
        );
        // Still positive and still ordered -- tightening must not invert the
        // relationship the layout depends on.
        assert!(shipped.row_height > 0 && shipped.padding > 0);
        assert!(
            shipped.header_height > shipped.row_height,
            "the toolbar still carries the largest tier"
        );
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

    /// The browser must agree with the *other installed clients* on the
    /// shared palette, not merely with `[colors]` in the abstract.
    ///
    /// The launcher pins `background`/`foreground` explicitly and the monitor
    /// pins its own `background`; all three are supposed to be the same
    /// Kanagawa Dragon surface. Since each app resolves its own accessors,
    /// nothing structurally stops one drifting -- so this asserts the three
    /// agree given the same document, which is the actual desktop-consistency
    /// requirement rather than a restatement of the mirroring rule.
    #[test]
    fn the_browser_agrees_with_the_other_clients_on_the_shared_surface() {
        let doc = br##"
[colors]
foreground = "#c5c9c5"
background = "#181616"
[launcher]
foreground = "#c5c9c5"
background = "#181616"
highlight  = "#727169"
[monitor]
background = "#181616"
"##;
        let cfg = config::Config::from_bytes(doc).expect("parse");
        let theme = Theme::from_config(&cfg);
        assert_eq!(
            theme.background,
            Color::from(cfg.launcher_background()),
            "the browser and launcher must share one window surface"
        );
        assert_eq!(
            theme.background,
            Color::from(cfg.monitor_background()),
            "the browser and monitor must share one window surface"
        );
        assert_eq!(
            theme.text,
            Color::from(cfg.launcher_foreground()),
            "and one text colour"
        );
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

    /// The regression this type was introduced for.
    ///
    /// Both derivation stages must compose: whichever order they run in, an
    /// unset band ends up at least the guide's height. The shipped bug was
    /// that `with_metrics` running first left `header_height` different from
    /// the default, so `with_spacing` read that as "user-stated" and skipped
    /// the toolbar -- leaving a 30px band holding a 33px filter box, which is
    /// what "the search input is vertically cramped" looked like.
    #[test]
    fn the_two_derivation_stages_compose_in_either_order() {
        let spacing = Spacing::new(14.0, 1.0);
        let (row, header, status) = (28, 30, 24);
        let spacing_first = Theme::default()
            .with_spacing(spacing)
            .with_metrics(row, header, status);
        let metrics_first = Theme::default()
            .with_metrics(row, header, status)
            .with_spacing(spacing);
        assert_eq!(
            spacing_first.header_height, metrics_first.header_height,
            "stage order must not change the result"
        );
        for theme in [spacing_first, metrics_first] {
            // Stated against the density-scaled toolbar rather than the guide's
            // raw 58px: the shipped policy tightens the band deliberately, so
            // the guide's number is no longer the floor. What must still hold
            // is that the band is not shorter than the policy asks for *and*
            // still contains its own controls -- which is the property the
            // original bug actually violated.
            assert!(
                theme.header_height >= theme.density.row(spacing.toolbar_height()),
                "the toolbar must never end up shorter than its density asks"
            );
            assert!(
                theme.header_height > theme.control_height(),
                "the toolbar ({}) must still contain its controls ({})",
                theme.header_height,
                theme.control_height()
            );
        }
    }

    /// The invariant that makes the toolbar look right: the band must be able
    /// to contain the controls the guide puts inside it, at any font size.
    ///
    /// This is the assertion that would have caught the crowding directly --
    /// a control is 31px in a 58px band per the guide, so a toolbar shorter
    /// than its own control height is always wrong regardless of how it was
    /// derived.
    #[test]
    fn the_toolbar_always_contains_its_controls() {
        for font in [11.0, 13.0, 14.0, 16.0, 20.0, 28.0] {
            let spacing = Spacing::new(font, 1.0);
            let scale = crate::typography::Typography::load(&config::Config::default(), 1.0);
            let theme = Theme::from_config(&config::Config::default())
                .with_spacing(spacing)
                .with_metrics(
                    scale.natural_row_height(),
                    scale.natural_header_height(),
                    scale.natural_status_height(),
                );
            // Both sides read through the theme, so the band and its contents
            // are compared on the *same* density policy. Comparing a tightened
            // band against the guide's raw control height would fail for a
            // reason that is not a bug -- and comparing a guide band against a
            // tightened control would hide a real one.
            assert!(
                theme.header_height > theme.control_height(),
                "at {font}px the toolbar ({}) must exceed its controls ({})",
                theme.header_height,
                theme.control_height()
            );
        }
    }

    /// Density must scale a band and the controls inside it *together*.
    ///
    /// The ratio is the guide's actual design (a 31px control in a 58px
    /// toolbar); tightening only the band is how a control ends up crowded
    /// against its own edges, which is the bug the toolbar tests above exist
    /// to catch. Checked at both policies so neither can drift.
    #[test]
    fn a_band_and_its_controls_scale_on_the_same_policy() {
        for density in [Density::GUIDE, Density::COMPACT] {
            for font in [11.0, 13.0, 16.0, 20.0, 28.0] {
                let spacing = Spacing::new(font, 1.0);
                let theme = Theme::default().with_spacing_at(spacing, density);
                let slack = theme.header_height - theme.control_height();
                assert!(
                    slack > 0,
                    "the control must fit at {font}px under {density:?}"
                );
                // Not merely fitting: the guide leaves 27 of 58px around a
                // control, so a band that has tightened to a hairline of slack
                // has lost the proportion even though it still "contains" it.
                assert!(
                    slack * 4 >= theme.header_height,
                    "only {slack}px of slack in a {}px band at {font}px under {density:?}",
                    theme.header_height
                );
            }
        }
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

    /// A configured metric equal to the built-in default is still the user's
    /// choice, and must survive both derivation stages.
    ///
    /// The old default-comparison could not express this: `header_height = 24`
    /// was indistinguishable from "unset", so the value was overwritten.
    #[test]
    fn configuring_the_default_value_is_still_respected() {
        let cfg = config::Config::from_bytes(b"[files]\nheader_height = 24\n").expect("parse");
        let theme = Theme::from_config(&cfg)
            .with_spacing(Spacing::new(14.0, 1.0))
            .with_metrics(30, 40, 26);
        assert_eq!(
            theme.header_height, 24,
            "an explicit value must not be mistaken for an unset one"
        );
    }
}

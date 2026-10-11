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
use crate::pinned_card::CardMetrics;
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
    /// Retained as a distinct role from [`Self::sidebar_bg`] because the
    /// config exposes it and a user may have set it; the card mirrors it
    /// by default, so they only diverge if someone asks them to.
    #[allow(dead_code)]
    pub header_bg: Color,
    /// `[files].status_bg`. The status line now sits inside the pinned card
    /// and paints on the card's surface; kept so the documented config key
    /// still parses into a role.
    #[allow(dead_code)]
    pub status_bg: Color,
    pub error: Color,
    /// `[files].scrollbar_track`. The storage meter that used it is gone;
    /// kept so the documented config key still parses into a role.
    #[allow(dead_code)]
    pub scrollbar_track: Color,
    /// `[files].scrollbar_thumb`. The tiles do not scroll; kept so the
    /// documented config key still parses into a role.
    #[allow(dead_code)]
    pub scrollbar_thumb: Color,
    /// Fill behind the filter box while it has focus.
    pub filter_bg: Color,
    pub caret: Color,

    /// Fill behind the pinned chrome card. Slightly off the list background
    /// so the card reads as a separate surface.
    pub sidebar_bg: Color,
    /// The hairline between the sidebar and the list.
    pub sidebar_border: Color,
    /// Fill behind the sidebar row for the directory currently open.
    pub sidebar_active: Color,

    /// The card's surface where the disk is used: the same opacity as
    /// [`Self::sidebar_bg`], a different tone, rising from the card's bottom
    /// edge in proportion to how full the disk is.
    pub card_fill: Color,
    /// Hovered row on [`Self::card_fill`]; [`Self::hover`] is derived for the
    /// plain surface and would nearly vanish on the filled one.
    pub card_fill_hover: Color,
    /// The open place's row on [`Self::card_fill`].
    pub card_fill_active: Color,
    /// The filter box's well on [`Self::card_fill`].
    pub card_fill_input: Color,

    /// The four resting tile fills, drawn at the card's opacity. Each tile
    /// takes the one its place in the colouring assigns, so bordering tiles
    /// differ.
    pub tile_fills: [Color; 4],

    /// Fill under a hovered sidebar row or control (the filter box, a toolbar
    /// button). Tiles do not use it: they answer the pointer with motion.
    pub hover: Color,
    /// Selection fill for a directory tile, tinted toward [`Self::dir`] so the
    /// highlight says what kind of thing is selected.
    pub selection_dir: Color,
    /// Selection fill for a file row.
    pub selection_file: Color,
    /// Outline around toolbar controls (the filter box, the view switch).
    pub control_border: Color,
    /// Outline around a control while it has keyboard focus.
    pub focus_ring: Color,
    /// The accent: active location, selection, and feedback. Used sparingly
    /// -- per the guide it indicates state rather than decorating.
    pub accent: Color,
    /// A quiet wash of the accent, for the fill *behind* accented content:
    /// the active sidebar row and the lit hidden-files toggle.
    ///
    /// Distinct from [`Self::accent`] because the guide uses the accent for
    /// two different jobs -- a 2px rule at full strength, and a background at
    /// a fraction of it. Painting a fill with the full accent would drown the
    /// text sitting on top of it.
    pub accent_surface: Color,
    /// Fill behind a floating panel -- the password modal's card.
    ///
    /// A *raised* surface, so it must sit further from the window background
    /// than any chrome band does. The chrome recedes (see
    /// [`Self::sidebar_bg`], which is derived by stepping *away* from the
    /// foreground); a modal is the opposite, because it is in front of
    /// everything rather than behind it. Painting a floating card in a
    /// receding colour makes it read as a hole cut in the window.
    pub panel_surface: Color,
    /// The outline around a floating panel.
    ///
    /// A distinct role from [`Self::control_border`] because that one is
    /// derived from the same value as the panel fill, which would make the
    /// outline invisible on exactly the surface it is drawn on.
    pub panel_border: Color,
    /// What a modal scrim is made of.
    ///
    /// Black, on every palette, because dimming is dimming: a scrim states
    /// that the window behind it is not accepting input, and the only colour
    /// that darkens both a near-black and a near-white palette is black.
    ///
    /// Emphatically *not* the window background. Blending a colour over
    /// itself is a no-op, so a background-coloured scrim leaves the list
    /// exactly as it was and *lightens* the darker chrome bands -- the
    /// opposite of dimming, and invisible to any test that only asks whether
    /// pixels changed.
    pub panel_scrim: Color,

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
            card_fill: Color::rgb(38, 43, 63),
            card_fill_hover: Color::rgb(55, 60, 81),
            card_fill_active: Color::rgb(65, 70, 92),
            card_fill_input: Color::rgb(60, 65, 86),
            tile_fills: [
                Color::rgb(0x1d, 0x25, 0x36),
                Color::rgb(0x1d, 0x2a, 0x2a),
                Color::rgb(0x28, 0x22, 0x33),
                Color::rgb(0x2b, 0x28, 0x25),
            ],

            hover: Color::rgb(0x28, 0x28, 0x3c),
            // Washes of the type colours over the list background: the same
            // hue the filename already uses, at a fraction that reads as a
            // surface rather than as ink. Hover is the quieter pair, selection
            // the stronger, so a selected row still wins under the pointer.
            selection_dir: Color::rgb(0x33, 0x40, 0x5c),
            selection_file: Color::rgb(0x3a, 0x3d, 0x50),
            control_border: Color::rgb(0x31, 0x32, 0x44),
            focus_ring: Color::rgb(0x89, 0xb4, 0xfa),
            accent: Color::rgb(0x89, 0xb4, 0xfa),
            accent_surface: Color::rgb(0x31, 0x32, 0x44),
            // Lifted off the window background, not dropped below it: the
            // card floats in front of the list.
            panel_surface: Color::rgb(0x2a, 0x2a, 0x3c),
            panel_border: Color::rgb(0x45, 0x47, 0x5a),
            panel_scrim: Color::rgb(0x00, 0x00, 0x00),
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
    /// How far a selection carries its type colour over the background.
    ///
    /// Measured rather than trusted: against this palette's near-black
    /// `#181616` list background a small fraction of a colour is a very small
    /// absolute step, which reads as "slightly less black" rather than as a
    /// highlight.
    const SELECTION_WASH: f32 = 0.30;

    /// How far the modal card is lifted from the window background toward
    /// the text colour.
    ///
    /// Above [`Self::files_filter_bg`]'s 0.13 so the reply field, which uses
    /// that value, still reads as an inset well within the card. Below the
    /// selection wash so the card stays a surface rather than becoming the
    /// brightest thing on screen -- the prompt text is what should draw the
    /// eye, not the box around it.
    ///
    /// [`Self::files_filter_bg`]: config::Config::files_filter_bg
    const PANEL_LIFT: f32 = 0.20;

    /// The card's outline, lifted further still so it reads as an edge.
    ///
    /// A floating panel needs a visible boundary in a way a chrome band does
    /// not: the band is bounded by the window, while the card's edge is the
    /// only thing separating it from the dimmed list behind it.
    const PANEL_BORDER_LIFT: f32 = 0.34;

    /// How far from the sidebar's background toward its *active* fill a hovered
    /// place sits.
    ///
    /// A fraction of the distance between two real endpoints rather than an
    /// absolute strength, so "hover is quieter than active" holds on every
    /// palette. Past the midpoint so the highlight is comfortably visible,
    /// short of 1.0 so it never reads as a second active row.
    const SIDEBAR_HOVER_MIX: f32 = 0.6;

    /// How far the used-space tone leans from the card surface toward the
    /// accent. Enough to read as a different tone, little enough that the
    /// filter box, whose own fill is derived from the same palette, still
    /// stands off it on a full disk.
    const CARD_FILL_MIX: f32 = 0.12;

    /// Row washes over the used-space tone: ink mixed into the tone itself,
    /// so the ordering plain < hover < active holds on whichever tone a row
    /// happens to sit.
    const CARD_FILL_HOVER_MIX: f32 = 0.10;
    const CARD_FILL_ACTIVE_MIX: f32 = 0.16;
    const CARD_FILL_INPUT_MIX: f32 = 0.14;

    /// Mix `ink` into `surface` by `t`, as a paint colour.
    ///
    /// Named rather than inlined because it is applied four times with two
    /// strengths, and the thing that must hold is that all four use the *same*
    /// derivation -- otherwise the directory and file highlights end up on
    /// subtly different curves and only one of them looks right.
    ///
    /// `ink` is first re-scaled to the brightness of `reference` so that the
    /// directory and file highlights differ in *hue* rather than in lightness.
    /// Without this the two washes are two greys at different brightness --
    /// which the eye reads as one colour at two opacities, not as two colours.
    /// This palette is the case in point: `dir` (`#8ba4b0`) is noticeably
    /// darker than `text` (`#c5c9c5`), so at the same wash strength the file
    /// fill simply came out lighter than the directory fill, and the type
    /// distinction was invisible on screen even though the values differed.
    fn wash(surface: config::Color, ink: config::Color, reference: config::Color, t: f32) -> Color {
        surface.mix(ink.at_luma_of(reference), t).into()
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
        let card_fill = cfg
            .files_header_bg()
            .mix(cfg.files_cursor(), Self::CARD_FILL_MIX);
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
            card_fill: card_fill.into(),
            card_fill_hover: card_fill
                .mix(cfg.files_text(), Self::CARD_FILL_HOVER_MIX)
                .into(),
            card_fill_active: card_fill
                .mix(cfg.files_text(), Self::CARD_FILL_ACTIVE_MIX)
                .into(),
            card_fill_input: card_fill
                .mix(cfg.files_text(), Self::CARD_FILL_INPUT_MIX)
                .into(),
            tile_fills: std::array::from_fn(|slot| cfg.files_tile_color(slot).into()),

            // Hover sits *between* the sidebar's own background and its active
            // fill, and is derived by mixing those two rather than by washing
            // a third colour over the background.
            //
            // That is the whole point: a wash of some ink at a fixed strength
            // has no relationship to where `sidebar_active` happens to land, so
            // it can overshoot it. On this palette it did -- the active fill is
            // only thirty levels above a near-black sidebar, while a 0.22 wash
            // of the text colour reached forty, making a merely-hovered row
            // brighter than the row marking where you actually are. Mixing
            // between the two endpoints makes the ordering hold by
            // construction, on any palette, instead of being a property of one
            // tuned constant.
            hover: cfg
                .files_header_bg()
                .mix(cfg.files_filter_bg(), Self::SIDEBAR_HOVER_MIX)
                .into(),

            // Row highlights are washes of the *type* colour over the list
            // background, so the highlight carries the same information the
            // filename's colour does -- which matters most exactly when the
            // highlight is covering that filename up.
            //
            // Derived rather than configured: these follow `[files].dir` and
            // `[files].text`, so re-theming those moves the highlights with
            // them instead of stranding four hand-picked shades. The two
            // strengths keep selection ahead of hover on the same row.
            // `files_text` is the brightness reference for both kinds, so the
            // directory ink contributes only its hue and the two fills sit at
            // one lightness -- see `Self::wash`.
            selection_dir: Self::wash(
                cfg.files_background(),
                cfg.files_dir(),
                cfg.files_text(),
                Self::SELECTION_WASH,
            ),
            selection_file: Self::wash(
                cfg.files_background(),
                cfg.files_text(),
                cfg.files_text(),
                Self::SELECTION_WASH,
            ),
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

            // The modal card is lifted *toward* the foreground, while the
            // chrome bands recede away from it. Same palette, opposite
            // direction, because a floating panel and a background strip are
            // opposite things: one is in front of the list and one is behind
            // it. Derived from the same two endpoints as every other surface
            // so a re-themed palette carries the modal with it.
            //
            // Placed past `filter_bg` (0.13) so the reply field stays
            // readable as an inset well *within* the card rather than
            // dissolving into it.
            panel_surface: cfg
                .files_background()
                .mix(cfg.files_text(), Self::PANEL_LIFT)
                .into(),
            panel_border: cfg
                .files_background()
                .mix(cfg.files_text(), Self::PANEL_BORDER_LIFT)
                .into(),
            // Black on every palette: see the field's documentation for why
            // the window background is the one colour this must not be.
            panel_scrim: Color::rgb(0, 0, 0),
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

    /// Height of a toolbar control (the filter box), packed to this theme's
    /// density.
    ///
    /// Exists so call sites cannot reach for the raw `spacing.control_height()`
    /// and land a guide-sized control in a density-tightened band. Reading the
    /// height through the theme keeps the band and its contents on one policy.
    #[must_use]
    pub fn control_height(&self) -> i32 {
        self.density.row(self.spacing.control_height())
    }

    /// The section heights the pinned card asks for. The navigation row is
    /// never shorter than a button plus a gap, whatever `header_height` says,
    /// because a shorter row would drop every button.
    #[must_use]
    pub fn card_metrics(&self) -> CardMetrics {
        let button_row = self.spacing.nav_button_height() + self.spacing.s();
        CardMetrics {
            nav: self.header_height.max(button_row),
            crumbs: self.sidebar_row(),
            filter: self.control_height() + self.spacing.m(),
            storage: self.spacing.storage_height(),
        }
    }

    /// A sidebar navigation row, packed to this theme's density.
    #[must_use]
    pub fn sidebar_row(&self) -> i32 {
        self.density.row(self.spacing.sidebar_row())
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
            assert_ne!(t.sidebar_bg, t.background, "the chrome card");
            assert_ne!(t.card_fill, t.sidebar_bg, "the used-space tone");
            assert_ne!(
                t.card_fill_input, t.card_fill,
                "the filter box on a full disk"
            );
            assert_ne!(t.control_border, t.sidebar_bg, "a control outline");
            assert_ne!(t.focus_ring, t.sidebar_bg, "the focus ring");
            assert_ne!(t.accent, t.background, "the accent");
            assert_ne!(t.accent_surface, t.background, "the accent wash");
            // Disabled must be distinguishable from live muted text, or a
            // working timestamp reads as a dead control.
            assert_ne!(t.disabled, t.muted, "disabled vs muted");
            assert_ne!(t.disabled, t.sidebar_bg, "a disabled glyph");
        }
    }

    /// The two kinds must be told apart by *hue*, not by brightness.
    ///
    /// The shipped bug this pins: `[files].dir` is darker than `[files].text`
    /// on this palette, so washing each at the same strength produced two
    /// greys of different lightness -- which reads as one colour at two
    /// opacities rather than as two colours, and the type distinction was
    /// invisible on screen even though the stored values differed.
    ///
    /// Asserted as a blue-over-red *lean* rather than a literal pair of
    /// colours, so the property survives a re-themed palette: whatever the
    /// hues are, the directory fill must carry the directory ink's cast and
    /// the file fill must not, at a brightness the two share.
    /// A configured header shorter than the buttons must not drop them.
    #[test]
    fn the_card_navigation_row_always_holds_its_buttons() {
        let cfg = config::Config::from_bytes(b"[files]\nheader_height = 10\n").expect("parse");
        let theme = Theme::from_config(&cfg).with_spacing(Spacing::new(13.0, 1.0));
        assert!(theme.card_metrics().nav >= theme.spacing.nav_button_height());
    }

    #[test]
    fn the_four_tile_fills_are_distinct_and_follow_config() {
        let plain = Theme::from_config(&config::Config::default());
        for (i, a) in plain.tile_fills.iter().enumerate() {
            for b in &plain.tile_fills[i + 1..] {
                assert_ne!(a, b);
            }
            assert_ne!(*a, plain.background, "a tile against the window");
        }
        let cfg =
            config::Config::from_bytes(b"[files]\ntile_color_3 = \"#102030\"\n").expect("parse");
        let themed = Theme::from_config(&cfg);
        assert_eq!(themed.tile_fills[2], Color::rgb(0x10, 0x20, 0x30));
        assert_eq!(themed.tile_fills[0], plain.tile_fills[0]);
    }

    #[test]
    fn the_selection_fill_stands_off_every_resting_fill() {
        let luma = |c: Color| config::Color::from_rgb(c.r, c.g, c.b).luma();
        for toml in [
            "",
            "[colors]\nbackground = \"#181616\"\nforeground = \"#c5c9c5\"\n",
        ] {
            let cfg = config::Config::from_bytes(toml.as_bytes()).expect("parse");
            let t = Theme::from_config(&cfg);
            for fill in t.tile_fills {
                assert!(
                    luma(t.selection_file) > luma(fill),
                    "selection ({}) must be brighter than a resting tile ({}) for {toml:?}",
                    luma(t.selection_file),
                    luma(fill)
                );
            }
        }
    }

    /// The sidebar's three row states must read in order: plain background,
    /// then hover, then the active place. Asserted on *brightness* rather than
    /// on inequality, because the failure this exists to catch is not "two
    /// colours are the same" but "the middle one overshot the top one" -- which
    /// is exactly what a fixed-strength wash did here, making a merely-hovered
    /// row brighter than the row marking where you actually are.
    #[test]
    fn the_sidebar_row_states_get_brighter_in_order() {
        // Across a spread of palettes, not just the default: the ordering is
        // meant to hold by construction, and a single palette cannot show that.
        // Reuse the real luma rather than restating the coefficients, so this
        // cannot drift from the code it checks.
        let luma = |c: Color| config::Color::from_rgb(c.r, c.g, c.b).luma();
        for toml in [
            "",
            "[colors]\nbackground = \"#181616\"\nforeground = \"#c5c9c5\"\n",
            "[colors]\nbackground = \"#ffffff\"\nforeground = \"#000000\"\n",
            "[files]\nheader_bg = \"#000000\"\nfilter_bg = \"#0a0a0a\"\n",
        ] {
            let cfg = config::Config::from_bytes(toml.as_bytes()).expect("valid config");
            let t = Theme::from_config(&cfg);
            let bg = luma(t.sidebar_bg);
            let hover = luma(t.hover);
            let active = luma(t.sidebar_active);
            assert!(
                (bg - hover).abs() > 0.5,
                "hover must be visible against the sidebar (bg {bg}, hover {hover}) for {toml:?}"
            );
            assert!(
                (hover - bg).abs() < (active - bg).abs(),
                "hover must sit nearer the background than the active fill \
                 (bg {bg}, hover {hover}, active {active}) for {toml:?}"
            );
        }
    }

    #[test]
    fn the_two_row_kinds_differ_in_hue_not_lightness() {
        // A cool directory ink and a near-neutral text ink at clearly
        // different brightness -- the shape of the real palette.
        let cfg = config::Config::from_bytes(
            b"[files]\nbackground = \"#181616\"\ndir = \"#8ba4b0\"\ntext = \"#c5c9c5\"\n",
        )
        .expect("parse");
        let t = Theme::from_config(&cfg);
        {
            let (dir_fill, file_fill, what) = (t.selection_dir, t.selection_file, "selection");
            let lean = |c: Color| i32::from(c.b) - i32::from(c.r);
            assert!(
                lean(dir_fill) > lean(file_fill) + 4,
                "the {what} fills must separate by hue: dir lean {} vs file lean {}",
                lean(dir_fill),
                lean(file_fill)
            );
            // Reuse the real luma rather than restating the coefficients, so
            // this cannot drift from the implementation it is checking.
            let luma = |c: Color| config::Color::from_rgb(c.r, c.g, c.b).luma();
            assert!(
                (luma(dir_fill) - luma(file_fill)).abs() < 6.0,
                "the {what} fills must sit at one brightness: {} vs {}",
                luma(dir_fill),
                luma(file_fill)
            );
        }
    }

    /// The bug this pins: the scrim was painted in the window background, and
    /// blending a colour over itself changes nothing. The list stayed exactly
    /// as bright while the darker chrome bands were *lightened* -- the
    /// opposite of dimming. A test that only asked whether pixels changed
    /// passed on it.
    #[test]
    fn the_scrim_darkens_every_surface_it_covers() {
        let luma = |c: Color| config::Color::from_rgb(c.r, c.g, c.b).luma();
        for toml in [
            "",
            "[colors]\nbackground = \"#181616\"\nforeground = \"#c5c9c5\"\n",
            "[colors]\nbackground = \"#ffffff\"\nforeground = \"#000000\"\n",
        ] {
            let cfg = config::Config::from_bytes(toml.as_bytes()).expect("valid config");
            let t = Theme::from_config(&cfg);
            let scrim = luma(t.panel_scrim);
            for (surface, name) in [
                (t.background, "the list"),
                (t.sidebar_bg, "the chrome card"),
                (t.status_bg, "the status strip"),
            ] {
                assert!(
                    scrim < luma(surface),
                    "the scrim ({scrim}) must darken {name} ({}) for {toml:?}",
                    luma(surface)
                );
            }
        }
    }

    /// A floating card must sit in *front* of the window, and the chrome
    /// bands recede behind it. Deriving the card from a chrome colour made it
    /// darker than the list it floats over, so it read as a hole rather than
    /// a raised dialog.
    #[test]
    fn the_modal_card_is_lifted_while_the_chrome_recedes() {
        let luma = |c: Color| config::Color::from_rgb(c.r, c.g, c.b).luma();
        for toml in [
            "",
            "[colors]\nbackground = \"#181616\"\nforeground = \"#c5c9c5\"\n",
            "[colors]\nbackground = \"#ffffff\"\nforeground = \"#000000\"\n",
            "[files]\nbackground = \"#202020\"\ntext = \"#f0f0f0\"\n",
        ] {
            let cfg = config::Config::from_bytes(toml.as_bytes()).expect("valid config");
            let t = Theme::from_config(&cfg);
            let lift = (luma(t.panel_surface) - luma(t.background)).abs();
            let recede = (luma(t.sidebar_bg) - luma(t.background)).abs();
            assert!(
                lift > recede,
                "the card ({lift} from the background) must stand off further \
                 than the chrome ({recede}) for {toml:?}"
            );
        }
    }

    /// The reply field is drawn *inside* the card, so it must be
    /// distinguishable from it -- otherwise the box the user types into has
    /// no visible edge at all.
    #[test]
    fn the_reply_field_stays_visible_against_the_card() {
        let luma = |c: Color| config::Color::from_rgb(c.r, c.g, c.b).luma();
        for toml in [
            "",
            "[colors]\nbackground = \"#181616\"\nforeground = \"#c5c9c5\"\n",
            "[colors]\nbackground = \"#ffffff\"\nforeground = \"#000000\"\n",
        ] {
            let cfg = config::Config::from_bytes(toml.as_bytes()).expect("valid config");
            let t = Theme::from_config(&cfg);
            assert_ne!(t.filter_bg, t.panel_surface, "for {toml:?}");
            assert!(
                (luma(t.panel_surface) - luma(t.filter_bg)).abs() > 3.0,
                "the field ({}) must separate from the card ({}) for {toml:?}",
                luma(t.filter_bg),
                luma(t.panel_surface)
            );
            // The card's own edge is the only boundary against the dimmed
            // list behind it.
            assert!(
                (luma(t.panel_border) - luma(t.panel_surface)).abs() > 3.0,
                "the card outline must be visible on the card for {toml:?}"
            );
        }
    }

    /// The shipped palette, stated in numbers rather than trusted to the eye.
    ///
    /// Pins the *ordering* against the real Kanagawa Dragon config, so a
    /// change to any derivation that inverted the stack here would fail with
    /// the actual figures rather than a vague "looks wrong".
    #[test]
    fn the_modal_stacks_correctly_on_the_shipped_palette() {
        let cfg = config::Config::from_bytes(
            b"[colors]\nforeground = \"#c5c9c5\"\nbackground = \"#181616\"\n",
        )
        .expect("parse");
        let t = Theme::from_config(&cfg);
        let luma = |c: Color| config::Color::from_rgb(c.r, c.g, c.b).luma();
        // Darkest to brightest: scrim, chrome, list, field, card, outline.
        let stack = [
            ("scrim", luma(t.panel_scrim)),
            ("chrome", luma(t.sidebar_bg)),
            ("list", luma(t.background)),
            ("field", luma(t.filter_bg)),
            ("card", luma(t.panel_surface)),
            ("outline", luma(t.panel_border)),
        ];
        for pair in stack.windows(2) {
            let (below, above) = (pair[0], pair[1]);
            assert!(
                below.1 < above.1,
                "{} ({:.1}) must sit below {} ({:.1})",
                below.0,
                below.1,
                above.0,
                above.1
            );
        }
    }

    /// The modal must follow a re-themed palette like every other surface,
    /// rather than stranding a hand-picked colour.
    #[test]
    fn the_modal_follows_a_retheme() {
        let dark =
            config::Config::from_bytes(b"[colors]\nbackground = \"#181616\"\n").expect("parse");
        let light =
            config::Config::from_bytes(b"[colors]\nbackground = \"#fdf6e3\"\n").expect("parse");
        assert_ne!(
            Theme::from_config(&dark).panel_surface,
            Theme::from_config(&light).panel_surface
        );
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

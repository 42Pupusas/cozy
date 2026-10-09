//! [`Typography`] -- the browser's type scale.
//!
//! The browser used to draw every string with the same 8x16 bitmap font, so a
//! section heading, a filename and a byte count were typographically
//! identical and the eye had nothing to latch onto. `bins/monitor` solves
//! this with a small set of named tiers sized as multiples of the configured
//! font size; this is the same idea with the tiers a file browser needs.
//!
//! Four tiers, each with a stated job:
//!
//! - [`Typography::title`] -- the path in the header. The most important
//!   string on screen: it tells you where you are.
//! - [`Typography::body`] -- filenames, the reading size.
//! - [`Typography::label`] -- sidebar section headings. Small, and drawn in
//!   the muted colour, so a heading reads as a divider rather than an entry.
//! - [`Typography::detail`] -- sizes, counts, the status line. Subordinate
//!   information that must not compete with filenames.
//!
//! Sharing one font file across tiers matters: opening a large TTF four times
//! during window creation is measurable startup cost, so the bytes are read
//! once and handed to each renderer.

use statusbar::TextRenderer;

use crate::density::Density;
use crate::tile_geometry::LineHeights;

/// The four text renderers the browser draws with.
pub struct Typography {
    pub title: TextRenderer,
    pub body: TextRenderer,
    pub label: TextRenderer,
    pub detail: TextRenderer,
}

impl Typography {
    /// Title size, as a multiple of the configured font size.
    const TITLE: f32 = 1.15;
    /// Body size: the baseline, so filenames render at exactly the size the
    /// user configured.
    const BODY: f32 = 1.0;
    /// Section headings. Small enough to read as structure, not content.
    const LABEL: f32 = 0.78;
    /// Sizes and counts.
    const DETAIL: f32 = 0.85;

    /// Load every tier from the configured font, at `scale` (the output's
    /// `HiDPI` scale factor), under the shipped [`Density`].
    ///
    /// Falls back to the embedded bitmap font per tier if the configured face
    /// cannot be read -- a missing font must degrade to a readable window,
    /// never a blank one.
    #[must_use]
    pub fn load(cfg: &config::Config, scale: f32) -> Self {
        Self::load_with_density(cfg, scale, Density::default())
    }

    /// [`Self::load`] with the density stated explicitly, so a test can load
    /// the guide's own sizes and compare against them.
    #[must_use]
    pub fn load_with_density(cfg: &config::Config, scale: f32, density: Density) -> Self {
        let size = cfg.font.px_size() * scale;
        let path = cfg.font.path.clone();
        let emoji = cfg.font.emoji_path.clone();
        // Read the face once and share the bytes: four independent `load`
        // calls would parse the same multi-megabyte file four times while the
        // window waits to show its first frame.
        let data = path.as_deref().and_then(|p| std::fs::read(p).ok());
        let fallbacks = [emoji.as_deref()];
        // Density scales every tier by one factor, so the *relative* scale
        // the tests pin is preserved while the whole list gets smaller.
        let tier = |mult: f32| {
            let mult = density.text(mult);
            match (path.as_deref(), data.as_deref()) {
                // The happy path: one read, four faces off the same bytes.
                (Some(p), Some(bytes)) => {
                    TextRenderer::load_with_fallbacks_from_bytes(p, bytes, &fallbacks, size * mult)
                }
                // No configured font, or it could not be read.
                // `load_with_fallbacks` owns the diagnostic and the
                // bitmap-font fallback, so the window still comes up readable
                // instead of blank.
                _ => TextRenderer::load_with_fallbacks(path.as_deref(), &fallbacks, size * mult),
            }
        };
        Self {
            title: tier(Self::TITLE),
            body: tier(Self::BODY),
            label: tier(Self::LABEL),
            detail: tier(Self::DETAIL),
        }
    }

    /// The natural row height for list rows at this scale.
    ///
    /// Derived from the body tier plus breathing room rather than fixed in
    /// the theme, so raising the font size in config grows the rows with it
    /// instead of clipping the glyphs.
    ///
    /// This is a *floor* (see `Theme::with_metrics`), so the air added here
    /// is the minimum a row needs to not clip its own descenders -- 4px,
    /// tight on purpose. The comfortable spacing is the guide's job; this
    /// only has to keep glyphs off the separator.
    #[must_use]
    pub fn natural_row_height(&self) -> i32 {
        self.body.line_height() + 4
    }

    /// The natural header height: the title tier plus generous padding, since
    /// the header is the one strip that should feel deliberately roomy.
    #[must_use]
    pub fn natural_header_height(&self) -> i32 {
        self.title.line_height() + 14
    }

    /// The line heights a tile lays its text out with.
    #[must_use]
    pub fn tile_lines(&self) -> LineHeights {
        LineHeights {
            title: self.title.line_height(),
            body: self.body.line_height(),
            detail: self.detail.line_height(),
        }
    }

    /// The natural status-strip height.
    #[must_use]
    pub fn natural_status_height(&self) -> i32 {
        self.detail.line_height() + 8
    }
}

#[cfg(test)]
mod tests {
    use super::Typography;

    /// The tiers must be genuinely distinct, in the stated order. If two
    /// collapse to the same pixel size the whole point of the scale is lost.
    #[test]
    fn the_tiers_are_ordered_and_distinct() {
        const {
            assert!(
                Typography::LABEL < Typography::DETAIL,
                "headings are the smallest tier"
            );
            assert!(
                Typography::DETAIL < Typography::BODY,
                "detail must not compete with filenames"
            );
            assert!(
                Typography::BODY < Typography::TITLE,
                "the path is the most prominent string"
            );
        }
    }

    /// Body is the baseline: a configured size of N must render filenames at
    /// N, not at some multiple the user did not ask for.
    #[test]
    fn body_is_exactly_the_configured_size() {
        assert!((Typography::BODY - 1.0).abs() < f32::EPSILON);
    }

    /// Every multiplier must be positive and sane -- a zero or negative one
    /// would produce an invisible or nonsensical face.
    #[test]
    fn every_multiplier_is_a_sane_positive_scale() {
        for mult in [
            Typography::TITLE,
            Typography::BODY,
            Typography::LABEL,
            Typography::DETAIL,
        ] {
            assert!(mult > 0.0, "a tier must have positive size");
            assert!(mult < 4.0, "a tier that large would break every layout");
        }
    }

    /// Derived metrics must leave room around the glyphs: a row exactly as
    /// tall as its line height clips descenders and looks cramped.
    #[test]
    fn derived_metrics_leave_breathing_room_around_the_glyphs() {
        let t = Typography::load(&config::Config::default(), 1.0);
        assert!(t.natural_row_height() > t.body.line_height());
        assert!(t.natural_header_height() > t.title.line_height());
        assert!(t.natural_status_height() > t.detail.line_height());
    }

    /// The header must be the tallest strip -- it carries the largest tier.
    #[test]
    fn the_header_is_taller_than_a_list_row() {
        let t = Typography::load(&config::Config::default(), 1.0);
        assert!(t.natural_header_height() > t.natural_row_height());
    }

    /// A missing font file must still produce a usable window rather than
    /// panicking or yielding zero-height rows.
    #[test]
    fn a_missing_font_degrades_to_a_readable_fallback() {
        let cfg = config::Config::from_bytes(b"[font]\npath = \"/nonexistent/font.ttf\"\n")
            .expect("parse");
        let t = Typography::load(&cfg, 1.0);
        assert!(t.body.line_height() > 0);
        assert!(t.natural_row_height() > 0);
    }
}

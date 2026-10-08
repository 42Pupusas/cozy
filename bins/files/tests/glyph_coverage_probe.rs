//! Every glyph the chrome draws must be visible in the face that draws it.
//!
//! # Why this is a test and not a comment
//!
//! A missing glyph is not an error anywhere in the stack. `cmap` returns
//! `.notdef`, the rasteriser draws `.notdef`'s empty box, and the window comes
//! up looking fine to every automated check we have -- the user sees tofu.
//! Nothing else in this workspace would catch it, and it is exactly what
//! happened: the view toggle's grid segment was `\u{22ee}`, which the shipped
//! face does not have.
//!
//! # The two failure modes
//!
//! They are different and both matter:
//!
//! - **Unmapped.** The face has no glyph, so `.notdef` is drawn: a hollow box.
//! - **Blank.** The face has a glyph whose outline rasterizes to no ink at the
//!   size actually used. A hairline detail can survive at 18px and vanish at
//!   12px, so coverage must be checked at the *tier* sizes, not at one size.
//!
//! A third mode has no test here because no test can have it: a glyph that is
//! present, inked, and simply *looks* wrong. `\u{25a1}` WHITE SQUARE was that
//! -- a hollow box, pixel-for-pixel the shape a missing glyph draws. Choosing
//! a filled shape instead is a judgement, and the comment at the call site is
//! where it lives.
//!
//! When no font is configured the probe reports what it skipped rather than
//! passing silently: a green tick that checked nothing is worse than a skip.

/// The pixel sizes the browser actually rasterizes chrome at.
///
/// Mirrors `Typography`'s tier multipliers and `Density::COMPACT`'s text
/// factor. Duplicated rather than imported because those live in the binary
/// crate, which an integration test cannot reach -- and a drift between the
/// two costs a false pass, not a false failure.
struct TierSizes;

impl TierSizes {
    /// `Density::COMPACT.text`, applied to every tier.
    const DENSITY: f32 = 0.86;

    /// Each tier's name and its multiple of the configured font size.
    const TIERS: [(&'static str, f32); 4] = [
        ("title", 1.15),
        ("body", 1.0),
        ("label", 0.78),
        ("detail", 0.85),
    ];

    /// The pixel sizes to check, given the configured base size.
    fn all(base: f32) -> Vec<(&'static str, f32)> {
        Self::TIERS
            .iter()
            .map(|(name, mult)| (*name, base * mult * Self::DENSITY))
            .collect()
    }
}

/// Every glyph the browser's chrome draws, with what it is for.
///
/// The table is the point: a glyph that is not listed here is not checked,
/// so adding a button means adding its glyph here too.
struct ChromeGlyphs;

impl ChromeGlyphs {
    /// `(what it draws, the string drawn)`.
    const ALL: [(&'static str, &'static str); 6] = [
        ("back chevron", "\u{2039}"),
        ("forward chevron", "\u{203a}"),
        ("up arrow", "\u{2191}"),
        ("hidden toggle, on", "\u{25cf}"),
        ("hidden toggle, off", "\u{25cb}"),
        ("tile labels", "\u{2026}"),
    ];
}

/// The configured face, and the questions worth asking of it.
struct ConfiguredFace {
    path: Option<String>,
    base_px: f32,
    emoji_path: Option<String>,
}

impl ConfiguredFace {
    /// Read what `bins/files` would load for its chrome.
    fn load() -> Self {
        let cfg = config::Config::load().unwrap_or_default();
        Self {
            path: cfg.font.path.clone(),
            base_px: cfg.font.px_size(),
            emoji_path: cfg.font.emoji_path,
        }
    }

    /// Whether a face is configured at all.
    const fn is_configured(&self) -> bool {
        self.path.is_some()
    }

    /// What was configured, for a skip message that names the gap.
    fn describe(&self) -> String {
        self.path
            .clone()
            .unwrap_or_else(|| "<no [font].path configured>".to_owned())
    }

    /// Load the primary face and the emoji fallback at `px`.
    ///
    /// Both, because the renderer consults the fallback for what the primary
    /// lacks -- checking only the primary would report a glyph as missing
    /// that the user can actually see.
    fn faces_at(&self, px: f32) -> Option<(font::FontFace, Option<font::FontFace>)> {
        let primary = font::FontFace::load(self.path.as_deref()?, px).ok()?;
        let fallback = self
            .emoji_path
            .as_deref()
            .and_then(|p| font::FontFace::load(p, px).ok());
        Some((primary, fallback))
    }

    /// Characters of `text` that no available face maps.
    fn unmapped(
        primary: &font::FontFace,
        fallback: Option<&font::FontFace>,
        text: &str,
    ) -> Vec<char> {
        text.chars()
            .filter(|c| {
                !primary.has_glyph(*c as u32) && !fallback.is_some_and(|f| f.has_glyph(*c as u32))
            })
            .collect()
    }

    /// Characters of `text` that rasterize to no ink at all.
    fn inkless(
        primary: &mut font::FontFace,
        fallback: Option<&mut font::FontFace>,
        text: &str,
    ) -> Vec<char> {
        let mut fallback = fallback;
        text.chars()
            .filter(|c| {
                let code = *c as u32;
                let glyph = if primary.has_glyph(code) {
                    primary.rasterize(code)
                } else if let Some(f) = fallback.as_deref_mut().filter(|f| f.has_glyph(code)) {
                    f.rasterize(code)
                } else {
                    primary.rasterize(code)
                };
                glyph.bitmap.data.iter().all(|byte| *byte == 0)
            })
            .collect()
    }
}

/// No chrome glyph may be missing from the face that will draw it.
///
/// This is the check that would have caught `\u{22ee}` in the view toggle.
#[test]
fn every_chrome_glyph_exists_in_the_configured_face() {
    let cfg = ConfiguredFace::load();
    if !cfg.is_configured() {
        println!("skipped: {}", cfg.describe());
        return;
    }
    let Some((primary, fallback)) = cfg.faces_at(cfg.base_px) else {
        println!("skipped: {} could not be loaded", cfg.describe());
        return;
    };
    let mut absent = Vec::new();
    for (what, text) in ChromeGlyphs::ALL {
        let missing = ConfiguredFace::unmapped(&primary, fallback.as_ref(), text);
        if !missing.is_empty() {
            absent.push(format!("{what}: {missing:?}"));
        }
    }
    assert!(
        absent.is_empty(),
        "these glyphs would render as tofu in {}:\n  {}",
        cfg.describe(),
        absent.join("\n  ")
    );
}

/// No chrome glyph may rasterize to nothing at any size it is drawn at.
///
/// Separate from the mapping check because the symptom differs: an unmapped
/// codepoint draws a box, while an inkless one draws a blank button. Both
/// are bugs; only this one depends on the tier size.
#[test]
fn every_chrome_glyph_has_ink_at_every_tier_size() {
    let cfg = ConfiguredFace::load();
    if !cfg.is_configured() {
        println!("skipped: {}", cfg.describe());
        return;
    }
    let mut blank = Vec::new();
    for (tier, px) in TierSizes::all(cfg.base_px) {
        let Some((mut primary, mut fallback)) = cfg.faces_at(px) else {
            continue;
        };
        for (what, text) in ChromeGlyphs::ALL {
            let empty = ConfiguredFace::inkless(&mut primary, fallback.as_mut(), text);
            if !empty.is_empty() {
                blank.push(format!("{tier} ({px:.1}px) {what}: {empty:?}"));
            }
        }
    }
    assert!(
        blank.is_empty(),
        "these glyphs rasterize to no ink, so their control paints blank:\n  {}",
        blank.join("\n  ")
    );
}

/// Report the whole table, so `--nocapture` answers "what does this face do
/// with the chrome" without a throwaway binary.
#[test]
fn report_chrome_glyph_coverage() {
    let cfg = ConfiguredFace::load();
    if !cfg.is_configured() {
        println!("skipped: {}", cfg.describe());
        return;
    }
    println!("face: {} at {:.1}px", cfg.describe(), cfg.base_px);
    for (tier, px) in TierSizes::all(cfg.base_px) {
        let Some((mut primary, mut fallback)) = cfg.faces_at(px) else {
            continue;
        };
        let mut row = Vec::new();
        for (_, text) in ChromeGlyphs::ALL {
            for ch in text.chars() {
                let ink = {
                    let code = ch as u32;
                    let glyph = if primary.has_glyph(code) {
                        primary.rasterize(code)
                    } else if let Some(f) = fallback.as_mut().filter(|f| f.has_glyph(code)) {
                        f.rasterize(code)
                    } else {
                        primary.rasterize(code)
                    };
                    glyph.bitmap.data.iter().filter(|b| **b > 0).count()
                };
                row.push(format!("{ch}={ink}"));
            }
        }
        println!("  {tier:<7} {px:>5.1}px  {}", row.join(" "));
    }
}

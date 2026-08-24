//! A per-mode account of a face's block-glyph coverage.

use crate::glyph_coverage::GlyphCoverage;
use crate::subcell_grid::SubcellGrid;
use font::FontFace;

/// What one subcell mode costs on one face.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeCoverage {
    pub mode: SubcellGrid,
    pub required: usize,
    pub missing: Vec<char>,
}

impl ModeCoverage {
    /// Measure `mode` against `face`.
    #[must_use]
    pub fn measure(mode: SubcellGrid, face: &FontFace) -> Self {
        let coverage = GlyphCoverage::new(mode);
        Self {
            mode,
            required: coverage.required().len(),
            missing: coverage.missing(face),
        }
    }

    /// Whether the face draws every glyph the mode needs.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.missing.is_empty()
    }

    /// Glyphs the face supplies.
    #[must_use]
    pub const fn present(&self) -> usize {
        self.required - self.missing.len()
    }

    /// A one-line verdict.
    #[must_use]
    pub fn summary(&self) -> String {
        let name = self.mode.name();
        let present = self.present();
        let required = self.required;
        if self.is_complete() {
            return format!("{name:<9} {present}/{required} complete");
        }
        let sample: String = self.missing.iter().take(8).collect();
        format!(
            "{name:<9} {present}/{required} MISSING {} e.g. {sample}",
            self.missing.len()
        )
    }
}

/// Every mode measured against one face.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontReport {
    pub path: String,
    pub modes: Vec<ModeCoverage>,
}

impl FontReport {
    /// Measure every mode against `face`, loaded from `path`.
    #[must_use]
    pub fn measure(path: impl Into<String>, face: &FontFace) -> Self {
        Self {
            path: path.into(),
            modes: GlyphCoverage::modes()
                .into_iter()
                .map(|mode| ModeCoverage::measure(mode, face))
                .collect(),
        }
    }

    /// The widest mode the face draws completely.
    #[must_use]
    pub fn best_complete(&self) -> Option<SubcellGrid> {
        self.modes
            .iter()
            .find(|coverage| coverage.is_complete())
            .map(|coverage| coverage.mode)
    }

    /// The full report, one mode per line.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = format!("font {}\n", self.path);
        for coverage in &self.modes {
            out.push_str("  ");
            out.push_str(&coverage.summary());
            out.push('\n');
        }
        match self.best_complete() {
            Some(mode) => {
                out.push_str("  -> use --cells ");
                out.push_str(mode.name());
                out.push('\n');
            }
            None => out.push_str("  -> no mode is complete on this face\n"),
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::{FontReport, ModeCoverage};
    use crate::subcell_grid::SubcellGrid;

    fn complete(mode: SubcellGrid, required: usize) -> ModeCoverage {
        ModeCoverage {
            mode,
            required,
            missing: Vec::new(),
        }
    }

    fn broken(mode: SubcellGrid, required: usize, missing: Vec<char>) -> ModeCoverage {
        ModeCoverage {
            mode,
            required,
            missing,
        }
    }

    #[test]
    fn a_complete_mode_reports_every_glyph_present() {
        let coverage = complete(SubcellGrid::Half, 3);
        assert!(coverage.is_complete());
        assert_eq!(coverage.present(), 3);
        assert!(coverage.summary().contains("complete"));
    }

    #[test]
    fn a_broken_mode_counts_and_names_the_gap() {
        let coverage = broken(SubcellGrid::Sextant, 63, vec!['\u{1FB00}', '\u{1FB01}']);
        assert!(!coverage.is_complete());
        assert_eq!(coverage.present(), 61);
        let summary = coverage.summary();
        assert!(summary.contains("MISSING 2"), "{summary}");
    }

    #[test]
    fn the_best_complete_mode_is_the_widest_one() {
        let report = FontReport {
            path: "f.ttf".to_owned(),
            modes: vec![
                broken(SubcellGrid::Sextant, 63, vec!['\u{1FB00}']),
                complete(SubcellGrid::Quadrant, 15),
                complete(SubcellGrid::Half, 3),
            ],
        };
        assert_eq!(report.best_complete(), Some(SubcellGrid::Quadrant));
        assert!(report.render().contains("--cells quadrant"));
    }

    #[test]
    fn a_face_with_no_complete_mode_says_so() {
        let report = FontReport {
            path: "f.ttf".to_owned(),
            modes: vec![
                broken(SubcellGrid::Sextant, 63, vec!['\u{1FB00}']),
                broken(SubcellGrid::Quadrant, 15, vec!['\u{2598}']),
                broken(SubcellGrid::Half, 3, vec!['\u{2580}']),
            ],
        };
        assert_eq!(report.best_complete(), None);
        assert!(report.render().contains("no mode is complete"));
    }

    #[test]
    fn the_report_names_the_face_and_every_mode() {
        let report = FontReport {
            path: "/usr/share/fonts/x.ttf".to_owned(),
            modes: vec![complete(SubcellGrid::Sextant, 63)],
        };
        let text = report.render();
        assert!(text.contains("/usr/share/fonts/x.ttf"));
        assert!(text.contains("sextant"));
    }
}

//! [`MetricOverrides`] -- which chrome measurements the user actually stated.
//!
//! The theme's measurements have three possible sources, in strict priority
//! order: an explicit `[files]` key, the design guide's geometry scaled to the
//! configured font, and a built-in default. Resolving that priority needs one
//! piece of information the resolved theme does not carry -- *did the user
//! state this value?* -- and the answer cannot be recovered afterwards by
//! comparing the theme against its own defaults.
//!
//! That comparison is exactly the bug this type exists to remove. `Theme` used
//! to ask "does `header_height` still equal the default?" as a proxy for "is
//! it unset", which fails in two directions:
//!
//! - A user who explicitly configures the same number as the default has their
//!   value silently treated as absent and overwritten.
//! - Worse, and what actually shipped: once *any* stage had written a derived
//!   value, the field no longer equalled the default, so every *later* stage
//!   decided the value was user-stated and skipped itself. Chaining two
//!   derivation stages meant the second one silently did nothing, leaving the
//!   toolbar at the font-derived height while its controls were sized from the
//!   guide -- controls taller than the band containing them.
//!
//! Recording the overrides up front makes "unset" a fact rather than an
//! inference, so derivation stages compose in any order.

/// The `[files]` measurements the user explicitly configured.
///
/// Each `None` means "not stated, derive it". Held alongside the resolved
/// numbers so a derivation stage can tell an inherited default apart from a
/// deliberate choice.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MetricOverrides {
    row_height: Option<i32>,
    header_height: Option<i32>,
    status_height: Option<i32>,
    padding: Option<i32>,
}

impl MetricOverrides {
    /// Read which metrics the `[files]` table states.
    ///
    /// Reads the raw `Option` fields rather than the `files_*()` accessors:
    /// those accessors exist to *resolve* a value against its fallback, which
    /// would collapse precisely the distinction being recorded here.
    #[must_use]
    pub fn from_config(cfg: &config::Config) -> Self {
        // Metrics are validated to 0..=512 on the way in, so each conversion
        // is trivially in range.
        let px = |n: Option<u32>| n.map(|v| i32::try_from(v).unwrap_or(0));
        Self {
            row_height: px(cfg.files.row_height),
            header_height: px(cfg.files.header_height),
            status_height: px(cfg.files.status_height),
            padding: px(cfg.files.padding),
        }
    }

    /// The stated row height, if any.
    #[must_use]
    pub const fn row_height(self) -> Option<i32> {
        self.row_height
    }

    /// The stated toolbar height, if any.
    #[must_use]
    pub const fn header_height(self) -> Option<i32> {
        self.header_height
    }

    /// The stated status-strip height, if any.
    #[must_use]
    pub const fn status_height(self) -> Option<i32> {
        self.status_height
    }

    /// The stated row padding, if any.
    #[must_use]
    pub const fn padding(self) -> Option<i32> {
        self.padding
    }

    /// Whether nothing at all was stated.
    ///
    /// Used by tests and by callers building a theme with no config behind it.
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub const fn is_empty(self) -> bool {
        self.row_height.is_none()
            && self.header_height.is_none()
            && self.status_height.is_none()
            && self.padding.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::MetricOverrides;

    /// With no `[files]` table, nothing is stated -- every metric is free to
    /// be derived.
    #[test]
    fn an_absent_files_table_states_nothing() {
        let cfg = config::Config::default();
        let o = MetricOverrides::from_config(&cfg);
        assert!(o.is_empty(), "no table means no overrides");
        assert_eq!(o.row_height(), None);
        assert_eq!(o.header_height(), None);
    }

    /// A stated key is recorded, and its neighbours stay free.
    #[test]
    fn a_stated_metric_is_recorded_without_claiming_the_others() {
        let cfg = config::Config::from_bytes(b"[files]\nrow_height = 44\n").expect("parse");
        let o = MetricOverrides::from_config(&cfg);
        assert_eq!(o.row_height(), Some(44));
        assert_eq!(o.header_height(), None, "unstated stays derivable");
        assert!(!o.is_empty());
    }

    /// The case the old default-comparison got wrong: stating exactly the
    /// built-in default is still an explicit choice and must be recorded, or
    /// the user's value gets overwritten by a derivation.
    #[test]
    fn stating_the_default_value_still_counts_as_stated() {
        // 24 is `Theme::default().header_height`, the old sentinel.
        let cfg = config::Config::from_bytes(b"[files]\nheader_height = 24\n").expect("parse");
        let o = MetricOverrides::from_config(&cfg);
        assert_eq!(
            o.header_height(),
            Some(24),
            "an explicit value equal to the default is not 'unset'"
        );
    }

    /// Every metric is wired to its own key -- a copy-paste slip here would
    /// silently pin the wrong measurement.
    #[test]
    fn each_key_maps_to_its_own_metric() {
        let cfg = config::Config::from_bytes(
            b"[files]\nrow_height = 41\nheader_height = 42\nstatus_height = 43\npadding = 7\n",
        )
        .expect("parse");
        let o = MetricOverrides::from_config(&cfg);
        assert_eq!(o.row_height(), Some(41));
        assert_eq!(o.header_height(), Some(42));
        assert_eq!(o.status_height(), Some(43));
        assert_eq!(o.padding(), Some(7));
    }
}

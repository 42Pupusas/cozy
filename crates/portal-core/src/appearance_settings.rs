//! `org.freedesktop.appearance`: the one namespace this backend's
//! `impl.portal.Settings` implementation currently answers for.
//!
//! Owns the mapping from a `config::Config` snapshot to the four
//! documented `org.freedesktop.appearance` keys (`color-scheme`,
//! `accent-color`, `contrast`, `reduced-motion`), and answers `Read`/
//! `ReadAll` in terms of them. Deliberately knows nothing about D-Bus
//! object paths, method dispatch, or the `.portal`/`portals.conf` routing
//! that gets calls here at all -- see `docs/portal-scoping.md`'s
//! `portal-core` vs `bins/portal` split: this is the pure domain object,
//! wiring it to a live `Dispatcher` is a thin `bins/portal` concern.

use config::Config;
use dbus::value::Value;

use crate::accent_color::AccentColor;
use crate::color_scheme::ColorScheme;
use crate::namespace_filter::NamespaceFilter;

/// The one namespace name this backend answers for.
///
/// A `const` rather than repeating the literal at every call site --
/// `Self::read`/`Self::read_all` and their tests all need to agree on it,
/// and a typo in one copy silently making a key unreachable is exactly
/// the class of bug a shared constant rules out.
pub const NAMESPACE: &str = "org.freedesktop.appearance";

/// `org.freedesktop.appearance`'s four keys, snapshotted from a
/// `config::Config` at construction time.
///
/// Contrast and reduced-motion have no `crates/config` representation
/// today -- there's no `[general].contrast` or `[general].reduced_motion`
/// key -- so both report the spec's documented "no preference" (`0`)
/// rather than inventing a config surface this project doesn't otherwise
/// need yet. `color-scheme` and `accent-color`, in contrast, both have an
/// honest source: the terminal's own background luma and its `Theme`'s
/// accent, respectively -- values a caller like a GTK app genuinely wants
/// to match.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AppearanceSettings {
    color_scheme: ColorScheme,
    accent_color: AccentColor,
}

impl AppearanceSettings {
    /// Snapshot the appearance keys from `cfg`.
    ///
    /// The accent comes from `cfg.files_cursor()`, mirroring
    /// `bins/files::Theme::from_config`'s own derivation (`theme.accent`)
    /// -- the file browser is the one binary in this workspace that
    /// already treats a config color as "the accent", so this reuses that
    /// judgment rather than picking a second, potentially-diverging
    /// source (e.g. the raw palette blue) for the same concept.
    #[must_use]
    pub fn from_config(cfg: &Config) -> Self {
        Self {
            color_scheme: ColorScheme::from_background(cfg.files_background()),
            accent_color: AccentColor::from_config_color(cfg.files_cursor()),
        }
    }

    /// `Read(namespace, key)`: the single value at `key`, or `None` if
    /// `namespace` isn't `org.freedesktop.appearance` or `key` isn't one
    /// of the four documented keys -- either case is the caller's cue to
    /// answer the real D-Bus method with an error, per the spec's "Returns
    /// an error on any unknown namespace or key."
    #[must_use]
    pub fn read(&self, namespace: &str, key: &str) -> Option<Value> {
        if namespace != NAMESPACE {
            return None;
        }
        self.key_value(key)
    }

    /// `ReadAll(namespaces)`: every namespace this backend knows about
    /// whose name matches the filter, each as its own `key -> value`
    /// dict -- i.e. the full `a{sa{sv}}` reply body's single entry (there
    /// being only one namespace today) when `namespaces` matches it, or
    /// an empty outer dict when it doesn't.
    #[must_use]
    pub fn read_all(&self, namespaces: &[String]) -> Value {
        let filter = NamespaceFilter::new(namespaces);
        if !filter.matches(NAMESPACE) {
            return Value::Array(vec![]);
        }
        Value::Array(vec![Value::DictEntry(
            Box::new(Value::Str(NAMESPACE.to_string())),
            Box::new(self.namespace_dict()),
        )])
    }

    /// This namespace's four keys as an `a{sv}` value -- the inner dict
    /// `ReadAll`'s outer dict maps `NAMESPACE` to.
    fn namespace_dict(&self) -> Value {
        Value::Array(
            Self::KEYS
                .iter()
                .map(|&key| {
                    Value::DictEntry(
                        Box::new(Value::Str(key.to_string())),
                        Box::new(Value::Variant(Box::new(
                            self.key_value(key).expect("KEYS only lists keys we answer"),
                        ))),
                    )
                })
                .collect(),
        )
    }

    /// Every key name this backend answers, in the order `ReadAll`'s
    /// inner dict renders them -- kept as one list so `namespace_dict`
    /// and any future "does this backend know about key X" check derive
    /// from a single source instead of two lists silently drifting apart.
    const KEYS: [&str; 4] = ["color-scheme", "accent-color", "contrast", "reduced-motion"];

    /// Compare `self` (the previous snapshot) against `new`, returning
    /// every documented key whose value actually changed, each paired
    /// with its new wire value -- exactly what a `SettingChanged`
    /// emitter needs after a config reload to know which signals (if
    /// any) to fire. `contrast`/`reduced-motion` never appear here since
    /// both are a constant "no preference" regardless of config (see
    /// the struct doc comment) -- they can never actually change.
    #[must_use]
    pub fn changed_from(&self, new: Self) -> Vec<(&'static str, Value)> {
        let mut changed = Vec::new();
        if self.color_scheme != new.color_scheme {
            changed.push(("color-scheme", Value::U32(new.color_scheme.as_u32())));
        }
        if self.accent_color != new.accent_color {
            changed.push(("accent-color", new.accent_color.as_value()));
        }
        changed
    }

    /// One key's current value, or `None` if `key` isn't one of
    /// [`Self::KEYS`].
    fn key_value(&self, key: &str) -> Option<Value> {
        match key {
            "color-scheme" => Some(Value::U32(self.color_scheme.as_u32())),
            "accent-color" => Some(self.accent_color.as_value()),
            // Neither has a config source yet -- see the struct doc
            // comment -- so both report the spec's "no preference" `0`.
            "contrast" | "reduced-motion" => Some(Value::U32(0)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use config::Color;

    fn settings_with(background: Color, accent: Color) -> AppearanceSettings {
        AppearanceSettings {
            color_scheme: ColorScheme::from_background(background),
            accent_color: AccentColor::from_config_color(accent),
        }
    }

    #[test]
    fn from_config_derives_color_scheme_from_the_files_background() {
        let cfg = Config::default();
        let settings = AppearanceSettings::from_config(&cfg);
        assert_eq!(
            settings.color_scheme,
            ColorScheme::from_background(cfg.files_background())
        );
    }

    #[test]
    fn read_on_the_wrong_namespace_is_none() {
        let settings = settings_with(Color::from_rgb(0x18, 0x18, 0x18), Color::from_rgb(0, 0, 0));
        assert_eq!(settings.read("org.example.other", "color-scheme"), None);
    }

    #[test]
    fn read_on_an_unknown_key_is_none() {
        let settings = settings_with(Color::from_rgb(0x18, 0x18, 0x18), Color::from_rgb(0, 0, 0));
        assert_eq!(settings.read(NAMESPACE, "does-not-exist"), None);
    }

    #[test]
    fn read_color_scheme_reports_the_dark_preference() {
        let settings = settings_with(Color::from_rgb(0x18, 0x18, 0x18), Color::from_rgb(0, 0, 0));
        assert_eq!(
            settings.read(NAMESPACE, "color-scheme"),
            Some(Value::U32(1))
        );
    }

    #[test]
    fn read_accent_color_reports_a_ddd_struct() {
        let accent = Color::from_rgb(0x89, 0xb4, 0xfa);
        let settings = settings_with(Color::from_rgb(0x18, 0x18, 0x18), accent);
        let value = settings.read(NAMESPACE, "accent-color").unwrap();
        assert_eq!(value.signature(), "(ddd)");
    }

    #[test]
    fn read_contrast_and_reduced_motion_report_no_preference() {
        let settings = settings_with(Color::from_rgb(0x18, 0x18, 0x18), Color::from_rgb(0, 0, 0));
        assert_eq!(settings.read(NAMESPACE, "contrast"), Some(Value::U32(0)));
        assert_eq!(
            settings.read(NAMESPACE, "reduced-motion"),
            Some(Value::U32(0))
        );
    }

    #[test]
    fn read_all_with_no_filter_includes_the_one_namespace() {
        let settings = settings_with(Color::from_rgb(0x18, 0x18, 0x18), Color::from_rgb(0, 0, 0));
        let value = settings.read_all(&[]);
        let Value::Array(entries) = &value else {
            panic!("expected an array of namespace dict entries");
        };
        assert_eq!(entries.len(), 1);
        let Value::DictEntry(name, _) = &entries[0] else {
            panic!("expected a dict entry");
        };
        assert_eq!(**name, Value::Str(NAMESPACE.to_string()));
    }

    #[test]
    fn read_all_filtered_to_a_non_matching_namespace_is_empty() {
        let settings = settings_with(Color::from_rgb(0x18, 0x18, 0x18), Color::from_rgb(0, 0, 0));
        let value = settings.read_all(&["org.example.other".to_string()]);
        assert_eq!(value, Value::Array(vec![]));
    }

    #[test]
    fn read_all_carries_every_key_this_backend_answers() {
        let settings = settings_with(Color::from_rgb(0x18, 0x18, 0x18), Color::from_rgb(0, 0, 0));
        let value = settings.read_all(&[]);
        let Value::Array(entries) = &value else {
            panic!()
        };
        let Value::DictEntry(_, inner) = &entries[0] else {
            panic!()
        };
        let Value::Array(pairs) = inner.as_ref() else {
            panic!("expected the inner a{{sv}} dict")
        };
        let names: Vec<&str> = pairs
            .iter()
            .map(|entry| {
                let Value::DictEntry(name, _) = entry else {
                    panic!()
                };
                let Value::Str(name) = name.as_ref() else {
                    panic!()
                };
                name.as_str()
            })
            .collect();
        assert_eq!(
            names,
            vec!["color-scheme", "accent-color", "contrast", "reduced-motion"]
        );
    }

    #[test]
    fn read_all_reply_signature_matches_the_spec_shape() {
        let settings = settings_with(Color::from_rgb(0x18, 0x18, 0x18), Color::from_rgb(0, 0, 0));
        let value = settings.read_all(&[]);
        assert_eq!(value.signature(), "a{sa{sv}}");
    }

    #[test]
    fn changed_from_reports_nothing_when_the_snapshot_is_identical() {
        let before = settings_with(Color::from_rgb(0x18, 0x18, 0x18), Color::from_rgb(0, 0, 0));
        let after = before;
        assert_eq!(before.changed_from(after), vec![]);
    }

    #[test]
    fn changed_from_reports_color_scheme_when_the_background_crosses_the_threshold() {
        let before = settings_with(Color::from_rgb(0x18, 0x18, 0x18), Color::from_rgb(0, 0, 0));
        let after = settings_with(Color::from_rgb(0xf8, 0xf8, 0xf8), Color::from_rgb(0, 0, 0));
        assert_eq!(
            before.changed_from(after),
            vec![("color-scheme", Value::U32(2))]
        );
    }

    #[test]
    fn changed_from_reports_accent_color_when_it_changes() {
        let before = settings_with(Color::from_rgb(0x18, 0x18, 0x18), Color::from_rgb(0, 0, 0));
        let after = settings_with(
            Color::from_rgb(0x18, 0x18, 0x18),
            Color::from_rgb(0xff, 0xff, 0xff),
        );
        let changed = before.changed_from(after);
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].0, "accent-color");
    }

    #[test]
    fn changed_from_reports_both_keys_when_both_change() {
        let before = settings_with(Color::from_rgb(0x18, 0x18, 0x18), Color::from_rgb(0, 0, 0));
        let after = settings_with(
            Color::from_rgb(0xf8, 0xf8, 0xf8),
            Color::from_rgb(0xff, 0xff, 0xff),
        );
        let changed = before.changed_from(after);
        assert_eq!(changed.len(), 2);
    }
}

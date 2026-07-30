//! `ReadAll`'s `namespaces` argument, matched against candidate namespace
//! strings.
//!
//! An empty list (or a list containing an empty string) means
//! "everything", otherwise each entry either names a namespace exactly or
//! globs its *trailing* section with a `*` -- e.g. `"org.freedesktop.*"`
//! matches `"org.freedesktop.appearance"` but not
//! `"org.example.appearance"`, and the spec only promises trailing globs,
//! not `"*.appearance"` or a glob in the middle.
//!
//! Split out from [`crate::appearance_settings::AppearanceSettings`]
//! because this matching logic has nothing to do with *what* the
//! settings are -- it would be identical for any future namespace this
//! backend grows -- and is fiddly enough (three matching modes: match-all,
//! exact, prefix-glob) to deserve its own focused tests rather than being
//! proven only indirectly through `ReadAll` integration tests.

/// A parsed `ReadAll` `namespaces` argument, ready to test candidate
/// namespace strings against.
pub struct NamespaceFilter<'a> {
    patterns: &'a [String],
}

impl<'a> NamespaceFilter<'a> {
    /// Wrap the raw `namespaces` argument as received off the wire.
    #[must_use]
    pub const fn new(patterns: &'a [String]) -> Self {
        Self { patterns }
    }

    /// Whether `namespace` should be included in the `ReadAll` reply.
    #[must_use]
    pub fn matches(&self, namespace: &str) -> bool {
        // Per spec: empty array, or an array containing an empty string,
        // matches everything.
        if self.patterns.is_empty() || self.patterns.iter().any(String::is_empty) {
            return true;
        }
        self.patterns
            .iter()
            .any(|pattern| Self::pattern_matches(pattern, namespace))
    }

    /// One pattern against one candidate: exact match, or a trailing
    /// `prefix.*` glob against `prefix` being a leading segment run of
    /// `namespace`.
    fn pattern_matches(pattern: &str, namespace: &str) -> bool {
        pattern.strip_suffix('*').map_or_else(
            || pattern == namespace,
            |prefix| namespace.starts_with(prefix),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_patterns_matches_everything() {
        let filter = NamespaceFilter::new(&[]);
        assert!(filter.matches("org.freedesktop.appearance"));
        assert!(filter.matches("org.example.anything"));
    }

    #[test]
    fn a_single_empty_string_pattern_matches_everything() {
        let patterns = vec![String::new()];
        let filter = NamespaceFilter::new(&patterns);
        assert!(filter.matches("org.freedesktop.appearance"));
    }

    #[test]
    fn an_exact_pattern_matches_only_itself() {
        let patterns = vec!["org.freedesktop.appearance".to_string()];
        let filter = NamespaceFilter::new(&patterns);
        assert!(filter.matches("org.freedesktop.appearance"));
        assert!(!filter.matches("org.freedesktop.other"));
    }

    #[test]
    fn a_trailing_glob_matches_any_namespace_sharing_its_prefix() {
        let patterns = vec!["org.freedesktop.*".to_string()];
        let filter = NamespaceFilter::new(&patterns);
        assert!(filter.matches("org.freedesktop.appearance"));
        assert!(filter.matches("org.freedesktop.anything.else"));
        assert!(!filter.matches("org.example.appearance"));
    }

    #[test]
    fn multiple_patterns_are_matched_with_or_semantics() {
        let patterns = vec![
            "org.example.*".to_string(),
            "org.freedesktop.appearance".to_string(),
        ];
        let filter = NamespaceFilter::new(&patterns);
        assert!(filter.matches("org.freedesktop.appearance"));
        assert!(filter.matches("org.example.thing"));
        assert!(!filter.matches("org.freedesktop.other"));
    }

    #[test]
    fn a_glob_only_matches_as_a_trailing_prefix_not_mid_string() {
        // The spec explicitly scopes globbing to trailing sections --
        // "*.appearance" (a leading glob) is not a supported shape, so a
        // literal "*" pattern should only ever match namespaces that
        // start with the empty prefix, i.e. everything -- not be treated
        // as a suffix wildcard.
        let patterns = vec!["*".to_string()];
        let filter = NamespaceFilter::new(&patterns);
        assert!(filter.matches("org.freedesktop.appearance"));
        assert!(filter.matches("anything"));
    }
}

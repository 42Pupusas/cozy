//! A caller's unique bus name, mangled into the form that appears as the
//! `SENDER` path segment of a request or session handle.
//!
//! The spec's rule is one sentence -- drop the leading `:`, replace every
//! `.` with `_` -- but it is shared by *both* handle shapes
//! (`.../request/SENDER/TOKEN` and `.../session/SENDER/TOKEN`), so it
//! lives here rather than being written twice and drifting once. It also
//! carries the validation both handles depend on: a unique name that
//! doesn't look like a unique name would produce a path segment this
//! backend cannot later match against a real caller.

/// A caller's unique bus name (`:1.42`), ready to be spliced into a
/// handle's object path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallerId {
    mangled: String,
}

impl CallerId {
    /// Mangle a unique bus name.
    ///
    /// Returns `None` unless `name` is a well-formed unique name: a
    /// leading `:`, then dot-separated non-empty segments of
    /// `[A-Za-z0-9_-]`. Well-known names (`org.example.App`) are rejected
    /// -- the spec builds these paths from the caller's *unique* name, and
    /// accepting anything else would mint a handle that no `Close` call
    /// could ever be matched back to.
    #[must_use]
    pub fn from_unique_name(name: &str) -> Option<Self> {
        let body = name.strip_prefix(':')?;
        if !Self::is_valid_body(body) {
            return None;
        }
        Some(Self {
            mangled: body.replace('.', "_"),
        })
    }

    fn is_valid_body(body: &str) -> bool {
        !body.is_empty()
            && body.split('.').all(|segment| {
                !segment.is_empty()
                    && segment
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            })
    }

    /// The `SENDER` path segment.
    #[must_use]
    pub fn as_segment(&self) -> &str {
        &self.mangled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment_of(name: &str) -> String {
        CallerId::from_unique_name(name)
            .expect("valid unique name")
            .as_segment()
            .to_string()
    }

    #[test]
    fn the_leading_colon_is_dropped_and_dots_become_underscores() {
        assert_eq!(segment_of(":1.42"), "1_42");
    }

    #[test]
    fn every_dot_is_replaced_not_only_the_first() {
        assert_eq!(segment_of(":1.2.3.4"), "1_2_3_4");
    }

    #[test]
    fn a_name_without_the_unique_prefix_is_rejected() {
        assert_eq!(CallerId::from_unique_name("1.42"), None);
        assert_eq!(CallerId::from_unique_name("org.example.App"), None);
    }

    #[test]
    fn an_empty_or_bare_colon_name_is_rejected() {
        assert_eq!(CallerId::from_unique_name(""), None);
        assert_eq!(CallerId::from_unique_name(":"), None);
    }

    #[test]
    fn an_empty_segment_is_rejected() {
        assert_eq!(CallerId::from_unique_name(":1."), None);
        assert_eq!(CallerId::from_unique_name(":.1"), None);
        assert_eq!(CallerId::from_unique_name(":1..2"), None);
    }

    #[test]
    fn a_segment_with_a_path_separator_is_rejected() {
        // Otherwise the mangled sender would open a new path element and
        // the handle would address an object nobody exported.
        assert_eq!(CallerId::from_unique_name(":1/42"), None);
        assert_eq!(CallerId::from_unique_name(":../../etc"), None);
    }

    #[test]
    fn hyphens_and_underscores_survive_unchanged() {
        assert_eq!(segment_of(":1.foo-bar_baz"), "1_foo-bar_baz");
    }
}

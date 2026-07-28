//! Filename glob patterns from the shared-mime-info database.
//!
//! The overwhelming majority of patterns are a plain `*.ext` suffix, which
//! we detect and answer with a cheap extension comparison. But the real
//! database on this machine also contains patterns like
//! `application/x-sharedlib:*.so.[0-9]*` and `video/mpeg:[0-9][0-9][0-9].vdr`,
//! so a naive "strip the `*.`" reader would mis-file those. Hence a small
//! matcher supporting the three wildcard constructs the spec allows:
//! `*` (any run), `?` (one character), and `[...]` character classes with
//! ranges and `!`/`^` negation.
//!
//! This is deliberately *not* a general regex engine -- it's a backtracking
//! matcher over a pattern language with no alternation or capture, which is
//! a few dozen lines and has no pathological cases worth worrying about at
//! filename lengths.

/// How a pattern is matched, chosen once at parse time.
///
/// Splitting the common cases out of the general matcher is what keeps a
/// ~1400-entry database cheap to query: the vast majority of lookups become
/// a lowercase suffix comparison rather than a character walk.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Shape {
    /// `*.ext` -- match any name ending in `.ext`. Stores the extension
    /// *including* the leading dot.
    Suffix(String),
    /// A pattern with no wildcards at all, e.g. `Makefile` -- the whole
    /// filename must equal it.
    Literal(String),
    /// Anything else: walk it with the general wildcard matcher.
    Wildcard(Vec<char>),
}

/// One filename pattern from the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobPattern {
    shape: Shape,
    /// Whether matching respects case.
    ///
    /// shared-mime-info patterns are case-insensitive unless explicitly
    /// flagged, and users overwhelmingly expect `PHOTO.JPG` to be a JPEG,
    /// so case-insensitive is the default here.
    case_sensitive: bool,
}

impl GlobPattern {
    /// Parse a pattern string, classifying it into the cheapest shape that
    /// can represent it.
    #[must_use]
    pub fn parse(pattern: &str) -> Self {
        Self::with_case(pattern, false)
    }

    /// Parse with an explicit case-sensitivity flag (the `cs` flag the
    /// globs2 format can carry in its optional fourth field).
    #[must_use]
    pub fn with_case(pattern: &str, case_sensitive: bool) -> Self {
        let has_wildcards = pattern.contains(['*', '?', '[']);

        let shape = if !has_wildcards {
            Self::literal_shape(pattern, case_sensitive)
        } else if let Some(ext) = Self::simple_suffix(pattern) {
            // `*.ext` with no further wildcards -- by far the common case.
            Self::suffix_shape(ext, case_sensitive)
        } else {
            Self::wildcard_shape(pattern, case_sensitive)
        };

        Self {
            shape,
            case_sensitive,
        }
    }

    fn literal_shape(pattern: &str, case_sensitive: bool) -> Shape {
        Shape::Literal(Self::fold(pattern, case_sensitive))
    }

    fn suffix_shape(ext: &str, case_sensitive: bool) -> Shape {
        Shape::Suffix(Self::fold(ext, case_sensitive))
    }

    fn wildcard_shape(pattern: &str, case_sensitive: bool) -> Shape {
        Shape::Wildcard(Self::fold(pattern, case_sensitive).chars().collect())
    }

    /// Recognise `*.something` where `something` holds no further wildcards,
    /// returning the extension *with* its leading dot.
    fn simple_suffix(pattern: &str) -> Option<&str> {
        let rest = pattern.strip_prefix('*')?;
        // Must start with a dot and contain no other wildcard, otherwise
        // it's a general pattern like `*.so.[0-9]*`.
        if rest.starts_with('.') && !rest.contains(['*', '?', '[']) {
            Some(rest)
        } else {
            None
        }
    }

    /// Normalise case for storage/comparison when matching is insensitive.
    fn fold(s: &str, case_sensitive: bool) -> String {
        if case_sensitive {
            s.to_owned()
        } else {
            s.to_lowercase()
        }
    }

    /// The literal extension this pattern matches, if it is a simple
    /// `*.ext` suffix. Lets an index bucket patterns by extension.
    #[must_use]
    pub fn suffix(&self) -> Option<&str> {
        match &self.shape {
            Shape::Suffix(ext) => Some(ext),
            _ => None,
        }
    }

    /// How specific this pattern is, used to break ties between two
    /// patterns of equal database weight.
    ///
    /// The spec says the longest pattern wins, which is what makes
    /// `*.tar.gz` beat `*.gz` even when both are weight 50.
    #[must_use]
    pub const fn specificity(&self) -> usize {
        match &self.shape {
            Shape::Suffix(ext) => ext.len(),
            Shape::Literal(lit) => lit.len(),
            Shape::Wildcard(chars) => chars.len(),
        }
    }

    /// Whether `filename` (a bare file name, not a path) matches.
    #[must_use]
    pub fn matches(&self, filename: &str) -> bool {
        let name = Self::fold(filename, self.case_sensitive);
        match &self.shape {
            Shape::Suffix(ext) => name.ends_with(ext.as_str()),
            Shape::Literal(lit) => name == *lit,
            Shape::Wildcard(pattern) => {
                let name_chars: Vec<char> = name.chars().collect();
                Self::wildcard_match(pattern, &name_chars)
            }
        }
    }

    /// Backtracking wildcard match over `*`, `?`, and `[...]` classes.
    ///
    /// Iterative rather than recursive: the `*` case records a backtrack
    /// point and resumes from it on mismatch, which keeps stack depth
    /// constant no matter how many `*`s a pattern contains.
    fn wildcard_match(pattern: &[char], name: &[char]) -> bool {
        let (mut p, mut n) = (0usize, 0usize);
        // Where to resume if the current `*` expansion turns out to be
        // wrong: `star` is the pattern index of that `*`, `retry` the name
        // index we'd next let it consume.
        let (mut star, mut retry) = (None, 0usize);

        while n < name.len() {
            match pattern.get(p) {
                Some('*') => {
                    // Record this star and first try matching it against
                    // the empty string, expanding only on later failure.
                    star = Some(p);
                    retry = n;
                    p += 1;
                }
                Some('?') => {
                    p += 1;
                    n += 1;
                }
                Some('[') => {
                    if let Some((matched, next_p)) = Self::class_match(pattern, p, name[n]) {
                        if matched {
                            p = next_p;
                            n += 1;
                        } else if let Some(s) = star {
                            // Let the star swallow one more character.
                            p = s + 1;
                            retry += 1;
                            n = retry;
                        } else {
                            return false;
                        }
                    } else {
                        // Unterminated `[` -- treat it as a literal
                        // bracket rather than failing the whole database.
                        if name[n] == '[' {
                            p += 1;
                            n += 1;
                        } else if let Some(s) = star {
                            p = s + 1;
                            retry += 1;
                            n = retry;
                        } else {
                            return false;
                        }
                    }
                }
                Some(&c) if c == name[n] => {
                    p += 1;
                    n += 1;
                }
                _ => {
                    if let Some(s) = star {
                        p = s + 1;
                        retry += 1;
                        n = retry;
                    } else {
                        return false;
                    }
                }
            }
        }

        // Any trailing `*`s can absorb the empty remainder.
        while matches!(pattern.get(p), Some('*')) {
            p += 1;
        }
        p == pattern.len()
    }

    /// Match one character against a `[...]` class starting at `open`.
    ///
    /// Returns `(matched, index just past the class)`, or `None` if the
    /// class is unterminated.
    fn class_match(pattern: &[char], open: usize, c: char) -> Option<(bool, usize)> {
        let mut i = open + 1;
        let negated = matches!(pattern.get(i), Some('!' | '^'));
        if negated {
            i += 1;
        }

        let mut matched = false;
        let mut first = true;
        loop {
            let &ch = pattern.get(i)?;
            // A `]` in the first position is a literal, per glob tradition.
            if ch == ']' && !first {
                let result = if negated { !matched } else { matched };
                return Some((result, i + 1));
            }
            first = false;

            // A range like `0-9`, but only when the `-` isn't the last
            // character before the closing bracket.
            if matches!(pattern.get(i + 1), Some('-'))
                && pattern.get(i + 2).is_some_and(|&e| e != ']')
            {
                let end = pattern[i + 2];
                if ch <= c && c <= end {
                    matched = true;
                }
                i += 3;
            } else {
                if ch == c {
                    matched = true;
                }
                i += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_extension_patterns_use_the_suffix_fast_path() {
        let g = GlobPattern::parse("*.png");
        assert_eq!(g.suffix(), Some(".png"));
        assert!(g.matches("photo.png"));
        assert!(!g.matches("photo.jpg"));
    }

    #[test]
    fn extension_matching_is_case_insensitive_by_default() {
        let g = GlobPattern::parse("*.jpg");
        assert!(g.matches("HOLIDAY.JPG"), "users expect .JPG to be a JPEG");
        assert!(g.matches("Holiday.Jpg"));
    }

    #[test]
    fn case_sensitive_patterns_respect_case() {
        let g = GlobPattern::with_case("*.C", true);
        assert!(g.matches("main.C"));
        assert!(!g.matches("main.c"), "cs flag must suppress case folding");
    }

    #[test]
    fn literal_patterns_match_whole_names_only() {
        let g = GlobPattern::parse("Makefile");
        assert!(g.suffix().is_none());
        assert!(g.matches("makefile"));
        assert!(!g.matches("Makefile.am"), "must not match as a prefix");
        assert!(!g.matches("GNUmakefile"), "must not match as a suffix");
    }

    /// The real database ships this pattern; a suffix-only reader would
    /// mis-handle it entirely.
    #[test]
    fn matches_shared_library_versions() {
        let g = GlobPattern::parse("*.so.[0-9]*");
        assert!(g.matches("libc.so.6"));
        assert!(g.matches("libfoo.so.1.2.3"));
        assert!(!g.matches("libc.so"), "needs a version component");
        assert!(!g.matches("libc.so.x"), "version must start with a digit");
    }

    /// Also straight from the database: three leading digits, then `.vdr`.
    #[test]
    fn matches_leading_character_classes() {
        let g = GlobPattern::parse("[0-9][0-9][0-9].vdr");
        assert!(g.matches("001.vdr"));
        assert!(g.matches("999.vdr"));
        assert!(!g.matches("01.vdr"), "needs exactly three digits");
        assert!(!g.matches("abc.vdr"));
    }

    #[test]
    fn question_mark_matches_exactly_one_character() {
        let g = GlobPattern::parse("file?.txt");
        assert!(g.matches("file1.txt"));
        assert!(g.matches("fileA.txt"));
        assert!(!g.matches("file.txt"), "? requires a character");
        assert!(!g.matches("file12.txt"), "? matches only one");
    }

    #[test]
    fn negated_classes_invert() {
        let g = GlobPattern::parse("[!0-9]*.txt");
        assert!(g.matches("abc.txt"));
        assert!(!g.matches("1abc.txt"));
    }

    #[test]
    fn star_backtracks_correctly() {
        // The first `*` must not greedily eat the text the tail needs.
        let g = GlobPattern::parse("*foo*bar");
        assert!(g.matches("xxfooyybar"));
        assert!(g.matches("foobar"));
        assert!(!g.matches("fooba"));
        assert!(!g.matches("barfoo"));
    }

    #[test]
    fn bare_star_matches_anything_nonempty_and_empty_tail() {
        let g = GlobPattern::parse("*");
        assert!(g.matches("anything"));
        assert!(g.matches(""));
    }

    /// Specificity is what makes `*.tar.gz` win over `*.gz` at equal
    /// database weight -- the spec's longest-pattern rule.
    #[test]
    fn longer_patterns_are_more_specific() {
        let long = GlobPattern::parse("*.tar.gz");
        let short = GlobPattern::parse("*.gz");
        assert!(long.specificity() > short.specificity());
    }

    #[test]
    fn unterminated_class_is_treated_literally_not_panicking() {
        // A malformed database line must not take the process down.
        let g = GlobPattern::parse("file[0-9.txt");
        assert!(!g.matches("file1.txt"));
        // The important property is simply that it returned.
        assert!(!g.matches(""));
    }
}

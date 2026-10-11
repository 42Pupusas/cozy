use dbus::value::Value;

/// One pattern inside a filter: `(u, s)` on the wire, 0 = glob, 1 = MIME type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterRule {
    Glob(String),
    Mime(String),
}

impl FilterRule {
    const GLOB: u32 = 0;
    const MIME: u32 = 1;

    fn from_value(value: &Value) -> Option<Self> {
        let [Value::U32(kind), Value::Str(pattern)] = value.as_items()? else {
            return None;
        };
        match *kind {
            Self::GLOB => Some(Self::Glob(pattern.clone())),
            Self::MIME => Some(Self::Mime(pattern.clone())),
            _ => None,
        }
    }

    fn to_value(&self) -> Value {
        let (kind, pattern) = match self {
            Self::Glob(pattern) => (Self::GLOB, pattern),
            Self::Mime(pattern) => (Self::MIME, pattern),
        };
        Value::Struct(vec![Value::U32(kind), Value::Str(pattern.clone())])
    }

    #[must_use]
    pub fn matches(&self, file_name: &str, mime: Option<&str>) -> bool {
        match self {
            Self::Glob(pattern) => GlobPattern::matches(pattern, file_name),
            Self::Mime(wanted) => mime.is_some_and(|actual| MimePattern::matches(wanted, actual)),
        }
    }
}

struct MimePattern;

impl MimePattern {
    fn matches(wanted: &str, actual: &str) -> bool {
        wanted.strip_suffix("/*").map_or_else(
            || wanted == actual,
            |major| {
                actual
                    .split_once('/')
                    .is_some_and(|(actual_major, _)| actual_major == major)
            },
        )
    }
}

/// Shell-style matching of a whole name with `*` and `?`.
struct GlobPattern;

impl GlobPattern {
    fn matches(pattern: &str, name: &str) -> bool {
        let pattern: Vec<char> = pattern.chars().collect();
        let name: Vec<char> = name.chars().collect();
        let mut p = 0;
        let mut n = 0;
        let mut resume: Option<(usize, usize)> = None;
        while n < name.len() {
            match pattern.get(p) {
                Some('*') => {
                    resume = Some((p, n));
                    p += 1;
                }
                Some('?') => {
                    p += 1;
                    n += 1;
                }
                Some(literal) if *literal == name[n] => {
                    p += 1;
                    n += 1;
                }
                _ => match resume {
                    Some((star, matched)) => {
                        p = star + 1;
                        n = matched + 1;
                        resume = Some((star, matched + 1));
                    }
                    None => return false,
                },
            }
        }
        pattern[p..].iter().all(|c| *c == '*')
    }
}

/// A named file filter: `(s, a(us))`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFilter {
    name: String,
    rules: Vec<FilterRule>,
}

impl FileFilter {
    #[must_use]
    pub fn new(name: impl Into<String>, rules: Vec<FilterRule>) -> Self {
        Self {
            name: name.into(),
            rules,
        }
    }

    #[must_use]
    pub fn from_value(value: &Value) -> Option<Self> {
        let value = value.clone().into_inner();
        let [Value::Str(name), rules] = value.as_items()? else {
            return None;
        };
        let rules = rules
            .as_items()?
            .iter()
            .filter_map(FilterRule::from_value)
            .collect();
        Some(Self {
            name: name.clone(),
            rules,
        })
    }

    #[must_use]
    pub fn list_from_value(value: &Value) -> Vec<Self> {
        value
            .clone()
            .into_inner()
            .as_items()
            .map_or_else(Vec::new, |items| {
                items.iter().filter_map(Self::from_value).collect()
            })
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn rules(&self) -> &[FilterRule] {
        &self.rules
    }

    #[must_use]
    pub fn accepts(&self, file_name: &str, mime: Option<&str>) -> bool {
        self.rules
            .iter()
            .any(|rule| rule.matches(file_name, mime))
    }

    /// `None` for a filter with no rules: an empty `a(us)` cannot carry its
    /// own signature inside a variant.
    #[must_use]
    pub fn to_value(&self) -> Option<Value> {
        if self.rules.is_empty() {
            return None;
        }
        Some(Value::Struct(vec![
            Value::Str(self.name.clone()),
            Value::Array(self.rules.iter().map(FilterRule::to_value).collect()),
        ]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Wire;

    impl Wire {
        fn filter(name: &str, rules: &[(u32, &str)]) -> Value {
            Value::Struct(vec![
                Value::Str(name.into()),
                Value::Array(
                    rules
                        .iter()
                        .map(|(kind, pattern)| {
                            Value::Struct(vec![Value::U32(*kind), Value::Str((*pattern).into())])
                        })
                        .collect(),
                ),
            ])
        }
    }

    #[test]
    fn a_glob_filter_round_trips_through_the_wire_shape() {
        let wire = Wire::filter("Text", &[(0, "*.txt"), (1, "text/plain")]);
        let filter = FileFilter::from_value(&wire).expect("filter");
        assert_eq!(filter.name(), "Text");
        assert_eq!(
            filter.rules(),
            [
                FilterRule::Glob("*.txt".into()),
                FilterRule::Mime("text/plain".into())
            ]
        );
        assert_eq!(filter.to_value(), Some(wire));
    }

    #[test]
    fn a_variant_wrapped_filter_is_read() {
        let wire = Value::Variant(Box::new(Wire::filter("All", &[(0, "*")])));
        assert!(FileFilter::from_value(&wire).is_some());
    }

    #[test]
    fn an_unknown_rule_kind_is_dropped() {
        let wire = Wire::filter("X", &[(9, "?"), (0, "*.rs")]);
        let filter = FileFilter::from_value(&wire).expect("filter");
        assert_eq!(filter.rules(), [FilterRule::Glob("*.rs".into())]);
    }

    #[test]
    fn a_malformed_filter_is_rejected() {
        assert_eq!(FileFilter::from_value(&Value::Str("x".into())), None);
        assert_eq!(
            FileFilter::from_value(&Value::Struct(vec![Value::U32(1)])),
            None
        );
    }

    #[test]
    fn a_filter_with_no_rules_has_no_wire_value() {
        let filter = FileFilter::new("Empty", vec![]);
        assert_eq!(filter.to_value(), None);
    }

    #[test]
    fn list_from_value_reads_every_filter() {
        let wire = Value::Array(vec![
            Wire::filter("A", &[(0, "*.a")]),
            Wire::filter("B", &[(0, "*.b")]),
        ]);
        let names: Vec<_> = FileFilter::list_from_value(&wire)
            .iter()
            .map(|f| f.name().to_owned())
            .collect();
        assert_eq!(names, ["A", "B"]);
    }

    #[test]
    fn glob_star_matches_any_run() {
        let rule = FilterRule::Glob("*.txt".into());
        assert!(rule.matches("a.txt", None));
        assert!(rule.matches(".txt", None));
        assert!(!rule.matches("a.txt.bak", None));
        assert!(!rule.matches("a.TXT", None));
    }

    #[test]
    fn glob_question_matches_one_character() {
        let rule = FilterRule::Glob("a?c".into());
        assert!(rule.matches("abc", None));
        assert!(!rule.matches("ac", None));
        assert!(!rule.matches("abbc", None));
    }

    #[test]
    fn glob_backtracks_over_repeated_stars() {
        let rule = FilterRule::Glob("*a*b".into());
        assert!(rule.matches("xaayb", None));
        assert!(rule.matches("ab", None));
        assert!(!rule.matches("ba", None));
    }

    #[test]
    fn a_lone_star_matches_everything_including_empty() {
        let rule = FilterRule::Glob("*".into());
        assert!(rule.matches("", None));
        assert!(rule.matches("anything", None));
    }

    #[test]
    fn mime_rules_need_a_known_mime_type() {
        let rule = FilterRule::Mime("text/plain".into());
        assert!(rule.matches("x", Some("text/plain")));
        assert!(!rule.matches("x", Some("text/html")));
        assert!(!rule.matches("x", None));
    }

    #[test]
    fn a_mime_major_wildcard_matches_the_family() {
        let rule = FilterRule::Mime("image/*".into());
        assert!(rule.matches("x", Some("image/png")));
        assert!(!rule.matches("x", Some("video/mp4")));
        assert!(!rule.matches("x", Some("imagery")));
    }

    #[test]
    fn a_filter_accepts_when_any_rule_matches() {
        let filter = FileFilter::new(
            "Docs",
            vec![
                FilterRule::Glob("*.md".into()),
                FilterRule::Mime("application/pdf".into()),
            ],
        );
        assert!(filter.accepts("README.md", None));
        assert!(filter.accepts("x", Some("application/pdf")));
        assert!(!filter.accepts("x.png", Some("image/png")));
    }
}

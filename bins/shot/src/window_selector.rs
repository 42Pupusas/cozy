#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowInfo {
    pub id: u64,
    pub title: Option<String>,
    pub app_id: Option<String>,
}

impl WindowInfo {
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "{}\t{}\t{}",
            self.id,
            self.app_id.as_deref().unwrap_or("-"),
            self.title.as_deref().unwrap_or("-")
        )
    }

    fn contains(&self, needle: &str) -> bool {
        [&self.title, &self.app_id]
            .into_iter()
            .flatten()
            .any(|field| field.to_lowercase().contains(needle))
    }

    fn equals(&self, needle: &str) -> bool {
        [&self.title, &self.app_id]
            .into_iter()
            .flatten()
            .any(|field| field.to_lowercase() == needle)
    }
}

pub struct WindowSelector<'a> {
    windows: &'a [WindowInfo],
}

impl<'a> WindowSelector<'a> {
    #[must_use]
    pub const fn new(windows: &'a [WindowInfo]) -> Self {
        Self { windows }
    }

    #[must_use]
    pub fn listing(&self) -> String {
        self.windows
            .iter()
            .map(WindowInfo::describe)
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn select(&self, query: &str) -> Result<&'a WindowInfo, String> {
        if let Some(window) = query
            .parse::<u64>()
            .ok()
            .and_then(|id| self.windows.iter().find(|w| w.id == id))
        {
            return Ok(window);
        }
        let needle = query.to_lowercase();
        let candidates: Vec<&'a WindowInfo> = self
            .windows
            .iter()
            .filter(|w| w.contains(&needle))
            .collect();
        match candidates.as_slice() {
            [] => Err(format!(
                "no window matches `{query}`; open windows:\n{}",
                self.listing()
            )),
            [only] => Ok(*only),
            many => {
                let exact: Vec<&'a WindowInfo> =
                    many.iter().copied().filter(|w| w.equals(&needle)).collect();
                if let [only] = exact.as_slice() {
                    return Ok(*only);
                }
                let listing = many
                    .iter()
                    .map(|w| w.describe())
                    .collect::<Vec<_>>()
                    .join("\n");
                Err(format!(
                    "`{query}` matches {} windows; pass one id instead:\n{listing}",
                    many.len()
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{WindowInfo, WindowSelector};

    struct Fixture;

    impl Fixture {
        fn window(id: u64, app_id: &str, title: &str) -> WindowInfo {
            WindowInfo {
                id,
                title: Some(title.to_string()),
                app_id: Some(app_id.to_string()),
            }
        }

        fn desktop() -> Vec<WindowInfo> {
            vec![
                Self::window(3, "firefox", "Rust docs - Mozilla Firefox"),
                Self::window(7, "foot", "~/code"),
                Self::window(9, "foot", "~/notes"),
                Self::window(12, "firefox-developer", "Inspector"),
            ]
        }
    }

    #[test]
    fn an_exact_id_wins_over_text_matching() {
        let windows = Fixture::desktop();
        let found = WindowSelector::new(&windows).select("7").expect("found");
        assert_eq!(found.id, 7);
    }

    #[test]
    fn app_id_and_title_match_case_insensitively_by_substring() {
        let windows = Fixture::desktop();
        let selector = WindowSelector::new(&windows);
        assert_eq!(selector.select("MOZILLA").expect("found").id, 3);
        assert_eq!(selector.select("inspect").expect("found").id, 12);
    }

    #[test]
    fn an_exact_app_id_beats_a_longer_sibling() {
        let windows = Fixture::desktop();
        let found = WindowSelector::new(&windows)
            .select("firefox")
            .expect("exact app id disambiguates");
        assert_eq!(found.id, 3);
    }

    #[test]
    fn an_ambiguous_query_lists_the_candidates_instead_of_guessing() {
        let windows = Fixture::desktop();
        let error = WindowSelector::new(&windows)
            .select("foot")
            .expect_err("two foot windows");
        assert!(error.contains("matches 2 windows"));
        assert!(error.contains("~/code") && error.contains("~/notes"));
    }

    #[test]
    fn an_unknown_query_lists_what_is_open() {
        let windows = Fixture::desktop();
        let error = WindowSelector::new(&windows)
            .select("gimp")
            .expect_err("nothing matches");
        assert!(error.contains("no window matches `gimp`"));
        assert!(error.contains("Inspector"));
    }

    #[test]
    fn windows_without_a_title_or_app_id_are_listed_with_placeholders() {
        let window = WindowInfo {
            id: 5,
            title: None,
            app_id: None,
        };
        assert_eq!(window.describe(), "5\t-\t-");
        let windows = [window];
        assert!(WindowSelector::new(&windows).select("anything").is_err());
    }

    #[test]
    fn an_id_that_is_not_open_falls_back_to_text_and_then_fails() {
        let windows = Fixture::desktop();
        assert!(WindowSelector::new(&windows).select("99").is_err());
    }
}

use std::ffi::{OsStr, OsString};

/// What changed in the open directory: the names of the entries touched, or
/// everything when the cause is unknown (a manual refresh, lost events, the
/// directory itself moving).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListingChange {
    Entries(Vec<OsString>),
    Everything,
}

impl ListingChange {
    const MOST_NAMES: usize = 256;

    #[must_use]
    pub const fn none() -> Self {
        Self::Entries(Vec::new())
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        matches!(self, Self::Entries(names) if names.is_empty())
    }

    /// Record that `name` changed. Past a few hundred names the list stops
    /// being cheaper than starting over, so it becomes [`Self::Everything`].
    pub fn add_name(&mut self, name: &OsStr) {
        let Self::Entries(names) = self else {
            return;
        };
        if names.iter().any(|known| known == name) {
            return;
        }
        if names.len() >= Self::MOST_NAMES {
            *self = Self::Everything;
            return;
        }
        names.push(name.to_owned());
    }

    pub fn merge(&mut self, other: &Self) {
        match other {
            Self::Everything => *self = Self::Everything,
            Self::Entries(names) => {
                for name in names {
                    self.add_name(name);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ListingChange;
    use std::ffi::{OsStr, OsString};

    #[test]
    fn names_are_kept_once_each() {
        let mut change = ListingChange::none();
        assert!(change.is_empty());
        change.add_name(OsStr::new("a"));
        change.add_name(OsStr::new("b"));
        change.add_name(OsStr::new("a"));
        assert_eq!(
            change,
            ListingChange::Entries(vec![OsString::from("a"), OsString::from("b")])
        );
    }

    #[test]
    fn too_many_names_become_everything() {
        let mut change = ListingChange::none();
        for i in 0..=ListingChange::MOST_NAMES {
            change.add_name(OsStr::new(&format!("n{i}")));
        }
        assert_eq!(change, ListingChange::Everything);
    }

    #[test]
    fn merging_everything_wins_and_names_accumulate() {
        let mut change = ListingChange::Entries(vec![OsString::from("a")]);
        change.merge(&ListingChange::Entries(vec![OsString::from("b")]));
        assert_eq!(
            change,
            ListingChange::Entries(vec![OsString::from("a"), OsString::from("b")])
        );
        change.merge(&ListingChange::Everything);
        assert_eq!(change, ListingChange::Everything);
        change.add_name(OsStr::new("c"));
        assert_eq!(change, ListingChange::Everything);
    }
}

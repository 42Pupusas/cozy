//! [`Breadcrumb`] -- the toolbar's clickable path.
//!
//! The design guide's toolbar shows the location as shell-convention
//! segments (`~ / home`) rather than as a raw path string, with the final
//! segment emphasised and the earlier ones navigable. That last part is the
//! reason this is a model and not a `format!`: each segment has to know the
//! path it would navigate *to*, so clicking `~` goes home rather than
//! reparsing the string at click time.
//!
//! Home is collapsed to `~` because that is what every shell on the machine
//! does, and because `/home/somebody-with-a-long-name/projects` spends a
//! third of the toolbar telling you something you already know.
//!
//! Where the path is too long for the toolbar, *leading* segments are elided
//! rather than trailing ones: the tail says where you are, the head only says
//! you are under `/home`.

use std::path::{Path, PathBuf};

/// One clickable step in the path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crumb {
    /// What the segment reads as: a component name, or `~` for home.
    pub label: String,
    /// Where clicking it navigates.
    pub target: PathBuf,
    /// Whether this is the directory currently open -- drawn emphasised, and
    /// not worth navigating to since you are already there.
    pub current: bool,
}

/// The path, as navigable segments.
#[derive(Debug, Clone, Default)]
pub struct Breadcrumb {
    crumbs: Vec<Crumb>,
}

impl Breadcrumb {
    /// The marker shown in place of elided leading segments.
    pub const ELLIPSIS: &'static str = "…";

    /// Build the crumbs for `path`, collapsing `home` to `~`.
    ///
    /// `home` is passed in rather than read from the environment so this is
    /// testable without mutating process-global state -- and so a browser
    /// opened on another user's tree can be told not to collapse at all.
    #[must_use]
    pub fn new(path: &Path, home: Option<&Path>) -> Self {
        let mut crumbs = Vec::new();

        // The home-relative case: start at `~` and add only what follows it.
        if let Some(home) = home
            && let Ok(rest) = path.strip_prefix(home)
        {
            crumbs.push(Crumb {
                label: "~".to_owned(),
                target: home.to_path_buf(),
                current: false,
            });
            let mut cursor = home.to_path_buf();
            for part in rest.components() {
                cursor.push(part);
                crumbs.push(Crumb {
                    label: part.as_os_str().to_string_lossy().into_owned(),
                    target: cursor.clone(),
                    current: false,
                });
            }
            Self::mark_current(&mut crumbs);
            return Self { crumbs };
        }

        // The absolute case: `/` is itself a place you can navigate to, so it
        // gets a crumb rather than being an invisible prefix.
        let mut cursor = PathBuf::new();
        for part in path.components() {
            cursor.push(part.as_os_str());
            let label = match part {
                std::path::Component::RootDir => "/".to_owned(),
                other => other.as_os_str().to_string_lossy().into_owned(),
            };
            crumbs.push(Crumb {
                label,
                target: cursor.clone(),
                current: false,
            });
        }
        Self::mark_current(&mut crumbs);
        Self { crumbs }
    }

    /// Flag the final crumb as the open directory.
    const fn mark_current(crumbs: &mut [Crumb]) {
        if let Some(last) = crumbs.last_mut() {
            last.current = true;
        }
    }

    /// Every segment, root first.
    ///
    /// The painter reaches for [`Self::tail`] instead, since it always has a
    /// width budget; this is the unbudgeted view the tests assert against.
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn crumbs(&self) -> &[Crumb] {
        &self.crumbs
    }

    /// Whether the path produced no segments at all (an empty path).
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub const fn is_empty(&self) -> bool {
        self.crumbs.is_empty()
    }

    /// The last `max` segments, for a toolbar too narrow to show them all.
    ///
    /// Returns the kept crumbs and whether anything was dropped, so the
    /// painter can draw a leading ellipsis. Trailing segments are kept
    /// because the tail is the part that says where you are.
    #[must_use]
    pub fn tail(&self, max: usize) -> (&[Crumb], bool) {
        if max == 0 {
            return (&[], !self.crumbs.is_empty());
        }
        if self.crumbs.len() <= max {
            return (&self.crumbs, false);
        }
        (&self.crumbs[self.crumbs.len() - max..], true)
    }
}

#[cfg(test)]
mod tests {
    use super::Breadcrumb;
    use std::path::{Path, PathBuf};

    fn labels(b: &Breadcrumb) -> Vec<String> {
        b.crumbs().iter().map(|c| c.label.clone()).collect()
    }

    /// The guide's own example: home collapses to `~`.
    #[test]
    fn home_collapses_to_a_tilde() {
        let home = PathBuf::from("/home/user");
        let b = Breadcrumb::new(Path::new("/home/user/projects"), Some(&home));
        assert_eq!(labels(&b), vec!["~", "projects"]);
    }

    /// Home itself is a single `~` crumb, and it is the current one.
    #[test]
    fn home_itself_is_one_crumb() {
        let home = PathBuf::from("/home/user");
        let b = Breadcrumb::new(&home, Some(&home));
        assert_eq!(labels(&b), vec!["~"]);
        assert!(b.crumbs()[0].current);
    }

    /// Every crumb must carry the path it navigates to -- recomputing that
    /// from the label at click time is where off-by-one bugs live.
    #[test]
    fn each_crumb_knows_where_it_goes() {
        let home = PathBuf::from("/home/user");
        let b = Breadcrumb::new(Path::new("/home/user/a/b"), Some(&home));
        let targets: Vec<&Path> = b.crumbs().iter().map(|c| c.target.as_path()).collect();
        assert_eq!(
            targets,
            vec![
                Path::new("/home/user"),
                Path::new("/home/user/a"),
                Path::new("/home/user/a/b"),
            ]
        );
    }

    /// A path outside home keeps its real components, with `/` as a real,
    /// navigable crumb rather than an invisible prefix.
    #[test]
    fn an_absolute_path_starts_at_the_root() {
        let home = PathBuf::from("/home/user");
        let b = Breadcrumb::new(Path::new("/etc/systemd"), Some(&home));
        assert_eq!(labels(&b), vec!["/", "etc", "systemd"]);
        assert_eq!(b.crumbs()[0].target, Path::new("/"));
        assert_eq!(b.crumbs()[1].target, Path::new("/etc"));
    }

    /// The filesystem root alone is a single crumb, not an empty list.
    #[test]
    fn the_root_alone_is_one_crumb() {
        let b = Breadcrumb::new(Path::new("/"), None);
        assert_eq!(labels(&b), vec!["/"]);
        assert!(b.crumbs()[0].current);
    }

    /// With no home known, nothing collapses -- a browser on another user's
    /// tree must not claim their directory is yours.
    #[test]
    fn without_a_home_nothing_collapses() {
        let b = Breadcrumb::new(Path::new("/home/user/projects"), None);
        assert_eq!(labels(&b), vec!["/", "home", "user", "projects"]);
    }

    /// A *sibling* of home must not be mistaken for being inside it: a
    /// plain string prefix check would collapse `/home/user2` to `~2`.
    #[test]
    fn a_sibling_of_home_is_not_collapsed() {
        let home = PathBuf::from("/home/user");
        let b = Breadcrumb::new(Path::new("/home/user2/docs"), Some(&home));
        assert_eq!(labels(&b), vec!["/", "home", "user2", "docs"]);
    }

    /// Only the last crumb is current, and exactly one always is.
    #[test]
    fn exactly_the_last_crumb_is_current() {
        let home = PathBuf::from("/home/user");
        let b = Breadcrumb::new(Path::new("/home/user/a/b/c"), Some(&home));
        let current: Vec<bool> = b.crumbs().iter().map(|c| c.current).collect();
        assert_eq!(current, vec![false, false, false, true]);
    }

    /// A narrow toolbar keeps the *tail*: the head only says you are under
    /// /home, the tail says where you actually are.
    #[test]
    fn a_narrow_toolbar_keeps_the_tail() {
        let b = Breadcrumb::new(Path::new("/a/b/c/d/e"), None);
        let (kept, elided) = b.tail(2);
        assert!(elided);
        assert_eq!(
            kept.iter().map(|c| c.label.clone()).collect::<Vec<_>>(),
            vec!["d", "e"]
        );
    }

    /// A toolbar with room for everything elides nothing.
    #[test]
    fn a_wide_toolbar_elides_nothing() {
        let b = Breadcrumb::new(Path::new("/a/b"), None);
        let (kept, elided) = b.tail(10);
        assert!(!elided);
        assert_eq!(kept.len(), b.crumbs().len());
    }

    /// Degenerate widths must not panic or slice out of bounds.
    #[test]
    fn a_zero_width_toolbar_is_handled() {
        let b = Breadcrumb::new(Path::new("/a/b"), None);
        let (kept, elided) = b.tail(0);
        assert!(kept.is_empty());
        assert!(elided, "everything was dropped, so say so");
    }

    /// An empty path yields no crumbs rather than a bogus one.
    #[test]
    fn an_empty_path_yields_nothing() {
        let b = Breadcrumb::new(Path::new(""), None);
        assert!(b.is_empty());
        let (kept, elided) = b.tail(0);
        assert!(kept.is_empty());
        assert!(!elided, "nothing was dropped because there was nothing");
    }

    /// Non-ASCII components must survive intact -- they are extremely common
    /// in real home directories.
    #[test]
    fn non_ascii_components_survive() {
        let home = PathBuf::from("/home/user");
        let b = Breadcrumb::new(Path::new("/home/user/文書/写真"), Some(&home));
        assert_eq!(labels(&b), vec!["~", "文書", "写真"]);
    }
}

//! [`Rename`] -- the inline edit box F2 opens over a row.
//!
//! # What this owns, and what it refuses to own
//!
//! It owns the *editing session*: which path is being renamed, the text
//! typed so far, and whether that text is a legal new name. It does not
//! touch the filesystem and it does not know where the row is on screen --
//! the browser performs the rename, the window paints the box. That split is
//! what lets every rule below be tested without a compositor or a temp
//! directory.
//!
//! # Why the extension is not selected initially
//!
//! Desktop file managers open a rename with the stem selected and the
//! extension left alone, so typing immediately replaces `photo` but keeps
//! `.png`. This box has no selection model, so it does the honest
//! equivalent: it starts with the caret at the end and the whole name
//! intact. Nothing is destroyed by a stray keystroke, which is the property
//! that actually matters.
//!
//! # The rules
//!
//! A new name is refused when it:
//!
//! * is empty, or is only whitespace
//! * contains `/` -- that is a path, not a name, and would move the file
//! * is `.` or `..` -- both already name real directories
//! * contains a NUL byte, which no filesystem accepts
//!
//! Refusing is deliberately silent-but-visible: [`Self::is_valid`] lets the
//! window paint the box in an error colour while the name is unusable, and
//! [`Self::commit`] returns `None`, so Enter on a bad name does nothing
//! rather than reporting an error the user can already see.

use std::path::{Path, PathBuf};
use widget::TextInput;

/// What a committed rename asks the caller to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Renamed {
    /// The entry as it exists now.
    pub from: PathBuf,
    /// Where it should end up -- always a sibling of `from`, because a name
    /// containing `/` is refused.
    pub to: PathBuf,
}

/// An in-progress inline rename.
///
/// `None` inside means no rename is open; every method is a no-op in that
/// state rather than a panic, so the window can route keys through
/// unconditionally and ask afterwards whether anything happened.
#[derive(Debug, Default)]
pub struct Rename {
    /// The path being renamed, and the field holding its new name.
    session: Option<Session>,
}

#[derive(Debug)]
struct Session {
    path: PathBuf,
    /// The row the box is painted over, so the window need not re-resolve
    /// the path to a row on every frame.
    row: usize,
    input: TextInput,
}

impl Rename {
    #[must_use]
    pub const fn new() -> Self {
        Self { session: None }
    }

    /// Open a rename of `path`, shown over `row`, pre-filled with its
    /// current file name and the caret at the end.
    ///
    /// A path with no file name (the filesystem root) cannot be renamed, so
    /// no session opens.
    pub fn begin(&mut self, path: &Path, row: usize) -> bool {
        let Some(name) = path.file_name().and_then(std::ffi::OsStr::to_str) else {
            return false;
        };
        self.session = Some(Session {
            path: path.to_path_buf(),
            row,
            input: TextInput::with_text(name),
        });
        true
    }

    /// Whether a rename box is open.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.session.is_some()
    }

    /// The row the box is painted over, if one is open.
    #[must_use]
    pub fn row(&self) -> Option<usize> {
        self.session.as_ref().map(|s| s.row)
    }

    /// The path being renamed, if a box is open.
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn path(&self) -> Option<&Path> {
        self.session.as_ref().map(|s| s.path.as_path())
    }

    /// The edit field, for the window to paint and route edits into.
    #[must_use]
    pub fn input(&self) -> Option<&TextInput> {
        self.session.as_ref().map(|s| &s.input)
    }

    /// The edit field, mutably -- how the window applies a keystroke.
    pub fn input_mut(&mut self) -> Option<&mut TextInput> {
        self.session.as_mut().map(|s| &mut s.input)
    }

    /// Whether the name currently typed could be committed.
    ///
    /// False with no session open: there is no name, so there is nothing
    /// valid about it.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|s| Self::is_legal_name(s.input.text()))
    }

    /// Abandon the rename, discarding whatever was typed.
    pub fn cancel(&mut self) {
        self.session = None;
    }

    /// Close the box and report the rename to perform.
    ///
    /// Returns `None` -- leaving the box open -- when the typed name is
    /// illegal, so Enter on a bad name is inert rather than destructive. A
    /// name equal to the current one closes the box and reports nothing:
    /// renaming a file to itself is not a filesystem operation worth
    /// attempting.
    pub fn commit(&mut self) -> Option<Renamed> {
        let session = self.session.as_ref()?;
        let name = session.input.text().trim();
        if !Self::is_legal_name(name) {
            return None;
        }
        let unchanged = session.path.file_name().and_then(std::ffi::OsStr::to_str) == Some(name);
        let from = session.path.clone();
        let to = from.parent().unwrap_or_else(|| Path::new("")).join(name);
        self.session = None;
        (!unchanged).then_some(Renamed { from, to })
    }

    /// Whether `name` is usable as a file name -- see the module docs for
    /// each rule's reason.
    fn is_legal_name(name: &str) -> bool {
        let name = name.trim();
        !name.is_empty()
            && name != "."
            && name != ".."
            && !name.contains('/')
            && !name.contains('\0')
    }
}

#[cfg(test)]
mod tests {
    use super::Rename;
    use std::path::{Path, PathBuf};

    fn opened() -> Rename {
        let mut rename = Rename::new();
        assert!(rename.begin(Path::new("/home/u/photo.png"), 3));
        rename
    }

    fn type_name(rename: &mut Rename, name: &str) {
        let input = rename.input_mut().expect("a session is open");
        input.clear();
        for c in name.chars() {
            input.insert(c);
        }
    }

    #[test]
    fn a_fresh_rename_is_closed() {
        let rename = Rename::new();
        assert!(!rename.is_active());
        assert_eq!(rename.row(), None);
        assert_eq!(rename.path(), None);
    }

    /// Opening must not destroy the existing name: the user may only want to
    /// change one character of it.
    #[test]
    fn opening_prefills_the_current_name_with_the_caret_at_the_end() {
        let rename = opened();
        assert!(rename.is_active());
        assert_eq!(rename.row(), Some(3));
        let input = rename.input().expect("a session is open");
        assert_eq!(input.text(), "photo.png");
        assert_eq!(
            input.caret(),
            "photo.png".len(),
            "the caret must start at the end so typing appends"
        );
    }

    #[test]
    fn the_filesystem_root_cannot_be_renamed() {
        let mut rename = Rename::new();
        assert!(!rename.begin(Path::new("/"), 0));
        assert!(!rename.is_active());
    }

    #[test]
    fn committing_a_new_name_reports_a_sibling_path() {
        let mut rename = opened();
        type_name(&mut rename, "holiday.png");
        let renamed = rename.commit().expect("a legal name commits");
        assert_eq!(renamed.from, PathBuf::from("/home/u/photo.png"));
        assert_eq!(renamed.to, PathBuf::from("/home/u/holiday.png"));
        assert!(!rename.is_active(), "committing closes the box");
    }

    #[test]
    fn cancelling_discards_the_edit() {
        let mut rename = opened();
        type_name(&mut rename, "holiday.png");
        rename.cancel();
        assert!(!rename.is_active());
    }

    /// Renaming a file to the name it already has is not worth a syscall,
    /// but it must still close the box -- the user pressed Enter.
    #[test]
    fn committing_an_unchanged_name_closes_without_reporting_work() {
        let mut rename = opened();
        assert_eq!(rename.commit(), None);
        assert!(!rename.is_active());
    }

    /// A `/` would move the file to another directory, which is not what a
    /// rename box promises.
    #[test]
    fn a_name_containing_a_slash_is_refused_and_the_box_stays_open() {
        let mut rename = opened();
        type_name(&mut rename, "sub/photo.png");
        assert!(!rename.is_valid());
        assert_eq!(rename.commit(), None);
        assert!(
            rename.is_active(),
            "an illegal name must leave the box open to be corrected"
        );
    }

    #[test]
    fn an_empty_name_is_refused() {
        let mut rename = opened();
        type_name(&mut rename, "");
        assert!(!rename.is_valid());
        assert_eq!(rename.commit(), None);
        assert!(rename.is_active());
    }

    #[test]
    fn a_whitespace_only_name_is_refused() {
        let mut rename = opened();
        type_name(&mut rename, "   ");
        assert!(!rename.is_valid());
        assert_eq!(rename.commit(), None);
    }

    #[test]
    fn the_dot_directories_are_refused() {
        for name in [".", ".."] {
            let mut rename = opened();
            type_name(&mut rename, name);
            assert!(!rename.is_valid(), "{name} must be refused");
            assert_eq!(rename.commit(), None);
        }
    }

    /// Surrounding spaces are almost always a typo, and a file named
    /// `" photo.png "` is near-impossible to work with on a command line.
    #[test]
    fn a_name_is_trimmed_before_it_is_used() {
        let mut rename = opened();
        type_name(&mut rename, "  holiday.png  ");
        let renamed = rename.commit().expect("a trimmable name is legal");
        assert_eq!(renamed.to, PathBuf::from("/home/u/holiday.png"));
    }

    #[test]
    fn a_dotfile_can_be_renamed_to_another_dotfile() {
        let mut rename = Rename::new();
        assert!(rename.begin(Path::new("/home/u/.bashrc"), 0));
        type_name(&mut rename, ".zshrc");
        assert!(rename.is_valid());
        let renamed = rename.commit().expect("a dotfile name is legal");
        assert_eq!(renamed.to, PathBuf::from("/home/u/.zshrc"));
    }

    #[test]
    fn a_valid_name_reports_valid_while_typing() {
        let mut rename = opened();
        type_name(&mut rename, "holiday.png");
        assert!(rename.is_valid());
    }

    /// `TextInput::insert` rejects control characters, so a NUL cannot be
    /// typed -- but the guard is the thing that makes that safe to rely on.
    #[test]
    fn a_nul_byte_is_refused() {
        let mut rename = opened();
        rename
            .input_mut()
            .expect("a session is open")
            .set_text("pho\0to.png");
        assert!(!rename.is_valid());
        assert_eq!(rename.commit(), None);
    }
}

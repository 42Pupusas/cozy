//! [`FileClipboard`] -- the paths Ctrl-C / Ctrl-X put aside, and which of
//! the two verbs put them there.
//!
//! # Why a mode, and not two path lists
//!
//! Copy and cut differ only at *paste* time, and only in one respect: a cut
//! moves the source, a copy duplicates it. Modelling that as two collections
//! (`copied` and `cut`) makes every reader ask "which one is live?", and
//! makes "cut, then copy something else" a state with two plausible answers.
//! One list plus a [`Transfer`] verb has exactly one answer, and the answer
//! is the thing paste needs to know anyway.
//!
//! # Why a cut does not remove anything
//!
//! Ctrl-X in a file manager is not a deletion. The files stay exactly where
//! they are until a paste lands, because the paste is what decides where
//! they go -- and a paste that never comes must leave the directory
//! untouched. So a cut records intent only; [`Self::take_for_paste`] is what
//! turns that intent into filesystem work.
//!
//! A cut is *consumed* by its paste (the files have moved; pasting again
//! would fail on a source that no longer exists), while a copy survives, so
//! one Ctrl-C can feed several pastes into several directories. That
//! asymmetry is the whole reason `take_for_paste` reports the verb and
//! clears the state itself, instead of leaving both to the caller.

use std::path::{Path, PathBuf};

/// What a paste of the current clipboard should do to the source files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transfer {
    /// Duplicate the sources, leaving them in place (Ctrl-C).
    Copy,
    /// Relocate the sources (Ctrl-X).
    Move,
}

/// A pending paste: the paths and the verb that will be applied to them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub paths: Vec<PathBuf>,
    pub transfer: Transfer,
}

/// The file-manager clipboard: a set of paths plus the verb that will be
/// applied to them on paste.
///
/// Empty means nothing has been copied or cut, in which case paste is a
/// no-op rather than an error -- Ctrl-V on an empty clipboard is a user
/// mistake, not a fault to report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileClipboard {
    paths: Vec<PathBuf>,
    transfer: Option<Transfer>,
}

impl FileClipboard {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            paths: Vec::new(),
            transfer: None,
        }
    }

    /// Put `paths` on the clipboard to be duplicated on paste.
    ///
    /// An empty `paths` clears the clipboard rather than leaving the
    /// previous contents live: Ctrl-C with nothing selected must not silently
    /// paste whatever was copied ten minutes ago.
    pub fn copy(&mut self, paths: Vec<PathBuf>) {
        self.set(paths, Transfer::Copy);
    }

    /// Put `paths` on the clipboard to be relocated on paste. The files stay
    /// where they are until that paste happens -- see the module docs.
    pub fn cut(&mut self, paths: Vec<PathBuf>) {
        self.set(paths, Transfer::Move);
    }

    fn set(&mut self, paths: Vec<PathBuf>, transfer: Transfer) {
        if paths.is_empty() {
            self.clear();
            return;
        }
        self.paths = paths;
        self.transfer = Some(transfer);
    }

    /// Forget everything on the clipboard.
    pub fn clear(&mut self) {
        self.paths.clear();
        self.transfer = None;
    }

    /// Whether a paste would do anything.
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub const fn is_empty(&self) -> bool {
        self.transfer.is_none()
    }

    /// The paths currently held.
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    /// The verb a paste would apply, if anything is held.
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub const fn transfer(&self) -> Option<Transfer> {
        self.transfer
    }

    /// Whether `path` is on the clipboard awaiting a *move* -- the state a
    /// file manager shows by dimming the entry, so the user can see what
    /// their Ctrl-X marked.
    ///
    /// Deliberately false for a copy: a copied file is not going anywhere,
    /// so dimming it would promise a change that is not coming.
    #[must_use]
    pub fn is_cut(&self, path: &Path) -> bool {
        self.transfer == Some(Transfer::Move) && self.paths.iter().any(|p| p == path)
    }

    /// Take the clipboard's contents for a paste.
    ///
    /// A move is consumed (the sources will not exist afterwards); a copy is
    /// retained so it can be pasted into several places. Returns `None` when
    /// nothing is held.
    pub fn take_for_paste(&mut self) -> Option<Pending> {
        let transfer = self.transfer?;
        let paths = match transfer {
            Transfer::Copy => self.paths.clone(),
            Transfer::Move => {
                self.transfer = None;
                std::mem::take(&mut self.paths)
            }
        };
        Some(Pending { paths, transfer })
    }
}

#[cfg(test)]
mod tests {
    use super::{FileClipboard, Transfer};
    use std::path::PathBuf;

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn a_fresh_clipboard_holds_nothing() {
        let mut clip = FileClipboard::new();
        assert!(clip.is_empty());
        assert_eq!(clip.take_for_paste(), None);
    }

    #[test]
    fn copying_records_the_paths_and_the_copy_verb() {
        let mut clip = FileClipboard::new();
        clip.copy(paths(&["/a/one.txt", "/a/two.txt"]));
        assert!(!clip.is_empty());
        assert_eq!(clip.transfer(), Some(Transfer::Copy));
        assert_eq!(clip.paths(), paths(&["/a/one.txt", "/a/two.txt"]));
    }

    #[test]
    fn cutting_records_the_move_verb() {
        let mut clip = FileClipboard::new();
        clip.cut(paths(&["/a/one.txt"]));
        assert_eq!(clip.transfer(), Some(Transfer::Move));
    }

    /// A cut marks intent only. Nothing may be moved until a paste says
    /// where to -- so the clipboard still reports the path as held.
    #[test]
    fn cutting_does_not_consume_the_paths_before_a_paste() {
        let mut clip = FileClipboard::new();
        clip.cut(paths(&["/a/one.txt"]));
        assert_eq!(clip.paths(), paths(&["/a/one.txt"]));
        assert!(!clip.is_empty());
    }

    #[test]
    fn a_copy_survives_its_paste_so_it_can_be_pasted_again() {
        let mut clip = FileClipboard::new();
        clip.copy(paths(&["/a/one.txt"]));
        let first = clip.take_for_paste().expect("a copy is pending");
        assert_eq!(first.transfer, Transfer::Copy);
        assert_eq!(first.paths, paths(&["/a/one.txt"]));
        let second = clip.take_for_paste().expect("a copy outlives one paste");
        assert_eq!(second.paths, paths(&["/a/one.txt"]));
    }

    /// The sources no longer exist after a move, so a second paste could only
    /// fail. The clipboard empties itself rather than leaving that trap.
    #[test]
    fn a_cut_is_consumed_by_its_paste() {
        let mut clip = FileClipboard::new();
        clip.cut(paths(&["/a/one.txt"]));
        let first = clip.take_for_paste().expect("a cut is pending");
        assert_eq!(first.transfer, Transfer::Move);
        assert_eq!(first.paths, paths(&["/a/one.txt"]));
        assert!(clip.is_empty(), "a move must not survive its paste");
        assert_eq!(clip.take_for_paste(), None);
    }

    #[test]
    fn a_later_copy_replaces_an_earlier_cut() {
        let mut clip = FileClipboard::new();
        clip.cut(paths(&["/a/one.txt"]));
        clip.copy(paths(&["/a/two.txt"]));
        assert_eq!(clip.transfer(), Some(Transfer::Copy));
        assert_eq!(clip.paths(), paths(&["/a/two.txt"]));
    }

    /// Ctrl-C with an empty selection must not leave a stale clipboard live:
    /// the next Ctrl-V would paste something the user forgot they copied.
    #[test]
    fn copying_nothing_clears_rather_than_keeping_the_previous_contents() {
        let mut clip = FileClipboard::new();
        clip.copy(paths(&["/a/one.txt"]));
        clip.copy(Vec::new());
        assert!(clip.is_empty());
        assert_eq!(clip.paths(), &[] as &[PathBuf]);
    }

    #[test]
    fn cutting_nothing_clears_too() {
        let mut clip = FileClipboard::new();
        clip.copy(paths(&["/a/one.txt"]));
        clip.cut(Vec::new());
        assert!(clip.is_empty());
    }

    #[test]
    fn only_cut_paths_report_as_cut() {
        let mut clip = FileClipboard::new();
        clip.cut(paths(&["/a/one.txt"]));
        assert!(clip.is_cut(&PathBuf::from("/a/one.txt")));
        assert!(!clip.is_cut(&PathBuf::from("/a/other.txt")));
    }

    /// Dimming a copied file would promise a move that is not coming.
    #[test]
    fn a_copied_path_is_never_reported_as_cut() {
        let mut clip = FileClipboard::new();
        clip.copy(paths(&["/a/one.txt"]));
        assert!(!clip.is_cut(&PathBuf::from("/a/one.txt")));
    }

    #[test]
    fn clearing_empties_everything() {
        let mut clip = FileClipboard::new();
        clip.cut(paths(&["/a/one.txt"]));
        clip.clear();
        assert!(clip.is_empty());
        assert_eq!(clip.transfer(), None);
        assert_eq!(clip.paths(), &[] as &[PathBuf]);
    }
}

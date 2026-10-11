//! [`History`] -- the back/forward stacks behind the toolbar's chevrons.
//!
//! # Why this is its own type
//!
//! Back and forward look like one "where have I been" list, but they are two
//! stacks with an asymmetric rule tying them together: going somewhere *new*
//! must discard the forward stack, while going back must not. Folding that
//! into [`crate::browser::Browser`] would mean the rule lives in the middle of
//! `navigate`, next to filesystem reads and cursor placement, where nothing
//! can test it in isolation. Here it is a dozen lines of pure `PathBuf`
//! juggling with no filesystem at all.
//!
//! # What this type deliberately does not know
//!
//! It never stores "where you are now". The open directory has exactly one
//! home -- the `DirectoryView` -- and a second copy here would be free to
//! disagree with it. So the current path is *passed in* at each step and
//! handed back out, which is why [`Self::go_back`] takes the path being left.
//! The type is a pair of stacks, not a cursor into a list.

use std::path::{Path, PathBuf};

/// Whether a navigation should be recorded in the history.
///
/// Exists so that `Browser::navigate` -- the single chokepoint every navigation
/// passes through -- can tell an ordinary move apart from replaying one that is
/// already in the history. Without the distinction, pressing back would record
/// the step it was undoing, and the button would toggle between two
/// directories forever instead of walking backwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trail {
    /// A new move: remember where we came from, and abandon the forward path.
    Record,
    /// A back/forward step: the stacks have already been updated by hand.
    Replay,
}

/// The directories visited before and after the one now open.
#[derive(Debug, Default, Clone)]
pub struct History {
    /// Directories behind the current one, oldest first.
    back: Vec<PathBuf>,
    /// Directories ahead of the current one, *nearest first*.
    ///
    /// Stored nearest-first (rather than as a stack to push onto) so that a
    /// chain of backs replays forward in the order the user walked it.
    forward: Vec<PathBuf>,
}

impl History {
    /// How many steps each direction remembers.
    ///
    /// A file manager left open for a week would otherwise accumulate an
    /// unbounded list of every directory ever opened. The limit is generous
    /// enough that no realistic session reaches it and small enough that the
    /// memory is irrelevant either way.
    const CAPACITY: usize = 128;

    /// An empty history: nowhere to go in either direction.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            back: Vec::new(),
            forward: Vec::new(),
        }
    }

    /// Whether the back chevron should be live.
    #[must_use]
    pub const fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }

    /// Whether the forward chevron should be live.
    #[must_use]
    pub const fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    /// Record that the user navigated *away from* `left` to somewhere new.
    ///
    /// This is the call that discards the forward stack, and it is the whole
    /// asymmetry of the type: branching off mid-history means the old forward
    /// path is no longer somewhere the user can get back to by going forward.
    /// Every browser works this way, and the alternative -- keeping it --
    /// would let "forward" jump to a directory the user never came from.
    pub fn visit(&mut self, left: &Path) {
        self.push_back(left.to_path_buf());
        self.forward.clear();
    }

    /// Where a back step would land, without taking it.
    ///
    /// Split from [`Self::commit_back`] so the caller can attempt the read
    /// first and only spend the history step if it succeeded. A directory that
    /// has been unmounted since it was visited would otherwise consume a step
    /// while leaving the user where they stood.
    #[must_use]
    pub fn peek_back(&self) -> Option<&Path> {
        self.back.last().map(PathBuf::as_path)
    }

    /// Where a forward step would land, without taking it.
    #[must_use]
    pub fn peek_forward(&self) -> Option<&Path> {
        self.forward.first().map(PathBuf::as_path)
    }

    /// Complete the back step that [`Self::peek_back`] proposed, given the path
    /// just left.
    ///
    /// The path being left moves onto the *front* of the forward stack, so an
    /// immediate forward returns exactly here.
    pub fn commit_back(&mut self, left: &Path) {
        if self.back.pop().is_some() {
            Self::push_bounded(&mut self.forward, left.to_path_buf(), 0);
        }
    }

    /// Complete the forward step that [`Self::peek_forward`] proposed.
    pub fn commit_forward(&mut self, left: &Path) {
        if !self.forward.is_empty() {
            self.forward.remove(0);
            self.push_back(left.to_path_buf());
        }
    }

    /// Push onto the back stack, dropping the oldest entry when full.
    ///
    /// The *oldest* goes, not the newest: a full history should forget where
    /// you were an hour ago, not refuse to remember where you were a second
    /// ago.
    fn push_back(&mut self, path: PathBuf) {
        self.back.push(path);
        if self.back.len() > Self::CAPACITY {
            self.back.remove(0);
        }
    }

    /// Insert `path` at `at` in `stack`, keeping it within capacity by
    /// dropping from the far end.
    fn push_bounded(stack: &mut Vec<PathBuf>, path: PathBuf, at: usize) {
        stack.insert(at.min(stack.len()), path);
        if stack.len() > Self::CAPACITY {
            stack.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::History;
    use std::path::{Path, PathBuf};

    /// Walk a chain of `visit`s, returning the history and the final path, so
    /// the tests read as journeys rather than as stack pokes.
    struct Journey {
        history: History,
        at: PathBuf,
    }

    impl Journey {
        fn starting_at(path: &str) -> Self {
            Self {
                history: History::new(),
                at: PathBuf::from(path),
            }
        }

        /// Navigate somewhere new, as clicking a directory does.
        fn go_to(&mut self, path: &str) -> &mut Self {
            self.history.visit(&self.at);
            self.at = PathBuf::from(path);
            self
        }

        /// Press the back chevron, if it is live -- peek then commit, exactly
        /// as the browser does around a successful directory read.
        fn back(&mut self) -> &mut Self {
            if let Some(target) = self.history.peek_back().map(PathBuf::from) {
                let left = std::mem::replace(&mut self.at, target);
                self.history.commit_back(&left);
            }
            self
        }

        /// Press the forward chevron, if it is live.
        fn forward(&mut self) -> &mut Self {
            if let Some(target) = self.history.peek_forward().map(PathBuf::from) {
                let left = std::mem::replace(&mut self.at, target);
                self.history.commit_forward(&left);
            }
            self
        }

        /// Press back but have the directory read *fail*, as an unmounted or
        /// deleted directory does: the browser stays put and does not commit.
        fn back_but_the_read_fails(&mut self) -> &mut Self {
            let _ = self.history.peek_back();
            self
        }

        fn at(&self) -> &Path {
            &self.at
        }
    }

    #[test]
    fn a_fresh_history_goes_nowhere() {
        let history = History::new();
        assert!(
            !history.can_go_back(),
            "with nothing behind it, back must be dead"
        );
        assert!(
            !history.can_go_forward(),
            "with nothing ahead of it, forward must be dead"
        );
    }

    #[test]
    fn back_on_an_empty_history_is_refused_rather_than_guessed() {
        let mut history = History::new();
        assert!(
            history.peek_back().is_none(),
            "there is nowhere behind the first directory"
        );
        // Committing a step that was never proposed must not invent one.
        history.commit_back(Path::new("/home"));
        assert!(
            !history.can_go_forward(),
            "a refused back must not invent a forward entry"
        );
        assert!(!history.can_go_back(), "nor a back entry");
    }

    #[test]
    fn a_failed_read_does_not_consume_a_history_step() {
        // The reason peek and commit are separate. A directory unmounted since
        // it was visited leaves the browser standing where it was, and the back
        // button must still be aimed at it rather than having eaten the step.
        let mut journey = Journey::starting_at("/home");
        journey.go_to("/mnt/usb").back_but_the_read_fails();
        assert_eq!(
            journey.at(),
            Path::new("/mnt/usb"),
            "a failed read leaves the user where they were"
        );
        assert!(
            journey.history.can_go_back(),
            "the step must survive to be retried"
        );
        assert!(
            !journey.history.can_go_forward(),
            "and must not have leaked into the forward stack"
        );
        // Retrying, successfully this time, still works.
        journey.back();
        assert_eq!(journey.at(), Path::new("/home"));
    }

    #[test]
    fn visiting_somewhere_makes_back_live() {
        let mut journey = Journey::starting_at("/home");
        journey.go_to("/home/code");
        assert!(
            journey.history.can_go_back(),
            "after one navigation there is somewhere to go back to"
        );
        assert!(
            !journey.history.can_go_forward(),
            "a fresh navigation leaves nothing ahead"
        );
    }

    #[test]
    fn back_returns_to_where_you_came_from() {
        let mut journey = Journey::starting_at("/home");
        journey.go_to("/home/code").back();
        assert_eq!(journey.at(), Path::new("/home"));
    }

    #[test]
    fn forward_returns_to_where_back_left() {
        let mut journey = Journey::starting_at("/home");
        journey.go_to("/home/code").back().forward();
        assert_eq!(journey.at(), Path::new("/home/code"));
    }

    #[test]
    fn a_deep_walk_unwinds_in_reverse_order() {
        let mut journey = Journey::starting_at("/");
        journey.go_to("/home").go_to("/home/code").go_to("/tmp");
        for expected in ["/home/code", "/home", "/"] {
            journey.back();
            assert_eq!(
                journey.at(),
                Path::new(expected),
                "back must retrace the walk exactly"
            );
        }
        assert!(
            !journey.history.can_go_back(),
            "unwinding the whole walk must exhaust the back stack"
        );
    }

    #[test]
    fn a_chain_of_backs_replays_forward_in_the_order_it_was_walked() {
        // The reason `forward` is stored nearest-first: three backs then three
        // forwards must land where you started, not walk the path inverted.
        let mut journey = Journey::starting_at("/");
        journey.go_to("/home").go_to("/home/code").go_to("/tmp");
        journey.back().back().back();
        for expected in ["/home", "/home/code", "/tmp"] {
            journey.forward();
            assert_eq!(
                journey.at(),
                Path::new(expected),
                "forward must replay the walk in its original order"
            );
        }
        assert!(
            !journey.history.can_go_forward(),
            "replaying the whole walk must exhaust the forward stack"
        );
    }

    #[test]
    fn navigating_somewhere_new_discards_the_forward_stack() {
        // The load-bearing asymmetry: after branching off, "forward" must not
        // offer a directory the user can no longer reach by going forward.
        let mut journey = Journey::starting_at("/home");
        journey.go_to("/home/code").back();
        assert!(
            journey.history.can_go_forward(),
            "back must leave something ahead"
        );
        journey.go_to("/etc");
        assert!(
            !journey.history.can_go_forward(),
            "branching off must discard the abandoned forward path"
        );
        assert!(
            journey.history.can_go_back(),
            "branching off must still remember where it branched from"
        );
    }

    #[test]
    fn forward_on_an_exhausted_stack_is_refused() {
        let mut journey = Journey::starting_at("/home");
        journey.go_to("/home/code").back().forward();
        assert!(
            journey.history.peek_forward().is_none(),
            "there is nothing ahead once the forward stack is replayed"
        );
    }

    #[test]
    fn revisiting_the_same_directory_is_still_a_step() {
        // Deliberately *not* deduplicated: a user who went out of a directory
        // and back into it walked two steps, and collapsing them would make
        // the back button skip one of them.
        let mut journey = Journey::starting_at("/home");
        journey.go_to("/home/code").go_to("/home").go_to("/tmp");
        journey.back();
        assert_eq!(journey.at(), Path::new("/home"));
        journey.back();
        assert_eq!(journey.at(), Path::new("/home/code"));
    }

    #[test]
    fn a_long_session_stays_bounded_and_keeps_the_recent_past() {
        let mut journey = Journey::starting_at("/start");
        for i in 0..History::CAPACITY * 3 {
            journey.go_to(&format!("/dir{i}"));
        }
        assert!(
            journey.history.back.len() <= History::CAPACITY,
            "a long-lived session must not grow without bound"
        );
        // The most recent step is the one that matters, so it must survive.
        journey.back();
        let last = History::CAPACITY * 3 - 1;
        assert_eq!(
            journey.at(),
            Path::new(&format!("/dir{}", last - 1)),
            "the newest history must be kept, not the oldest"
        );
    }

    #[test]
    fn a_bounded_forward_stack_also_stays_within_capacity() {
        let mut journey = Journey::starting_at("/start");
        for i in 0..History::CAPACITY {
            journey.go_to(&format!("/dir{i}"));
        }
        for _ in 0..History::CAPACITY {
            journey.back();
        }
        assert!(
            journey.history.forward.len() <= History::CAPACITY,
            "unwinding a full back stack must not overflow the forward one"
        );
    }

    #[test]
    fn non_utf8_paths_survive_a_round_trip() {
        // Paths are bytes, and a file manager that mangled them would be
        // unable to navigate back into a directory it had just opened.
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let odd = PathBuf::from(OsStr::from_bytes(b"/tmp/\xff\xfeodd"));
        let mut history = History::new();
        history.visit(&odd);
        assert_eq!(history.peek_back(), Some(&*odd));
        history.commit_back(Path::new("/tmp"));
        assert_eq!(history.peek_forward(), Some(Path::new("/tmp")));
    }
}

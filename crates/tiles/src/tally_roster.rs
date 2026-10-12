//! [`TallyRoster`] -- the size tallies currently running, and which one each
//! unsettled directory's total is expected from.
//!
//! Remeasuring one changed folder starts a small tally beside the running
//! one instead of restarting it. A folder handed to a newer tally is
//! released by the older one, whose late reports for it are then ignored:
//! they may describe the contents from before the change.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use fsmodel::tally::{SizeTally, Tallied};

struct RunningTally {
    id: u64,
    tally: SizeTally,
}

pub struct TallyRoster {
    wake: waker::Waker,
    next: u64,
    running: Vec<RunningTally>,
    owner: HashMap<PathBuf, u64>,
}

impl TallyRoster {
    #[must_use]
    pub fn new(wake: waker::Waker) -> Self {
        Self {
            wake,
            next: 0,
            running: Vec::new(),
            owner: HashMap::new(),
        }
    }

    /// Whether a running tally will report `path`.
    #[must_use]
    pub fn owns(&self, path: &Path) -> bool {
        self.owner.contains_key(path)
    }

    /// Measure `paths` in a new tally.
    pub fn start(&mut self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let wake = self.wake.clone();
        let Ok(tally) = SizeTally::start(paths.clone(), move || wake.signal())
            .inspect_err(|error| eprintln!("files: size tally: {error}"))
        else {
            return;
        };
        let id = self.next;
        self.next += 1;
        self.owner.extend(paths.into_iter().map(|path| (path, id)));
        self.running.push(RunningTally { id, tally });
    }

    /// Stop expecting `path` from whichever tally has it.
    pub fn release(&mut self, path: &Path) {
        self.owner.remove(path);
    }

    /// Stop expecting every child of `dir`.
    pub fn release_children_of(&mut self, dir: &Path) {
        self.owner.retain(|path, _| path.parent() != Some(dir));
    }

    /// Keep expecting only paths in `wanted`, cancelling tallies left with
    /// nothing wanted.
    pub fn keep_only(&mut self, wanted: &HashSet<&Path>) {
        self.owner.retain(|path, _| wanted.contains(path.as_path()));
        self.cancel_idle();
    }

    /// The reports that arrived from each path's current tally. Tallies left
    /// with nothing to report are cancelled.
    pub fn drain(&mut self) -> Vec<Tallied> {
        let mut arrived = Vec::new();
        for running in &mut self.running {
            for tallied in running.tally.drain() {
                if self.owner.get(&tallied.path) != Some(&running.id) {
                    continue;
                }
                if tallied.settled {
                    self.owner.remove(&tallied.path);
                }
                arrived.push(tallied);
            }
        }
        self.cancel_idle();
        arrived
    }

    fn cancel_idle(&mut self) {
        let busy: HashSet<u64> = self.owner.values().copied().collect();
        self.running.retain(|running| busy.contains(&running.id));
    }

    #[cfg(test)]
    const fn running(&self) -> usize {
        self.running.len()
    }
}

#[cfg(test)]
mod tests {
    use super::TallyRoster;
    use crate::test_support::TempDir;
    use std::collections::HashSet;
    use std::path::Path;

    fn roster() -> (TallyRoster, waker::Waker) {
        let wake = waker::Waker::new().expect("eventfd");
        (TallyRoster::new(wake.clone()), wake)
    }

    fn settle(roster: &mut TallyRoster, wake: &waker::Waker) -> Vec<fsmodel::tally::Tallied> {
        let mut got = Vec::new();
        while roster.running() > 0 {
            assert!(wake.wait(5000), "the tally never signalled");
            wake.drain();
            got.extend(roster.drain());
        }
        got
    }

    #[test]
    fn a_started_path_is_owned_until_it_settles() {
        let dir = TempDir::new("roster_settle");
        let folder = dir.create_dir("folder");
        dir.create_file("folder/a", &[0; 9]);
        let (mut roster, wake) = roster();
        roster.start(vec![folder.clone()]);
        assert!(roster.owns(&folder));
        let got = settle(&mut roster, &wake);
        assert_eq!(got.last().map(|tallied| tallied.bytes), Some(9));
        assert!(!roster.owns(&folder));
    }

    #[test]
    fn a_released_path_reports_nothing_and_its_tally_is_cancelled() {
        let dir = TempDir::new("roster_release");
        let folder = dir.create_dir("folder");
        let (mut roster, _wake) = roster();
        roster.start(vec![folder.clone()]);
        roster.release(&folder);
        assert!(roster.drain().is_empty());
        assert_eq!(roster.running(), 0);
    }

    #[test]
    fn a_newer_tally_takes_over_a_path_from_the_older_one() {
        let dir = TempDir::new("roster_takeover");
        let one = dir.create_dir("one");
        let two = dir.create_dir("two");
        dir.create_file("two/b", &[0; 4]);
        let (mut roster, wake) = roster();
        roster.start(vec![one, two.clone()]);
        roster.release(&two);
        roster.start(vec![two.clone()]);
        assert_eq!(roster.running(), 2);
        let got = settle(&mut roster, &wake);
        let settled_two = got.iter().filter(|t| t.path == two && t.settled).count();
        assert_eq!(settled_two, 1, "only the newer tally's total counts");
    }

    #[test]
    fn keeping_only_other_paths_cancels_the_tally() {
        let dir = TempDir::new("roster_keep");
        let folder = dir.create_dir("folder");
        let (mut roster, _wake) = roster();
        roster.start(vec![folder]);
        roster.keep_only(&HashSet::from([Path::new("/elsewhere")]));
        assert_eq!(roster.running(), 0);
    }

    #[test]
    fn releasing_a_folder_s_children_spares_other_folders() {
        let (mut roster, _wake) = roster();
        let dir = TempDir::new("roster_children");
        let inside = dir.create_dir("d/a");
        let outside = dir.create_dir("e/b");
        roster.start(vec![inside.clone(), outside.clone()]);
        roster.release_children_of(&dir.path().join("d"));
        assert!(!roster.owns(&inside));
        assert!(roster.owns(&outside));
    }
}

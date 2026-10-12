//! [`ProcessLevel`] -- one level of process tiles (the groups, or one
//! group's members): its entries by name and their smoothed ranking.

use std::collections::HashMap;

use crate::group_smoother::{GroupSmoother, Share};
use crate::process_group::ProcessGroup;

#[derive(Default)]
pub struct ProcessLevel {
    smoother: GroupSmoother,
    groups: HashMap<String, ProcessGroup>,
}

impl ProcessLevel {
    #[must_use]
    pub fn new(groups: Vec<ProcessGroup>) -> Self {
        let mut level = Self::default();
        level.update(groups);
        level
    }

    pub fn update(&mut self, groups: Vec<ProcessGroup>) {
        self.smoother.update(&groups);
        self.groups = groups
            .into_iter()
            .map(|group| (group.name.clone(), group))
            .collect();
    }

    /// Largest first.
    #[must_use]
    pub fn shares(&self) -> &[Share] {
        self.smoother.shares()
    }

    #[must_use]
    pub fn group(&self, name: &str) -> Option<&ProcessGroup> {
        self.groups.get(name)
    }

    #[must_use]
    pub fn cpu(&self, name: &str) -> f64 {
        self.smoother.cpu(name).unwrap_or(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::ProcessLevel;
    use crate::process_group::ProcessGroup;

    fn group(name: &str, memory_kb: u64) -> ProcessGroup {
        ProcessGroup {
            name: name.to_owned(),
            count: 1,
            cpu: 2.0,
            memory_kb,
        }
    }

    #[test]
    fn entries_rank_by_memory_and_are_found_by_name() {
        let level = ProcessLevel::new(vec![group("a", 10), group("b", 30)]);
        let names: Vec<&str> = level.shares().iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["b", "a"]);
        assert_eq!(level.group("a").map(|g| g.memory_kb), Some(10));
        assert!((level.cpu("b") - 2.0).abs() < 1e-9);
        assert!(level.group("z").is_none());
        assert!(level.cpu("z").abs() < 1e-9);
    }
}

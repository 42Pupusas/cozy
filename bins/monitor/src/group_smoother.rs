//! [`GroupSmoother`] -- an exponential average of each group's memory and
//! CPU across process sweeps, so tiles do not jump on every sample.

use std::collections::HashMap;

use crate::process_group::ProcessGroup;

/// A group's share of all memory, after smoothing.
#[derive(Clone, Debug, PartialEq)]
pub struct Share {
    pub name: String,
    pub share: f64,
}

#[derive(Clone, Copy)]
struct Level {
    memory: f64,
    cpu: f64,
}

#[derive(Default)]
pub struct GroupSmoother {
    levels: HashMap<String, Level>,
    shares: Vec<Share>,
}

impl GroupSmoother {
    const ALPHA: f64 = 0.3;

    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[allow(clippy::cast_precision_loss)]
    pub fn update(&mut self, groups: &[ProcessGroup]) {
        let mut next = HashMap::with_capacity(groups.len());
        for group in groups {
            let memory = group.memory_kb as f64;
            let cpu = f64::from(group.cpu);
            let level = self
                .levels
                .get(&group.name)
                .map_or(Level { memory, cpu }, |previous| Level {
                    memory: Self::ALPHA.mul_add(memory - previous.memory, previous.memory),
                    cpu: Self::ALPHA.mul_add(cpu - previous.cpu, previous.cpu),
                });
            next.insert(group.name.clone(), level);
        }
        self.levels = next;
        self.rank();
    }

    fn rank(&mut self) {
        let total: f64 = self
            .levels
            .values()
            .map(|level| level.memory.max(0.0))
            .sum();
        let mut ranked: Vec<(&String, Level)> = self
            .levels
            .iter()
            .map(|(name, level)| (name, *level))
            .collect();
        ranked.sort_by(|a, b| {
            b.1.memory
                .total_cmp(&a.1.memory)
                .then_with(|| b.1.cpu.total_cmp(&a.1.cpu))
                .then_with(|| a.0.cmp(b.0))
        });
        self.shares = ranked
            .into_iter()
            .map(|(name, level)| Share {
                name: name.clone(),
                share: if total > 0.0 {
                    level.memory.max(0.0) / total
                } else {
                    0.0
                },
            })
            .collect();
    }

    /// Largest first.
    #[must_use]
    pub fn shares(&self) -> &[Share] {
        &self.shares
    }

    /// The smoothed CPU percentage of `name`.
    #[must_use]
    pub fn cpu(&self, name: &str) -> Option<f64> {
        self.levels.get(name).map(|level| level.cpu)
    }
}

#[cfg(test)]
mod tests {
    use super::GroupSmoother;
    use crate::process_group::ProcessGroup;

    fn group(name: &str, cpu: f32, memory_kb: u64) -> ProcessGroup {
        ProcessGroup {
            name: name.to_owned(),
            count: 1,
            cpu,
            memory_kb,
        }
    }

    #[test]
    fn shares_sum_to_one_largest_first() {
        let mut smoother = GroupSmoother::new();
        smoother.update(&[
            group("a", 0.0, 100),
            group("b", 0.0, 300),
            group("c", 0.0, 600),
        ]);
        let names: Vec<&str> = smoother.shares().iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["c", "b", "a"]);
        let total: f64 = smoother.shares().iter().map(|s| s.share).sum();
        assert!((total - 1.0).abs() < 1e-9);
        assert!((smoother.shares()[0].share - 0.6).abs() < 1e-9);
    }

    #[test]
    fn a_jump_is_only_partly_followed() {
        let mut smoother = GroupSmoother::new();
        smoother.update(&[group("a", 0.0, 1000)]);
        smoother.update(&[group("a", 100.0, 2000)]);
        let cpu = smoother.cpu("a").expect("a is tracked");
        assert!(cpu > 0.0 && cpu < 100.0, "{cpu}");
        let jump = 0.3_f64.mul_add(1000.0, 1000.0);
        let again = {
            let mut other = GroupSmoother::new();
            other.update(&[group("a", 0.0, 1000), group("z", 0.0, 1000)]);
            other.update(&[group("a", 0.0, 2000), group("z", 0.0, 1000)]);
            other.shares()[0].share
        };
        assert!((again - jump / (jump + 1000.0)).abs() < 1e-9, "{again}");
    }

    #[test]
    fn groups_that_vanish_are_forgotten_and_new_ones_start_at_their_value() {
        let mut smoother = GroupSmoother::new();
        smoother.update(&[group("old", 5.0, 100)]);
        smoother.update(&[group("new", 8.0, 100)]);
        assert!(smoother.cpu("old").is_none());
        assert!((smoother.cpu("new").expect("tracked") - 8.0).abs() < 1e-9);
    }

    #[test]
    fn without_any_memory_the_busier_group_ranks_first() {
        let mut smoother = GroupSmoother::new();
        smoother.update(&[group("idle", 0.0, 0), group("busy", 9.0, 0)]);
        assert_eq!(smoother.shares()[0].name, "busy");
        assert!(smoother.shares().iter().all(|s| s.share == 0.0));
    }
}

//! [`ProcessGrouper`] -- folds processes into one [`ProcessGroup`] per name
//! family, so `kworker/u8:3-events` and `kworker/0:1H` share a tile.

use std::collections::HashMap;

use sysmon::ProcessInfo;

#[derive(Clone, Debug, PartialEq)]
pub struct ProcessGroup {
    pub name: String,
    pub count: usize,
    pub cpu: f32,
    pub memory_kb: u64,
}

pub struct ProcessGrouper;

impl ProcessGrouper {
    const APPIMAGE_SUFFIX: &'static str = ".appimage";

    /// The family a process name belongs to: the part before the first `/`,
    /// without an `.AppImage` suffix and without a trailing `_<digits>` index
    /// (`scsi_eh_3` -> `scsi_eh`). Digits that are part of a name (`python3`)
    /// are kept.
    #[must_use]
    pub fn family(name: &str) -> &str {
        let stem = name.split('/').next().unwrap_or(name);
        let stem = Self::without_appimage(stem);
        let trimmed = stem.trim_end_matches(|c: char| c.is_ascii_digit());
        let indexed = trimmed.len() < stem.len() && trimmed.ends_with('_');
        let family = if indexed {
            trimmed.trim_end_matches('_')
        } else {
            stem
        };
        if family.is_empty() { name } else { family }
    }

    fn without_appimage(stem: &str) -> &str {
        let cut = stem.len().saturating_sub(Self::APPIMAGE_SUFFIX.len());
        match stem.get(cut..) {
            Some(tail) if cut > 0 && tail.eq_ignore_ascii_case(Self::APPIMAGE_SUFFIX) => {
                &stem[..cut]
            }
            _ => stem,
        }
    }

    /// One group per family, largest memory first; ties go to the busier
    /// group, then to the name.
    #[must_use]
    pub fn group(processes: &[ProcessInfo]) -> Vec<ProcessGroup> {
        let mut groups: HashMap<&str, ProcessGroup> = HashMap::new();
        for process in processes {
            let family = Self::family(&process.name);
            let group = groups.entry(family).or_insert_with(|| ProcessGroup {
                name: family.to_owned(),
                count: 0,
                cpu: 0.0,
                memory_kb: 0,
            });
            group.count += 1;
            group.cpu += process.cpu;
            group.memory_kb += process.memory_kb;
        }
        let mut grouped: Vec<ProcessGroup> = groups.into_values().collect();
        grouped.sort_by(|a, b| {
            b.memory_kb
                .cmp(&a.memory_kb)
                .then_with(|| b.cpu.total_cmp(&a.cpu))
                .then_with(|| a.name.cmp(&b.name))
        });
        grouped
    }
}

#[cfg(test)]
mod tests {
    use super::ProcessGrouper;
    use sysmon::ProcessInfo;

    fn process(pid: u32, name: &str, cpu: f32, memory_kb: u64) -> ProcessInfo {
        ProcessInfo {
            pid,
            name: name.to_owned(),
            cpu,
            memory_kb,
        }
    }

    #[test]
    fn slash_suffixes_fold_into_the_family() {
        for name in [
            "kworker/u8:3-events",
            "kworker/0:1H",
            "kworker/R-rcu_gp",
            "kworker",
        ] {
            assert_eq!(ProcessGrouper::family(name), "kworker", "{name}");
        }
        assert_eq!(ProcessGrouper::family("ksoftirqd/12"), "ksoftirqd");
        assert_eq!(ProcessGrouper::family("irq/24-pciehp"), "irq");
    }

    #[test]
    fn an_appimage_runtime_joins_the_program_it_launches() {
        assert_eq!(ProcessGrouper::family("helium.AppImage"), "helium");
        assert_eq!(ProcessGrouper::family("helium"), "helium");
        assert_eq!(ProcessGrouper::family(".AppImage"), ".AppImage");
    }

    #[test]
    fn trailing_indices_fold_only_after_an_underscore() {
        assert_eq!(ProcessGrouper::family("scsi_eh_3"), "scsi_eh");
        assert_eq!(ProcessGrouper::family("python3"), "python3");
        assert_eq!(ProcessGrouper::family("gnome-shell"), "gnome-shell");
        assert_eq!(ProcessGrouper::family("_1"), "_1");
        assert_eq!(ProcessGrouper::family("/odd"), "/odd");
    }

    #[test]
    fn a_family_sums_its_members() {
        let groups = ProcessGrouper::group(&[
            process(1, "kworker/0:1", 1.5, 0),
            process(2, "kworker/u8:3-events", 2.5, 0),
            process(3, "firefox", 10.0, 900),
            process(4, "firefox", 5.0, 100),
        ]);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].name, "firefox");
        assert_eq!(groups[0].count, 2);
        assert_eq!(groups[0].memory_kb, 1000);
        assert!((groups[0].cpu - 15.0).abs() < f32::EPSILON);
        assert_eq!(groups[1].name, "kworker");
        assert_eq!(groups[1].count, 2);
        assert!((groups[1].cpu - 4.0).abs() < f32::EPSILON);
    }

    #[test]
    fn equal_memory_ranks_the_busier_group_first() {
        let groups = ProcessGrouper::group(&[
            process(1, "idle", 0.0, 0),
            process(2, "busy", 9.0, 0),
            process(3, "also-idle", 0.0, 0),
        ]);
        let names: Vec<&str> = groups.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["busy", "also-idle", "idle"]);
    }

    #[test]
    fn no_processes_make_no_groups() {
        assert!(ProcessGrouper::group(&[]).is_empty());
    }
}

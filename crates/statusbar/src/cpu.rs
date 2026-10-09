//! System-wide CPU utilization, from successive `/proc/stat` samples.
//!
//! Currently unused by the bar (see `Snapshot::right_segments`'s comment on
//! why the CPU segment is disabled) but kept as a real module rather than
//! deleted, since re-enabling it is a one-line change in the worker.

use std::fs;

/// Idle/total jiffy counts from one `/proc/stat` read.
#[derive(Clone, Copy)]
struct CpuTimes {
    idle: u64,
    total: u64,
}

impl CpuTimes {
    fn read() -> Option<Self> {
        let text = fs::read_to_string("/proc/stat").ok()?;
        Self::parse(text.lines().next()?)
    }

    fn parse(line: &str) -> Option<Self> {
        let mut fields = line.split_whitespace();
        fields.next()?;
        let nums: Vec<u64> = fields.filter_map(|f| f.parse().ok()).collect();
        if nums.len() < 4 {
            return None;
        }
        let idle = nums[3] + nums.get(4).copied().unwrap_or(0);
        let total: u64 = nums.iter().sum();
        Some(Self { idle, total })
    }

    /// Busy percentage over the jiffies elapsed since `earlier`.
    #[allow(clippy::cast_precision_loss)]
    fn busy_percent_since(self, earlier: Self) -> f64 {
        let idle_delta = self.idle.saturating_sub(earlier.idle);
        let total_delta = self.total.saturating_sub(earlier.total);
        if total_delta == 0 {
            return 0.0;
        }
        100.0 * (1.0 - idle_delta as f64 / total_delta as f64)
    }
}

/// CPU utilization between consecutive calls to [`Cpu::usage`].
///
/// The first sample is taken at construction, so the first `usage` call
/// reports the span since then. Spacing the calls is the caller's business;
/// nothing here sleeps.
pub struct Cpu {
    previous: Option<CpuTimes>,
}

impl Cpu {
    #[must_use]
    pub fn new() -> Self {
        Self {
            previous: CpuTimes::read(),
        }
    }

    /// Utilization since the previous call, formatted for the bar.
    pub fn usage(&mut self) -> Option<String> {
        let now = CpuTimes::read()?;
        let earlier = self.previous.replace(now)?;
        let pct = now.busy_percent_since(earlier);
        Some(format!("{pct:>3.0}% "))
    }
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sums_all_fields_and_counts_iowait_as_idle() {
        let times = CpuTimes::parse("cpu  10 0 10 70 10 0 0 0 0 0").unwrap();
        assert_eq!(times.idle, 80);
        assert_eq!(times.total, 100);
    }

    #[test]
    fn parse_rejects_short_lines() {
        assert!(CpuTimes::parse("cpu 1 2 3").is_none());
    }

    #[test]
    fn busy_percent_is_the_non_idle_share_of_the_delta() {
        let earlier = CpuTimes {
            idle: 100,
            total: 200,
        };
        let now = CpuTimes {
            idle: 150,
            total: 300,
        };
        assert!((now.busy_percent_since(earlier) - 50.0).abs() < f64::EPSILON);
    }

    #[test]
    fn no_elapsed_jiffies_reads_as_idle() {
        let times = CpuTimes { idle: 5, total: 10 };
        assert!(times.busy_percent_since(times).abs() < f64::EPSILON);
    }

    #[test]
    fn second_call_reports_a_percentage() {
        let mut cpu = Cpu::new();
        let text = cpu.usage().expect("proc stat readable");
        assert!(text.trim_end().ends_with('%'));
    }
}

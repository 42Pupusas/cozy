//! System-wide CPU utilization, from two `/proc/stat` samples.
//!
//! Currently unused by the bar (see `Snapshot::right_segments`'s comment on
//! why the CPU segment is disabled) but kept as a real module rather than
//! deleted, since re-enabling it is a one-line change in the worker.

use std::fs;

/// Idle/total jiffy counts from one `/proc/stat` read -- the two numbers
/// [`Cpu::usage`] diffs across a sampling window to get a percentage.
struct CpuTimes {
    idle: u64,
    total: u64,
}

impl CpuTimes {
    fn read() -> Option<Self> {
        let text = fs::read_to_string("/proc/stat").ok()?;
        let line = text.lines().next()?; // "cpu  user nice system idle iowait irq softirq steal ..."
        let mut fields = line.split_whitespace();
        fields.next()?; // "cpu"
        let nums: Vec<u64> = fields.filter_map(|f| f.parse().ok()).collect();
        if nums.len() < 4 {
            return None;
        }
        let idle = nums[3] + nums.get(4).copied().unwrap_or(0); // idle + iowait
        let total: u64 = nums.iter().sum();
        Some(Self { idle, total })
    }
}

/// CPU utilization queries -- a marker type (like [`crate::volume::Volume`])
/// since every call is a fresh pair of `/proc/stat` reads.
pub struct Cpu;

impl Cpu {
    /// System-wide CPU utilization percentage over a short sampling window,
    /// from two `/proc/stat` reads ~100ms apart. Blocks the calling thread
    /// for that window -- callers should poll this off the UI thread's hot
    /// path (e.g. a dedicated module-refresh timer), not every frame.
    #[must_use]
    pub fn usage() -> Option<String> {
        let a = CpuTimes::read()?;
        std::thread::sleep(std::time::Duration::from_millis(120));
        let b = CpuTimes::read()?;
        let idle_delta = b.idle.saturating_sub(a.idle);
        let total_delta = b.total.saturating_sub(a.total);
        if total_delta == 0 {
            return Some("0%".into());
        }
        #[allow(clippy::cast_precision_loss)]
        let pct = 100.0 * (1.0 - idle_delta as f64 / total_delta as f64);
        Some(format!("{pct:>3.0}% "))
    }
}

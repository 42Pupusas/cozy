//! [`ProcessSample`] -- a process's CPU time and resident memory, read from
//! `/proc/<pid>`.

use std::time::Duration;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcessSample {
    pub cpu: Duration,
    pub rss_kb: u64,
}

impl ProcessSample {
    /// `/proc` reports CPU time in clock ticks of 1/100 s on Linux.
    const TICKS_PER_SECOND: u64 = 100;

    #[must_use]
    pub fn now() -> Self {
        Self::read("self")
    }

    #[must_use]
    pub fn of(pid: u32) -> Self {
        Self::read(&pid.to_string())
    }

    fn read(proc_entry: &str) -> Self {
        let stat = std::fs::read_to_string(format!("/proc/{proc_entry}/stat")).unwrap_or_default();
        let status =
            std::fs::read_to_string(format!("/proc/{proc_entry}/status")).unwrap_or_default();
        Self {
            cpu: Self::parse_cpu(&stat),
            rss_kb: Self::parse_rss(&status),
        }
    }

    /// `utime` and `stime` are fields 14 and 15; the command name before
    /// them may contain spaces, so counting starts after its closing paren.
    fn parse_cpu(stat: &str) -> Duration {
        let after_name = stat.rsplit_once(')').map_or("", |(_, rest)| rest);
        let fields: Vec<&str> = after_name.split_whitespace().collect();
        let ticks = |i: usize| {
            fields
                .get(i)
                .and_then(|f| f.parse::<u64>().ok())
                .unwrap_or(0)
        };
        let total = ticks(11) + ticks(12);
        Duration::from_millis(total * 1000 / Self::TICKS_PER_SECOND)
    }

    fn parse_rss(status: &str) -> u64 {
        status
            .lines()
            .find_map(|line| line.strip_prefix("VmRSS:"))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|kb| kb.parse().ok())
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::ProcessSample;
    use std::time::Duration;

    #[test]
    fn cpu_is_utime_plus_stime_after_the_name() {
        let stat = "42 (tile compare) S 1 2 3 4 5 6 7 8 9 10 150 50 0 0";
        assert_eq!(ProcessSample::parse_cpu(stat), Duration::from_secs(2));
    }

    #[test]
    fn rss_reads_the_vmrss_line() {
        let status = "Name:\tx\nVmRSS:\t   12345 kB\nThreads:\t2\n";
        assert_eq!(ProcessSample::parse_rss(status), 12_345);
    }

    #[test]
    fn this_process_reports_some_memory() {
        assert!(ProcessSample::now().rss_kb > 0);
    }

    #[test]
    fn this_process_by_pid_matches_self() {
        assert!(ProcessSample::of(std::process::id()).rss_kb > 0);
    }
}

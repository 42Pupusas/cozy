//! Headless `monitor --snapshot`: collect one real telemetry sample and print
//! it as a text report, without opening a Wayland window.
//!
//! The GUI's `App` owns a live window, a background sampler thread, and a 60
//! Hz paint loop -- none of which a one-shot report needs. This module is a
//! separate, thin entry point that talks to `sysmon::MonitorReader` directly
//! and formats the result through the same [`DashboardReport`] the painter
//! uses, so the printed numbers are provably the numbers the GUI would show
//! for the same sample, not a second hand-maintained formatting path.

use std::fmt::Write as _;
use std::time::Duration;

use monitor::dashboard_report::{CardReport, DashboardReport};
use sysmon::MonitorReader;

/// Command-line surface for the headless snapshot. Kept as its own struct
/// (rather than a free function parsing `std::env::args`) so argument
/// handling has an obvious owner and can grow additional flags/tests without
/// touching `main`.
pub struct SnapshotCli {
    /// Process rows are a two-sample delta (CPU% needs an elapsed-time
    /// baseline), so a real report waits this long between the priming sweep
    /// and the reported sweep. Exposed for tests so they don't pay a real
    /// wall-clock wait.
    settle: Duration,
}

impl SnapshotCli {
    const FLAG: &'static str = "--snapshot";

    #[must_use]
    pub const fn new() -> Self {
        // One second matches the worker thread's normal SAMPLE_INTERVAL, so
        // the one-shot report observes the same effective window a live
        // dashboard's first steady-state sample would.
        Self {
            settle: Duration::from_secs(1),
        }
    }

    /// True when `args` requests the headless snapshot rather than the GUI.
    /// Checked against the raw argv so `main` can decide before doing any
    /// window/thread setup.
    #[must_use]
    pub fn requested(args: &[String]) -> bool {
        args.iter().any(|arg| arg == Self::FLAG)
    }

    /// Collect one real sample and print the report to stdout. Blocking is
    /// expected and intentional: this is a one-shot CLI invocation, not a
    /// tick inside an event loop.
    pub fn run(&self) {
        let mut reader = MonitorReader::new();
        // Two full sweeps bracketing a real sleep give per-process CPU% a
        // genuine elapsed-time baseline, exactly like the GUI worker's
        // startup priming in `App::spawn_worker`.
        reader.prime_processes();
        // The CPU-usage counters need the same bracketing: the very first
        // read has no prior /proc/stat snapshot to diff against.
        reader.sample();
        std::thread::sleep(self.settle);
        let processes = reader.prime_processes();
        let sample = reader.sample();
        let report = DashboardReport::from_sample(&sample, &processes);
        print!("{}", Self::render(&report));
    }

    /// Render a full [`DashboardReport`] as a plain-text document. Split out
    /// from `run` so formatting is unit-testable without touching `/proc`.
    #[must_use]
    fn render(report: &DashboardReport) -> String {
        let mut out = String::new();
        for card in [
            &report.cpu,
            &report.memory,
            &report.temperature,
            &report.network,
        ] {
            Self::render_card(&mut out, card);
        }
        Self::render_processes(&mut out, report);
        out
    }

    fn render_card(out: &mut String, card: &CardReport) {
        let _ = writeln!(out, "== {} ==", card.title);
        if !card.meta.is_empty() {
            let _ = writeln!(out, "{}", card.meta);
        }
        let _ = writeln!(out, "{}{}", card.hero, card.unit);
        for (label, value) in &card.details {
            let _ = writeln!(out, "  {label}: {value}");
        }
        out.push('\n');
    }

    fn render_processes(out: &mut String, report: &DashboardReport) {
        let _ = writeln!(out, "== Processes ({}) ==", report.processes.len());
        let _ = writeln!(
            out,
            "{:>8}  {:<24}  {:>7}  {:>10}",
            "PID", "NAME", "CPU%", "MEM(MB)"
        );
        for process in &report.processes {
            let _ = writeln!(
                out,
                "{:>8}  {:<24}  {:>7.1}  {:>10.1}",
                process.pid, process.name, process.cpu, process.memory_mb
            );
        }
    }
}

impl Default for SnapshotCli {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::SnapshotCli;
    use monitor::dashboard_report::DashboardReport;
    use sysmon::{MonitorSample, ProcessInfo};

    #[test]
    fn requested_detects_the_snapshot_flag_anywhere_in_argv() {
        assert!(SnapshotCli::requested(&[
            "monitor".to_owned(),
            "--snapshot".to_owned()
        ]));
        assert!(!SnapshotCli::requested(&["monitor".to_owned()]));
    }

    #[test]
    fn render_includes_every_card_title_and_the_process_table() {
        let sample = MonitorSample {
            cpu: 42.0,
            cpu_brand: "Test CPU".to_owned(),
            cpu_cores: 8,
            ..Default::default()
        };
        let processes = vec![ProcessInfo {
            pid: 123,
            name: "demo".to_owned(),
            cpu: 5.5,
            memory_kb: 4096,
        }];
        let report = DashboardReport::from_sample(&sample, &processes);
        let text = SnapshotCli::render(&report);
        assert!(text.contains("== CPU =="));
        assert!(text.contains("== Memory =="));
        assert!(text.contains("== Temperature =="));
        assert!(text.contains("== Network =="));
        assert!(text.contains("42%"));
        assert!(text.contains("Test CPU"));
        assert!(text.contains("== Processes (1) =="));
        assert!(text.contains("demo"));
        assert!(text.contains("123"));
    }
}

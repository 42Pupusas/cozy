//! Text projection of a telemetry sample, shared by the pixel painter and the
//! headless `--snapshot` report.
//!
//! `Painter::paint` used to build every card's label/value strings inline
//! with its own `format!` calls, and a headless reporter that wanted "the
//! same numbers" would have had to duplicate each one by hand -- a
//! duplication that silently drifts the moment either copy is edited. This
//! module is the single source of truth instead: both the GUI (which lays
//! these strings out as glyphs) and the headless snapshot (which prints them
//! as plain text) consume the same [`DashboardReport`], so "the exact same
//! data the GUI shows" is true by construction, not by convention.

use sysmon::{MonitorSample, ProcessInfo};

/// One dashboard card's text content, in the exact order it is painted: a
/// title, right-aligned metadata, a hero number with its unit, and a row of
/// label/value detail pairs.
pub struct CardReport {
    pub title: &'static str,
    pub meta: String,
    pub hero: String,
    pub unit: &'static str,
    pub details: Vec<(&'static str, String)>,
}

/// One row of the process table, already unit-converted the way the table
/// displays it (memory in MB, CPU as a one-core percentage).
pub struct ProcessRow {
    pub pid: u32,
    pub name: String,
    pub cpu: f32,
    pub memory_mb: f32,
}

/// The complete text content of the dashboard for one telemetry sample: the
/// four cards plus the process table, in painting order.
pub struct DashboardReport {
    pub cpu: CardReport,
    pub memory: CardReport,
    pub temperature: CardReport,
    pub network: CardReport,
    pub processes: Vec<ProcessRow>,
}

impl DashboardReport {
    #[must_use]
    pub fn from_sample(sample: &MonitorSample, processes: &[ProcessInfo]) -> Self {
        Self {
            cpu: Self::cpu_card(sample),
            memory: Self::memory_card(sample),
            temperature: Self::temperature_card(sample),
            network: Self::network_card(sample),
            processes: processes
                .iter()
                .map(|process| ProcessRow {
                    pid: process.pid,
                    name: process.name.clone(),
                    cpu: process.cpu,
                    #[allow(clippy::cast_precision_loss)]
                    memory_mb: process.memory_kb as f32 / 1024.0,
                })
                .collect(),
        }
    }

    /// Matches the CPU card's title-row metadata: a possibly-truncated brand
    /// string is a paint-time layout concern (it depends on measured glyph
    /// widths), so the report keeps the untruncated brand and core count --
    /// the full information a text report should show anyway.
    fn cpu_card(sample: &MonitorSample) -> CardReport {
        CardReport {
            title: "CPU",
            meta: format!("{} · {} cores", sample.cpu_brand, sample.cpu_cores),
            hero: format!("{:.0}", sample.cpu),
            unit: "%",
            details: vec![
                ("Load", format!("{:.2}", sample.load[0])),
                ("Disk I/O", format!("{:.1} MB/s", sample.disk_io_mb_s)),
                ("Freq", format!("{:.2} GHz", sample.cpu_frequency_ghz)),
            ],
        }
    }

    fn memory_card(sample: &MonitorSample) -> CardReport {
        let swap = if sample.swap_total_mb <= 0.0 {
            "ZRAM —".to_owned()
        } else {
            format!(
                "{:.1}/{:.1} GB",
                sample.swap_used_mb / 1024.0,
                sample.swap_total_mb / 1024.0
            )
        };
        CardReport {
            title: "Memory",
            meta: sample.memory_device.clone(),
            hero: format!("{:.0}", sample.memory),
            unit: "%",
            details: vec![
                ("Used", format!("{:.0}%", sample.memory)),
                (
                    "Available",
                    format!("{:.1} GB", sample.memory_available_mb / 1024.0),
                ),
                ("Swap", swap),
            ],
        }
    }

    fn temperature_card(sample: &MonitorSample) -> CardReport {
        CardReport {
            title: "Temperature",
            meta: sample.thermal_device.clone(),
            hero: format!("{:.0}", sample.temperature),
            unit: "°C",
            details: vec![
                ("Max", format!("{:.1} °C", sample.thermal_max)),
                ("Average", format!("{:.1} °C", sample.thermal_avg)),
                ("Min", format!("{:.1} °C", sample.thermal_min)),
            ],
        }
    }

    fn network_card(sample: &MonitorSample) -> CardReport {
        let link = if sample.network_link_mbps > 0.0 {
            format!("{:.0} Mb/s", sample.network_link_mbps)
        } else {
            "Up".to_owned()
        };
        CardReport {
            title: "Network",
            meta: sample.network_device.clone(),
            hero: format!("{:.1}", sample.network_ping_ms),
            unit: "ms",
            details: vec![
                ("RX", format!("{:.2} MB/s", sample.network_rx)),
                ("TX", format!("{:.2} MB/s", sample.network_tx)),
                ("LINK", link),
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DashboardReport;
    use sysmon::MonitorSample;

    #[test]
    fn memory_card_reports_zram_when_there_is_no_swap_partition() {
        let sample = MonitorSample {
            swap_total_mb: 0.0,
            ..Default::default()
        };
        let report = DashboardReport::from_sample(&sample, &[]);
        assert_eq!(report.memory.details[2].1, "ZRAM —");
    }

    #[test]
    fn memory_card_reports_swap_used_over_total_when_present() {
        let sample = MonitorSample {
            swap_used_mb: 512.0,
            swap_total_mb: 2048.0,
            ..Default::default()
        };
        let report = DashboardReport::from_sample(&sample, &[]);
        assert_eq!(report.memory.details[2].1, "0.5/2.0 GB");
    }

    #[test]
    fn network_card_falls_back_to_up_without_a_reported_link_speed() {
        let sample = MonitorSample {
            network_link_mbps: 0.0,
            ..Default::default()
        };
        let report = DashboardReport::from_sample(&sample, &[]);
        assert_eq!(report.network.details[2].1, "Up");
    }

    #[test]
    fn process_rows_convert_kilobytes_to_megabytes() {
        let processes = vec![sysmon::ProcessInfo {
            pid: 7,
            name: "demo".to_owned(),
            cpu: 12.5,
            memory_kb: 2048,
        }];
        let report = DashboardReport::from_sample(&MonitorSample::default(), &processes);
        assert_eq!(report.processes.len(), 1);
        assert!((report.processes[0].memory_mb - 2.0).abs() < f32::EPSILON);
    }
}

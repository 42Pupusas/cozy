//! Non-rendering monitor workflow stress test.
//!
//! Samples real procfs/sysfs telemetry and drives the same interpolation and
//! display-quantization path as the dashboard, without opening a window.

use headless_stress::StressOptions;
use monitor::{display_snapshot::DisplaySnapshot, sample_interpolator::SampleInterpolator};
use sysmon::MonitorReader;

/// Exercises monitor sampling and display-state production.
pub struct MonitorStress;

impl MonitorStress {
    /// Run a bounded telemetry workflow.
    pub fn run(options: StressOptions) {
        let mut reader = MonitorReader::new();
        let mut interpolator = SampleInterpolator::new();
        let mut snapshot = sysmon::MonitorSample::default();
        let completed = options.run(|_| {
            let sample = reader.sample();
            interpolator.accept(sample);
            interpolator.blend_into(&mut snapshot);
            let _ = DisplaySnapshot::from_sample(&snapshot);
            let _ = reader.processes_tick();
        });
        println!("monitor headless stress: {completed} rounds");
    }
}

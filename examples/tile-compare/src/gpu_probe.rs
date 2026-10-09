//! [`GpuProbe`] -- amdgpu busy percentage and package power from sysfs,
//! accumulated over a run.

use std::fmt::Write;
use std::path::PathBuf;

pub struct GpuProbe {
    busy: PathBuf,
    power: Option<PathBuf>,
    samples: u64,
    busy_sum: u64,
    power_sum_uw: u64,
}

impl GpuProbe {
    #[must_use]
    pub fn find() -> Option<Self> {
        let device = std::fs::read_dir("/sys/class/drm")
            .ok()?
            .filter_map(Result::ok)
            .map(|entry| entry.path().join("device"))
            .find(|device| device.join("gpu_busy_percent").exists())?;
        let power = std::fs::read_dir(device.join("hwmon"))
            .ok()
            .and_then(|mut dirs| dirs.find_map(Result::ok))
            .map(|hwmon| hwmon.path().join("power1_input"))
            .filter(|path| path.exists());
        Some(Self {
            busy: device.join("gpu_busy_percent"),
            power,
            samples: 0,
            busy_sum: 0,
            power_sum_uw: 0,
        })
    }

    pub fn sample(&mut self) {
        self.samples += 1;
        self.busy_sum += Self::read_number(&self.busy);
        if let Some(power) = &self.power {
            self.power_sum_uw += Self::read_number(power);
        }
    }

    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn summary(&self) -> String {
        let samples = self.samples.max(1) as f64;
        let mut line = format!("gpu busy        {:.1}%", self.busy_sum as f64 / samples);
        if self.power.is_some() {
            let _ = write!(
                line,
                "\npackage power   {:.2} W",
                self.power_sum_uw as f64 / samples / 1e6
            );
        }
        line
    }

    fn read_number(path: &PathBuf) -> u64 {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    }
}

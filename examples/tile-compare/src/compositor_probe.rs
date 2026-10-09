//! [`CompositorProbe`] -- finds the compositor process by name and samples
//! its CPU time, resident memory and GPU usage.

use crate::drm_usage::DrmUsage;
use crate::process_sample::ProcessSample;

#[derive(Clone, Copy, Debug, Default)]
pub struct CompositorSample {
    pub process: ProcessSample,
    pub drm: DrmUsage,
}

pub struct CompositorProbe {
    pid: u32,
    name: String,
}

impl CompositorProbe {
    const NAMES: [&'static str; 1] = ["niri"];

    #[must_use]
    pub fn find() -> Option<Self> {
        std::fs::read_dir("/proc")
            .ok()?
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
            .find_map(|pid| {
                let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
                let name = comm.trim();
                Self::NAMES.contains(&name).then(|| Self {
                    pid,
                    name: name.to_owned(),
                })
            })
    }

    #[must_use]
    pub fn sample(&self) -> CompositorSample {
        CompositorSample {
            process: ProcessSample::of(self.pid),
            drm: DrmUsage::of(self.pid),
        }
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

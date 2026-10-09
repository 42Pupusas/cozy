//! [`DrmUsage`] -- a process's GPU engine time and GPU memory, summed over
//! its DRM clients from the `drm-*` keys amdgpu writes into
//! `/proc/<pid>/fdinfo`.

use std::collections::HashSet;
use std::time::Duration;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrmUsage {
    pub gfx: Duration,
    pub vram_kb: u64,
    pub gtt_kb: u64,
}

impl DrmUsage {
    #[must_use]
    pub fn of(pid: u32) -> Self {
        let Ok(entries) = std::fs::read_dir(format!("/proc/{pid}/fdinfo")) else {
            return Self::default();
        };
        let mut seen = HashSet::new();
        let mut usage = Self::default();
        for entry in entries.filter_map(Result::ok) {
            let Ok(info) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            if let Some(client) = Self::field(&info, "drm-client-id")
                && seen.insert(client)
            {
                usage.add(&info);
            }
        }
        usage
    }

    fn add(&mut self, info: &str) {
        self.gfx += Duration::from_nanos(Self::field(info, "drm-engine-gfx").unwrap_or(0));
        self.vram_kb += Self::field(info, "drm-memory-vram").unwrap_or(0);
        self.gtt_kb += Self::field(info, "drm-memory-gtt").unwrap_or(0);
    }

    fn field(info: &str, key: &str) -> Option<u64> {
        info.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            (name.trim() == key).then(|| value.split_whitespace().next()?.parse().ok())?
        })
    }
}

#[cfg(test)]
mod tests {
    use super::DrmUsage;
    use std::time::Duration;

    const FDINFO: &str = "pos:\t0\nflags:\t02100002\ndrm-driver:\tamdgpu\n\
drm-client-id:\t42\ndrm-memory-vram:\t1024 KiB\ndrm-memory-gtt:\t2048 KiB\n\
drm-engine-gfx:\t1500000 ns\ndrm-engine-compute:\t0 ns\n";

    #[test]
    fn reads_engine_time_and_memory() {
        let mut usage = DrmUsage::default();
        usage.add(FDINFO);
        assert_eq!(usage.gfx, Duration::from_micros(1500));
        assert_eq!(usage.vram_kb, 1024);
        assert_eq!(usage.gtt_kb, 2048);
    }

    #[test]
    fn a_missing_key_reads_as_none() {
        assert_eq!(DrmUsage::field(FDINFO, "drm-engine-dma"), None);
        assert_eq!(DrmUsage::field(FDINFO, "drm-client-id"), Some(42));
    }
}

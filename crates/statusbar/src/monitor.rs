//! Structured system-monitor readings shared by graphical monitor clients.
//! The readers stay here beside the status-bar sensors so `/proc` parsing and
//! sysfs policy are not duplicated by a second binary.

use std::fs;
use std::path::Path;

#[derive(Clone, Debug, Default)]
pub struct MonitorSample {
    pub cpu: f32,
    pub cores: Vec<f32>,
    pub load: [f32; 3],
    pub memory: f32,
    pub temperature: f32,
    pub disk: f32,
    pub network_rx: f32,
    pub network_tx: f32,
}

#[derive(Clone, Debug, Default)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub cpu: f32,
    pub memory_kb: u64,
}

#[derive(Default)]
struct CpuState { previous: Vec<(u64, u64)> }

impl CpuState {
    #[allow(clippy::cast_precision_loss)]
    fn usage(&mut self) -> (f32, Vec<f32>) {
        let current = Self::read_times();
        let mut values = Vec::with_capacity(current.len());
        for (index, &(total, idle)) in current.iter().enumerate() {
            values.push(self.previous.get(index).map_or(0.0, |&(old_total, old_idle)| {
                #[allow(clippy::cast_precision_loss)]
                let total_delta = total.saturating_sub(old_total);
                if total_delta == 0 { 0.0 } else { 100.0 * (1.0 - idle.saturating_sub(old_idle) as f32 / total_delta as f32) }
            }));
        }
        self.previous = current;
        (values.first().copied().unwrap_or(0.0), values.into_iter().skip(1).collect())
    }

    fn read_times() -> Vec<(u64, u64)> {
        fs::read_to_string("/proc/stat").map_or_else(|_| Vec::new(), |text| text.lines().filter(|line| line.starts_with("cpu")).filter_map(|line| {
            let mut fields = line.split_whitespace();
            fields.next()?;
            let values: Vec<u64> = fields.filter_map(|v| v.parse().ok()).collect();
            if values.len() < 4 { return None; }
            Some((values.iter().sum(), values[3] + values.get(4).copied().unwrap_or(0)))
        }).collect())
    }
}

#[derive(Default)]
struct NetworkState { previous: Option<(u64, u64, std::time::Instant)> }

impl NetworkState {
    fn throughput(&mut self) -> (f32, f32) {
        let (rx, tx) = fs::read_to_string("/proc/net/dev").map_or((0, 0), |text| text.lines().skip(2).filter_map(|line| line.split_once(':')).filter(|(name, _)| *name != "lo").map(|(_, data)| {
            let fields: Vec<u64> = data.split_whitespace().filter_map(|v| v.parse().ok()).collect();
            (fields.first().copied().unwrap_or(0), fields.get(8).copied().unwrap_or(0))
        }).fold((0, 0), |(rx, tx), (a, b)| (rx + a, tx + b)));
        let now = std::time::Instant::now();
        let result = self.previous.map_or((0.0, 0.0), |(old_rx, old_tx, old_time)| {
            let seconds = now.duration_since(old_time).as_secs_f32().max(0.001);
            #[allow(clippy::cast_precision_loss)]
            let received = rx.saturating_sub(old_rx) as f32 / seconds / 1_000_000.0;
            #[allow(clippy::cast_precision_loss)]
            let transmitted = tx.saturating_sub(old_tx) as f32 / seconds / 1_000_000.0;
            (received, transmitted)
        });
        self.previous = Some((rx, tx, now));
        result
    }
}

pub struct MonitorReader { cpu: CpuState, network: NetworkState }

impl MonitorReader {
    #[must_use]
    pub fn new() -> Self { Self { cpu: CpuState::default(), network: NetworkState::default() } }

    pub fn sample(&mut self) -> MonitorSample {
        let (cpu, cores) = self.cpu.usage();
        let load = fs::read_to_string("/proc/loadavg").map_or([0.0; 3], |s| { let mut v = s.split_whitespace().filter_map(|x| x.parse().ok()); [v.next().unwrap_or(0.0), v.next().unwrap_or(0.0), v.next().unwrap_or(0.0)] });
        let (memory, _) = MemoryValues::read();
        let temperature = TemperatureValue::read();
        let disk = DiskValue::read();
        let (network_rx, network_tx) = self.network.throughput();
        MonitorSample { cpu, cores, load, memory, temperature, disk, network_rx, network_tx }
    }

    #[must_use]
    pub fn processes(&self) -> Vec<ProcessInfo> {
        let mut processes = fs::read_dir("/proc").map_or_else(|_| Vec::new(), |entries| entries.flatten().filter_map(|entry| {
            let pid = entry.file_name().to_string_lossy().parse().ok()?;
            let stat = fs::read_to_string(entry.path().join("stat")).ok()?;
            let name = stat.split(')').next()?.split_once('(')?.1.to_owned();
            Some(ProcessInfo { pid, name, cpu: 0.0, memory_kb: Self::rss_kb(&entry.path()) })
        }).collect());
        processes.sort_by_key(|process| std::cmp::Reverse(process.memory_kb));
        processes.truncate(24);
        processes
    }

    fn rss_kb(path: &Path) -> u64 { fs::read_to_string(path.join("status")).ok().and_then(|s| s.lines().find_map(|l| l.strip_prefix("VmRSS:")?.split_whitespace().nth(0)?.parse().ok())).unwrap_or(0) }
}

struct MemoryValues;
impl MemoryValues { fn read() -> (f32, u64) { let text = fs::read_to_string("/proc/meminfo").unwrap_or_default(); let find = |key| text.lines().find_map(|l| l.strip_prefix(key)?.split_whitespace().next()?.parse::<u64>().ok()).unwrap_or(0); let total = find("MemTotal:"); let available = find("MemAvailable:"); #[allow(clippy::cast_precision_loss)]
        let used = if total == 0 { 0.0 } else { 100.0 * (total - available) as f32 / total as f32 };
        (used, total) } }
struct TemperatureValue;
impl TemperatureValue { fn read() -> f32 { fs::read_dir("/sys/class/thermal").map_or(0.0, |entries| entries.flatten().find_map(|e| fs::read_to_string(e.path().join("temp")).ok()?.trim().parse::<f32>().ok()).map_or(0.0, |v| v / 1000.0)) } }
struct DiskValue;
impl DiskValue {
    fn read() -> f32 {
        let output = std::process::Command::new("df").args(["-P", "/"]).output().ok();
        output.and_then(|result| String::from_utf8(result.stdout).ok()).and_then(|text| text.lines().nth(1).map(str::to_owned)).and_then(|line| line.split_whitespace().nth(4).map(str::to_owned)).and_then(|value| value.strip_suffix('%').map(str::to_owned)).and_then(|value| value.parse().ok()).unwrap_or(0.0)
    }
}

impl Default for MonitorReader { fn default() -> Self { Self::new() } }

// Keep the unused marker's ownership explicit until disk accounting is added.
#[allow(dead_code)] struct _DiskPath;

#[cfg(test)]
mod tests { use super::*; #[test] fn reader_has_stable_shape() { let mut reader = MonitorReader::new(); let sample = reader.sample(); assert!(sample.cpu >= 0.0); } }

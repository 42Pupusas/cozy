//! Structured system-monitor readings shared by graphical monitor clients.
//! The readers stay here beside the status-bar sensors so `/proc` parsing and
//! sysfs policy are not duplicated by a second binary.

use std::collections::{HashMap, VecDeque};
use std::fmt::Write as FmtWrite;
use std::fs;
use std::net::Ipv4Addr;
use std::os::raw::{c_char, c_int, c_long, c_ulong};
use std::process::Command;
use std::time::Instant;

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
    pub network_device: String,
    pub network_ping_ms: f32,
    pub network_link_mbps: f32,
    pub cpu_brand: String,
    pub cpu_cores: usize,
    pub cpu_frequency_ghz: f32,
    pub disk_io_mb_s: f32,
    pub memory_available_mb: f32,
    pub swap_used_mb: f32,
    pub swap_total_mb: f32,
}

#[derive(Clone, Debug, Default)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub cpu: f32,
    pub memory_kb: u64,
}

/// `/proc/stat` counters advance on scheduler ticks, which are coarser than a
/// 100 ms collection tick on many kernels. Keep a ~200 ms counter window so
/// each collected per-core percentage uses enough elapsed ticks to avoid
/// 0/100% quantization, while the UI interpolates it at 60 Hz.
#[derive(Default)]
struct CpuState {
    window: VecDeque<Vec<(u64, u64)>>,
    scratch: Vec<(u64, u64)>,
    smoothed: Vec<f32>,
    last_sample: Option<Instant>,
    brand: String,
    core_count: usize,
}

impl CpuState {
    const WINDOW_SAMPLES: usize = 3;

    #[allow(clippy::cast_precision_loss)]
    fn usage(&mut self) -> (f32, Vec<f32>) {
        self.ensure_info();
        Self::read_times_into(&mut self.scratch);
        let now = Instant::now();
        let Some(previous) = self.window.front() else {
            self.window.push_back(std::mem::take(&mut self.scratch));
            return (0.0, Vec::new());
        };
        let mut values = Vec::with_capacity(self.scratch.len());
        for (index, &(total, idle)) in self.scratch.iter().enumerate() {
            values.push(previous.get(index).map_or(0.0, |&(old_total, old_idle)| {
                #[allow(clippy::cast_precision_loss)]
                let total_delta = total.saturating_sub(old_total);
                if total_delta == 0 {
                    0.0
                } else {
                    100.0 * (1.0 - idle.saturating_sub(old_idle) as f32 / total_delta as f32)
                }
            }));
        }
        self.window.push_back(std::mem::take(&mut self.scratch));
        if self.window.len() > Self::WINDOW_SAMPLES {
            self.scratch = self.window.pop_front().unwrap_or_default();
        }
        self.smooth(values, now);
        (
            self.smoothed.first().copied().unwrap_or(0.0),
            self.smoothed.iter().copied().skip(1).collect(),
        )
    }

    /// Counter-derived CPU percentages are necessarily quantized by kernel
    /// scheduler ticks. A time-correct exponential response turns that stepped
    /// measurement into a continuous display signal without tying its feel to
    /// a particular frame rate. A 180 ms time constant is quick enough for a
    /// monitor while suppressing single-tick per-core jitter.
    fn smooth(&mut self, values: Vec<f32>, now: Instant) {
        let Some(last) = self.last_sample else {
            self.smoothed = values;
            self.last_sample = Some(now);
            return;
        };
        let seconds = now.duration_since(last).as_secs_f32();
        let alpha = 1.0 - (-seconds / 0.180).exp();
        self.smoothed.resize(values.len(), 0.0);
        for (smoothed, measured) in self.smoothed.iter_mut().zip(values) {
            *smoothed = (measured - *smoothed).mul_add(alpha, *smoothed);
        }
        self.last_sample = Some(now);
    }

    fn ensure_info(&mut self) {
        if !self.brand.is_empty() && self.core_count != 0 {
            return;
        }
        if let Ok(text) = fs::read_to_string("/proc/cpuinfo") {
            let mut brand = None;
            let mut core_count = 0;
            for line in text.lines() {
                if line.starts_with("processor") {
                    core_count += 1;
                } else if brand.is_none() {
                    brand = line
                        .split_once(':')
                        .filter(|(key, _)| key.trim() == "model name")
                        .map(|(_, value)| value.trim().to_owned());
                }
            }
            self.brand = brand.unwrap_or_else(|| "unknown cpu".to_owned());
            self.core_count = core_count;
        }
    }

    fn brand(&self) -> &str {
        &self.brand
    }

    const fn core_count(&self) -> usize {
        self.core_count
    }

    fn read_times_into(output: &mut Vec<(u64, u64)>) {
        output.clear();
        if let Ok(text) = fs::read_to_string("/proc/stat") {
            Self::parse_times_into(&text, output);
        }
    }

    /// Parse the CPU records in `/proc/stat` without creating line or field
    /// iterators. `/proc` is an ASCII kernel interface, so a byte scanner is
    /// both more precise and cheaper than Unicode whitespace parsing here.
    fn parse_times_into(text: &str, result: &mut Vec<(u64, u64)>) {
        let bytes = text.as_bytes();
        let mut line_start = 0;
        while line_start < bytes.len() {
            let line_end = bytes[line_start..]
                .iter()
                .position(|&byte| byte == b'\n')
                .map_or(bytes.len(), |offset| line_start + offset);
            let line = &bytes[line_start..line_end];
            if !line.starts_with(b"cpu") {
                break;
            }
            if let Some(times) = Self::parse_cpu_line(line) {
                result.push(times);
            } else {
                break;
            }
            line_start = line_end.saturating_add(1);
        }
    }

    fn parse_cpu_line(line: &[u8]) -> Option<(u64, u64)> {
        let mut index = 3;
        while index < line.len() && line[index] > b' ' {
            index += 1;
        }
        while index < line.len() && line[index] <= b' ' {
            index += 1;
        }

        let mut field = 0;
        let mut total = 0u64;
        let mut idle = None;
        let mut iowait = 0u64;
        while index < line.len() {
            let mut value = 0u64;
            let start = index;
            while index < line.len() {
                let byte = line[index];
                if !(b'0'..=b'9').contains(&byte) {
                    break;
                }
                value = value
                    .saturating_mul(10)
                    .saturating_add(u64::from(byte - b'0'));
                index += 1;
            }
            if start == index {
                return None;
            }

            total = total.saturating_add(value);
            if field == 3 {
                idle = Some(value);
            } else if field == 4 {
                iowait = value;
            }
            field += 1;
            while index < line.len() && line[index] <= b' ' {
                index += 1;
            }
        }
        Some((total, idle?.saturating_add(iowait)))
    }

    #[allow(dead_code)]
    pub fn parse_cpu_times_for_bench(text: &str) -> Vec<(u64, u64)> {
        let mut result = Vec::new();
        Self::parse_times_into(text, &mut result);
        result
    }
}

#[derive(Default)]
struct NetworkState {
    previous: Option<(u64, u64, std::time::Instant)>,
    device: Option<String>,
    last_ping: Option<(Instant, f32)>,
}

impl NetworkState {
    fn device_label(&mut self) -> String {
        if let Some(device) = &self.device {
            return device.clone();
        }
        let interfaces = fs::read_dir("/sys/class/net")
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name != "lo")
            .collect::<Vec<_>>();
        // Prefer the interface carrying the default route. A directory-order
        // choice can otherwise select docker0 or another virtual bridge even
        // though it is not the adapter providing network connectivity.
        let default_interface = Self::default_route_interface();
        let selected = default_interface
            .as_deref()
            .and_then(|name| {
                interfaces
                    .iter()
                    .find(|candidate| candidate.as_str() == name)
                    .filter(|candidate| {
                        Self::hardware_label(candidate).is_some()
                            && Self::interface_is_up(candidate)
                    })
            })
            .or_else(|| {
                interfaces.iter().find(|name| {
                    Self::hardware_label(name).is_some() && Self::interface_is_up(name)
                })
            })
            .or_else(|| {
                interfaces
                    .iter()
                    .find(|name| !Self::virtual_interface(name))
            })
            .or_else(|| interfaces.first());
        let label = selected
            .map(|name| Self::hardware_label(name).unwrap_or_else(|| name.clone()))
            .unwrap_or_else(|| "NO NETWORK".to_owned());
        self.device = Some(label.clone());
        label
    }

    fn ping_ms(&mut self) -> f32 {
        let now = Instant::now();
        if let Some((at, value)) = self.last_ping {
            if now.duration_since(at) < std::time::Duration::from_secs(1) {
                return value;
            }
        }
        let value = Self::default_gateway()
            .and_then(|gateway| {
                let output = Command::new("ping")
                    .args(["-n", "-c", "1", "-W", "1", &gateway.to_string()])
                    .output()
                    .ok()?;
                if !output.status.success() {
                    return None;
                }
                let text = String::from_utf8_lossy(&output.stdout);
                text.split_whitespace()
                    .find_map(|field| field.strip_prefix("time="))
                    .and_then(|value| value.parse::<f32>().ok())
            })
            .unwrap_or(0.0);
        self.last_ping = Some((now, value));
        value
    }

    fn link_mbps(&mut self) -> f32 {
        let interface = Self::default_route_interface().or_else(|| {
            self.device.as_ref().and_then(|label| {
                fs::read_dir("/sys/class/net").ok()?.flatten().find_map(|entry| {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    (Self::hardware_label(&name).as_deref() == Some(label.as_str())).then_some(name)
                })
            })
        });
        interface
            .and_then(|name| fs::read_to_string(format!("/sys/class/net/{name}/speed")).ok())
            .and_then(|speed| speed.trim().parse::<f32>().ok())
            .filter(|speed| *speed > 0.0)
            .unwrap_or(0.0)
    }

    fn default_gateway() -> Option<Ipv4Addr> {
        let routes = fs::read_to_string("/proc/net/route").ok()?;
        routes.lines().skip(1).find_map(|line| {
            let mut fields = line.split_whitespace();
            let _interface = fields.next()?;
            let destination = fields.next()?;
            let gateway = fields.next()?;
            let flags = fields.next()?;
            if destination != "00000000" || gateway == "00000000" || flags == "0000" {
                return None;
            }
            let value = u32::from_str_radix(gateway, 16).ok()?;
            Some(Ipv4Addr::from(value.to_le_bytes()))
        })
    }

    fn default_route_interface() -> Option<String> {
        let routes = fs::read_to_string("/proc/net/route").ok()?;
        routes.lines().skip(1).find_map(|line| {
            let mut fields = line.split_whitespace();
            let interface = fields.next()?;
            let destination = fields.next()?;
            let gateway = fields.next()?;
            let flags = fields.next()?;
            (destination == "00000000" && gateway != "00000000" && flags != "0000")
                .then(|| interface.to_owned())
        })
    }

    fn interface_is_up(interface: &str) -> bool {
        fs::read_to_string(format!("/sys/class/net/{interface}/operstate"))
            .map(|state| matches!(state.trim(), "up" | "unknown"))
            .unwrap_or(false)
    }

    fn virtual_interface(interface: &str) -> bool {
        interface.starts_with("docker")
            || interface.starts_with("veth")
            || interface.starts_with("br-")
            || interface.starts_with("virbr")
            || interface.starts_with("tun")
            || interface.starts_with("tap")
            || interface.starts_with("wg")
    }

    fn hardware_label(interface: &str) -> Option<String> {
        let root = format!("/sys/class/net/{interface}/device");
        if !std::path::Path::new(&root).exists() {
            return None;
        }
        let uevent = fs::read_to_string(format!("{root}/uevent")).ok()?;
        let mut driver = None;
        let mut pci_id = None;
        for line in uevent.lines() {
            if let Some(value) = line.strip_prefix("DRIVER=") {
                driver = Some(value.to_owned());
            } else if let Some(value) = line.strip_prefix("PCI_ID=") {
                pci_id = Some(value.to_owned());
            }
        }
        match (driver, pci_id) {
            (Some(driver), Some(pci_id)) => Some(format!("{driver} {pci_id}")),
            (Some(driver), None) => Some(driver),
            (None, Some(pci_id)) => Some(pci_id),
            (None, None) => Some(interface.to_owned()),
        }
    }
}

impl NetworkState {
    fn throughput(&mut self) -> (f32, f32) {
        let (rx, tx) = fs::read_to_string("/proc/net/dev").map_or((0, 0), |text| {
            text.lines()
                .skip(2)
                .filter_map(|line| line.split_once(':'))
                .filter(|(name, _)| *name != "lo")
                .map(|(_, data)| {
                    let mut received = 0;
                    let mut transmitted = 0;
                    for (index, field) in data.split_whitespace().enumerate() {
                        let Ok(value) = field.parse::<u64>() else {
                            continue;
                        };
                        match index {
                            0 => received = value,
                            8 => transmitted = value,
                            _ => {}
                        }
                    }
                    (received, transmitted)
                })
                .fold((0, 0), |(rx, tx), (a, b)| (rx + a, tx + b))
        });
        let now = std::time::Instant::now();
        let result = self
            .previous
            .map_or((0.0, 0.0), |(old_rx, old_tx, old_time)| {
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

/// Retains process CPU counters between the deliberately infrequent process
/// scans. Unlike process memory, CPU is a rate, so it only has meaning as a
/// delta over real elapsed time.
/// Incrementally scans `/proc` over the monitor's ten 100 ms worker ticks.
/// A full scan burst previously cost 15–20 ms once per second; spreading its
/// identical file reads prevents that burst from becoming visible scheduler
/// noise on one core.
#[derive(Default)]
struct ProcessScanner {
    pending: Vec<u32>,
    cursor: usize,
    collected: Vec<ProcessInfo>,
    stat_path: ProcessStatPath,
}

impl ProcessScanner {
    const SLICES: usize = 10;

    fn begin(&mut self) {
        self.pending.clear();
        if let Ok(entries) = fs::read_dir("/proc") {
            for entry in entries.flatten() {
                if let Ok(pid) = entry.file_name().to_string_lossy().parse() {
                    self.pending.push(pid);
                }
            }
        }
        self.cursor = 0;
        self.collected.clear();
    }

    /// Parse one bounded batch. Returning `Some` marks the end of a complete
    /// sweep; callers retain their old published list between completions.
    fn advance(&mut self, cpu: &mut ProcessCpuState) -> Option<Vec<ProcessInfo>> {
        if self.cursor >= self.pending.len() {
            self.begin();
            cpu.begin_scan();
        }
        if self.pending.is_empty() {
            return Some(Vec::new());
        }
        let batch = self.pending.len().div_ceil(Self::SLICES).max(1);
        let end = (self.cursor + batch).min(self.pending.len());
        let now = Instant::now();
        for &pid in &self.pending[self.cursor..end] {
            self.stat_path.set_pid(pid);
            let Some(stat) = fs::read_to_string(self.stat_path.as_str()).ok() else {
                continue;
            };
            let Some((name, ticks, resident_pages)) = MonitorReader::process_stat(&stat) else {
                continue;
            };
            self.collected.push(ProcessInfo {
                pid,
                name,
                cpu: cpu.percentage(pid, ticks, now),
                memory_kb: resident_pages.saturating_mul(cpu.page_size_kb),
            });
        }
        self.cursor = end;
        if self.cursor < self.pending.len() {
            return None;
        }
        cpu.retain_live();
        self.collected.sort_by(|left, right| {
            right
                .cpu
                .total_cmp(&left.cpu)
                .then_with(|| right.memory_kb.cmp(&left.memory_kb))
        });
        self.collected.truncate(24);
        Some(std::mem::take(&mut self.collected))
    }
}

#[derive(Default)]
struct ProcessStatPath {
    value: String,
}

impl ProcessStatPath {
    fn set_pid(&mut self, pid: u32) {
        self.value.clear();
        self.value.push_str("/proc/");
        let _ = write!(self.value, "{pid}");
        self.value.push_str("/stat");
    }

    fn as_str(&self) -> &str {
        &self.value
    }
}

struct ProcessCpuState {
    previous: HashMap<u32, (u64, Instant, u64)>,
    generation: u64,
    ticks_per_second: f32,
    page_size_kb: u64,
}

impl ProcessCpuState {
    /// `_SC_CLK_TCK` is Linux's `sysconf` selector for scheduler clock ticks.
    /// Asking libc keeps the `/proc/<pid>/stat` conversion correct on kernels
    /// whose tick rate is not the common 100 Hz default.
    const CLOCK_TICKS_SELECTOR: c_int = 2;
    /// Linux `_SC_PAGESIZE`: byte size of a resident page.
    const PAGE_SIZE_SELECTOR: c_int = 30;

    fn new() -> Self {
        // SAFETY: `sysconf` has no pointer arguments and this selector is a
        // Linux-defined query. A non-positive result is handled by the
        // conservative 100 Hz fallback below.
        let ticks = unsafe { sysconf(Self::CLOCK_TICKS_SELECTOR) };
        // `_SC_PAGESIZE` gives the byte size of one resident page reported in
        // `/proc/<pid>/stat` field 24. Both selectors have no pointer inputs.
        // SAFETY: both selectors are Linux-defined `sysconf` queries with no
        // pointer inputs. Conservative defaults retain usable instrumentation
        // on an unusual libc/runtime failure.
        let page_size = unsafe { sysconf(Self::PAGE_SIZE_SELECTOR) };
        #[allow(clippy::cast_precision_loss)]
        let ticks_per_second = if ticks > 0 { ticks as f32 } else { 100.0 };
        #[allow(clippy::cast_sign_loss)]
        let page_size_kb = if page_size > 0 {
            u64::try_from(page_size).unwrap_or(4096) / 1024
        } else {
            4
        };
        Self {
            previous: HashMap::new(),
            generation: 0,
            ticks_per_second,
            page_size_kb,
        }
    }

    /// Return utilization in units of one logical CPU: 100% means one core
    /// fully busy, while a multi-threaded process may legitimately exceed it.
    fn begin_scan(&mut self) {
        self.generation = self.generation.wrapping_add(1).max(1);
    }

    fn percentage(&mut self, pid: u32, ticks: u64, now: Instant) -> f32 {
        let result = self
            .previous
            .get(&pid)
            .map_or(0.0, |&(old_ticks, then, _)| {
                let elapsed = now.duration_since(then).as_secs_f32();
                if elapsed <= 0.0 {
                    return 0.0;
                }
                #[allow(clippy::cast_precision_loss)]
                let consumed = ticks.saturating_sub(old_ticks) as f32 / self.ticks_per_second;
                (consumed / elapsed * 100.0).max(0.0)
            });
        self.previous.insert(pid, (ticks, now, self.generation));
        result
    }

    /// Forget exited processes so a long-running monitor never retains stale
    /// PID entries (and so reused PIDs always begin with a fresh baseline).
    fn retain_live(&mut self) {
        let generation = self.generation;
        self.previous.retain(|_, (_, _, seen)| *seen == generation);
    }
}

#[derive(Default)]
struct DiskIoState {
    previous_sectors: u64,
    previous_at: Option<Instant>,
}

impl DiskIoState {
    fn throughput_mb_s(&mut self) -> f32 {
        let now = Instant::now();
        let sectors = fs::read_to_string("/proc/diskstats")
            .map(|contents| {
                contents
                    .lines()
                    .filter_map(|line| {
                        let fields: Vec<_> = line.split_whitespace().collect();
                        let device = fields.get(2)?;
                        if device.starts_with("loop") || device.starts_with("ram") {
                            return None;
                        }
                        let read = fields.get(5)?.parse::<u64>().ok()?;
                        let written = fields.get(9)?.parse::<u64>().ok()?;
                        Some(read.saturating_add(written))
                    })
                    .sum()
            })
            .unwrap_or(0);
        let Some(previous_at) = self.previous_at.replace(now) else {
            self.previous_sectors = sectors;
            return 0.0;
        };
        let elapsed = now.duration_since(previous_at).as_secs_f32();
        let delta = sectors.saturating_sub(self.previous_sectors);
        self.previous_sectors = sectors;
        if elapsed <= 0.0 {
            0.0
        } else {
            // Linux diskstats reports sectors; the kernel convention is
            // 512 bytes per sector regardless of the device's physical size.
            delta as f32 * 512.0 / elapsed / 1_048_576.0
        }
    }
}

struct CpuFrequency;

impl CpuFrequency {
    fn read_ghz() -> f32 {
        [
            "/sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq",
            "/sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_cur_freq",
        ]
        .iter()
        .find_map(|path| {
            let khz = fs::read_to_string(path).ok()?.trim().parse::<f32>().ok()?;
            Some(khz / 1_000_000.0)
        })
        .unwrap_or(0.0)
    }
}

pub struct MonitorReader {
    cpu: CpuState,
    network: NetworkState,
    disk_io: DiskIoState,
    processes: ProcessCpuState,
    scanner: ProcessScanner,
}

impl MonitorReader {
    #[must_use]
    pub fn new() -> Self {
        Self {
            cpu: CpuState::default(),
            network: NetworkState::default(),
            processes: ProcessCpuState::new(),
            scanner: ProcessScanner::default(),
            disk_io: DiskIoState::default(),
        }
    }

    pub fn sample(&mut self) -> MonitorSample {
        let (cpu, cores) = self.cpu.usage();
        let load = fs::read_to_string("/proc/loadavg").map_or([0.0; 3], |s| {
            let mut v = s.split_whitespace().filter_map(|x| x.parse().ok());
            [
                v.next().unwrap_or(0.0),
                v.next().unwrap_or(0.0),
                v.next().unwrap_or(0.0),
            ]
        });
        let (memory, memory_available_mb, swap_used_mb, swap_total_mb) = MemoryValues::read();
        let temperature = TemperatureValue::read();
        let disk = DiskValue::read();
        let disk_io_mb_s = self.disk_io.throughput_mb_s();
        let cpu_frequency_ghz = CpuFrequency::read_ghz();
        let (network_rx, network_tx) = self.network.throughput();
        let network_device = self.network.device_label();
        MonitorSample {
            cpu,
            cores,
            load,
            memory,
            temperature,
            disk,
            network_rx,
            network_tx,
            network_device,
            network_ping_ms: self.network.ping_ms(),
            network_link_mbps: self.network.link_mbps(),
            cpu_brand: self.cpu.brand().to_owned(),
            cpu_cores: self.cpu.core_count(),
            cpu_frequency_ghz,
            disk_io_mb_s,
            memory_available_mb,
            swap_used_mb,
            swap_total_mb,
        }
    }

    /// Advance one tenth of the process scan. This performs bounded procfs
    /// work every worker tick and publishes a complete CPU-sorted list after
    /// all PIDs have been measured.
    #[must_use]
    pub fn processes_tick(&mut self) -> Option<Vec<ProcessInfo>> {
        self.scanner.advance(&mut self.processes)
    }

    /// Extract fields from `/proc/<pid>/stat` without being confused by a
    /// process name containing spaces or closing parentheses. After the final
    /// `)`, field 0 is the state; utime/stime are fields 11/12 and RSS pages
    /// is field 21 there (kernel fields 14, 15, and 24 respectively).
    /// Parse one proc-stat record without filesystem access, for diagnostics and
    /// benchmarks that need to isolate parsing from procfs latency.
    #[must_use]
    pub fn parse_process_stat(stat: &str) -> Option<(String, u64, u64)> {
        Self::process_stat(stat)
    }

    /// Parse procfs CPU counters without performing the filesystem read.
    #[must_use]
    pub fn parse_cpu_times_for_bench(text: &str) -> Vec<(u64, u64)> {
        let mut result = Vec::with_capacity(8);
        CpuState::parse_times_into(text, &mut result);
        result
    }

    fn process_stat(stat: &str) -> Option<(String, u64, u64)> {
        let bytes = stat.as_bytes();
        let close = bytes.iter().rposition(|&byte| byte == b')')?;
        let open = bytes[..close].iter().position(|&byte| byte == b'(')?;
        let name = stat.get(open + 1..close)?.to_owned();
        let mut index = close + 1;
        let mut field = 0;
        let mut user = None;
        let mut system = None;
        let mut resident_pages = None;
        while field <= 21 {
            while index < bytes.len() && bytes[index].is_ascii_whitespace() {
                index += 1;
            }
            let token_start = index;
            while index < bytes.len() && !bytes[index].is_ascii_whitespace() {
                index += 1;
            }
            if token_start == index {
                return None;
            }
            if field == 0 {
                // The state field is a single alphabetic byte; all later
                // fields needed here are decimal counters.
                field += 1;
                continue;
            }
            let mut value = 0u64;
            for &byte in &bytes[token_start..index] {
                if !byte.is_ascii_digit() {
                    return None;
                }
                value = value
                    .saturating_mul(10)
                    .saturating_add(u64::from(byte - b'0'));
            }
            match field {
                11 => user = Some(value),
                12 => system = Some(value),
                21 => resident_pages = Some(value),
                _ => {}
            }
            field += 1;
        }
        Some((name, user?.saturating_add(system?), resident_pages?))
    }
}

struct MemoryValues;
impl MemoryValues {
    fn read() -> (f32, f32, f32, f32) {
        let text = fs::read_to_string("/proc/meminfo").unwrap_or_default();
        let find = |key| {
            text.lines()
                .find_map(|l| {
                    l.strip_prefix(key)?
                        .split_whitespace()
                        .next()?
                        .parse::<u64>()
                        .ok()
                })
                .unwrap_or(0)
        };
        let total = find("MemTotal:");
        let available = find("MemAvailable:");
        let swap_total = find("SwapTotal:");
        let swap_free = find("SwapFree:");
        #[allow(clippy::cast_precision_loss)]
        let used = if total == 0 {
            0.0
        } else {
            100.0 * (total.saturating_sub(available)) as f32 / total as f32
        };
        #[allow(clippy::cast_precision_loss)]
        (
            used,
            available as f32 / 1024.0,
            swap_total.saturating_sub(swap_free) as f32 / 1024.0,
            swap_total as f32 / 1024.0,
        )
    }
}
struct TemperatureValue;
impl TemperatureValue {
    fn read() -> f32 {
        fs::read_dir("/sys/class/thermal").map_or(0.0, |entries| {
            entries
                .flatten()
                .find_map(|e| {
                    fs::read_to_string(e.path().join("temp"))
                        .ok()?
                        .trim()
                        .parse::<f32>()
                        .ok()
                })
                .map_or(0.0, |v| v / 1000.0)
        })
    }
}
/// Linux's `statvfs(2)` layout. The monitor targets Linux, and the fields we
/// read are ABI-stable unsigned-long values from the kernel/libc interface.
#[repr(C)]
struct StatVfs {
    blocks_size: c_ulong,
    fragment_size: c_ulong,
    blocks: c_ulong,
    free_blocks: c_ulong,
    available_blocks: c_ulong,
    files: c_ulong,
    free_files: c_ulong,
    available_files: c_ulong,
    filesystem_id: c_ulong,
    flags: c_ulong,
    name_max: c_ulong,
    spare: [c_int; 6],
}

unsafe extern "C" {
    fn statvfs(path: *const c_char, buffer: *mut StatVfs) -> c_int;
    fn sysconf(name: c_int) -> c_long;
}

struct DiskValue;
impl DiskValue {
    /// Read root capacity directly from the kernel's VFS accounting. Unlike
    /// `df`, this neither forks nor parses formatted text, so it is safe on the
    /// monitor's 60 Hz sample path.
    fn read() -> f32 {
        let path = c"/";
        // SAFETY: `path` is NUL-terminated and `stat` is valid writable storage
        // for the duration of this libc call.
        let mut stat = unsafe { std::mem::zeroed::<StatVfs>() };
        if unsafe { statvfs(path.as_ptr(), &raw mut stat) } != 0 || stat.blocks == 0 {
            return 0.0;
        }
        #[allow(clippy::cast_precision_loss)]
        {
            100.0 * (1.0 - stat.available_blocks as f32 / stat.blocks as f32)
        }
    }
}

impl Default for MonitorReader {
    fn default() -> Self {
        Self::new()
    }
}

// Keep the unused marker's ownership explicit until disk accounting is added.
#[allow(dead_code)]
struct _DiskPath;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reader_has_stable_shape() {
        let mut reader = MonitorReader::new();
        let sample = reader.sample();
        assert!(sample.cpu >= 0.0);
    }

    #[test]
    fn process_stat_reads_cpu_ticks_after_a_parenthesized_name() {
        let stat = "42 (worker) S 1 2 3 4 5 6 7 8 9 10 123 456 13 14 15 16 17 18 19 20 321";
        let (name, ticks, resident_pages) =
            MonitorReader::process_stat(stat).expect("valid proc stat");
        assert_eq!(name, "worker");
        assert_eq!(ticks, 579);
        assert_eq!(resident_pages, 321);
    }

    #[test]
    fn cpu_times_byte_parser_preserves_aggregate_and_core_records() {
        let text = "cpu  123 45 67 890 12 3 4 5 6 7\ncpu0 1 2 3 4 5 6 7 8 9 10\nintr 99\n";
        assert_eq!(
            {
                let mut result = Vec::new();
                CpuState::parse_times_into(text, &mut result);
                result
            },
            vec![(1_162, 902), (55, 9)]
        );
    }

    #[test]
    fn cpu_times_byte_parser_rejects_incomplete_records() {
        assert!(CpuState::parse_cpu_times_for_bench("cpu 1 2 3\ncpu0 x 2 3 4\n").is_empty());
    }
}

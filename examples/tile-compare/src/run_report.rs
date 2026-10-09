//! [`RunReport`] -- what one mode cost over a run, on the client, in the
//! compositor and on the GPU.

use std::time::{Duration, Instant};

use crate::compositor_probe::{CompositorProbe, CompositorSample};
use crate::gpu_probe::GpuProbe;
use crate::process_sample::ProcessSample;
use crate::run_numbers::RunNumbers;

pub struct RunReport {
    mode: &'static str,
    started: Instant,
    start: ProcessSample,
    frames: u64,
    frame_work: Duration,
    peak_rss_kb: u64,
    requests: u64,
    compositor: Option<(CompositorProbe, CompositorSample)>,
    gpu: Option<GpuProbe>,
    last_gpu_sample: Instant,
}

impl RunReport {
    const GPU_SAMPLE_EVERY: Duration = Duration::from_millis(50);

    #[must_use]
    pub fn begin(mode: &'static str) -> Self {
        let start = ProcessSample::now();
        let compositor = CompositorProbe::find().map(|probe| {
            let sample = probe.sample();
            (probe, sample)
        });
        Self {
            mode,
            started: Instant::now(),
            start,
            frames: 0,
            frame_work: Duration::ZERO,
            peak_rss_kb: start.rss_kb,
            requests: 0,
            compositor,
            gpu: GpuProbe::find(),
            last_gpu_sample: Instant::now(),
        }
    }

    pub fn record_frame(&mut self, work: Duration, requests: usize) {
        self.frames += 1;
        self.frame_work += work;
        self.requests += requests as u64;
        if self.frames.is_multiple_of(30) {
            self.peak_rss_kb = self.peak_rss_kb.max(ProcessSample::now().rss_kb);
        }
    }

    pub fn sample_gpu(&mut self) {
        if self.last_gpu_sample.elapsed() < Self::GPU_SAMPLE_EVERY {
            return;
        }
        self.last_gpu_sample = Instant::now();
        if let Some(gpu) = &mut self.gpu {
            gpu.sample();
        }
    }

    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    #[allow(clippy::cast_precision_loss)]
    pub fn print(&self, extra: &str) {
        let end = ProcessSample::now();
        let wall = self.started.elapsed();
        let cpu = end.cpu.saturating_sub(self.start.cpu);
        let frames = self.frames.max(1) as f64;
        let percent = |d: Duration| 100.0 * d.as_secs_f64() / wall.as_secs_f64();
        let mut numbers = RunNumbers {
            fps: self.frames as f64 / wall.as_secs_f64(),
            client_cpu: percent(cpu),
            work_us: self.frame_work.as_secs_f64() * 1e6 / frames,
            ..RunNumbers::default()
        };
        println!("mode            {}", self.mode);
        println!("wall            {:.1} s", wall.as_secs_f64());
        println!(
            "frames          {} ({:.1} fps)",
            self.frames,
            self.frames as f64 / wall.as_secs_f64()
        );
        println!(
            "client cpu      {:.2} s ({:.1}% of one core)",
            cpu.as_secs_f64(),
            percent(cpu)
        );
        println!(
            "work per frame  {:.1} us",
            self.frame_work.as_secs_f64() * 1e6 / frames
        );
        if self.requests > 0 {
            println!("requests/frame  {:.1}", self.requests as f64 / frames);
        }
        println!("rss start       {} kB", self.start.rss_kb);
        println!("rss peak        {} kB", self.peak_rss_kb.max(end.rss_kb));
        println!("rss end         {} kB", end.rss_kb);
        if let Some((probe, start)) = &self.compositor {
            let now = probe.sample();
            let cpu = now.process.cpu.saturating_sub(start.process.cpu);
            let gfx = now.drm.gfx.saturating_sub(start.drm.gfx);
            numbers.compositor_cpu = percent(cpu);
            numbers.compositor_gfx = percent(gfx);
            numbers.compositor_gpu_mem_kb =
                now.drm.memory_kb() as f64 - start.drm.memory_kb() as f64;
            println!(
                "{:<15} {:.2} s ({:.1}% of one core), rss {} -> {} kB",
                probe.name(),
                cpu.as_secs_f64(),
                percent(cpu),
                start.process.rss_kb,
                now.process.rss_kb
            );
            println!(
                "{:<15} {:.1} ms ({:.2}% busy, {:.1} us/frame)",
                format!("{} gfx", probe.name()),
                gfx.as_secs_f64() * 1e3,
                percent(gfx),
                gfx.as_secs_f64() * 1e6 / frames
            );
            println!(
                "{:<15} vram {} -> {} kB, gtt {} -> {} kB",
                format!("{} gpu mem", probe.name()),
                start.drm.vram_kb,
                now.drm.vram_kb,
                start.drm.gtt_kb,
                now.drm.gtt_kb
            );
        }
        if let Some(gpu) = &self.gpu {
            numbers.gpu_busy = gpu.busy_percent();
            numbers.power_w = gpu.power_watts();
            println!("{}", gpu.summary());
        }
        if !extra.is_empty() {
            println!("{extra}");
        }
        println!("{}", numbers.to_line());
    }
}

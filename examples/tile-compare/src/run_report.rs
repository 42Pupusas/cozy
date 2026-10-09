//! [`RunReport`] -- what one mode cost over a run.

use std::time::{Duration, Instant};

use crate::process_sample::ProcessSample;

pub struct RunReport {
    mode: &'static str,
    started: Instant,
    start: ProcessSample,
    frames: u64,
    frame_work: Duration,
    peak_rss_kb: u64,
    requests: u64,
}

impl RunReport {
    #[must_use]
    pub fn begin(mode: &'static str) -> Self {
        let start = ProcessSample::now();
        Self {
            mode,
            started: Instant::now(),
            start,
            frames: 0,
            frame_work: Duration::ZERO,
            peak_rss_kb: start.rss_kb,
            requests: 0,
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
        println!("mode            {}", self.mode);
        println!("wall            {:.1} s", wall.as_secs_f64());
        println!(
            "frames          {} ({:.1} fps)",
            self.frames,
            self.frames as f64 / wall.as_secs_f64()
        );
        println!(
            "cpu             {:.2} s ({:.1}% of one core)",
            cpu.as_secs_f64(),
            100.0 * cpu.as_secs_f64() / wall.as_secs_f64()
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
        if !extra.is_empty() {
            println!("{extra}");
        }
    }
}

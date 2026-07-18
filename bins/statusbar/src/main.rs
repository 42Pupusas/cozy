#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::doc_markdown,
    clippy::duration_suboptimal_units,
    clippy::unchecked_time_subtraction
)]
//! `statusbar` — a wlroots-layer-shell status bar, built on the same
//! `io_uring` Wayland connection / software canvas as the `terminal` binary.
//!
//! Single-threaded: modules are refreshed on plain wall-clock cadences
//! (`Instant`-based), computed synchronously in the main loop, and painted
//! into the bar's canvas. None of the modules are so slow that this needs a
//! worker thread (the slowest, `cpu_usage`, deliberately sleeps ~120ms to
//! sample two `/proc/stat` snapshots — acceptable at a multi-second refresh
//! period, but see the `Refresh` cadence table below).

use std::time::{Duration, Instant};

use platform::draw::{Canvas, Color};
use statusbar::{BarWindow, Edge, TextRenderer};

/// How often each module recomputes. Cheap `/proc`/`/sys` reads run often;
/// modules that shell out to a CLI (`iw`, `pactl`, `bluetoothctl`) run less
/// often to keep the main loop from spawning processes every tick.
const CLOCK_PERIOD: Duration = Duration::from_millis(1000);
const FAST_PERIOD: Duration = Duration::from_secs(2); // battery, temp, mem
const SLOW_PERIOD: Duration = Duration::from_secs(5); // wifi, bluetooth, volume, brightness
/// `cpu_usage` samples twice ~120ms apart, so give it its own slightly
/// longer period rather than sharing `FAST_PERIOD`.
const CPU_PERIOD: Duration = Duration::from_millis(2000);

struct Modules {
    clock: String,
    date: String,
    battery: String,
    temp: String,
    cpu: String,
    mem: String,
    wifi: String,
    bluetooth: String,
    vol_out: String,
    vol_in: String,
    brightness: String,

    last_clock: Instant,
    last_fast: Instant,
    last_slow: Instant,
    last_cpu: Instant,
}

const DASH: &str = "—";

impl Modules {
    fn new() -> Self {
        let now = Instant::now();
        // Back-date every timer so the very first `refresh` call (right
        // after the bar maps) populates every module immediately instead of
        // waiting out a full period on a blank bar.
        let stale = now - Duration::from_secs(3600);
        Self {
            clock: String::new(),
            date: String::new(),
            battery: String::new(),
            temp: String::new(),
            cpu: String::new(),
            mem: String::new(),
            wifi: String::new(),
            bluetooth: String::new(),
            vol_out: String::new(),
            vol_in: String::new(),
            brightness: String::new(),
            last_clock: stale,
            last_fast: stale,
            last_slow: stale,
            last_cpu: stale,
        }
    }

    /// Recompute whichever modules are due; returns whether anything changed
    /// (so the caller only redraws on real updates, not every idle poll).
    fn refresh(&mut self) -> bool {
        let now = Instant::now();
        let mut changed = false;

        if now.duration_since(self.last_clock) >= CLOCK_PERIOD {
            self.last_clock = now;
            if let Some(c) = statusbar::modules::clock() {
                changed |= self.clock != c;
                self.clock = c;
            }
        }

        if now.duration_since(self.last_fast) >= FAST_PERIOD {
            self.last_fast = now;
            let d = statusbar::modules::date().unwrap_or_else(|| DASH.into());
            changed |= self.date != d;
            self.date = d;
            let b = statusbar::modules::battery().unwrap_or_else(|| DASH.into());
            changed |= self.battery != b;
            self.battery = b;
            let t = statusbar::modules::temperature().unwrap_or_else(|| DASH.into());
            changed |= self.temp != t;
            self.temp = t;
            let m = statusbar::modules::memory().unwrap_or_else(|| DASH.into());
            changed |= self.mem != m;
            self.mem = m;
        }

        if now.duration_since(self.last_cpu) >= CPU_PERIOD {
            self.last_cpu = now;
            let c = statusbar::modules::cpu_usage().unwrap_or_else(|| DASH.into());
            changed |= self.cpu != c;
            self.cpu = c;
        }

        if now.duration_since(self.last_slow) >= SLOW_PERIOD {
            self.last_slow = now;
            let w = statusbar::modules::wifi().unwrap_or_else(|| DASH.into());
            changed |= self.wifi != w;
            self.wifi = w;
            let bt = statusbar::modules::bluetooth().unwrap_or_else(|| DASH.into());
            changed |= self.bluetooth != bt;
            self.bluetooth = bt;
            let vo = statusbar::modules::volume_out().unwrap_or_else(|| DASH.into());
            changed |= self.vol_out != vo;
            self.vol_out = vo;
            let vi = statusbar::modules::volume_in().unwrap_or_else(|| DASH.into());
            changed |= self.vol_in != vi;
            self.vol_in = vi;
            let br = statusbar::modules::brightness().unwrap_or_else(|| DASH.into());
            changed |= self.brightness != br;
            self.brightness = br;
        }

        changed
    }

    /// Every module's rendered form, right-to-left draw order (rightmost
    /// module first), separated by a thin divider.
    fn right_segments(&self) -> Vec<String> {
        vec![
            self.clock.clone(),
            self.date.clone(),
            self.battery.clone(),
            self.temp.clone(),
            self.cpu.clone(),
            self.mem.clone(),
            self.wifi.clone(),
            self.bluetooth.clone(),
            self.vol_out.clone(),
            self.vol_in.clone(),
            self.brightness.clone(),
        ]
    }
}

#[allow(clippy::too_many_arguments)]
fn paint(
    canvas: &mut Canvas,
    modules: &Modules,
    text: &mut TextRenderer,
    fg: Color,
    bg: Color,
    bg_alpha: u8,
    bar_height: i32,
    scale: i32,
) {
    let y = text.baseline_y(bar_height);
    let pad = 8 * scale;
    let gap = 16 * scale;

    // Left: nothing wired up yet (workspaces/window-title would land here
    // once a sway-IPC client is added) — leave the gutter clear.

    // Right-to-left layout: draw each segment ending `gap` to the left of
    // the previous one's start.
    let mut right_edge = canvas.width() - pad;
    for seg in modules.right_segments() {
        if seg.is_empty() {
            continue;
        }
        text.draw_text_right_alpha(canvas, &seg, right_edge, y, fg, bg, bg_alpha);
        let w = text.measure(&seg);
        right_edge -= w + gap;
    }
}

fn main() {
    let cfg = config::Config::load().unwrap_or_else(|e| {
        eprintln!("statusbar: config error: {e} — using defaults");
        config::Config::default()
    });

    let wake = waker::Waker::new().expect("create waker");
    let bar_height = cfg.bar_height() as i32;
    let mut window = match BarWindow::new(Edge::Top, bar_height, cfg.bar_opacity(), wake) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("statusbar: failed to open layer-shell window: {e}");
            std::process::exit(1);
        }
    };

    let scale = window.scale();
    let bg = {
        let c = cfg.bar_background();
        Color::rgb(c.r, c.g, c.b)
    };
    let fg = {
        let c = cfg.bar_foreground();
        Color::rgb(c.r, c.g, c.b)
    };
    let bg_alpha = (cfg.bar_opacity().clamp(0.0, 1.0) * 255.0).round() as u8;
    window.set_background(bg);

    // Font size is a point size (like the terminal's own `[font] size`);
    // convert to pixels at 96 DPI, then scale by the output's physical
    // pixel density so it renders crisp on HiDPI, matching
    // `platform::Renderer::with_scale`'s convention.
    let px_size = cfg.bar_font_px_size() * scale as f32;
    let mut text = TextRenderer::load_with_emoji(cfg.bar_font_path(), cfg.bar_emoji_path(), px_size);

    let mut modules = Modules::new();
    let bar_height = window.height();

    loop {
        let changed = modules.refresh();
        if changed {
            window.request_redraw();
        }
        match window.tick(|canvas| {
            paint(canvas, &modules, &mut text, fg, bg, bg_alpha, bar_height, scale);
        }) {
            Ok(true) => {}
            Ok(false) => break,
            Err(e) => {
                eprintln!("statusbar: {e}");
                break;
            }
        }
        // Poll at ~10fps when idle -- modules refresh on their own slower
        // cadences above; this just keeps the Wayland event loop responsive
        // (resize/output changes) without busy-spinning.
        window.wait(100);
    }
}

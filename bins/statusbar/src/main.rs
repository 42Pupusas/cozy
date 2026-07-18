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

/// Kanagawa Dragon accents, lifted verbatim from waybar's own
/// `~/.config/waybar/style.css` `@define-color` block -- so our bar's
/// per-module colors match waybar's exactly, not just the shape of the
/// idea. `fg`/`bg` aren't here: those already come from `[bar]`/`[colors]`
/// (see `cfg.bar_foreground`/`bar_background` in `main`), since unlike
/// waybar's hardcoded stylesheet, ours mirrors the terminal's configured
/// theme by default.
mod palette {
    use platform::draw::Color;

    pub const FG_DARK: Color = Color::rgb(0xa6, 0xa6, 0x9c);
    pub const RED: Color = Color::rgb(0xc4, 0x74, 0x6e);
    pub const ORANGE: Color = Color::rgb(0xb6, 0x92, 0x7b);
    pub const YELLOW: Color = Color::rgb(0xc4, 0xb2, 0x8a);
    pub const GREEN: Color = Color::rgb(0x8a, 0x9a, 0x7b);
    pub const BLUE: Color = Color::rgb(0x8b, 0xa4, 0xb0);
    pub const PURPLE: Color = Color::rgb(0xa2, 0x92, 0xa3);
    pub const TEAL: Color = Color::rgb(0x8e, 0xa4, 0xa2);
}

/// Which module a rendered segment came from -- just enough to pick its
/// color in `module_color`; not otherwise used for layout.
#[derive(Clone, Copy)]
enum ModuleKind {
    Clock,
    Date,
    Wifi,
    Bluetooth,
    Battery,
    Temp,
    Cpu,
    Mem,
    VolOut,
    VolIn,
    Brightness,
}

/// Per-module foreground color, matching waybar's `style.css` rules
/// (`#clock`, `#battery.warning`, `#network.disconnected`, etc.) -- state
/// is read straight off the icon glyph each module already picked
/// (`modules.rs`'s icons are unique per state: disconnected/muted/charging
/// all use a different glyph), so this needs no extra plumbing back to the
/// raw sensor data. Codepoints below were pulled with a throwaway probe
/// binary reading `modules.rs`'s actual string literals, not hand-copied,
/// after a hand-transcribed `\u{...}` escape caused a tofu regression once
/// already (see the CPU icon fix in this same commit).
fn module_color(kind: ModuleKind, text: &str) -> Color {
    use palette::{BLUE, FG_DARK, GREEN, ORANGE, PURPLE, RED, TEAL, YELLOW};
    match kind {
        ModuleKind::Clock | ModuleKind::Date => PURPLE,
        ModuleKind::Wifi => {
            if text.starts_with('\u{f092d}') {
                RED // waybar's #network.disconnected
            } else {
                BLUE
            }
        }
        ModuleKind::Bluetooth => {
            if text.starts_with('\u{f00b2}') {
                FG_DARK // waybar's #bluetooth.off/.disabled
            } else {
                PURPLE
            }
        }
        ModuleKind::Battery => {
            if text.starts_with('\u{f0084}') || text.starts_with('\u{f06a5}') {
                GREEN // waybar's #battery.charging/.plugged
            } else if let Some(pct) = trailing_percent(text) {
                if pct < 15 {
                    RED // waybar's #battery.critical
                } else if pct < 30 {
                    YELLOW // waybar's #battery.warning
                } else {
                    GREEN
                }
            } else {
                GREEN
            }
        }
        ModuleKind::Temp => ORANGE,
        ModuleKind::Cpu => GREEN,
        ModuleKind::Mem => BLUE,
        ModuleKind::VolOut => {
            if text.starts_with('\u{f075f}') {
                FG_DARK // waybar's #pulseaudio.muted
            } else {
                TEAL
            }
        }
        ModuleKind::VolIn => {
            if text.starts_with('\u{f036d}') {
                FG_DARK
            } else {
                ORANGE // waybar's #pulseaudio.microphone
            }
        }
        ModuleKind::Brightness => YELLOW,
    }
}

/// The percentage number at the end of a module string like "X 87%", if
/// any -- used for battery's warning/critical color thresholds.
fn trailing_percent(text: &str) -> Option<u32> {
    text.trim_end_matches('%').rsplit(' ').next()?.parse().ok()
}

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

    /// Left section, left-to-right draw order: clock + date lead, followed
    /// by the always-visible connectivity cluster.
    fn left_segments(&self) -> Vec<(ModuleKind, String)> {
        vec![
            (ModuleKind::Clock, self.clock.clone()),
            (ModuleKind::Date, self.date.clone()),
            (ModuleKind::Wifi, self.wifi.clone()),
            (ModuleKind::Bluetooth, self.bluetooth.clone()),
        ]
    }

    /// Center section -- unused for now (kept as an empty group so the
    /// three-section layout is exercised even with nothing docked there;
    /// a sway-IPC workspace/window-title module would land here).
    #[allow(clippy::unused_self, clippy::missing_const_for_fn)]
    fn center_segments(&self) -> Vec<(ModuleKind, String)> {
        Vec::new()
    }

    /// Right section, right-to-left draw order (rightmost module first):
    /// everything not claimed by `left_segments`.
    fn right_segments(&self) -> Vec<(ModuleKind, String)> {
        vec![
            (ModuleKind::Battery, self.battery.clone()),
            (ModuleKind::Temp, self.temp.clone()),
            (ModuleKind::Cpu, self.cpu.clone()),
            (ModuleKind::Mem, self.mem.clone()),
            (ModuleKind::VolOut, self.vol_out.clone()),
            (ModuleKind::VolIn, self.vol_in.clone()),
            (ModuleKind::Brightness, self.brightness.clone()),
        ]
    }
}

#[allow(clippy::too_many_arguments)]
fn paint(
    canvas: &mut Canvas,
    modules: &Modules,
    text: &mut TextRenderer,
    bg: Color,
    bg_alpha: u8,
    bar_height: i32,
    scale: i32,
) {
    let y = text.baseline_y(bar_height);
    let pad = 20 * scale;
    let gap = 28 * scale;

    // Left: left-to-right layout, each segment starting `gap` past the
    // previous one's end.
    let mut left_edge = pad;
    for (kind, seg) in modules.left_segments() {
        if seg.is_empty() {
            continue;
        }
        let color = module_color(kind, &seg);
        text.draw_text_alpha(canvas, &seg, left_edge, y, color, bg, bg_alpha);
        let w = text.measure(&seg);
        left_edge += w + gap;
    }

    // Center: horizontally centered as one contiguous cluster (joined by
    // `gap`), regardless of what the left/right sections are doing.
    let center = modules.center_segments();
    if !center.is_empty() {
        let joined_w: i32 = center.iter().map(|(_, s)| text.measure(s)).sum::<i32>()
            + gap * (center.len() as i32 - 1).max(0);
        let mut x = (canvas.width() - joined_w) / 2;
        for (kind, seg) in &center {
            if seg.is_empty() {
                continue;
            }
            let color = module_color(*kind, seg);
            let w = text.measure(seg);
            text.draw_text_alpha(canvas, seg, x, y, color, bg, bg_alpha);
            x += w + gap;
        }
    }

    // Right-to-left layout: draw each segment ending `gap` to the left of
    // the previous one's start.
    let mut right_edge = canvas.width() - pad;
    for (kind, seg) in modules.right_segments() {
        if seg.is_empty() {
            continue;
        }
        let color = module_color(kind, &seg);
        text.draw_text_right_alpha(canvas, &seg, right_edge, y, color, bg, bg_alpha);
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
    let bg_alpha = (cfg.bar_opacity().clamp(0.0, 1.0) * 255.0).round() as u8;
    window.set_background(bg);

    // Font size is a point size (like the terminal's own `[font] size`);
    // convert to pixels at 96 DPI, then scale by the output's physical
    // pixel density so it renders crisp on HiDPI, matching
    // `platform::Renderer::with_scale`'s convention.
    let px_size = cfg.bar_font_px_size() * scale as f32;
    let mut text = TextRenderer::load_with_fallbacks(
        cfg.bar_font_path(),
        &[cfg.bar_emoji_path(), cfg.bar_icon_path()],
        px_size,
    );

    let mut modules = Modules::new();
    let bar_height = window.height();

    loop {
        let changed = modules.refresh();
        if changed {
            window.request_redraw();
        }
        match window.tick(|canvas| {
            paint(canvas, &modules, &mut text, bg, bg_alpha, bar_height, scale);
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

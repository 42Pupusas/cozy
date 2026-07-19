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
use statusbar::{BarWindow, Edge, ModuleKind, ScrollAccumulator, TextRenderer, module_color, steps_to_delta_pct};

/// How often each module recomputes. Cheap `/proc`/`/sys` reads run often;
/// modules that shell out to a CLI (`iw`, `pactl`, `bluetoothctl`) run less
/// often to keep the main loop from spawning processes every tick.
const CLOCK_PERIOD: Duration = Duration::from_millis(1000);
const FAST_PERIOD: Duration = Duration::from_secs(2); // battery, temp, mem
const SLOW_PERIOD: Duration = Duration::from_secs(5); // wifi, bluetooth, volume, brightness
/// `cpu_usage` samples twice ~120ms apart, so give it its own slightly
/// longer period rather than sharing `FAST_PERIOD`.
const CPU_PERIOD: Duration = Duration::from_millis(2000);

/// Percentage points nudged per wheel detent when scroll-adjusting
/// vol-out/vol-in/brightness.
const SCROLL_STEP_PCT: i32 = 5;

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
    /// Debounce gate for `force_refresh_controls`: a rapid burst of wheel
    /// detents (the common case -- a single physical scroll gesture fires
    /// many discrete axis events) used to trigger one blocking
    /// `pactl get-*`/`brightnessctl` round-trip *per event*, stalling the
    /// paint loop on process I/O every tick -- that was the actual source
    /// of the reported jank, not the (non-blocking) `set-*-volume` nudge
    /// itself. Now a refresh only actually runs once this many ms have
    /// passed since the last one; the mixer/backlight settles well within
    /// that window so the displayed value still catches up quickly once
    /// scrolling pauses.
    last_control_refresh: Instant,
    /// Coalesces scroll-wheel adjustments: **raw wl_fixed-derived pixels**
    /// (not yet converted to a percentage step) accumulated since the last
    /// time we actually spawned `pactl`/`brightnessctl` (`None` when
    /// nothing's pending), plus which control they're for. Keeping this in
    /// raw pixels rather than pre-rounded steps matters: a single physical
    /// scroll gesture -- especially kinetic/smooth scrolling on a touchpad
    /// or a high-resolution mouse wheel -- delivers many *small*
    /// `wl_pointer.axis` events, each well under one detent (~15px) on its
    /// own. Rounding every individual event up to at least one step (the
    /// first version of this) meant a gentle scroll fired a burst of
    /// spawns and jumped the value around inconsistently. Now sub-detent
    /// deltas just accumulate here fractionally; `apply_pending_scroll`
    /// only converts (and drains) whole detents' worth into an actual
    /// step, leaving any remainder under one detent queued for next time.
    /// The accumulation math itself lives in `statusbar::scroll` (unit
    /// tested there); this just pairs it with which control it's for.
    pending_scroll: Option<(ModuleKind, ScrollAccumulator)>,
    last_scroll_apply: Instant,
}

/// Minimum spacing between `force_refresh_controls` calls, in milliseconds.
const CONTROL_REFRESH_DEBOUNCE_MS: u64 = 150;
/// Minimum spacing between actually spawning a `pactl`/`brightnessctl`
/// *adjustment* while scrolling -- see `Modules::pending_scroll`'s doc.
const SCROLL_APPLY_DEBOUNCE_MS: u64 = 60;

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
            last_control_refresh: stale,
            pending_scroll: None,
            last_scroll_apply: stale,
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

    /// Immediately recompute just the one control `kind` was scrolled on
    /// (skipping `SLOW_PERIOD`'s cooldown) so the new value is visible
    /// without waiting out the rest of that module's poll period --
    /// debounced to at most once per [`CONTROL_REFRESH_DEBOUNCE_MS`] so a
    /// burst of wheel-detent events (one physical scroll gesture routinely
    /// fires many) doesn't run a blocking `pactl get-*`/`brightnessctl`
    /// subprocess round-trip on every single tick of the paint loop -- that
    /// synchronous stall, not the (already non-blocking) volume/brightness
    /// *set* itself, was the actual source of scrolling feeling janky.
    /// Only reads one module (not all three) since scrolling one control
    /// doesn't change the others.
    fn force_refresh_control(&mut self, kind: ModuleKind) {
        let now = Instant::now();
        if now.duration_since(self.last_control_refresh) < Duration::from_millis(CONTROL_REFRESH_DEBOUNCE_MS)
        {
            return;
        }
        self.last_control_refresh = now;
        match kind {
            ModuleKind::VolOut => {
                self.vol_out = statusbar::modules::volume_out().unwrap_or_else(|| DASH.into());
            }
            ModuleKind::VolIn => {
                self.vol_in = statusbar::modules::volume_in().unwrap_or_else(|| DASH.into());
            }
            ModuleKind::Brightness => {
                self.brightness = statusbar::modules::brightness().unwrap_or_else(|| DASH.into());
            }
            _ => {}
        }
    }

    /// Accumulate a raw wheel-pixel delta (see `pending_scroll`'s doc) for
    /// `kind` without touching the mixer/backlight yet. Switching which
    /// control is being scrolled resets the accumulator to just the new
    /// delta rather than summing across controls (and drops any leftover
    /// sub-detent remainder from the previous control -- it's stale once
    /// the pointer's moved to a different segment).
    fn queue_scroll(&mut self, kind: ModuleKind, raw_px: i32) {
        match &mut self.pending_scroll {
            Some((k, acc)) if *k == kind => acc.add(raw_px),
            _ => {
                let mut acc = ScrollAccumulator::new();
                acc.add(raw_px);
                self.pending_scroll = Some((kind, acc));
            }
        }
    }

    /// Convert whatever whole detents' worth of raw pixels have
    /// accumulated into one `pactl`/`brightnessctl` spawn (if any, and if
    /// the debounce window has elapsed), then immediately re-reads just
    /// that control. Any leftover fractional pixels under one detent stay
    /// queued rather than being dropped, so a slow/gentle scroll still
    /// adds up correctly instead of needing to individually clear the
    /// detent threshold each event. Returns `true` if a redraw is
    /// warranted.
    fn apply_pending_scroll(&mut self) -> bool {
        let Some((kind, mut acc)) = self.pending_scroll else {
            return false;
        };
        let now = Instant::now();
        if now.duration_since(self.last_scroll_apply) < Duration::from_millis(SCROLL_APPLY_DEBOUNCE_MS) {
            return false;
        }
        self.last_scroll_apply = now;
        let steps = acc.take_steps();
        self.pending_scroll = Some((kind, acc));
        if steps == 0 {
            // Nothing to apply yet, but don't churn force_refresh_control
            // on sub-detent noise -- just wait for more to accumulate.
            return false;
        }
        let delta = steps_to_delta_pct(steps, SCROLL_STEP_PCT);
        match kind {
            ModuleKind::VolOut => statusbar::modules::adjust_volume_out(delta),
            ModuleKind::VolIn => statusbar::modules::adjust_volume_in(delta),
            ModuleKind::Brightness => statusbar::modules::adjust_brightness(delta),
            _ => {}
        }
        self.force_refresh_control(kind);
        true
    }

    /// Left section, left-to-right draw order: the always-visible
    /// connectivity cluster (clock/date live in `right_segments` now).
    fn left_segments(&self) -> Vec<(ModuleKind, String)> {
        vec![
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
            (ModuleKind::Date, self.date.clone()),
            (ModuleKind::Clock, self.clock.clone()),
            (ModuleKind::Battery, self.battery.clone()),
            (ModuleKind::Temp, self.temp.clone()),
            // (ModuleKind::Cpu, self.cpu.clone()), // disabled per user request
            // (ModuleKind::Mem, self.mem.clone()), // disabled per user request
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
    segment_rects: &mut Vec<(ModuleKind, i32, i32)>,
) {
    segment_rects.clear();
    let y = text.baseline_y(bar_height);
    let pad = 20 * scale;
    let gap = 18 * scale;
    // Scroll/click hit-testing only cares about x-ranges, not glyph pixels,
    // so pad each segment's interactive rect out by half the inter-segment
    // gap on each side -- glyph-tight hit boxes made it fiddly to land the
    // pointer precisely on vol-out/vol-in/brightness to scroll them. Half
    // the gap on each side means neighboring segments' padded rects meet
    // exactly at the gap's midpoint without overlapping.
    let hit_pad = gap / 2;

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
        segment_rects.push((kind, left_edge - hit_pad, left_edge + w + hit_pad));
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
            segment_rects.push((*kind, x - hit_pad, x + w + hit_pad));
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
        segment_rects.push((kind, right_edge - w - hit_pad, right_edge + hit_pad));
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
    let mut segment_rects: Vec<(ModuleKind, i32, i32)> = Vec::new();

    loop {
        let changed = modules.refresh();
        if changed {
            window.request_redraw();
        }
        match window.tick(|canvas| {
            paint(
                canvas,
                &modules,
                &mut text,
                bg,
                bg_alpha,
                bar_height,
                scale,
                &mut segment_rects,
            );
        }) {
            Ok(true) => {}
            Ok(false) => break,
            Err(e) => {
                eprintln!("statusbar: {e}");
                break;
            }
        }

        // Scroll-to-adjust: a wheel event over the vol-out/vol-in/brightness
        // segment queues a delta for that control -- it does NOT spawn
        // `pactl`/`brightnessctl` directly here. A single physical scroll
        // gesture (especially kinetic/smooth scrolling) delivers many small
        // `wl_pointer.axis` events in a burst; spawning a process per event
        // was the actual source of the reported jank. `queue_scroll` just
        // sums the deltas; `apply_pending_scroll` below drains the
        // accumulator into a single subprocess spawn, at most once per
        // `SCROLL_APPLY_DEBOUNCE_MS`.
        let scrolls = window.take_scroll();
        if !scrolls.is_empty() {
            let px = window.pointer_pos().x;
            if let Some((kind, ..)) = segment_rects
                .iter()
                .find(|(_, x0, x1)| px >= *x0 && px < *x1)
            {
                // Sum raw wheel pixels first, THEN convert to steps once --
                // converting per-event with a `.max(1)` floor (the previous
                // version) meant every individual micro-event out of a
                // smooth/kinetic scroll gesture (touchpads and hi-res mouse
                // wheels routinely fire many small ones per gesture) got
                // rounded *up* to a full detent, so a single gentle scroll
                // could register as a dozen-plus steps -- that's what made
                // the volume jump around inconsistently. A detent is ~15
                // wl_fixed-derived pixels; anything smaller now stays
                // fractional in `queue_scroll`'s px accumulator and only
                // turns into an actual step once enough of it has built up.
                let raw_px: i32 = scrolls.iter().sum();
                let kind = *kind;
                if matches!(
                    kind,
                    ModuleKind::VolOut | ModuleKind::VolIn | ModuleKind::Brightness
                ) {
                    modules.queue_scroll(kind, raw_px);
                }
            }
        }
        if modules.apply_pending_scroll() {
            window.request_redraw();
        }

        // Poll at ~10fps when idle -- modules refresh on their own slower
        // cadences above; this just keeps the Wayland event loop responsive
        // (resize/output changes) without busy-spinning. While a scroll
        // adjustment is queued, poll at the apply-debounce granularity
        // instead so it doesn't sit un-applied until the next unrelated
        // wakeup (a wheel event does wake us via the pointer axis event
        // itself, but the trailing queued remainder after scrolling stops
        // needs its own timely wakeup to actually flush).
        let wait_ms = if modules.pending_scroll.is_some() {
            SCROLL_APPLY_DEBOUNCE_MS as i32
        } else {
            100
        };
        window.wait(wait_ms);
    }
}

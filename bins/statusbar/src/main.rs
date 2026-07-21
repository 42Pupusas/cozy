//! `statusbar` — a wlroots-layer-shell status bar, built on the same
//! `io_uring` Wayland connection / software canvas as the `terminal` binary.
//!
//! Two threads:
//!   * **render/event thread** (`main`) — owns the `BarWindow` (the Wayland
//!     connection, pointer state, present loop). It never calls into
//!     `statusbar::modules` itself and never blocks on process I/O -- it
//!     just paints whatever [`Snapshot`] the worker last sent and forwards
//!     raw scroll deltas to the worker to act on.
//!   * **worker thread** (`worker::run`) — owns all the actual module
//!     computation (every one of which either shells out to a CLI --
//!     `pactl`, `iw`, `bluetoothctl`, `brightnessctl`'s sysfs equivalent --
//!     or, for `cpu_usage`, deliberately blocks ~120ms sampling
//!     `/proc/stat` twice) on its own wall-clock timers, and separately
//!     drains queued scroll deltas and applies volume/brightness nudges.
//!
//! This split exists because the two were previously fused on one thread:
//! every module poll *and* every scroll-triggered `pactl get-*` read-back
//! ran inline in the same loop that calls `BarWindow::tick`/`wait`, which
//! pumps the Wayland socket (pointer motion, scroll axis events, frame
//! callbacks, resize). A blocking subprocess round-trip or a 120ms
//! `thread::sleep` on that thread stalls Wayland event processing for its
//! whole duration -- exactly the "feels sluggish next to waybar" symptom
//! (waybar's own modules are async/non-blocking against D-Bus, never
//! forking per poll on its main loop). Moving all of that work to a
//! separate thread, connected only by a snapshot channel + the existing
//! `waker::Waker`, means the render thread's Wayland pump is never at the
//! mercy of a subprocess's exec/IPC latency.

mod worker;

use std::sync::mpsc;

use platform::draw::{Canvas, Color};
use statusbar::{BarWindow, Edge, ModuleKind, TextRenderer};
pub(crate) use worker::Snapshot;

/// Per-frame paint parameters that stay constant for the run's whole
/// lifetime (background color/alpha) or only change on resize/rescale
/// (bar height, output scale) -- grouped out of [`Painter::paint`]'s
/// argument list since they travel together and aren't part of the
/// per-frame `canvas`/`snapshot`/`text`/`segment_rects` identity.
struct PaintStyle {
    bg: Color,
    bg_alpha: u8,
    bar_height: i32,
    scale: i32,
}

/// The bar's per-frame paint pass -- a marker type (no state of its own;
/// every input is a parameter) grouping [`Self::paint`] instead of leaving
/// it as a free function with no owning type.
struct Painter;

impl Painter {
    fn paint(
        canvas: &mut Canvas,
        snapshot: &Snapshot,
        text: &mut TextRenderer,
        style: &PaintStyle,
        segment_rects: &mut Vec<(ModuleKind, i32, i32)>,
    ) {
        let PaintStyle {
            bg,
            bg_alpha,
            bar_height,
            scale,
        } = *style;
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
        for (kind, seg) in snapshot.left_segments() {
            if seg.is_empty() {
                continue;
            }
            let color = kind.color_for(seg);
            let pen_x = text.draw_text_alpha(canvas, seg, left_edge, y, color, bg, bg_alpha);
            let w = pen_x - left_edge;
            segment_rects.push((kind, left_edge - hit_pad, left_edge + w + hit_pad));
            left_edge += w + gap;
        }

        // Center: horizontally centered as one contiguous cluster (joined by
        // `gap`), regardless of what the left/right sections are doing.
        let center = Snapshot::center_segments();
        if !center.is_empty() {
            let center_len = i32::try_from(center.len()).expect("center segment count fits in i32");
            let joined_w: i32 = center.iter().map(|(_, s)| text.measure(s)).sum::<i32>()
                + gap * (center_len - 1).max(0);
            let mut x = (canvas.width() - joined_w) / 2;
            for (kind, seg) in &center {
                if seg.is_empty() {
                    continue;
                }
                let color = kind.color_for(seg);
                let pen_x = text.draw_text_alpha(canvas, seg, x, y, color, bg, bg_alpha);
                let w = pen_x - x;
                segment_rects.push((*kind, x - hit_pad, x + w + hit_pad));
                x += w + gap;
            }
        }

        // Right-to-left layout: draw each segment ending `gap` to the left of
        // the previous one's start.
        let mut right_edge = canvas.width() - pad;
        for (kind, seg) in snapshot.right_segments() {
            if seg.is_empty() {
                continue;
            }
            let color = kind.color_for(seg);
            let w = text.draw_text_right_alpha(canvas, seg, right_edge, y, color, bg, bg_alpha);
            segment_rects.push((kind, right_edge - w - hit_pad, right_edge + hit_pad));
            right_edge -= w + gap;
        }
    }
}

fn main() {
    let cfg = config::Config::load().unwrap_or_else(|e| {
        eprintln!("statusbar: config error: {e} — using defaults");
        config::Config::default()
    });

    let wake = waker::Waker::new().expect("create waker");
    let bar_height = i32::try_from(cfg.bar_height()).unwrap_or(i32::MAX);
    let mut window = match BarWindow::new(Edge::Top, bar_height, cfg.bar_opacity(), wake.clone()) {
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
    // `.clamp(0.0, 255.0)` right before the cast lets clippy prove the
    // truncation/sign-loss can't happen (rather than needing an allow).
    let bg_alpha = (cfg.bar_opacity().clamp(0.0, 1.0) * 255.0)
        .round()
        .clamp(0.0, 255.0) as u8;
    window.set_background(bg);

    // Font size is a point size (like the terminal's own `[font] size`);
    // convert to pixels at 96 DPI, then scale by the output's physical
    // pixel density so it renders crisp on HiDPI, matching
    // `platform::Renderer::with_scale`'s convention.
    // `From<i16> for f32` is lossless (i16 fits entirely inside f32's
    // mantissa) -- narrow the always-small (1, 2, 3...) output scale to
    // `i16` first so this multiply needs no lossy `as` cast at all.
    let scale_f32 = f32::from(i16::try_from(scale).unwrap_or(i16::MAX));
    let px_size = cfg.bar_font_px_size() * scale_f32;
    let mut text = TextRenderer::load_with_fallbacks(
        cfg.bar_font_path(),
        &[cfg.bar_emoji_path(), cfg.bar_icon_path()],
        px_size,
    );

    // Worker thread: owns every module computation and scroll-adjustment
    // (all subprocess/`/proc`/`/sys` I/O -- see the module doc). `snapshot_rx`
    // delivers a full [`Snapshot`] whenever anything changes; `scroll_tx`
    // forwards raw wheel-pixel deltas the worker accumulates and, once a
    // whole detent's built up, turns into a debounced `pactl`/sysfs nudge.
    let (snapshot_tx, snapshot_rx) = mpsc::channel::<Snapshot>();
    let (scroll_tx, scroll_rx) = mpsc::channel::<(ModuleKind, i32)>();
    let (click_tx, click_rx) = mpsc::channel::<ModuleKind>();
    let notification_rules = cfg.bar_notifications().to_vec();
    worker::Worker::spawn_thread(snapshot_tx, scroll_rx, click_rx, wake, notification_rules);

    let bar_height = window.height();
    let paint_style = PaintStyle {
        bg,
        bg_alpha,
        bar_height,
        scale,
    };
    let mut segment_rects: Vec<(ModuleKind, i32, i32)> = Vec::new();
    let mut snapshot = Snapshot::default();

    loop {
        // Drain every snapshot the worker has produced since the last tick
        // (cheap `Vec<String>` moves, not I/O) -- only the most recent one
        // matters for painting.
        let mut changed = false;
        while let Ok(s) = snapshot_rx.try_recv() {
            snapshot = s;
            changed = true;
        }
        if changed {
            window.request_redraw();
        }

        match window.tick(|canvas| {
            Painter::paint(canvas, &snapshot, &mut text, &paint_style, &mut segment_rects);
        }) {
            Ok(true) => {}
            Ok(false) => break,
            Err(e) => {
                eprintln!("statusbar: {e}");
                break;
            }
        }

        // Scroll-to-adjust: forward the raw wheel-pixel delta to the worker
        // thread over a plain non-blocking channel send -- the render
        // thread does no accumulation, debouncing, or subprocess spawning
        // of its own anymore; see the module doc for why that moved.
        let scrolls = window.take_scroll();
        if !scrolls.is_empty() {
            let px = window.pointer_pos().x;
            if let Some((kind, ..)) = segment_rects
                .iter()
                .find(|(_, x0, x1)| px >= *x0 && px < *x1)
            {
                let raw_px: i32 = scrolls.iter().sum();
                let kind = *kind;
                if matches!(
                    kind,
                    ModuleKind::VolOut | ModuleKind::VolIn | ModuleKind::Brightness
                ) {
                    let _ = scroll_tx.send((kind, raw_px));
                }
            }
        }

        // Left-click: open the module's COSMIC settings page (mirrors
        // waybar's own "on-click": "cosmic-popup <page>" bindings -- see
        // `statusbar::ModuleKind::settings_page`). Same hit-test as scroll;
        // the actual `cosmic-settings`/`cosmic-popup` spawn happens on the
        // worker thread so a slow launch never stalls the Wayland pump.
        if window.take_click() {
            let px = window.pointer_pos().x;
            if let Some((kind, ..)) = segment_rects
                .iter()
                .find(|(_, x0, x1)| px >= *x0 && px < *x1)
            {
                let _ = click_tx.send(*kind);
            }
        }

        // Poll at ~10fps when idle -- the worker wakes us directly (via the
        // shared `waker::Waker`) the instant a new snapshot is ready, so
        // this is just a safety-net cadence for resize/output-change
        // events that don't otherwise signal, not the module refresh
        // clock anymore.
        window.wait(100);
    }
}

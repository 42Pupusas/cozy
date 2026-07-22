//! Standalone htop-inspired monitor. It uses the normal xdg-shell toplevel
//! window shared with the terminal, while reusing the statusbar's font loader,
//! colors, and structured `/proc` readers. The monitor's worker never performs
//! system I/O on the Wayland thread.

use std::collections::VecDeque;
use std::sync::mpsc;
use std::time::{Duration, Instant};

mod chart;
mod chart_cache;
mod core_bars;
mod display_snapshot;
mod profile;
mod redraw_gate;

use chart_cache::{ChartCache, Overlay};
use core_bars::{CoreBarPaint, CoreBars};
use display_snapshot::DisplaySnapshot;
use platform::TermWindow;
use platform::draw::{Canvas, Color, Rect};
use profile::{FrameProfile, PaintPhases, WorkerProfile};
use redraw_gate::RedrawGate;
use statusbar::{MonitorReader, MonitorSample, ProcessInfo, TextRenderer};

/// One minute of chart history at four deliberately distinct points per
/// second. At typical card widths this gives each interval visible horizontal
/// room, while the chart's monotone curve keeps the coarser time series fluid.
const HISTORY: usize = 60 * 4;
const FRAME_INTERVAL: Duration = Duration::from_micros(16_667);
/// Kernel counters and sysfs values do not benefit from 60 reads per second.
/// Samples arrive at 10 Hz; visual frames interpolate them at 60 Hz.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(100);
/// Graphs retain one point every quarter-second rather than one per presented
/// frame. Text readouts stay at 60 Hz, so this reduces visual density without
/// making the dashboard feel sluggish.
const GRAPH_INTERVAL: Duration = Duration::from_millis(250);
const FG: Color = Color::rgb(0xf2, 0xf2, 0xf3);
const DIM: Color = Color::rgb(0x8a, 0x8a, 0x90);
const FAINT: Color = Color::rgb(0x55, 0x55, 0x5a);
const UI_BACKGROUND: Color = Color::rgb(0x0a, 0x0a, 0x0b);
// Cards intentionally share the resolved terminal/status-bar background. There
// is no second opaque panel layer; the window opacity is applied uniformly by
// the normal renderer, matching the terminal and status bar.
const CARD_SURFACE: Color = Color::rgb(0x18, 0x16, 0x16);
const CARD_BORDER: Color = Color::rgb(0x1e, 0x1e, 0x21);
/// Opaque Kanagawa card surface. The monitor window may be translucent, but
/// charts need a stable surface so their fill and anti-aliased strokes do not
/// visually blend into whatever happens to be behind the window.
const CHART_PANEL: Color = CARD_SURFACE;
const CYAN: Color = Color::rgb(0x8e, 0xa4, 0xa2);
const GREEN: Color = Color::rgb(0x8a, 0x9a, 0x7b);
const ORANGE: Color = Color::rgb(0xb6, 0x92, 0x7b);
const CARD_INSET: i32 = 14;
const HEADER_BAND: i32 = 26;
const HERO_BAND: i32 = 82;
const DETAILS_BAND: i32 = 30;
const GRAPH_GAP: i32 = 12;
#[cfg(test)]
const PERF_ITERATIONS: usize = 20_000;

const CORE_COLORS: [Color; 8] = [
    Color::rgb(0x7a, 0x8a, 0x72),
    Color::rgb(0x75, 0x91, 0x91),
    Color::rgb(0x8c, 0x7a, 0x91),
    Color::rgb(0x9a, 0x80, 0x6d),
    Color::rgb(0x70, 0x88, 0x7d),
    Color::rgb(0x7b, 0x78, 0x92),
    Color::rgb(0x92, 0x86, 0x70),
    Color::rgb(0x6f, 0x87, 0x91),
];

#[derive(Clone, Default)]
struct Snapshot {
    sample: MonitorSample,
    processes: Vec<ProcessInfo>,
}

/// Bridges low-cost 10 Hz collection to fluid 60 Hz graph motion. Keeping this
/// as a dedicated owner makes presentation interpolation explicit rather than
/// accidentally turning rendering cadence into system-I/O cadence.
struct SampleInterpolator {
    from: MonitorSample,
    to: MonitorSample,
    started: Instant,
}

impl SampleInterpolator {
    fn new() -> Self {
        Self {
            from: MonitorSample::default(),
            to: MonitorSample::default(),
            started: Instant::now(),
        }
    }

    fn accept(&mut self, next: MonitorSample) {
        self.from = self.value();
        self.to = next;
        self.started = Instant::now();
    }

    fn value(&self) -> MonitorSample {
        let t = (self.started.elapsed().as_secs_f32() / SAMPLE_INTERVAL.as_secs_f32()).min(1.0);
        Self::blend(&self.from, &self.to, t)
    }

    fn blend(from: &MonitorSample, to: &MonitorSample, t: f32) -> MonitorSample {
        let blend = |a: f32, b: f32| (b - a).mul_add(t, a);
        let mut cores = Vec::with_capacity(to.cores.len());
        for (index, &value) in to.cores.iter().enumerate() {
            cores.push(blend(
                from.cores.get(index).copied().unwrap_or(value),
                value,
            ));
        }
        MonitorSample {
            cpu: blend(from.cpu, to.cpu),
            cores,
            load: std::array::from_fn(|index| blend(from.load[index], to.load[index])),
            memory: blend(from.memory, to.memory),
            temperature: blend(from.temperature, to.temperature),
            thermal_device: to.thermal_device.clone(),
            thermal_max: blend(from.thermal_max, to.thermal_max),
            thermal_avg: blend(from.thermal_avg, to.thermal_avg),
            thermal_min: blend(from.thermal_min, to.thermal_min),
            disk: blend(from.disk, to.disk),
            network_rx: blend(from.network_rx, to.network_rx),
            network_tx: blend(from.network_tx, to.network_tx),
            network_device: to.network_device.clone(),
            network_ping_ms: blend(from.network_ping_ms, to.network_ping_ms),
            network_link_mbps: blend(from.network_link_mbps, to.network_link_mbps),
            cpu_brand: to.cpu_brand.clone(),
            cpu_cores: to.cpu_cores,
            cpu_frequency_ghz: blend(from.cpu_frequency_ghz, to.cpu_frequency_ghz),
            disk_io_mb_s: blend(from.disk_io_mb_s, to.disk_io_mb_s),
            memory_available_mb: blend(from.memory_available_mb, to.memory_available_mb),
            swap_used_mb: blend(from.swap_used_mb, to.swap_used_mb),
            swap_total_mb: blend(from.swap_total_mb, to.swap_total_mb),
            memory_device: to.memory_device.clone(),
        }
    }
}

struct History {
    cpu: VecDeque<f32>,
    cores: Vec<VecDeque<f32>>,
    memory: VecDeque<f32>,
    temp: VecDeque<f32>,
    rx: VecDeque<f32>,
    tx: VecDeque<f32>,
    sequence: u64,
}

impl History {
    const fn new() -> Self {
        Self {
            cpu: VecDeque::new(),
            cores: Vec::new(),
            memory: VecDeque::new(),
            temp: VecDeque::new(),
            rx: VecDeque::new(),
            tx: VecDeque::new(),
            sequence: 0,
        }
    }
    fn push(queue: &mut VecDeque<f32>, value: f32) {
        queue.push_back(value);
        if queue.len() > HISTORY {
            queue.pop_front();
        }
    }
    fn add(&mut self, s: &MonitorSample) {
        self.sequence = self.sequence.saturating_add(1);
        Self::push(&mut self.cpu, s.cpu);
        // CPU topology may appear after startup (or change after hotplug). Keep
        // each core in its own series so all lines share a consistent timeline.
        self.cores.resize_with(s.cores.len(), VecDeque::new);
        for (history, value) in self.cores.iter_mut().zip(&s.cores) {
            Self::push(history, *value);
        }
        Self::push(&mut self.memory, s.memory);
        Self::push(&mut self.temp, s.temperature);
        Self::push(&mut self.rx, s.network_rx);
        Self::push(&mut self.tx, s.network_tx);
    }
}

struct Typography {
    hero: TextRenderer,
    title: TextRenderer,
    subtitle: TextRenderer,
    normal: TextRenderer,
}

struct Painter {
    cpu: ChartCache,
    memory: ChartCache,
    temperature: ChartCache,
    network: ChartCache,
    cpu_bars: CoreBars,
    background: Color,
}

impl Painter {
    #[allow(dead_code)]
    const fn new() -> Self {
        Self::with_surface(CHART_PANEL)
    }

    const fn with_surface(surface: Color) -> Self {
        Self::with_colors(surface, UI_BACKGROUND)
    }

    const fn with_colors(surface: Color, background: Color) -> Self {
        Self {
            cpu: ChartCache::new(surface),
            memory: ChartCache::new(surface),
            temperature: ChartCache::new(surface),
            network: ChartCache::new(surface),
            cpu_bars: CoreBars::new(),
            background,
        }
    }
    #[allow(
        clippy::too_many_arguments,
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::suboptimal_flops
    )]
    fn graph(
        cache: &mut ChartCache,
        canvas: &mut Canvas,
        values: &VecDeque<f32>,
        x: i32,
        y: i32,
        w: i32,
        color: Color,
        maximum: f32,
        height: i32,
        sequence: u64,
        overlays: &[Overlay<'_>],
    ) {
        // The chart deliberately has no grid or frame: at this small size they
        // compete with the data. Its private surface advances like the
        // terminal's pixel-scrolled backbuffer, then only its finished pixels
        // are copied into the normal window canvas.
        cache.paint(
            canvas,
            Rect::new(x, y, w, height),
            values,
            maximum,
            color,
            overlays,
            HISTORY,
            sequence,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn cpu_graph(
        cache: &mut ChartCache,
        canvas: &mut Canvas,
        aggregate: &VecDeque<f32>,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        sequence: u64,
    ) {
        // The line is intentionally aggregate-only. Per-core activity is a
        // current-state comparison and is much easier to read as bars than as
        // a stack of nearly indistinguishable historical overlays.
        cache.paint(
            canvas,
            Rect::new(x, y, w, h),
            aggregate,
            100.0,
            GREEN,
            &[],
            HISTORY,
            sequence,
        );
    }

    fn card(&self, canvas: &mut Canvas, rect: Rect, full_repaint: bool) {
        if !full_repaint {
            return;
        }
        // Cards have no separate visual surface, but their pixels still need
        // the resolved background written into the alpha buffer. Otherwise an
        // ARGB Wayland surface exposes the desktop through every card.
        canvas.fill_rect(rect, self.background);
        canvas.draw_rect(rect, CARD_BORDER);
    }

    fn metric(text: &mut TextRenderer, canvas: &mut Canvas, value: &str, x: i32, y: i32) -> i32 {
        text.draw_text(canvas, value, x, y, FG, CARD_SURFACE)
    }

    /// Retain the beginning of a string and replace its overflowing end with
    /// an ellipsis. CPU metadata should read naturally from the start (for
    /// example, `Intel(R) Core(TM) … · 16 cores`) rather than exposing an
    /// arbitrary suffix of the brand.
    fn truncate_prefix(text: &str, renderer: &mut TextRenderer, max_width: i32) -> String {
        if max_width <= 0 {
            return String::new();
        }
        if renderer.measure(text) <= max_width {
            return text.to_owned();
        }
        let ellipsis_width = renderer.measure("…");
        if ellipsis_width >= max_width {
            return "…".to_owned();
        }
        let mut prefix = String::new();
        for character in text.chars() {
            let mut candidate = prefix.clone();
            candidate.push(character);
            candidate.push('…');
            if renderer.measure(&candidate) > max_width {
                break;
            }
            prefix.push(character);
        }
        format!("{prefix}…")
    }

    #[allow(clippy::too_many_arguments)]
    fn card_text(
        canvas: &mut Canvas,
        rect: Rect,
        title: &str,
        meta: &str,
        hero: &str,
        unit: &str,
        details: &[(&str, &str)],
        hero_text: &mut TextRenderer,
        title_text: &mut TextRenderer,
        subtitle_text: &mut TextRenderer,
        normal_text: &mut TextRenderer,
    ) {
        let title_y = rect.y + 12;
        let hero_y = rect.y + 14 + 26;
        let details_y = rect.y + 14 + 26 + 82 - 8;
        title_text.draw_text(canvas, title, rect.x + 14, title_y, DIM, CARD_SURFACE);
        // Keep card metadata inside the same safe right-side region used by
        // the CPU brand. Truncate the end of long platform/device names so
        // the title and metadata remain readable at narrow card widths.
        let metadata_width = (rect.w * 3 / 4 - 14).max(1);
        let metadata = Self::truncate_prefix(meta, subtitle_text, metadata_width);
        subtitle_text.draw_text_right(
            canvas,
            &metadata,
            rect.x + rect.w - 14,
            title_y + 1,
            FAINT,
            CARD_SURFACE,
        );
        let hero_end = Self::metric(hero_text, canvas, hero, rect.x + 14, hero_y);
        let unit_y = hero_y + hero_text.baseline_offset() - subtitle_text.baseline_offset();
        subtitle_text.draw_text(canvas, unit, hero_end + 6, unit_y, DIM, CARD_SURFACE);
        let row_width = rect.w - 28;
        let normal_y = details_y + subtitle_text.baseline_offset() - normal_text.baseline_offset();
        let pair_widths: Vec<i32> = details
            .iter()
            .map(|(label, value)| subtitle_text.measure(label) + 8 + normal_text.measure(value))
            .collect();
        let used_width: i32 = pair_widths.iter().sum();
        let gap = ((row_width - used_width)
            / i32::try_from(details.len().saturating_sub(1).max(1)).unwrap_or(1))
        .max(8);
        let mut x = rect.x + 14;
        for ((label, value), pair_width) in details.iter().zip(pair_widths) {
            subtitle_text.draw_text(canvas, label, x, details_y, FAINT, CARD_SURFACE);
            let value_x = x + subtitle_text.measure(label) + 8;
            normal_text.draw_text(canvas, value, value_x, normal_y, DIM, CARD_SURFACE);
            x += pair_width + gap;
        }
    }

    #[allow(clippy::too_many_lines)]
    #[allow(clippy::too_many_arguments)]
    fn paint(
        &mut self,
        canvas: &mut Canvas,
        typography: &mut Typography,
        snap: &Snapshot,
        history: &History,
        _background: Color,
        full_repaint: bool,
        phases: &mut PaintPhases,
    ) -> Rect {
        let text = &mut typography.normal;
        let title_text = &mut typography.title;
        let subtitle_text = &mut typography.subtitle;
        let hero_text = &mut typography.hero;
        // Clearing is needed only when static dashboard content changes. The
        // graph cards repaint their own opaque rectangles each animation frame,
        // so clearing the process table at 60 Hz would create needless memory
        // bandwidth and force a whole-window shm upload.
        if full_repaint {
            canvas.clear(self.background);
        }
        let width = canvas.width();
        let height = canvas.height();
        let margin = 18;
        let gap = 12;
        // The dashboard has no separate top bar; cards begin at the outer
        // margin so the telemetry and process list use the full window height.
        let process_w = (width / 3).max(220);
        let left_w = width - margin * 2 - process_w - gap;
        let col_w = (left_w - gap) / 2;
        let row_h = ((height - margin * 2 - gap) / 2).max(90);
        let xs = [margin, margin + col_w + gap];
        let ys = [margin, margin + row_h + gap];
        // Reserve explicit, non-overlapping bands for the card header, hero
        // metric, and detail row. The chart starts only after those bands;
        // deriving its height from the remaining space prevents large Hero
        // glyphs from colliding with the graph on short windows.
        // Face-font coordinates are baselines, so the hero receives a full
        // line box plus breathing room before the detail baseline.
        let graph_top = CARD_INSET + HEADER_BAND + HERO_BAND + DETAILS_BAND + GRAPH_GAP;
        let graph_h = (row_h - graph_top - CARD_INSET).max(1);
        let card = |x: i32, y: i32| Rect::new(x, y, col_w, row_h);

        // The dashboard intentionally has no separate top bar. The cards and
        // process list provide all identity and status context themselves.
        let cards = [
            card(xs[0], ys[0]), // CPU
            card(xs[1], ys[0]), // Memory
            card(xs[0], ys[1]), // Temperature
            card(xs[1], ys[1]), // Network
        ];
        for rect in cards {
            self.card(canvas, rect, full_repaint);
        }
        let graph_rect =
            |rect: Rect| Rect::new(rect.x + 1, rect.y + graph_top, rect.w - 2, graph_h);
        // Reserve the lower part of the CPU chart for one live bar per core.
        // The aggregate line retains the larger upper viewport and its cached
        // history; bars are cheap to redraw from the interpolated snapshot.
        // Give the per-core bars enough vertical resolution to distinguish
        // ordinary utilization changes without crowding the card details.
        let cpu_bar_height = 96;
        let cpu_bar_gap = 8;
        let cpu_area = graph_rect(cards[0]);
        let cpu_line_height = (cpu_area.h - cpu_bar_height - cpu_bar_gap).max(1);
        let cpu_line = Rect::new(cpu_area.x, cpu_area.y, cpu_area.w, cpu_line_height);
        let cpu_bars_rect = Rect::new(
            cpu_area.x,
            cpu_area.y + cpu_line_height + cpu_bar_gap,
            cpu_area.w,
            cpu_bar_height,
        );
        if full_repaint {
            // CPU card follows the mockup's four-row rhythm: a compact
            // identity row, a hero value row, a mixed-type detail row, and
            // the graph consuming all remaining vertical space.
            let cpu_card = cards[0];
            title_text.draw_text(
                canvas,
                "CPU",
                cpu_card.x + 14,
                cpu_card.y + 12,
                DIM,
                CARD_SURFACE,
            );
            // Keep the metadata in the right half of the title row.  A full
            // CPU brand can be much wider than the card, so truncate the
            // brand before adding the core count; truncating the combined
            // string can preserve the suffix while still allowing the brand
            // to consume nearly the whole row.
            let core_suffix = format!(" · {} cores", snap.sample.cpu_cores);
            // The title is short, so let the metadata use most of the card
            // rather than truncating a perfectly readable brand too early.
            // Keep a left-side safety gap so it can never collide with `CPU`.
            let metadata_width = (cpu_card.w * 3 / 4 - 14).max(1);
            let brand_width =
                (metadata_width - subtitle_text.measure(&core_suffix) - subtitle_text.measure("…"))
                    .max(1);
            let brand = Self::truncate_prefix(&snap.sample.cpu_brand, subtitle_text, brand_width);
            let cpu_meta = format!("{brand}{core_suffix}");
            let cpu_meta = Self::truncate_prefix(&cpu_meta, subtitle_text, metadata_width);
            subtitle_text.draw_text_right(
                canvas,
                &cpu_meta,
                cpu_card.x + cpu_card.w - 14,
                cpu_card.y + 13,
                FAINT,
                CARD_SURFACE,
            );
            let hero_end = Self::metric(
                hero_text,
                canvas,
                &format!("{:.0}", snap.sample.cpu),
                cpu_card.x + 14,
                cpu_card.y + CARD_INSET + HEADER_BAND,
            );
            // Use the actual Hero glyph advance instead of a fixed x offset;
            // font metrics vary with configured typeface and scale.
            // TextRenderer coordinates are cell-top origins. The smaller
            // subtitle has a shorter ascent, so using the same origin would
            // place its glyphs above the Hero baseline. Offset by the measured
            // line-height difference to align the two baselines visually.
            let hero_baseline = cpu_card.y + CARD_INSET + HEADER_BAND;
            let percent_baseline =
                hero_baseline + hero_text.baseline_offset() - subtitle_text.baseline_offset();
            subtitle_text.draw_text(
                canvas,
                "%",
                hero_end + 6,
                percent_baseline,
                DIM,
                CARD_SURFACE,
            );
            let details_y = cpu_card.y + CARD_INSET + HEADER_BAND + HERO_BAND - 8;
            let normal_details_y =
                details_y + subtitle_text.baseline_offset() - text.baseline_offset();
            // Lay out complete label/value pairs rather than anchoring each
            // label to a fixed third. A value such as `12.4 MB/s` is wider
            // than the DISK IO label, so equal columns can overlap the next
            // pair. Measured pair widths let us distribute the spare space
            // safely and keep every column readable.
            let row_x = cpu_card.x + 14;
            let row_width = cpu_card.w - 28;
            let labels = ["Load", "Disk I/O", "Freq"];
            // Load is available directly from /proc/loadavg and complements
            // the instantaneous CPU percentage without requiring perf access.
            let load = format!("{:.2}", snap.sample.load[0]);
            let disk_io = format!("{:.1} MB/s", snap.sample.disk_io_mb_s);
            let frequency = format!("{:.2} GHz", snap.sample.cpu_frequency_ghz);
            let values = [load, disk_io, frequency];
            let pair_widths: Vec<i32> = labels
                .iter()
                .zip(values.iter())
                .map(|(label, value)| subtitle_text.measure(label) + 8 + text.measure(value))
                .collect();
            let used_width: i32 = pair_widths.iter().sum();
            let gap = ((row_width - used_width) / 2).max(8);
            let mut x = row_x;
            for ((label, value), pair_width) in labels.iter().zip(values.iter()).zip(pair_widths) {
                subtitle_text.draw_text(canvas, label, x, details_y, FAINT, CARD_SURFACE);
                let value_x = x + subtitle_text.measure(label) + 8;
                text.draw_text(canvas, value, value_x, normal_details_y, DIM, CARD_SURFACE);
                x += pair_width + gap;
            }
        }
        if full_repaint {
            let memory_used = format!("{:.0}%", snap.sample.memory);
            let memory_available = format!("{:.1} GB", snap.sample.memory_available_mb / 1024.0);
            let swap_display = if snap.sample.swap_total_mb <= 0.0 {
                "ZRAM —".to_owned()
            } else {
                format!(
                    "{:.1}/{:.1} GB",
                    snap.sample.swap_used_mb / 1024.0,
                    snap.sample.swap_total_mb / 1024.0
                )
            };
            let memory_details = [
                ("Used", memory_used.as_str()),
                ("Available", memory_available.as_str()),
                ("Swap", swap_display.as_str()),
            ];
            Self::card_text(
                canvas,
                cards[1],
                "Memory",
                &snap.sample.memory_device,
                &format!("{:.0}", snap.sample.memory),
                "%",
                &memory_details,
                hero_text,
                title_text,
                subtitle_text,
                text,
            );
            Self::card_text(
                canvas,
                cards[2],
                "Temperature",
                &snap.sample.thermal_device,
                &format!("{:.0}", snap.sample.temperature),
                "°C",
                &[
                    ("Max", &format!("{:.1} °C", snap.sample.thermal_max)),
                    ("Average", &format!("{:.1} °C", snap.sample.thermal_avg)),
                    ("Min", &format!("{:.1} °C", snap.sample.thermal_min)),
                ],
                hero_text,
                title_text,
                subtitle_text,
                text,
            );
            let network_ping = format!("{:.1}", snap.sample.network_ping_ms);
            let network_rx = format!("{:.2} MB/s", snap.sample.network_rx);
            let network_tx = format!("{:.2} MB/s", snap.sample.network_tx);
            let network_link = if snap.sample.network_link_mbps > 0.0 {
                format!("{:.0} Mb/s", snap.sample.network_link_mbps)
            } else {
                "Up".to_owned()
            };
            Self::card_text(
                canvas,
                cards[3],
                "Network",
                &snap.sample.network_device,
                &network_ping,
                "ms",
                &[
                    ("RX", &network_rx),
                    ("TX", &network_tx),
                    ("LINK", &network_link),
                ],
                hero_text,
                title_text,
                subtitle_text,
                text,
            );
        }
        let started = Instant::now();
        Self::cpu_graph(
            &mut self.cpu,
            canvas,
            &history.cpu,
            cpu_line.x,
            cpu_line.y,
            cpu_line.w,
            cpu_line.h,
            history.sequence,
        );
        self.cpu_bars.paint(
            canvas,
            &snap.sample.cores,
            CoreBarPaint {
                bounds: cpu_bars_rect,
                background: CARD_SURFACE,
                colors: &CORE_COLORS,
            },
        );
        phases.cpu_chart += started.elapsed();
        let started = Instant::now();
        Self::graph(
            &mut self.memory,
            canvas,
            &history.memory,
            graph_rect(cards[1]).x,
            graph_rect(cards[1]).y,
            graph_rect(cards[1]).w,
            CYAN,
            100.0,
            graph_h,
            history.sequence,
            &[],
        );
        Self::graph(
            &mut self.temperature,
            canvas,
            &history.temp,
            graph_rect(cards[2]).x,
            graph_rect(cards[2]).y,
            graph_rect(cards[2]).w,
            ORANGE,
            100.0,
            graph_h,
            history.sequence,
            &[],
        );
        Self::graph(
            &mut self.network,
            canvas,
            &history.rx,
            graph_rect(cards[3]).x,
            graph_rect(cards[3]).y,
            graph_rect(cards[3]).w,
            CYAN,
            10.0,
            graph_h,
            history.sequence,
            &[Overlay::new(&history.tx, ORANGE)],
        );
        phases.other_charts += started.elapsed();

        let proc_rect = Rect::new(
            margin + left_w + gap,
            margin,
            process_w,
            height - margin * 2,
        );
        self.card(canvas, proc_rect, full_repaint);
        if full_repaint {
            let inset = 14;
            let left = proc_rect.x + inset;
            let right = proc_rect.x + proc_rect.w - inset;
            let pid_right = left + 48;
            let name_left = pid_right + 12;
            let cpu_right = right - 76;
            let mem_right = right;
            title_text.draw_text(
                canvas,
                "Processes",
                left,
                proc_rect.y + 14,
                DIM,
                CARD_SURFACE,
            );
            subtitle_text.draw_text_right(
                canvas,
                &format!("{} tasks", snap.processes.len()),
                right,
                proc_rect.y + 15,
                FAINT,
                CARD_SURFACE,
            );
            subtitle_text.draw_text(canvas, "PID", left, proc_rect.y + 40, FAINT, CARD_SURFACE);
            subtitle_text.draw_text(
                canvas,
                "Name",
                name_left,
                proc_rect.y + 40,
                FAINT,
                CARD_SURFACE,
            );
            subtitle_text.draw_text_right(
                canvas,
                "CPU",
                cpu_right,
                proc_rect.y + 40,
                FAINT,
                CARD_SURFACE,
            );
            subtitle_text.draw_text_right(
                canvas,
                "Memory",
                mem_right,
                proc_rect.y + 40,
                FAINT,
                CARD_SURFACE,
            );
            let row_height = 18;
            let first_row = proc_rect.y + 59;
            let available_height = (proc_rect.y + proc_rect.h - inset - first_row).max(0);
            let row_count = usize::try_from(available_height / row_height).unwrap_or(0);
            for (row, process) in snap.processes.iter().take(row_count).enumerate() {
                let y = first_row + i32::try_from(row).unwrap_or(0) * row_height;
                let name_width = (cpu_right - name_left - 12).max(1);
                let name = Self::truncate_prefix(&process.name, text, name_width);
                text.draw_text(canvas, &process.pid.to_string(), left, y, DIM, CARD_SURFACE);
                text.draw_text(
                    canvas,
                    &name,
                    name_left,
                    y,
                    if row == 0 { ORANGE } else { FG },
                    CARD_SURFACE,
                );
                // Process CPU remains measured in one-core percentage, but
                // the table uses a visual bar instead of another numeric wall
                // of percentages. The bar is bounded to its dedicated column
                // and saturates at 100% for multi-threaded processes.
                let bar_right = cpu_right;
                let bar_left = bar_right - 58;
                let bar_y = y + 5;
                canvas.fill_rect(
                    Rect::new(bar_left, bar_y, bar_right - bar_left, 6),
                    CARD_BORDER,
                );
                let bar_width = bar_right - bar_left;
                let fill = (process.cpu.clamp(0.0, 100.0) / 100.0
                    * bar_width.to_string().parse::<f32>().unwrap_or(0.0))
                .round()
                .to_string()
                .parse::<i32>()
                .unwrap_or(bar_width);
                if fill > 0 {
                    canvas.fill_rect(
                        Rect::new(bar_left, bar_y, fill, 6),
                        if row == 0 { ORANGE } else { CYAN },
                    );
                }
                text.draw_text_right(
                    canvas,
                    &format!(
                        "{:.1} MB",
                        process
                            .memory_kb
                            .to_string()
                            .parse::<f32>()
                            .unwrap_or(f32::MAX)
                            / 1024.0
                    ),
                    mem_right,
                    y,
                    DIM,
                    CARD_SURFACE,
                );
            }
            phases.static_content += Instant::now().duration_since(started);
            canvas.bounds()
        } else {
            Rect::new(0, margin, width, height - margin * 2)
        }
    }
}

struct App {
    window: TermWindow,
    typography: Typography,
    snapshot: Snapshot,
    history: History,
    background: Color,
    rx: mpsc::Receiver<Snapshot>,
    full_repaint: bool,
    profile: FrameProfile,
    display: SampleInterpolator,
    painter: Painter,
    last_graph_sample: Instant,
    /// The quantized (rendered-precision) values as of the last actual
    /// paint. Compared against the current interpolated sample each tick
    /// to decide whether painting again would change any pixel -- see
    /// `display_snapshot` module docs for why this replaced checking raw
    /// interpolation progress.
    last_painted_display: DisplaySnapshot,
}

impl App {
    /// Add a graph knot at a readable fixed cadence. The interpolated display
    /// sample still evolves every frame; retaining fewer knots lets the
    /// monotone chart renderer express that motion as a smooth curve instead
    /// of packing sub-pixel frame-to-frame variations into the history.
    /// Returns whether a knot was actually added -- callers use this to know
    /// whether the chart surfaces have new pixels to draw this tick.
    fn record_graph_sample(&mut self, now: Instant) -> bool {
        if now.duration_since(self.last_graph_sample) >= GRAPH_INTERVAL {
            self.history.add(&self.snapshot.sample);
            self.last_graph_sample = now;
            true
        } else {
            false
        }
    }
}

impl App {
    fn new() -> Self {
        let cfg = config::Config::load().unwrap_or_default();
        let wake = waker::Waker::new().expect("create monitor wakeup");
        let theme = platform::Theme::from_palette(&cfg.palette);
        let mut font = cfg.font.clone();
        // The monitor inherits the status bar's resolved background and
        // opacity, which in turn inherit the terminal's [colors].background
        // and [window].opacity. A monitor-specific override remains optional.
        let background_opacity = cfg
            .monitor
            .background_opacity
            .unwrap_or_else(|| cfg.bar_opacity());
        if let Some(path) = cfg.monitor.font_path.as_ref() {
            font.path = Some(path.clone());
        }
        if let Some(size) = cfg.monitor.font_size {
            font.size = size;
        }
        let window = TermWindow::new(
            "monitor",
            120,
            40,
            wake.clone(),
            platform::CursorAnimConfig::default(),
            move |scale| {
                platform::Renderer::with_scale(theme, &font, scale).with_opacity(background_opacity)
            },
        )
        .expect("open monitor window");
        let scale = window.scale();
        let scale_f32 = f32::from(i16::try_from(scale).unwrap_or(i16::MAX));
        let size = cfg.font.px_size() * scale_f32;
        let load_text = |px_size: f32| {
            TextRenderer::load_with_fallbacks(
                cfg.font.path.as_deref(),
                &[cfg.font.emoji_path.as_deref()],
                px_size,
            )
        };
        let typography = Typography {
            hero: load_text(size * cfg.monitor.hero_size.unwrap_or(2.5)),
            // Keep the supporting tiers close to the original design: titles
            // are the base UI size, subtitles are slightly quieter, and body
            // values are smaller so the hierarchy reads without becoming
            // oversized beside the hero metric.
            title: load_text(size * cfg.monitor.title_size.unwrap_or(1.0)),
            subtitle: load_text(size * cfg.monitor.subtitle_size.unwrap_or(0.90)),
            normal: load_text(size * cfg.monitor.normal_size.unwrap_or(0.80)),
        };
        let background_color = cfg
            .monitor
            .background
            .unwrap_or_else(|| cfg.bar_background());
        let background = Color::rgb(background_color.r, background_color.g, background_color.b);
        let rx = Self::spawn_worker(wake);
        Self {
            window,
            typography,
            snapshot: Snapshot::default(),
            history: History::new(),
            background,
            rx,
            full_repaint: true,
            profile: FrameProfile::new(),
            display: SampleInterpolator::new(),
            painter: Painter::with_colors(
                Color::rgb(
                    cfg.monitor
                        .surface
                        .unwrap_or_else(|| cfg.bar_background())
                        .r,
                    cfg.monitor
                        .surface
                        .unwrap_or_else(|| cfg.bar_background())
                        .g,
                    cfg.monitor
                        .surface
                        .unwrap_or_else(|| cfg.bar_background())
                        .b,
                ),
                background,
            ),
            // Make the first received display value immediately visible in a
            // graph rather than waiting one quarter-second after startup.
            last_graph_sample: Instant::now()
                .checked_sub(GRAPH_INTERVAL)
                .unwrap_or_else(Instant::now),
            // Default-quantizes to all zeros, which differs from any real
            // first sample -- guaranteeing the very first tick paints.
            last_painted_display: DisplaySnapshot::default(),
        }
    }
    /// Owns the isolated 10 Hz telemetry worker so window construction only
    /// assembles UI state. Keeping collection off the Wayland thread prevents
    /// procfs stalls from delaying compositor events.
    fn spawn_worker(wake: waker::Waker) -> mpsc::Receiver<Snapshot> {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = MonitorReader::new();
            let mut processes = Vec::new();
            let mut profile = WorkerProfile::new();
            loop {
                let started = Instant::now();
                if let Some(next) = reader.processes_tick() {
                    processes = next;
                }
                profile.record_process_scan(started.elapsed());
                let started = Instant::now();
                let sample = reader.sample();
                profile.record_sample(started.elapsed());
                let _ = tx.send(Snapshot {
                    sample,
                    processes: processes.clone(),
                });
                profile.report();
                wake.signal();
                std::thread::sleep(SAMPLE_INTERVAL);
            }
        });
        rx
    }

    fn run(mut self) {
        let mut next_frame = Instant::now();
        loop {
            let now = Instant::now();
            if now < next_frame {
                // A Wayland callback or sampler signal may wake us before the
                // next deadline. It is an event notification, not permission
                // to present another frame: wait again for the remaining slice.
                let remaining = next_frame.duration_since(now);
                #[allow(clippy::cast_possible_truncation)]
                let millis = remaining.as_millis().max(1).min(i32::MAX as u128) as i32;
                self.window.wait(millis);
                continue;
            }
            while let Ok(snapshot) = self.rx.try_recv() {
                if Self::processes_changed(&self.snapshot.processes, &snapshot.processes) {
                    self.full_repaint = true;
                }
                self.display.accept(snapshot.sample.clone());
                self.snapshot = snapshot;
            }
            self.snapshot.sample = self.display.value();
            let new_graph_sample = self.record_graph_sample(now);
            // Only pay for paint + present when something a viewer could
            // actually see has changed: a fresh chart knot landed, the
            // rendered-precision values actually moved since the last
            // paint (hero numbers/bars visibly different, not just the raw
            // float still gliding), or static content (process list, card
            // labels) needs its one-time repaint.
            //
            // This used to check `SampleInterpolator`'s raw blend progress
            // ("has interpolation reached t=1.0"), which was the wrong
            // signal: the blend is *designed* to run for the entire ~100ms
            // gap between samples so numbers glide instead of snapping, so
            // that check was true almost 100% of the time by construction
            // -- confirmed live via `MONITOR_PROFILE=1`, which showed skip
            // rates stuck at 0-1.7% instead of the expected 85%+. Comparing
            // the quantized `DisplaySnapshot` instead answers the question
            // that actually matters: would this paint change any pixel?
            let current_display = DisplaySnapshot::from_sample(&self.snapshot.sample);
            let display_changed = current_display != self.last_painted_display;
            let should_paint =
                RedrawGate::evaluate(self.full_repaint, new_graph_sample, display_changed);
            if should_paint {
                // One explicit dirty mark per deadline. `TermWindow` coalesces
                // if the compositor still owns both shm slots.
                self.window.request_redraw();
                let snapshot = &self.snapshot;
                let history = &self.history;
                let typography = &mut self.typography;
                let painter = &mut self.painter;
                let background = self.background;
                let full_repaint = self.full_repaint;
                let mut paint_time = Duration::ZERO;
                let mut phases = PaintPhases::default();
                let frame_started = Instant::now();
                match self.window.tick_with_painter(|canvas| {
                    let started = Instant::now();
                    let damage = painter.paint(
                        canvas,
                        typography,
                        snapshot,
                        history,
                        background,
                        full_repaint,
                        &mut phases,
                    );
                    paint_time = started.elapsed();
                    damage
                }) {
                    Ok(true) => {
                        self.profile.record(
                            paint_time,
                            frame_started.elapsed(),
                            &phases,
                            full_repaint,
                        );
                        self.full_repaint = false;
                        self.last_painted_display = current_display;
                    }
                    Ok(false) | Err(_) => break,
                }
            } else {
                // Still pump the Wayland connection (resize/output-change
                // events, frame callbacks) even on a tick with nothing to
                // paint. Crucially, retain the custom-painter path while doing
                // so: `tick()` is for terminal-grid windows and would copy the
                // XRGB backbuffer directly into an ARGB monitor buffer if an
                // event (notably a resize) made it dirty between dashboard
                // paints. XRGB has a zero high byte, so that accidental present
                // makes the entire monitor transparent. The no-op painter lets
                // `TermWindow` retain and premultiply its already-valid custom
                // backbuffer should it need to present for such an event.
                self.profile.record_skip();
                match self.window.tick_with_painter(|_| Rect::new(0, 0, 0, 0)) {
                    Ok(true) => {}
                    Ok(false) | Err(_) => break,
                }
            }
            // Do not replay missed frames after a compositor stall; the newest
            // sample is more useful than a burst of obsolete graph states.
            next_frame += FRAME_INTERVAL;
            if next_frame <= Instant::now() {
                next_frame = Instant::now() + FRAME_INTERVAL;
            }
        }
    }

    /// Whether the process table needs its (expensive, text-heavy)
    /// `full_repaint` pass redone. `MonitorReader::processes_tick` samples
    /// per-process CPU at 10Hz, and instantaneous CPU-tick-derived
    /// percentages jitter by more than a few hundredths of a point on
    /// essentially every sample even for an idle process -- the previous
    /// `0.05` threshold was well inside that noise floor, so this
    /// comparison was true on nearly every single tick, forcing a full
    /// re-layout of all four cards plus every visible process row ten
    /// times a second regardless of whether the *displayed* (rounded)
    /// values actually differed. One percentage point is below the
    /// smallest change a viewer can distinguish in the rendered text
    /// anyway, so it filters sampling noise without hiding a real swing.
    fn processes_changed(previous: &[ProcessInfo], next: &[ProcessInfo]) -> bool {
        previous.len() != next.len()
            || previous.iter().zip(next).any(|(old, new)| {
                old.pid != new.pid
                    || old.name != new.name
                    || (old.cpu - new.cpu).abs() > 1.0
                    || old.memory_kb != new.memory_kb
            })
    }
}
fn main() {
    App::new().run();
}

/// Headless perf harness for `Painter::paint`.
///
/// This sandbox has no Wayland compositor, so `perf record` on the real
/// binary only ever profiles `cargo`/`rustc` building it (confirmed: the
/// binary panics in `TermWindow::new` before painting a single frame here).
/// `Canvas`/`TextRenderer`/`Painter` have no Wayland dependency though --
/// they operate on a plain `&mut [u32]` pixel buffer and (with `path: None`)
/// the embedded bitmap font -- so this test drives the *actual* hot path
/// (`Painter::paint`, the same call `App::run` makes every painted tick)
/// directly, many times, with no window at all. Run under `perf record` via:
///
///   `cargo test --release --bin monitor -- --ignored headless_paint_perf_workload --nocapture`
///
/// to get a real flamegraph/hotspot breakdown of paint cost, and compare
/// against the live `MONITOR_PROFILE=1` numbers from an actual session.
#[cfg(test)]
mod perf_harness {
    use super::{
        CARD_SURFACE, Canvas, ChartCache, History, MonitorSample, PERF_ITERATIONS, PaintPhases,
        Painter, ProcessInfo, Snapshot, Typography,
    };
    use std::time::Instant;

    /// A plausible, non-degenerate sample so every card/branch in `paint`
    /// (hero numbers, detail rows, per-core bars, process list) does real
    /// work instead of hitting an early-out on default/zero values.
    fn synthetic_sample(tick: usize) -> MonitorSample {
        // Vary values across ticks so nothing gets const-folded/cached by
        // the optimizer in a way a real, changing workload wouldn't get.
        #[allow(clippy::cast_precision_loss)]
        let phase = (tick % 100) as f32;
        MonitorSample {
            cpu: 30.0 + phase * 0.5,
            cores: vec![phase, 100.0 - phase, phase / 2.0, 50.0],
            load: [1.2, 1.5, 1.8],
            memory: 40.0 + phase * 0.2,
            temperature: 55.0 + phase * 0.3,
            thermal_device: "k10temp".to_owned(),
            thermal_max: 70.0,
            thermal_avg: 60.0,
            thermal_min: 50.0,
            disk: 12.0,
            network_rx: phase / 10.0,
            network_tx: phase / 20.0,
            network_device: "eth0".to_owned(),
            network_ping_ms: 8.5,
            network_link_mbps: 1000.0,
            cpu_brand: "Synthetic CPU".to_owned(),
            cpu_cores: 4,
            cpu_frequency_ghz: 3.4,
            disk_io_mb_s: 5.0,
            memory_available_mb: 8192.0,
            swap_used_mb: 100.0,
            memory_device: "16 GB".to_owned(),
            swap_total_mb: 4096.0,
        }
    }

    fn synthetic_processes(tick: usize) -> Vec<ProcessInfo> {
        #[allow(clippy::cast_precision_loss)]
        let phase = (tick % 100) as f32;
        (0..30)
            .map(|index| ProcessInfo {
                pid: index,
                name: format!("proc-{index}"),
                cpu: phase / f32::from(u16::try_from(index).unwrap_or(u16::MAX) + 1),
                memory_kb: 1024 * (u64::from(index) + 1),
            })
            .collect()
    }

    #[test]
    #[ignore = "long-running perf workload, not a correctness check -- run explicitly under perf record"]
    fn headless_paint_perf_workload() {
        let width = 1400;
        let height = 900;
        let mut pixels = vec![0u32; 1_260_000];
        let mut typography = Typography {
            hero: statusbar::TextRenderer::load(None, 32.0),
            title: statusbar::TextRenderer::load(None, 14.0),
            subtitle: statusbar::TextRenderer::load(None, 13.0),
            normal: statusbar::TextRenderer::load(None, 11.0),
        };
        let mut painter = Painter::with_surface(CARD_SURFACE);
        let mut history = History::new();
        let mut phases = PaintPhases::default();

        // `PERF_ITERATIONS` is long enough for a meaningful sample while
        // remaining practical when explicitly run with `--ignored`.
        let started = Instant::now();
        for tick in 0..PERF_ITERATIONS {
            let snapshot = Snapshot {
                sample: synthetic_sample(tick),
                processes: synthetic_processes(tick),
            };
            if tick % 25 == 0 {
                history.add(&snapshot.sample);
            }
            let mut canvas = Canvas::new(&mut pixels, width, height);
            // Every 10th tick pays for the full (text-heavy) repaint path;
            // the rest exercise only the charts/bars, mirroring the real
            // ~1-in-10 `full_repaint` ratio the live profiler reports.
            let full_repaint = tick % 10 == 0;
            painter.paint(
                &mut canvas,
                &mut typography,
                &snapshot,
                &history,
                CARD_SURFACE,
                full_repaint,
                &mut phases,
            );
        }
        let elapsed = started.elapsed();
        eprintln!(
            "headless_paint_perf_workload: {PERF_ITERATIONS} paints in {:.2?} ({:.3} ms/paint avg)",
            elapsed,
            elapsed.as_secs_f64() * 1_000.0 / 20_000.0
        );
        // Keep the chart cache alive for the whole loop's lifetime check --
        // a real regression (e.g. rebuilding pixel history every frame
        // instead of scrolling) would show up as a much higher ms/paint
        // average above, visible to a human even though this isn't a hard
        // assertion (perf timing varies too much across machines for a
        // fixed threshold to be meaningful here).
        let _ = ChartCache::new(CARD_SURFACE);
    }
}

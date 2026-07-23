//! Standalone htop-inspired monitor. It uses the normal xdg-shell toplevel
//! window shared with the terminal, while reusing the statusbar's font loader,
//! colors, and structured `/proc` readers. The monitor's worker never performs
//! system I/O on the Wayland thread.

use std::collections::VecDeque;
use std::sync::mpsc;
use std::time::{Duration, Instant};

// Paint/data leaf modules live in the library face (`lib.rs`) so `divan`
// benches can drive them headlessly with an allocation profiler. The binary
// only adds the modules that need the live window/thread context.
mod profile;
mod snapshot_cli;

use monitor::chart_cache::{ChartCache, Overlay};
use monitor::core_bars::{CoreBarPaint, CoreBars};
use monitor::damage::DamageList;
use monitor::dashboard_report::DashboardReport;
use monitor::display_snapshot::DisplaySnapshot;
use monitor::redraw_gate::RedrawGate;
use monitor::sample_interpolator::SampleInterpolator;
use platform::TermWindow;
use platform::draw::{Canvas, Color, Rect};
use profile::{FrameProfile, PaintPhases, WorkerProfile};
use statusbar::TextRenderer;
use sysmon::{MonitorReader, MonitorSample, ProcessInfo};

/// One minute of chart history at four deliberately distinct points per
/// second. At typical card widths this gives each interval visible horizontal
/// room, while the chart's monotone curve keeps the coarser time series fluid.
const HISTORY: usize = 60 * 4;
/// Tick cadence: how often the loop wakes to *check* whether a frame is worth
/// presenting. This is deliberately the full display refresh (60 Hz) rather
/// than a lower presentation cap, because the redraw gate decouples the two:
/// a present only happens when the quantized `DisplaySnapshot` actually
/// changes (a hero number crosses an integer, a bar moves a whole percent),
/// so the number of *presents* is bounded by how fast the displayed values
/// cross rendering boundaries -- NOT by this tick rate. Raising the tick rate
/// from 20 to 60 Hz therefore does not multiply present cost; it only lets the
/// eased glide land its integer steps on finer ticks (smoother motion) at the
/// price of more idle gate checks. Those checks were made allocation-cheap on
/// purpose (`DisplaySnapshot::from_sample` is ~110ns/1-alloc, proven by the
/// `idle_tick` bench) precisely so 60 Hz ticking stays affordable. ~16.67ms =
/// one 60 Hz frame.
const FRAME_INTERVAL: Duration = Duration::from_micros(16_667);
/// Kernel counters and sysfs values do not benefit from many reads per second:
/// a system dashboard's numbers are meaningful at ~1 Hz. Sampling once a second
/// keeps the worker thread near idle.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
/// Graphs retain one point every quarter-second rather than one per presented
/// frame. Text readouts stay at 60 Hz, so this reduces visual density without
/// making the dashboard feel sluggish.
const GRAPH_INTERVAL: Duration = Duration::from_millis(250);
/// The vertical rhythm shared by every metric card: a compact identity row,
/// a hero value row, a mixed-type detail row, then the graph consuming
/// whatever height remains. Bundling the five band sizes here (rather than
/// leaving them as five bare `const i32`s added up ad-hoc at each call site)
/// means every y-coordinate in `card`/`metric`/`card_text` is derived from
/// the *same* arithmetic, so a future tweak to one band can't silently drift
/// out of sync between the CPU card's hand-laid-out path and the generic
/// `card_text` path other cards use.
struct CardBands {
    inset: i32,
    header: i32,
    hero: i32,
    details: i32,
    graph_gap: i32,
}

impl CardBands {
    const fn new() -> Self {
        Self {
            inset: 14,
            header: 26,
            hero: 82,
            details: 30,
            graph_gap: 12,
        }
    }

    /// Baseline y for the hero metric row.
    const fn hero_baseline(&self, card_top: i32) -> i32 {
        card_top + self.inset + self.header
    }

    /// Baseline y for the detail row (label/value pairs beneath the hero).
    const fn details_y(&self, card_top: i32) -> i32 {
        self.hero_baseline(card_top) + self.hero - 8
    }

    /// Y offset (relative to the card's top) where the chart viewport begins,
    /// i.e. the total height consumed by the header/hero/details bands plus
    /// one gap before the graph.
    const fn graph_top(&self) -> i32 {
        self.inset + self.header + self.hero + self.details + self.graph_gap
    }

    /// Chart viewport height given the card's total row height, reserving
    /// the same `inset` at the bottom that's reserved at the top.
    fn graph_h(&self, row_h: i32) -> i32 {
        (row_h - self.graph_top() - self.inset).max(1)
    }
}

#[cfg(test)]
const PERF_ITERATIONS: usize = 20_000;

// A fixed 8-color rotation for per-core CPU bars. Unlike `MonitorPalette`'s
// fields below, this has no sensible single `[monitor]` key to hang off of
// (it's a *cycle*, not a semantic "this metric is this color" mapping, and
// real CPUs commonly have far more than 8 cores) -- so it stays a plain
// internal decoration constant rather than growing eight more TOML keys
// nobody would tune individually.
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

/// Every color the dashboard paints with, resolved once from [`config::Config`]
/// at startup instead of scattered as bare `const Color`s throughout this
/// file.
///
/// This replaced `FG`/`DIM`/`FAINT`/`UI_BACKGROUND`/`CARD_SURFACE`/
/// `CARD_BORDER`/`CHART_PANEL`/`CYAN`/`GREEN`/`ORANGE` -- ten module consts
/// that were completely disconnected from `config::MonitorConfig`'s
/// `cpu`/`memory`/`temperature`/`network` override fields (which existed in
/// the config format but were never actually read anywhere in this binary --
/// setting them in a TOML file silently did nothing). Worse, `card()` filled
/// each card rect with the *resolved* `self.background`, while every text
/// draw call anti-aliased against the *hardcoded* `CARD_SURFACE` literal as
/// its blend background -- so as soon as `[monitor]`/`[bar]`/`[colors]`
/// diverged from the built-in defaults, every glyph edge in every card would
/// visibly fringe against the real (different) background color underneath
/// it. Centralizing the resolution here means there is exactly one place
/// that can drift, and it's covered by `config::tests`.
struct MonitorPalette {
    /// Window background (canvas clear color).
    background: Color,
    /// Card / chart-panel fill -- also the anti-aliasing blend background
    /// for every piece of text drawn over a card (see the struct doc for
    /// the bug this fixes).
    surface: Color,
    /// Card border / bar-track color.
    border: Color,
    /// Hero/metric text.
    foreground: Color,
    /// Secondary text (card titles, hero units, process names/PIDs,
    /// non-highlighted process CPU bars).
    dim: Color,
    /// Tertiary text (detail-row labels, table headers).
    faint: Color,
    /// CPU card accent (hero line, core bar 0's row-0 process highlight).
    cpu: Color,
    /// Memory card accent.
    memory: Color,
    /// Temperature card accent.
    temperature: Color,
    /// Network card accent (download line; upload uses `temperature` as its
    /// overlay color, matching the original hardcoded `ORANGE`).
    network: Color,
}

impl MonitorPalette {
    fn from_config(cfg: &config::Config) -> Self {
        Self {
            background: cfg.monitor_background().into(),
            surface: cfg.monitor_surface().into(),
            border: cfg.monitor_border().into(),
            foreground: cfg.monitor_foreground().into(),
            dim: cfg.monitor_dim().into(),
            faint: cfg.monitor_faint().into(),
            cpu: cfg.monitor_cpu().into(),
            memory: cfg.monitor_memory().into(),
            temperature: cfg.monitor_temperature().into(),
            network: cfg.monitor_network().into(),
        }
    }
}

#[cfg(test)]
impl MonitorPalette {
    /// The dashboard's original hardcoded defaults, reconstructed here so
    /// `perf_harness` (a headless paint-cost microbenchmark, not a Wayland
    /// session) keeps exercising the exact geometry/contrast it always has
    /// without needing to parse a `config::Config` -- see module docs on
    /// [`Self::from_config`] for why those defaults now live in
    /// `config::Config::monitor_*` instead of here.
    const fn builtin() -> Self {
        Self {
            background: Color::rgb(0x0a, 0x0a, 0x0b),
            surface: Color::rgb(0x18, 0x16, 0x16),
            border: Color::rgb(0x1e, 0x1e, 0x21),
            foreground: Color::rgb(0xf2, 0xf2, 0xf3),
            dim: Color::rgb(0x8a, 0x8a, 0x90),
            faint: Color::rgb(0x55, 0x55, 0x5a),
            cpu: Color::rgb(0x8a, 0x9a, 0x7b),
            memory: Color::rgb(0x8e, 0xa4, 0xa2),
            temperature: Color::rgb(0xb6, 0x92, 0x7b),
            network: Color::rgb(0x8e, 0xa4, 0xa2),
        }
    }
}

#[derive(Clone, Default)]
struct Snapshot {
    sample: MonitorSample,
    processes: Vec<ProcessInfo>,
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

enum TypographyTier {
    Hero(TextRenderer),
    Title(TextRenderer),
    Subtitle(TextRenderer),
    Normal(TextRenderer),
}

struct Painter {
    cpu: ChartCache,
    memory: ChartCache,
    temperature: ChartCache,
    network: ChartCache,
    cpu_bars: CoreBars,
    palette: MonitorPalette,
    bands: CardBands,
}

impl Painter {
    /// Every chart's private surface shares the same card fill
    /// (`palette.surface`) -- see [`MonitorPalette::surface`]'s doc for why
    /// this is also the anti-aliasing blend background every text draw call
    /// in [`Self::paint`] uses.
    const fn with_palette(palette: MonitorPalette) -> Self {
        Self {
            cpu: ChartCache::new(palette.surface),
            memory: ChartCache::new(palette.surface),
            temperature: ChartCache::new(palette.surface),
            network: ChartCache::new(palette.surface),
            cpu_bars: CoreBars::new(),
            palette,
            bands: CardBands::new(),
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
        background_cleared: bool,
    ) -> DamageList {
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
            background_cleared,
        )
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
        color: Color,
        sequence: u64,
        background_cleared: bool,
    ) -> DamageList {
        // The line is intentionally aggregate-only. Per-core activity is a
        // current-state comparison and is much easier to read as bars than as
        // a stack of nearly indistinguishable historical overlays.
        cache.paint(
            canvas,
            Rect::new(x, y, w, h),
            aggregate,
            100.0,
            color,
            &[],
            HISTORY,
            sequence,
            background_cleared,
        )
    }

    fn card(&self, canvas: &mut Canvas, rect: Rect, full_repaint: bool) {
        if !full_repaint {
            return;
        }
        // Cards have no separate visual surface, but their pixels still need
        // the resolved background written into the alpha buffer. Otherwise an
        // ARGB Wayland surface exposes the desktop through every card.
        canvas.fill_rect(rect, self.palette.background);
        canvas.draw_rect(rect, self.palette.border);
    }

    fn metric(&self, text: &mut TextRenderer, canvas: &mut Canvas, value: &str, x: i32, y: i32) -> i32 {
        text.draw_text(canvas, value, x, y, self.palette.foreground, self.palette.surface)
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
        &self,
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
        let surface = self.palette.surface;
        let title_y = rect.y + 12;
        let hero_y = self.bands.hero_baseline(rect.y);
        let details_y = self.bands.details_y(rect.y);
        title_text.draw_text(canvas, title, rect.x + 14, title_y, self.palette.dim, surface);
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
            self.palette.faint,
            surface,
        );
        let hero_end = self.metric(hero_text, canvas, hero, rect.x + 14, hero_y);
        let unit_y = hero_y + hero_text.baseline_offset() - subtitle_text.baseline_offset();
        subtitle_text.draw_text(canvas, unit, hero_end + 6, unit_y, self.palette.dim, surface);
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
            subtitle_text.draw_text(canvas, label, x, details_y, self.palette.faint, surface);
            let value_x = x + subtitle_text.measure(label) + 8;
            normal_text.draw_text(canvas, value, value_x, normal_y, self.palette.dim, surface);
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
    ) -> DamageList {
        let text = &mut typography.normal;
        let title_text = &mut typography.title;
        let subtitle_text = &mut typography.subtitle;
        let hero_text = &mut typography.hero;
        // Accumulates the exact rectangles this frame draws. On a full repaint
        // everything (text, cards, process table) changes, so we short-circuit
        // to a single whole-window rect below; otherwise only the animated
        // chart edges and moved core bars are pushed, which is the whole point
        // -- premultiply/upload then touch ~0.15% of the window instead of all
        // of it.
        let mut damage = DamageList::new();
        // Clearing is needed only when static dashboard content changes. The
        // graph cards repaint their own opaque rectangles each animation frame,
        // so clearing the process table at 60 Hz would create needless memory
        // bandwidth and force a whole-window shm upload.
        if full_repaint {
            canvas.clear(self.palette.background);
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
        let graph_top = self.bands.graph_top();
        let graph_h = self.bands.graph_h(row_h);
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
        // Every number/label drawn below is sourced from `DashboardReport`
        // rather than re-derived with local `format!` calls. That report is
        // the exact same struct the headless `--snapshot` CLI prints, so the
        // GUI and the text report can never drift apart on rounding or units.
        let report = DashboardReport::from_sample(&snap.sample, &snap.processes);
        if full_repaint {
            // CPU card follows the mockup's four-row rhythm: a compact
            // identity row, a hero value row, a mixed-type detail row, and
            // the graph consuming all remaining vertical space.
            let cpu_card = cards[0];
            title_text.draw_text(
                canvas,
                report.cpu.title,
                cpu_card.x + 14,
                cpu_card.y + 12,
                self.palette.dim,
                self.palette.surface,
            );
            // Keep the metadata in the right half of the title row. The
            // report's `meta` is the untruncated brand + core count; a full
            // CPU brand can be much wider than the card, so truncate it here
            // at paint time, where the measured glyph widths live.
            let metadata_width = (cpu_card.w * 3 / 4 - 14).max(1);
            let cpu_meta = Self::truncate_prefix(&report.cpu.meta, subtitle_text, metadata_width);
            subtitle_text.draw_text_right(
                canvas,
                &cpu_meta,
                cpu_card.x + cpu_card.w - 14,
                cpu_card.y + 13,
                self.palette.faint,
                self.palette.surface,
            );
            let hero_end = self.metric(
                hero_text,
                canvas,
                &report.cpu.hero,
                cpu_card.x + 14,
                self.bands.hero_baseline(cpu_card.y),
            );
            // Use the actual Hero glyph advance instead of a fixed x offset;
            // font metrics vary with configured typeface and scale.
            // TextRenderer coordinates are cell-top origins. The smaller
            // subtitle has a shorter ascent, so using the same origin would
            // place its glyphs above the Hero baseline. Offset by the measured
            // line-height difference to align the two baselines visually.
            let hero_baseline = self.bands.hero_baseline(cpu_card.y);
            let percent_baseline =
                hero_baseline + hero_text.baseline_offset() - subtitle_text.baseline_offset();
            subtitle_text.draw_text(
                canvas,
                report.cpu.unit,
                hero_end + 6,
                percent_baseline,
                self.palette.dim,
                self.palette.surface,
            );
            let details_y = self.bands.details_y(cpu_card.y);
            let normal_details_y =
                details_y + subtitle_text.baseline_offset() - text.baseline_offset();
            // Lay out complete label/value pairs rather than anchoring each
            // label to a fixed third. A value such as `12.4 MB/s` is wider
            // than the DISK IO label, so equal columns can overlap the next
            // pair. Measured pair widths let us distribute the spare space
            // safely and keep every column readable.
            let row_x = cpu_card.x + 14;
            let row_width = cpu_card.w - 28;
            let pair_widths: Vec<i32> = report
                .cpu
                .details
                .iter()
                .map(|(label, value)| subtitle_text.measure(label) + 8 + text.measure(value))
                .collect();
            let used_width: i32 = pair_widths.iter().sum();
            let gap = ((row_width - used_width) / 2).max(8);
            let mut x = row_x;
            for ((label, value), pair_width) in report.cpu.details.iter().zip(pair_widths) {
                subtitle_text.draw_text(canvas, label, x, details_y, self.palette.faint, self.palette.surface);
                let value_x = x + subtitle_text.measure(label) + 8;
                text.draw_text(canvas, value, value_x, normal_details_y, self.palette.dim, self.palette.surface);
                x += pair_width + gap;
            }
        }
        if full_repaint {
            let memory_details: Vec<(&str, &str)> = report
                .memory
                .details
                .iter()
                .map(|(label, value)| (*label, value.as_str()))
                .collect();
            self.card_text(
                canvas,
                cards[1],
                report.memory.title,
                &report.memory.meta,
                &report.memory.hero,
                report.memory.unit,
                &memory_details,
                hero_text,
                title_text,
                subtitle_text,
                text,
            );
            let temperature_details: Vec<(&str, &str)> = report
                .temperature
                .details
                .iter()
                .map(|(label, value)| (*label, value.as_str()))
                .collect();
            self.card_text(
                canvas,
                cards[2],
                report.temperature.title,
                &report.temperature.meta,
                &report.temperature.hero,
                report.temperature.unit,
                &temperature_details,
                hero_text,
                title_text,
                subtitle_text,
                text,
            );
            let network_details: Vec<(&str, &str)> = report
                .network
                .details
                .iter()
                .map(|(label, value)| (*label, value.as_str()))
                .collect();
            self.card_text(
                canvas,
                cards[3],
                report.network.title,
                &report.network.meta,
                &report.network.hero,
                report.network.unit,
                &network_details,
                hero_text,
                title_text,
                subtitle_text,
                text,
            );
        }
        let started = Instant::now();
        let cpu_color = self.palette.cpu;
        let cpu_chart_damage = Self::cpu_graph(
            &mut self.cpu,
            canvas,
            &history.cpu,
            cpu_line.x,
            cpu_line.y,
            cpu_line.w,
            cpu_line.h,
            cpu_color,
            history.sequence,
            full_repaint,
        );
        phases.charts_active += u32::from(!cpu_chart_damage.is_empty());
        damage.extend(&cpu_chart_damage);
        damage.extend(&self.cpu_bars.paint(
            canvas,
            &snap.sample.cores,
            CoreBarPaint {
                bounds: cpu_bars_rect,
                background: self.palette.surface,
                colors: &CORE_COLORS,
                background_cleared: full_repaint,
            },
        ));
        phases.cpu_chart += started.elapsed();
        let started = Instant::now();
        let memory_damage = Self::graph(
            &mut self.memory,
            canvas,
            &history.memory,
            graph_rect(cards[1]).x,
            graph_rect(cards[1]).y,
            graph_rect(cards[1]).w,
            self.palette.memory,
            100.0,
            graph_h,
            history.sequence,
            &[],
            full_repaint,
        );
        phases.charts_active += u32::from(!memory_damage.is_empty());
        damage.extend(&memory_damage);
        let temp_damage = Self::graph(
            &mut self.temperature,
            canvas,
            &history.temp,
            graph_rect(cards[2]).x,
            graph_rect(cards[2]).y,
            graph_rect(cards[2]).w,
            self.palette.temperature,
            100.0,
            graph_h,
            history.sequence,
            &[],
            full_repaint,
        );
        phases.charts_active += u32::from(!temp_damage.is_empty());
        damage.extend(&temp_damage);
        let network_damage = Self::graph(
            &mut self.network,
            canvas,
            &history.rx,
            graph_rect(cards[3]).x,
            graph_rect(cards[3]).y,
            graph_rect(cards[3]).w,
            self.palette.network,
            10.0,
            graph_h,
            history.sequence,
            &[Overlay::new(&history.tx, self.palette.temperature)],
            full_repaint,
        );
        phases.charts_active += u32::from(!network_damage.is_empty());
        damage.extend(&network_damage);
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
                self.palette.dim,
                self.palette.surface,
            );
            subtitle_text.draw_text_right(
                canvas,
                &format!("{} tasks", snap.processes.len()),
                right,
                proc_rect.y + 15,
                self.palette.faint,
                self.palette.surface,
            );
            subtitle_text.draw_text(canvas, "PID", left, proc_rect.y + 40, self.palette.faint, self.palette.surface);
            subtitle_text.draw_text(
                canvas,
                "Name",
                name_left,
                proc_rect.y + 40,
                self.palette.faint,
                self.palette.surface,
            );
            subtitle_text.draw_text_right(
                canvas,
                "CPU",
                cpu_right,
                proc_rect.y + 40,
                self.palette.faint,
                self.palette.surface,
            );
            subtitle_text.draw_text_right(
                canvas,
                "Memory",
                mem_right,
                proc_rect.y + 40,
                self.palette.faint,
                self.palette.surface,
            );
            let row_height = 18;
            let first_row = proc_rect.y + 59;
            let available_height = (proc_rect.y + proc_rect.h - inset - first_row).max(0);
            let row_count = usize::try_from(available_height / row_height).unwrap_or(0);
            for (row, process) in snap.processes.iter().take(row_count).enumerate() {
                let y = first_row + i32::try_from(row).unwrap_or(0) * row_height;
                let name_width = (cpu_right - name_left - 12).max(1);
                let name = Self::truncate_prefix(&process.name, text, name_width);
                text.draw_text(canvas, &process.pid.to_string(), left, y, self.palette.dim, self.palette.surface);
                text.draw_text(
                    canvas,
                    &name,
                    name_left,
                    y,
                    if row == 0 { self.palette.temperature } else { self.palette.foreground },
                    self.palette.surface,
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
                    self.palette.border,
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
                        if row == 0 { self.palette.temperature } else { self.palette.cpu },
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
                    self.palette.dim,
                    self.palette.surface,
                );
            }
            phases.static_content += Instant::now().duration_since(started);
            // A full repaint rewrote text, cards, and the process table across
            // the whole surface, so the entire window is damage. Collapsing to
            // one rect (rather than the accumulated chart/bar regions) is both
            // correct and cheaper for the consumer to premultiply as one span.
            DamageList::single(canvas.bounds())
        } else {
            // Steady state: only the animated chart edges and moved core bars
            // were drawn; return exactly those so the platform layer
            // premultiplies and uploads a tiny fraction of the window.
            damage
        }
    }
}

struct App {
    window: TermWindow,
    typography: Typography,
    typography_rx: mpsc::Receiver<TypographyTier>,
    snapshot: Snapshot,
    history: History,
    background: Color,
    rx: mpsc::Receiver<Snapshot>,
    full_repaint: bool,
    has_received_snapshot: bool,
    typography_pending: usize,
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
        // The four typography tiers share the same font file. Read it once;
        // repeatedly opening a large TTF/OTF during window creation made the
        // first frame wait on avoidable filesystem work.
        let font_path = cfg.font.path.clone();
        let emoji_path = cfg.font.emoji_path.clone();
        let font_data = font_path
            .as_deref()
            .and_then(|path| std::fs::read(path).ok());
        let hero_size = size * cfg.monitor.hero_size.unwrap_or(2.5);
        let title_size = size * cfg.monitor.title_size.unwrap_or(1.0);
        let subtitle_size = size * cfg.monitor.subtitle_size.unwrap_or(0.90);
        let normal_size = size * cfg.monitor.normal_size.unwrap_or(0.80);
        let typography_rx = Self::spawn_typography_loaders(
            font_path.as_deref(),
            emoji_path.as_deref(),
            font_data.as_deref(),
            [hero_size, title_size, subtitle_size, normal_size],
        );
        // Paint immediately with the embedded font while the configured face
        // is parsed off the Wayland thread. The real typography is swapped in
        // as soon as it is ready, turning the multi-second startup stall into
        // a progressive upgrade instead of delaying text and graphs together.
        let typography = Typography {
            hero: TextRenderer::load(None, hero_size),
            title: TextRenderer::load(None, title_size),
            subtitle: TextRenderer::load(None, subtitle_size),
            normal: TextRenderer::load(None, normal_size),
        };
        let palette = MonitorPalette::from_config(&cfg);
        let background = palette.background;
        let rx = Self::spawn_worker(wake);
        Self {
            window,
            typography,
            typography_rx,
            snapshot: Snapshot::default(),
            history: History::new(),
            background,
            rx,
            full_repaint: true,
            has_received_snapshot: false,
            typography_pending: 4,
            profile: FrameProfile::new(),
            display: SampleInterpolator::new(),
            painter: Painter::with_palette(palette),
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
            let mut profile = WorkerProfile::new();
            // Prime the process table before entering the steady-state loop.
            // `processes_tick` deliberately spreads one `/proc` sweep across ten
            // 1 Hz ticks to keep steady-state cost low, but that leaves the
            // table empty for ~10 s at launch and withholds per-process CPU%
            // (a two-sweep delta) for ~20 s. Two full sweeps here -- separated
            // by a real gap so the second can compute a CPU rate against the
            // first's baseline -- populate names/memory on the very first frame
            // and real CPU% one interval later, after which we fall into the
            // cheap sliced cadence. The first sweep's sample is sent too so the
            // hero numbers and charts are not held hostage to the CPU baseline.
            let mut processes = reader.prime_processes();
            let sample = reader.sample();
            let _ = tx.send(Snapshot {
                sample,
                processes: processes.clone(),
            });
            wake.signal();
            std::thread::sleep(SAMPLE_INTERVAL);
            processes = reader.prime_processes();
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
                // Wake the UI immediately after useful telemetry is ready.
                // Reporting and the optional latency probe must not delay the
                // first graph frame after this notification.
                wake.signal();
                profile.report();
                reader.refresh_network_ping();
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
            while let Ok(update) = self.typography_rx.try_recv() {
                match update {
                    TypographyTier::Hero(renderer) => self.typography.hero = renderer,
                    TypographyTier::Title(renderer) => self.typography.title = renderer,
                    TypographyTier::Subtitle(renderer) => self.typography.subtitle = renderer,
                    TypographyTier::Normal(renderer) => self.typography.normal = renderer,
                }
                // Each tier is independently useful. Repaint as soon as one
                // arrives instead of waiting for the slowest face/size parse.
                self.typography_pending = self.typography_pending.saturating_sub(1);
                self.full_repaint = true;
            }
            while let Ok(snapshot) = self.rx.try_recv() {
                // The first frame can already contain card backgrounds and
                // chart surfaces, but the text layout is deliberately gated
                // behind `full_repaint`. Mark the first real sample as a
                // static-content repaint so labels and values are not left
                // waiting for the font loader or a later process sweep.
                if !self.has_received_snapshot {
                    self.full_repaint = true;
                    self.has_received_snapshot = true;
                }
                if Self::processes_changed(&self.snapshot.processes, &snapshot.processes) {
                    self.full_repaint = true;
                }
                self.display.accept(snapshot.sample.clone());
                self.snapshot = snapshot;
            }
            // Blend in place into the existing sample buffer: this runs every
            // 60 Hz tick (including skipped ones), so allocating a fresh sample
            // here -- as the old `= self.display.value()` did -- cloned four
            // device strings and a cores Vec ~60x/sec for byte-identical
            // output during the glide's hold. `blend_into` reuses the buffer.
            self.display.blend_into(&mut self.snapshot.sample);
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
                if !self.paint_and_present(current_display) {
                    break;
                }
            } else if !self.pump_idle() {
                break;
            }
            // Do not replay missed frames after a compositor stall; the newest
            // sample is more useful than a burst of obsolete graph states.
            next_frame += FRAME_INTERVAL;
            if next_frame <= Instant::now() {
                next_frame = Instant::now() + FRAME_INTERVAL;
            }
        }
    }

    /// Spawn one background thread per typography tier to parse and prewarm
    /// the configured font off the Wayland thread, returning the receiver the
    /// run loop drains as each tier arrives.
    ///
    /// Font parsing (a large TTF/OTF plus glyph prewarming) is the multi-
    /// second cost that used to stall the first frame. Each of the four tiers
    /// -- hero, title, subtitle, normal, passed in that order via `sizes` --
    /// loads independently so the dashboard can swap each in the instant it is
    /// ready rather than waiting for the slowest face. Kept off `new` so that
    /// constructor stays a straight-line assembly of fields.
    fn spawn_typography_loaders(
        font_path: Option<&str>,
        emoji_path: Option<&str>,
        font_data: Option<&[u8]>,
        sizes: [f32; 4],
    ) -> mpsc::Receiver<TypographyTier> {
        let (typography_tx, typography_rx) = mpsc::channel();
        // Own the borrowed inputs once so each spawned thread can capture its
        // own clone; the borrows don't outlive this call but the threads do.
        let font_path = font_path.map(str::to_owned);
        let emoji_path = emoji_path.map(str::to_owned);
        let font_data = font_data.map(<[u8]>::to_vec);
        for (tier, px_size) in (0_u8..).zip(sizes) {
            let tx = typography_tx.clone();
            let path = font_path.clone();
            let emoji = emoji_path.clone();
            let data = font_data.clone();
            std::thread::spawn(move || {
                let renderer = if let (Some(path), Some(data)) = (path.as_deref(), data.as_deref())
                {
                    TextRenderer::load_with_fallbacks_from_bytes(
                        path,
                        data,
                        &[emoji.as_deref()],
                        px_size,
                    )
                } else {
                    TextRenderer::load_with_fallbacks(path.as_deref(), &[emoji.as_deref()], px_size)
                };
                let mut renderer = renderer;
                renderer.prewarm_common_glyphs();
                let update = match tier {
                    0 => TypographyTier::Hero(renderer),
                    1 => TypographyTier::Title(renderer),
                    2 => TypographyTier::Subtitle(renderer),
                    _ => TypographyTier::Normal(renderer),
                };
                let _ = tx.send(update);
            });
        }
        typography_rx
    }

    /// Paint one frame and hand its damage to the compositor. Returns whether
    /// the run loop should continue (false only on a fatal `tick` error).
    ///
    /// Extracted from `run` so that loop stays a readable dispatcher: this is
    /// the whole "a viewer would see a change" branch -- request the redraw,
    /// run the painter inside `tick_with_painter`, and record the frame's
    /// profile. `current_display` is the quantized snapshot that justified
    /// this paint; it becomes `last_painted_display` only once the frame
    /// actually presented (a compositor that owned both shm slots skips the
    /// closure, so we must not advance the gate's baseline on a dropped tick).
    fn paint_and_present(&mut self, current_display: DisplaySnapshot) -> bool {
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
        let mut damage_rows = 0.0;
        let mut phases = PaintPhases::default();
        // Proof-of-paint. `tick_with_painter` returns `Ok(true)` even
        // when the compositor still owns both shm slots
        // (`NotReady`/`NoFreeSlot`): in that case it returns *before*
        // running this closure, so no pixels were produced. Text and
        // card chrome are drawn ONLY on a `full_repaint` frame, so if
        // we cleared `full_repaint` on such a dropped frame the static
        // content would never be redrawn -- it vanishes permanently.
        // The closure runs exactly when a slot was claimed and the
        // frame really presented, so setting this inside it is the one
        // reliable signal that the paint landed.
        let mut frame_presented = false;
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
            frame_presented = true;
            // Measure the row coverage the shm path will actually
            // upload (row-band granular), so the profile shows whether
            // present cost is dominated by many separated chart bands
            // or is already minimal. A full repaint uploads everything.
            damage_rows = if full_repaint {
                1.0
            } else {
                damage.row_coverage_fraction(canvas.height())
            };
            // A full repaint touched the whole surface; signal that
            // explicitly so the platform takes its whole-window premul
            // path. Otherwise hand over exactly the animated regions.
            if full_repaint {
                platform::draw::Damage::full()
            } else {
                damage.to_platform()
            }
        }) {
            Ok(true) => {
                if frame_presented {
                    self.profile.record(
                        paint_time,
                        frame_started.elapsed(),
                        &phases,
                        full_repaint,
                        damage_rows,
                    );
                    // Keep the static/text pass alive until every
                    // custom tier has arrived. This is intentionally
                    // limited to startup: it guarantees the temporary
                    // bitmap text is actually presented even if
                    // telemetry/chart damage would otherwise let the
                    // first full repaint get coalesced away.
                    self.full_repaint = self.typography_pending != 0;
                    self.last_painted_display = current_display;
                }
                // else: the compositor owned both slots this tick, so
                // nothing was drawn. Leave `full_repaint` and
                // `last_painted_display` untouched so the very next
                // tick retries the same paint instead of dropping the
                // static content on the floor.
                true
            }
            Ok(false) | Err(_) => false,
        }
    }

    /// Pump the Wayland connection on a tick with nothing to paint. Returns
    /// whether the run loop should continue.
    ///
    /// Still pump the connection (resize/output-change events, frame
    /// callbacks) even when the redraw gate skipped this tick. Crucially,
    /// retain the custom-painter path while doing so: `tick()` is for
    /// terminal-grid windows and would copy the XRGB backbuffer directly into
    /// an ARGB monitor buffer if an event (notably a resize) made it dirty
    /// between dashboard paints. XRGB has a zero high byte, so that accidental
    /// present makes the entire monitor transparent. The no-op painter lets
    /// `TermWindow` retain and premultiply its already-valid custom
    /// backbuffer should it need to present for such an event.
    fn pump_idle(&mut self) -> bool {
        self.profile.record_skip();
        matches!(
            self.window
                .tick_with_painter(|_| platform::draw::Damage::none()),
            Ok(true)
        )
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
    let args: Vec<String> = std::env::args().collect();
    if snapshot_cli::SnapshotCli::requested(&args) {
        // Headless path: never touches Wayland/the window/the paint loop.
        // `App::new` unconditionally opens a `TermWindow`, so the snapshot
        // report must branch before it, not skip presenting after one.
        snapshot_cli::SnapshotCli::new().run();
        return;
    }
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
        Canvas, History, MonitorPalette, MonitorSample, PERF_ITERATIONS, PaintPhases, Painter,
        ProcessInfo, Rect, Snapshot, Typography,
    };
    use std::time::{Duration, Instant};

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
        let iterations = std::env::var("MONITOR_BENCH_ITERS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(PERF_ITERATIONS);
        HeadlessPresent::new().run(iterations);
    }

    /// Headless reproduction of the *entire* present pipeline a translucent
    /// custom-painter window pays each frame -- paint into the backbuffer,
    /// premultiply into the ARGB cache, copy to a simulated shm slot -- so the
    /// growing `blit`/present spike the live `TERMINAL_DEBUG_FPS=log` trace
    /// showed (2.4ms -> 67ms over a session) can be reproduced, bucketed, and
    /// bisected here with no compositor. The old harness reported only a
    /// whole-run *average*, which mathematically masks growth; this reports
    /// per-bucket timing so a rising trend is visible in the numbers alone.
    struct HeadlessPresent {
        width: i32,
        height: i32,
        backbuffer: Vec<u32>,
        prev_backbuffer: Vec<u32>,
        custom_argb: Vec<u32>,
        shm_slot: Vec<u32>,
        last_dirty_pixels: usize,
        typography: Typography,
        painter: Painter,
        history: History,
        premultiplier: platform::premultiply::Premultiplier,
    }

    impl HeadlessPresent {
        fn new() -> Self {
            // Window pixel dimensions dominate premul + shm-copy cost (both
            // run over the *whole* surface every present). Override via env to
            // test the area-scaling hypothesis: `MONITOR_BENCH_W`/`_H`.
            let width: usize = std::env::var("MONITOR_BENCH_W")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1400);
            let height: usize = std::env::var("MONITOR_BENCH_H")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(900);
            let area = width as usize * height as usize;
            Self {
                width: width as i32,
                height: height as i32,
                backbuffer: vec![0u32; area],
                prev_backbuffer: vec![0u32; area],
                custom_argb: vec![0u32; area],
                shm_slot: vec![0u32; area],
                last_dirty_pixels: 0,
                typography: Typography {
                    hero: statusbar::TextRenderer::load(None, 32.0),
                    title: statusbar::TextRenderer::load(None, 14.0),
                    subtitle: statusbar::TextRenderer::load(None, 13.0),
                    normal: statusbar::TextRenderer::load(None, 11.0),
                },
                painter: Painter::with_palette(MonitorPalette::builtin()),
                history: History::new(),
                premultiplier: platform::premultiply::Premultiplier::new(),
            }
        }

        /// One present: the same three phases the live trace splits into
        /// (`paint`, `premul`, `shm-copy`). Returns each phase's duration so
        /// the caller can bucket them and expose any growth trend.
        fn present(
            &mut self,
            tick: usize,
            phases: &mut PaintPhases,
        ) -> (Duration, Duration, Duration) {
            let snapshot = Snapshot {
                sample: synthetic_sample(tick),
                processes: synthetic_processes(tick),
            };
            if tick % 25 == 0 {
                self.history.add(&snapshot.sample);
            }
            let full_repaint = tick % 10 == 0;

            // Snapshot the backbuffer so the exact number of pixels this paint
            // actually mutates can be counted by diffing afterwards. premul and
            // shm-copy are both strictly linear in pixel count, so the true
            // floor of *any* damage scheme -- single-rect, row-band, or
            // multi-region -- is "premultiply only the changed pixels". Diffing
            // gives that floor with no guessing about layout geometry: it's the
            // ground truth the multi-region damage list would have to match.
            let measure_dirty = std::env::var_os("MONITOR_BENCH_DIRTY").is_some();
            if measure_dirty {
                self.prev_backbuffer.copy_from_slice(&self.backbuffer);
            }

            let t0 = Instant::now();
            let damage = {
                let mut canvas = Canvas::new(&mut self.backbuffer, self.width, self.height);
                self.painter.paint(
                    &mut canvas,
                    &mut self.typography,
                    &snapshot,
                    &self.history,
                    MonitorPalette::builtin().surface,
                    full_repaint,
                    phases,
                )
            };
            let paint = t0.elapsed();

            let width = self.width.max(0) as usize;
            let rows = self.backbuffer.len() / width.max(1);
            let total_area = self.backbuffer.len();
            // Three scoping models to compare in one framework:
            // * FULL    -> whole window (the pre-fix behaviour, the bug)
            // * DIRTY   -> only pixels that actually changed (the true ceiling)
            // * default -> the SHIPPED behaviour: premultiply each region the
            //              painter's `DamageList` actually reported, per row
            //              band, exactly like `TermWindow::maybe_present` now
            //              does. On a full-repaint frame the list collapses to
            //              the whole window; steady state is a few small rects.
            if std::env::var_os("MONITOR_BENCH_FULL").is_some() {
                let t1 = Instant::now();
                self.premultiplier
                    .run(220, &self.backbuffer, &mut self.custom_argb);
                let premul = t1.elapsed();
                let t2 = Instant::now();
                self.shm_slot.copy_from_slice(&self.custom_argb);
                let shm = t2.elapsed();
                self.last_dirty_pixels = total_area;
                return (paint, premul, shm);
            }
            if measure_dirty {
                let (start, end) = self.dirty_span();
                let t1 = Instant::now();
                self.premultiplier.run(
                    220,
                    &self.backbuffer[start..end],
                    &mut self.custom_argb[start..end],
                );
                let premul = t1.elapsed();
                let t2 = Instant::now();
                self.shm_slot[start..end].copy_from_slice(&self.custom_argb[start..end]);
                let shm = t2.elapsed();
                return (paint, premul, shm);
            }
            // Shipped path: premultiply + upload each reported region as a row
            // band (the row-granular shm damage protocol). Full repaints report
            // one whole-window rect.
            let regions: Vec<Rect> = if full_repaint {
                vec![Rect::new(0, 0, self.width, self.height)]
            } else {
                damage.regions().to_vec()
            };
            let mut premul = Duration::ZERO;
            let mut shm = Duration::ZERO;
            let mut dirty_px = 0usize;
            for rect in regions {
                let y0 = (rect.y.max(0).min(self.height) as usize).min(rows);
                let y1 = (rect.bottom().max(rect.y).min(self.height) as usize).min(rows);
                let start = y0 * width;
                let end = y1 * width;
                if start >= end {
                    continue;
                }
                dirty_px += end - start;
                let t1 = Instant::now();
                self.premultiplier.run(
                    220,
                    &self.backbuffer[start..end],
                    &mut self.custom_argb[start..end],
                );
                premul += t1.elapsed();
                let t2 = Instant::now();
                self.shm_slot[start..end].copy_from_slice(&self.custom_argb[start..end]);
                shm += t2.elapsed();
            }
            self.last_dirty_pixels = dirty_px;
            (paint, premul, shm)
        }

        /// The contiguous `[first_changed, last_changed]` pixel span between
        /// `prev_backbuffer` and `backbuffer`, and the count of genuinely
        /// changed pixels within it (reported via `last_dirty_pixels`). A
        /// contiguous span (rather than a scatter list) is the fair proxy for
        /// premul cost: the kernel is linear and cache-friendly over any run,
        /// so timing one span of the same length as the true dirty *area* is
        /// within noise of premultiplying the real scattered regions.
        fn dirty_span(&mut self) -> (usize, usize) {
            let mut first = self.backbuffer.len();
            let mut last = 0usize;
            let mut count = 0usize;
            for (i, (a, b)) in self
                .prev_backbuffer
                .iter()
                .zip(self.backbuffer.iter())
                .enumerate()
            {
                if a != b {
                    first = first.min(i);
                    last = i;
                    count += 1;
                }
            }
            self.last_dirty_pixels = count;
            if first > last {
                (0, 0)
            } else {
                // Compress the span to the changed *area*: the scattered charts
                // span nearly the whole window, but only `count` pixels moved,
                // so premultiply a run of exactly that many to model a perfect
                // damage list rather than the (useless) bounding box.
                (0, count)
            }
        }

        fn run(&mut self, iterations: usize) {
            let mut phases = PaintPhases::default();
            let buckets = 8;
            let per = (iterations / buckets).max(1);
            let started = Instant::now();
            eprintln!(
                "headless present pipeline: {iterations} frames, {buckets} buckets of {per}\n\
                 bucket |  paint ms |  premul ms |  shm ms | worst-frame ms | dirty%"
            );
            let total_px = (self.width.max(0) as usize) * (self.height.max(0) as usize);
            for bucket in 0..buckets {
                let mut paint_sum = Duration::ZERO;
                let mut premul_sum = Duration::ZERO;
                let mut shm_sum = Duration::ZERO;
                let mut worst = Duration::ZERO;
                let mut dirty_sum = 0usize;
                for i in 0..per {
                    let tick = bucket * per + i;
                    let (paint, premul, shm) = self.present(tick, &mut phases);
                    paint_sum += paint;
                    premul_sum += premul;
                    shm_sum += shm;
                    dirty_sum += self.last_dirty_pixels;
                    worst = worst.max(paint + premul + shm);
                }
                #[allow(clippy::cast_precision_loss)]
                let n = per as f64;
                // Average fraction of the window actually mutated this bucket --
                // the number that decides whether damage-scoping is worth it.
                #[allow(clippy::cast_precision_loss)]
                let dirty_pct = if total_px > 0 {
                    dirty_sum as f64 / n / total_px as f64 * 100.0
                } else {
                    0.0
                };
                eprintln!(
                    "{bucket:>6} | {:>9.4} | {:>10.4} | {:>7.4} | {:>10.4} | {dirty_pct:>7.3}%",
                    paint_sum.as_secs_f64() * 1e3 / n,
                    premul_sum.as_secs_f64() * 1e3 / n,
                    shm_sum.as_secs_f64() * 1e3 / n,
                    worst.as_secs_f64() * 1e3,
                );
            }
            let elapsed = started.elapsed();
            #[allow(clippy::cast_precision_loss)]
            let avg_ms = elapsed.as_secs_f64() * 1e3 / iterations as f64;
            eprintln!("total {elapsed:.2?} ({avg_ms:.4} ms/frame avg over {iterations})");
            // If any bucket's average climbs materially over bucket 0, the
            // phase whose column grows is the accumulator -- exactly the
            // 2.4->67ms drift the live trace showed, now reproducible here.
        }
    }
}

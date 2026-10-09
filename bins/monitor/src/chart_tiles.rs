//! [`ChartTiles`] -- paints the four chart tiles: a title and live value,
//! a line of the device's other tracked readings, and a scrolling history
//! chart, with per-core bars under the CPU chart.

use platform::draw::{Canvas, Color, Rect};

use crate::chart_cache::{ChartCache, Overlay};
use crate::core_bars::{CoreBarPaint, CoreBars};
use crate::damage::DamageList;
use crate::dashboard_report::{CardReport, DashboardReport};
use crate::history::{HISTORY, History};
use crate::palette::MonitorPalette;
use crate::text_layout::TextLayout;
use crate::tile_layout::ChartKind;
use crate::tile_metrics::TileMetrics;
use crate::typography::Typography;

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

/// What a chart tile needs from the frame being painted.
pub struct ChartFrame<'a> {
    pub palette: &'a MonitorPalette,
    pub metrics: TileMetrics,
    pub history: &'a History,
    pub report: &'a DashboardReport,
    pub cores: &'a [f32],
    pub full: bool,
    pub text: bool,
}

pub struct ChartTiles {
    cpu: ChartCache,
    memory: ChartCache,
    temperature: ChartCache,
    network: ChartCache,
    cpu_bars: CoreBars,
}

impl ChartTiles {
    const BARS_HEIGHT: i32 = 96;
    const BARS_GAP: i32 = 8;
    const MIN_LINE_HEIGHT: i32 = 24;
    const NETWORK_SCALE: f32 = 10.0;

    #[must_use]
    pub const fn new(surface: Color) -> Self {
        Self {
            cpu: ChartCache::new(surface),
            memory: ChartCache::new(surface),
            temperature: ChartCache::new(surface),
            network: ChartCache::new(surface),
            cpu_bars: CoreBars::new(),
        }
    }

    /// Paint one chart tile whose rounded body is `body`. On a full repaint
    /// the caller has already drawn the tile's fill and outline.
    pub fn paint(
        &mut self,
        canvas: &mut Canvas<'_>,
        typography: &mut Typography,
        kind: ChartKind,
        body: Rect,
        frame: &ChartFrame<'_>,
    ) -> DamageList {
        let mut damage = DamageList::new();
        let pad = frame.metrics.pad;
        let title = Rect::new(
            body.x + pad,
            body.y + pad,
            body.w - pad * 2,
            typography.title.line_height(),
        );
        if title.w <= 0 || title.bottom() > body.bottom() {
            return damage;
        }
        let readings = Rect::new(
            title.x,
            title.bottom(),
            title.w,
            typography.subtitle.line_height(),
        );
        let with_readings = readings.bottom() + pad <= body.bottom();
        let header = if with_readings {
            Rect::new(title.x, title.y, title.w, readings.bottom() - title.y)
        } else {
            title
        };
        if frame.text {
            let card = Self::card(kind, frame.report);
            Self::header(canvas, typography, header, card, frame);
            if with_readings {
                Self::readings(canvas, typography, readings, card, frame);
            }
            if !frame.full {
                damage.push(header);
            }
        }
        let top = header.bottom() + frame.metrics.gutter;
        let area = Rect::new(header.x, top, header.w, body.bottom() - pad - top);
        if area.h > 0 {
            damage.extend(&self.chart(canvas, kind, area, frame));
        }
        damage
    }

    const fn card(kind: ChartKind, report: &DashboardReport) -> &CardReport {
        match kind {
            ChartKind::Cpu => &report.cpu,
            ChartKind::Memory => &report.memory,
            ChartKind::Temperature => &report.temperature,
            ChartKind::Network => &report.network,
        }
    }

    fn header(
        canvas: &mut Canvas<'_>,
        typography: &mut Typography,
        header: Rect,
        card: &CardReport,
        frame: &ChartFrame<'_>,
    ) {
        let palette = frame.palette;
        if !frame.full {
            canvas.fill_rect(header, palette.surface);
        }
        let value = format!("{}{}", card.hero, card.unit);
        typography.title.draw_text(
            canvas,
            card.title,
            header.x,
            header.y,
            palette.dim,
            palette.surface,
        );
        typography.title.draw_text_right(
            canvas,
            &value,
            header.right(),
            header.y,
            palette.foreground,
            palette.surface,
        );
    }

    fn readings(
        canvas: &mut Canvas<'_>,
        typography: &mut Typography,
        row: Rect,
        card: &CardReport,
        frame: &ChartFrame<'_>,
    ) {
        let text = card
            .details
            .iter()
            .map(|(label, value)| format!("{label} {value}"))
            .collect::<Vec<_>>()
            .join("  ·  ");
        let fitted = TextLayout::truncate_prefix(&text, &mut typography.subtitle, row.w);
        typography.subtitle.draw_text(
            canvas,
            &fitted,
            row.x,
            row.y,
            frame.palette.faint,
            frame.palette.surface,
        );
    }

    fn chart(
        &mut self,
        canvas: &mut Canvas<'_>,
        kind: ChartKind,
        area: Rect,
        frame: &ChartFrame<'_>,
    ) -> DamageList {
        let palette = frame.palette;
        let history = frame.history;
        match kind {
            ChartKind::Cpu => self.cpu_with_bars(canvas, area, frame),
            ChartKind::Memory => self.memory.paint(
                canvas,
                area,
                &history.memory,
                100.0,
                palette.memory,
                &[],
                HISTORY,
                history.sequence,
                frame.full,
            ),
            ChartKind::Temperature => self.temperature.paint(
                canvas,
                area,
                &history.temp,
                100.0,
                palette.temperature,
                &[],
                HISTORY,
                history.sequence,
                frame.full,
            ),
            ChartKind::Network => self.network.paint(
                canvas,
                area,
                &history.rx,
                Self::NETWORK_SCALE,
                palette.network,
                &[Overlay::new(&history.tx, palette.temperature)],
                HISTORY,
                history.sequence,
                frame.full,
            ),
        }
    }

    fn cpu_with_bars(
        &mut self,
        canvas: &mut Canvas<'_>,
        area: Rect,
        frame: &ChartFrame<'_>,
    ) -> DamageList {
        let room = area.h - Self::BARS_GAP;
        let bars = (room / 3).min(Self::BARS_HEIGHT);
        let bars = if room - bars >= Self::MIN_LINE_HEIGHT {
            bars
        } else {
            0
        };
        let line_height = if bars > 0 { room - bars } else { area.h };
        let line = Rect::new(area.x, area.y, area.w, line_height);
        let mut damage = self.cpu.paint(
            canvas,
            line,
            &frame.history.cpu,
            100.0,
            frame.palette.cpu,
            &[],
            HISTORY,
            frame.history.sequence,
            frame.full,
        );
        if bars > 0 {
            damage.extend(&self.cpu_bars.paint(
                canvas,
                frame.cores,
                CoreBarPaint {
                    bounds: Rect::new(area.x, line.bottom() + Self::BARS_GAP, area.w, bars),
                    background: frame.palette.surface,
                    colors: &CORE_COLORS,
                    background_cleared: frame.full,
                },
            ));
        }
        damage
    }
}

#[cfg(test)]
#[allow(
    clippy::manual_assert_eq,
    reason = "assert_eq on pixel buffers prints every pixel on failure"
)]
mod tests {
    use super::{ChartFrame, ChartTiles};
    use crate::dashboard_report::DashboardReport;
    use crate::history::History;
    use crate::palette::MonitorPalette;
    use crate::tile_layout::ChartKind;
    use crate::tile_metrics::TileMetrics;
    use crate::typography::Typography;
    use platform::draw::{Canvas, Rect};
    use sysmon::MonitorSample;

    const W: i32 = 600;
    const H: i32 = 500;

    struct Fixture {
        palette: MonitorPalette,
        typography: Typography,
        history: History,
        report: DashboardReport,
        sample: MonitorSample,
        tiles: ChartTiles,
        buf: Vec<u32>,
    }

    impl Fixture {
        fn new() -> Self {
            let palette = MonitorPalette::builtin();
            let sample = MonitorSample {
                cpu: 60.0,
                cores: vec![10.0, 90.0, 40.0],
                memory: 50.0,
                ..Default::default()
            };
            let mut history = History::new();
            for _ in 0..40 {
                history.add(&sample);
            }
            let mut buf = vec![0u32; (W * H) as usize];
            Canvas::new(&mut buf, W, H).clear(palette.surface);
            Self {
                tiles: ChartTiles::new(palette.surface),
                report: DashboardReport::from_sample(&sample, &[]),
                typography: Typography::embedded([32.0, 14.0, 13.0, 11.0]),
                palette,
                history,
                sample,
                buf,
            }
        }

        fn paint(&mut self, kind: ChartKind, body: Rect, full: bool, text: bool) -> usize {
            let metrics = TileMetrics::new(self.typography.title.line_height());
            let frame = ChartFrame {
                palette: &self.palette,
                metrics,
                history: &self.history,
                report: &self.report,
                cores: &self.sample.cores,
                full,
                text,
            };
            let mut canvas = Canvas::new(&mut self.buf, W, H);
            self.tiles
                .paint(&mut canvas, &mut self.typography, kind, body, &frame)
                .regions()
                .len()
        }
    }

    #[test]
    fn a_full_paint_draws_text_and_chart_inside_the_body() {
        let mut fixture = Fixture::new();
        let body = Rect::new(100, 100, 360, 280);
        let blank = fixture.buf.clone();
        fixture.paint(ChartKind::Cpu, body, true, true);
        assert!(fixture.buf != blank);
        for y in 0..H {
            for x in 0..W {
                if !body.contains(platform::draw::Point::new(x, y)) {
                    let at = usize::try_from(y * W + x).expect("in range");
                    assert_eq!(fixture.buf[at], blank[at], "({x},{y})");
                }
            }
        }
    }

    #[test]
    fn a_text_only_repaint_damages_just_the_header() {
        let mut fixture = Fixture::new();
        let body = Rect::new(100, 100, 360, 280);
        fixture.paint(ChartKind::Memory, body, true, true);
        let regions = fixture.paint(ChartKind::Memory, body, false, true);
        assert_eq!(regions, 1);
    }

    #[test]
    fn an_idle_repaint_damages_nothing() {
        let mut fixture = Fixture::new();
        let body = Rect::new(100, 100, 360, 280);
        for kind in [ChartKind::Cpu, ChartKind::Network, ChartKind::Temperature] {
            fixture.paint(kind, body, true, true);
            assert_eq!(fixture.paint(kind, body, false, false), 0, "{kind:?}");
        }
    }

    #[test]
    fn a_body_with_no_room_for_a_header_draws_nothing() {
        let mut fixture = Fixture::new();
        let blank = fixture.buf.clone();
        fixture.paint(ChartKind::Cpu, Rect::new(100, 100, 120, 20), true, true);
        assert!(fixture.buf == blank);
    }

    #[test]
    fn a_short_cpu_tile_drops_the_core_bars_and_keeps_the_line() {
        let mut fixture = Fixture::new();
        let tall = fixture.paint(ChartKind::Cpu, Rect::new(100, 50, 360, 400), true, true);
        let mut short = Fixture::new();
        let few = short.paint(ChartKind::Cpu, Rect::new(100, 50, 360, 130), true, true);
        assert!(tall >= few);
        assert!(short.buf != Fixture::new().buf);
    }
}

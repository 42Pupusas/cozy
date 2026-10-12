//! [`TileScene`] -- paints the whole window as tiles: the info card, the four
//! charts, one tile per process group, and the "more" tile.

use std::time::Instant;

use platform::draw::{Canvas, Point, Rect};
use sysmon::{MonitorSample, ProcessInfo};

use crate::chart_tiles::{ChartFrame, ChartTiles};
use crate::damage::DamageList;
use crate::dashboard_report::DashboardReport;
use crate::history::History;
use crate::info_card::InfoCard;
use crate::palette::MonitorPalette;
use crate::process_stage::{ProcessStage, StageFrame};
use crate::repaint::Repaint;
use crate::tile_field::TileField;
use crate::tile_hover::TileHover;
use crate::tile_layout::{PlacedTile, TileKind};
use crate::tile_metrics::TileMetrics;
use crate::typography::Typography;

/// Time spent in each part of a frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct ScenePhases {
    pub charts: std::time::Duration,
    pub chrome: std::time::Duration,
}

pub struct SceneFrame<'a> {
    pub sample: &'a MonitorSample,
    pub history: &'a History,
    pub repaint: Repaint,
    pub now_ms: u64,
}

pub struct TileScene {
    palette: MonitorPalette,
    charts: ChartTiles,
    field: TileField,
    hover: TileHover,
    stage: ProcessStage,
    stale_tiles: Vec<TileKind>,
    metrics: Option<TileMetrics>,
    area: Option<Rect>,
}

impl TileScene {
    #[must_use]
    pub fn new(palette: MonitorPalette) -> Self {
        Self {
            charts: ChartTiles::new(palette.surface),
            palette,
            field: TileField::new(),
            hover: TileHover::new(),
            stage: ProcessStage::new(),
            stale_tiles: Vec::new(),
            metrics: None,
            area: None,
        }
    }

    /// Whether tiles are still growing, gliding or shrinking, so another
    /// frame is owed.
    #[must_use]
    pub fn is_animating(&self) -> bool {
        self.stage.is_animating()
    }

    /// Fold in a finished process sweep. Returns whether any tile changed.
    pub fn accept_processes(&mut self, processes: &[ProcessInfo]) -> bool {
        self.field.accept(processes)
    }

    #[must_use]
    pub const fn field(&self) -> &TileField {
        &self.field
    }

    /// A left click at physical `at`. Returns whether the view changed.
    pub fn click(&mut self, at: Point) -> bool {
        self.field.click(at)
    }

    /// The pointer moved to physical `at`, or left the window. Returns whether
    /// a tile needs repainting.
    pub fn point(&mut self, at: Option<Point>) -> bool {
        let changed = self.hover.track(self.field.arrangement(), at);
        let any = !changed.is_empty();
        self.stale_tiles.extend(changed);
        any
    }

    /// A wheel turn; each line slides one tile. Returns whether the view
    /// changed.
    pub fn wheel(&mut self, lines: i32) -> bool {
        self.field.wheel(lines)
    }

    /// Slide back to the largest tiles, or leave the expanded group.
    pub fn back(&mut self) -> bool {
        self.field.back()
    }

    /// Where the process tiles are, for the window title.
    #[must_use]
    pub fn heading(&self) -> Option<String> {
        self.field.view().heading()
    }

    pub fn paint(
        &mut self,
        canvas: &mut Canvas<'_>,
        typography: &mut Typography,
        frame: &SceneFrame<'_>,
        phases: &mut ScenePhases,
    ) -> DamageList {
        let metrics = TileMetrics::new(typography.title.line_height());
        let area = Rect::new(
            metrics.margin,
            metrics.margin,
            canvas.width() - metrics.margin * 2,
            canvas.height() - metrics.margin * 2,
        );
        if self.field.prepare(area, metrics.minimum) {
            self.stale_tiles.push(TileKind::More { count: 0 });
        }
        self.stage.step(&self.field, area, frame.now_ms);
        let full = frame.repaint.full() || self.metrics != Some(metrics) || self.area != Some(area);
        self.metrics = Some(metrics);
        self.area = Some(area);
        let text = frame.repaint.text() || full;
        if full {
            canvas.clear(self.palette.background);
        }
        let report = DashboardReport::from_sample(frame.sample, &[]);
        let mut damage = DamageList::new();
        let started = Instant::now();
        let stale = std::mem::take(&mut self.stale_tiles);
        damage.extend(&self.stage.paint(
            canvas,
            typography,
            &StageFrame {
                palette: &self.palette,
                metrics,
                field: &self.field,
                hover: &self.hover,
                full,
                stale: &stale,
            },
        ));
        phases.chrome += started.elapsed();
        for tile in &self.field.arrangement().tiles {
            let started = Instant::now();
            let is_chart = matches!(tile.kind, TileKind::Chart(_));
            let tile_damage = Self::paint_band_tile(
                &self.palette,
                &mut self.charts,
                canvas,
                typography,
                tile,
                &Paint {
                    metrics,
                    full,
                    text,
                    frame,
                    report: &report,
                },
            );
            damage.extend(&tile_damage);
            if is_chart {
                phases.charts += started.elapsed();
            } else {
                phases.chrome += started.elapsed();
            }
        }
        if full {
            DamageList::single(canvas.bounds())
        } else {
            damage
        }
    }

    fn paint_band_tile(
        palette: &MonitorPalette,
        charts: &mut ChartTiles,
        canvas: &mut Canvas<'_>,
        typography: &mut Typography,
        tile: &PlacedTile,
        paint: &Paint<'_>,
    ) -> DamageList {
        let mut damage = DamageList::new();
        let metrics = paint.metrics;
        match &tile.kind {
            TileKind::Info => {
                let body = tile.rect.inset(metrics.gutter);
                if paint.full {
                    Self::chrome(palette, canvas, body, metrics);
                }
                let region = body.inset(metrics.pad);
                if paint.text && region.w > 0 && region.h > 0 {
                    if !paint.full {
                        canvas.fill_rect_glass(region, palette.surface);
                        damage.push(region);
                    }
                    InfoCard::new(palette, typography).paint(canvas, region, paint.report);
                }
            }
            TileKind::Chart(kind) => {
                let body = tile.rect.inset(metrics.gutter);
                if paint.full {
                    Self::chrome(palette, canvas, body, metrics);
                }
                let chart = ChartFrame {
                    palette,
                    metrics,
                    history: paint.frame.history,
                    report: paint.report,
                    cores: &paint.frame.sample.cores,
                    full: paint.full,
                    text: paint.text,
                };
                damage.extend(&charts.paint(canvas, typography, *kind, body, &chart));
            }
            TileKind::Process(_) | TileKind::More { .. } => {}
        }
        damage
    }

    fn chrome(palette: &MonitorPalette, canvas: &mut Canvas<'_>, body: Rect, metrics: TileMetrics) {
        canvas.fill_round_rect_glass(body, palette.surface, metrics.radius);
        canvas.draw_round_rect(body, palette.border, metrics.radius);
    }
}

struct Paint<'a> {
    metrics: TileMetrics,
    full: bool,
    text: bool,
    frame: &'a SceneFrame<'a>,
    report: &'a DashboardReport,
}

#[cfg(test)]
#[allow(
    clippy::manual_assert_eq,
    reason = "assert_eq on pixel buffers prints every pixel on failure"
)]
mod tests {
    use super::{SceneFrame, ScenePhases, TileScene};
    use crate::history::History;
    use crate::palette::MonitorPalette;
    use crate::repaint::Repaint;
    use crate::typography::Typography;
    use platform::SurfaceFormat;
    use platform::draw::Canvas;
    use sysmon::{MonitorSample, ProcessInfo};

    const GLASS_ALPHA: u8 = 204;
    const W: i32 = 1300;
    const H: i32 = 820;

    fn processes() -> Vec<ProcessInfo> {
        let mut list: Vec<ProcessInfo> = (0..8)
            .map(|pid| ProcessInfo {
                pid,
                name: format!("kworker/{pid}:1"),
                cpu: 3.0,
                memory_kb: 10_000,
            })
            .collect();
        list.extend((10..40).map(|pid| ProcessInfo {
            pid,
            name: format!("app{pid}"),
            cpu: 1.0,
            memory_kb: 400_000 / u64::from(pid),
        }));
        list
    }

    struct Rig {
        scene: TileScene,
        typography: Typography,
        history: History,
        sample: MonitorSample,
        buf: Vec<u32>,
        format: SurfaceFormat,
        now_ms: u64,
    }

    impl Rig {
        fn landed() -> Self {
            let mut rig = Self::new();
            rig.paint(Repaint::new(true, true));
            rig.settle();
            rig
        }

        fn settle(&mut self) {
            for _ in 0..200 {
                self.paint(Repaint::new(false, false));
                if !self.scene.is_animating() {
                    self.paint(Repaint::new(false, false));
                    return;
                }
            }
            panic!("the tiles never landed");
        }

        fn translucent() -> Self {
            Self {
                format: SurfaceFormat::with_bg_alpha(GLASS_ALPHA),
                ..Self::new()
            }
        }

        fn new() -> Self {
            let sample = MonitorSample {
                cpu: 30.0,
                cores: vec![10.0, 50.0, 90.0, 20.0],
                memory: 40.0,
                ..Default::default()
            };
            let mut history = History::new();
            for _ in 0..30 {
                history.add(&sample);
            }
            let mut scene = TileScene::new(MonitorPalette::builtin());
            scene.accept_processes(&processes());
            Self {
                scene,
                typography: Typography::embedded([32.0, 14.0, 13.0, 11.0]),
                history,
                sample,
                buf: vec![0; (W * H) as usize],
                format: SurfaceFormat::opaque(),
                now_ms: 0,
            }
        }

        fn paint(&mut self, repaint: Repaint) -> usize {
            self.now_ms += 16;
            let frame = SceneFrame {
                sample: &self.sample,
                history: &self.history,
                repaint,
                now_ms: self.now_ms,
            };
            let mut canvas = Canvas::new(&mut self.buf, W, H).with_format(self.format);
            let mut phases = ScenePhases::default();
            self.scene
                .paint(&mut canvas, &mut self.typography, &frame, &mut phases)
                .regions()
                .len()
        }
    }

    #[test]
    fn the_first_paint_is_a_full_paint() {
        let mut rig = Rig::new();
        assert_eq!(rig.paint(Repaint::new(false, false)), 1);
        assert!(rig.buf.iter().any(|&p| p != 0));
    }

    #[test]
    fn an_idle_repaint_after_the_tiles_land_damages_nothing() {
        let mut rig = Rig::landed();
        assert!(!rig.scene.is_animating());
        assert_eq!(rig.paint(Repaint::new(false, false)), 0);
    }

    #[test]
    fn a_text_repaint_damages_only_small_regions() {
        let mut rig = Rig::landed();
        let regions = rig.paint(Repaint::new(false, true));
        assert!(regions >= 1);
        let before = rig.buf.clone();
        rig.paint(Repaint::new(false, true));
        assert!(rig.buf == before, "a repeated text repaint is stable");
    }

    #[test]
    fn a_full_repaint_reproduces_the_incremental_frame() {
        let mut rig = Rig::landed();
        rig.paint(Repaint::new(false, true));
        let incremental = rig.buf.clone();
        rig.paint(Repaint::new(true, true));
        assert!(rig.buf == incremental);
    }

    #[test]
    fn text_stays_solid_on_a_translucent_window() {
        let mut rig = Rig::translucent();
        rig.paint(Repaint::new(true, true));
        rig.settle();
        let alpha = |pixel: u32| (pixel >> 24) as u8;
        let palette = MonitorPalette::builtin();
        let solid_text = rig.format.pack_paint(palette.foreground);
        assert!(
            rig.buf.contains(&solid_text),
            "the foreground ink must be packed fully opaque"
        );
        assert!(
            rig.buf.contains(&rig.format.pack_glass(palette.background)),
            "the window background keeps the configured opacity"
        );
        assert!(
            rig.buf.iter().all(|&pixel| alpha(pixel) >= GLASS_ALPHA),
            "no pixel may be more transparent than the window itself"
        );
    }

    #[test]
    fn hovering_a_tile_repaints_only_that_tile() {
        let mut rig = Rig::landed();
        let before = rig.buf.clone();
        let rect = rig
            .scene
            .field()
            .arrangement()
            .tiles
            .iter()
            .find(|tile| matches!(tile.kind, super::TileKind::Process(_)))
            .expect("a process tile")
            .rect;
        let inside = platform::draw::Point::new(rect.x + rect.w / 2, rect.y + rect.h / 2);
        assert!(rig.scene.point(Some(inside)));
        assert_eq!(rig.paint(Repaint::new(false, false)), 1);
        assert!(rig.buf != before, "the hovered tile shows its numbers");
        let changed_outside = (0..H).any(|y| {
            (0..W).any(|x| {
                let i = usize::try_from(y * W + x).expect("in range");
                !rect.contains(platform::draw::Point::new(x, y)) && rig.buf[i] != before[i]
            })
        });
        assert!(!changed_outside);
        assert!(rig.scene.point(None));
        rig.paint(Repaint::new(false, false));
        assert!(rig.buf == before, "leaving restores the quiet tile");
    }

    #[test]
    fn tiles_grow_in_from_nothing_then_land() {
        let mut rig = Rig::new();
        rig.paint(Repaint::new(true, true));
        assert!(rig.scene.is_animating(), "the entrance is under way");
        let first = rig.buf.clone();
        rig.settle();
        assert!(rig.buf != first, "the tiles grew");
    }

    #[test]
    fn the_wheel_slides_one_tile_and_animates_the_process_region_only() {
        let mut rig = Rig::new();
        let crowd: Vec<ProcessInfo> = (0..150)
            .map(|pid| ProcessInfo {
                pid,
                name: format!("p{pid}"),
                cpu: 0.0,
                memory_kb: 900_000 / (u64::from(pid) + 1),
            })
            .collect();
        rig.scene.accept_processes(&crowd);
        rig.paint(Repaint::new(true, true));
        rig.settle();
        let band_bottom = rig.scene.field().arrangement().tiles[..5]
            .iter()
            .map(|t| t.rect.bottom())
            .max()
            .expect("band");
        let band_before: Vec<u32> =
            rig.buf[..usize::try_from(band_bottom * W).expect("rows")].to_vec();
        let first = rig
            .scene
            .field()
            .arrangement()
            .tiles
            .iter()
            .find_map(|t| match &t.kind {
                super::TileKind::Process(name) => Some(name.clone()),
                _ => None,
            })
            .expect("a process");
        assert!(rig.scene.wheel(1), "the rig has more groups than fit");
        rig.paint(Repaint::new(false, false));
        assert!(rig.scene.is_animating(), "tiles glide into their new cells");
        let still_shown = rig
            .scene
            .field()
            .arrangement()
            .tiles
            .iter()
            .any(|t| t.kind == super::TileKind::Process(first.clone()));
        assert!(!still_shown, "the largest slid out of view");
        rig.settle();
        let band_after = &rig.buf[..band_before.len()];
        assert!(
            band_after == band_before.as_slice(),
            "the band never repainted"
        );
    }

    #[test]
    fn grouped_processes_become_a_single_tile_with_collective_totals() {
        let rig = Rig::new();
        let group = rig.scene.field().group("kworker").expect("one family");
        assert_eq!(group.count, 8);
        assert_eq!(group.memory_kb, 80_000);
        assert!((group.cpu - 24.0).abs() < 1e-3);
    }
}

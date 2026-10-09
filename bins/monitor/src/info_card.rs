//! [`InfoCard`] -- the static identity of the machine's devices in one tile.
//!
//! Each device gets its name and one identity line. Live values belong on the
//! chart tiles. The card picks the fewest columns that fit the region.

use platform::draw::{Canvas, Rect};
use statusbar::TextRenderer;

use crate::dashboard_report::{CardReport, DashboardReport};
use crate::palette::MonitorPalette;
use crate::text_layout::TextLayout;
use crate::typography::Typography;

struct Plan {
    columns: usize,
    width: i32,
}

pub struct InfoCard<'a> {
    palette: &'a MonitorPalette,
    typography: &'a mut Typography,
}

impl<'a> InfoCard<'a> {
    const COLUMN_GAP: i32 = 16;
    const ENTRY_GAP: i32 = 10;
    const COLUMN_CHOICES: [usize; 3] = [1, 2, 4];
    const MIN_COLUMN_LINES: i32 = 8;
    const UNKNOWN: &'static str = "—";

    pub const fn new(palette: &'a MonitorPalette, typography: &'a mut Typography) -> Self {
        Self {
            palette,
            typography,
        }
    }

    const fn cards(report: &DashboardReport) -> [&CardReport; 4] {
        [
            &report.cpu,
            &report.memory,
            &report.temperature,
            &report.network,
        ]
    }

    pub fn paint(&mut self, canvas: &mut Canvas<'_>, region: Rect, report: &DashboardReport) {
        let cards = Self::cards(report);
        let plan = self.plan(cards.len(), region);
        let entry = self.entry_height();
        let mut top = region.y;
        for row in cards.chunks(plan.columns) {
            if top + entry > region.bottom() {
                break;
            }
            for (column, card) in row.iter().enumerate() {
                let x =
                    region.x + i32::try_from(column).unwrap_or(0) * (plan.width + Self::COLUMN_GAP);
                self.entry(canvas, card, Rect::new(x, top, plan.width, entry));
            }
            top += entry + Self::ENTRY_GAP;
        }
    }

    fn entry_height(&self) -> i32 {
        self.typography.subtitle.line_height() + self.typography.title.line_height()
    }

    fn plan(&self, entries: usize, region: Rect) -> Plan {
        for columns in Self::COLUMN_CHOICES {
            let width = Self::column_width(region.w, columns);
            if columns > 1 && width < self.typography.title.line_height() * Self::MIN_COLUMN_LINES {
                continue;
            }
            let rows = i32::try_from(entries.div_ceil(columns)).unwrap_or(i32::MAX);
            let needed = rows * self.entry_height() + (rows - 1) * Self::ENTRY_GAP;
            if width > 0 && needed <= region.h {
                return Plan { columns, width };
            }
        }
        Plan {
            columns: 1,
            width: region.w.max(1),
        }
    }

    fn column_width(total: i32, columns: usize) -> i32 {
        let gaps = i32::try_from(columns.saturating_sub(1)).unwrap_or(0);
        let columns = i32::try_from(columns).unwrap_or(1);
        (total - Self::COLUMN_GAP * gaps) / columns
    }

    fn fit(renderer: &mut TextRenderer, text: &str, width: i32) -> Option<String> {
        let fitted = TextLayout::truncate_prefix(text, renderer, width);
        (!fitted.is_empty() && renderer.measure(&fitted) <= width).then_some(fitted)
    }

    fn entry(&mut self, canvas: &mut Canvas<'_>, card: &CardReport, bounds: Rect) {
        let palette = self.palette;
        let label_height = self.typography.subtitle.line_height();
        if let Some(label) = Self::fit(&mut self.typography.subtitle, card.title, bounds.w) {
            self.typography.subtitle.draw_text(
                canvas,
                &label,
                bounds.x,
                bounds.y,
                palette.faint,
                palette.surface,
            );
        }
        let identity = if card.meta.is_empty() {
            Self::UNKNOWN
        } else {
            &card.meta
        };
        if let Some(text) = Self::fit(&mut self.typography.title, identity, bounds.w) {
            self.typography.title.draw_text(
                canvas,
                &text,
                bounds.x,
                bounds.y + label_height,
                palette.dim,
                palette.surface,
            );
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::manual_assert_eq,
    clippy::cast_sign_loss,
    reason = "assert_eq on pixel buffers prints every pixel on failure"
)]
mod tests {
    use super::InfoCard;
    use crate::dashboard_report::DashboardReport;
    use crate::palette::MonitorPalette;
    use crate::typography::Typography;
    use platform::draw::{Canvas, Rect};
    use sysmon::MonitorSample;

    const W: i32 = 700;
    const H: i32 = 500;

    fn sample(cpu: f32) -> MonitorSample {
        MonitorSample {
            cpu,
            cpu_brand: "Test CPU With A Rather Long Marketing Name".to_owned(),
            cpu_cores: 8,
            memory_device: "32 GB".to_owned(),
            thermal_device: "k10temp".to_owned(),
            network_device: "eth0".to_owned(),
            ..Default::default()
        }
    }

    fn paint_sample(region: Rect, sample: &MonitorSample) -> Vec<u32> {
        let palette = MonitorPalette::builtin();
        let mut typography = Typography::embedded([32.0, 14.0, 13.0, 11.0]);
        let mut buf = vec![0u32; (W * H) as usize];
        let mut canvas = Canvas::new(&mut buf, W, H);
        canvas.clear(palette.surface);
        let report = DashboardReport::from_sample(sample, &[]);
        InfoCard::new(&palette, &mut typography).paint(&mut canvas, region, &report);
        buf
    }

    fn paint(region: Rect) -> Vec<u32> {
        paint_sample(region, &sample(42.0))
    }

    fn blank() -> Vec<u32> {
        let mut buf = vec![0u32; (W * H) as usize];
        Canvas::new(&mut buf, W, H).clear(MonitorPalette::builtin().surface);
        buf
    }

    fn changed_outside(painted: &[u32], region: Rect) -> bool {
        let blank = blank();
        (0..H).any(|y| {
            (0..W).any(|x| {
                let inside = region.contains(platform::draw::Point::new(x, y));
                let at = (y * W + x) as usize;
                !inside && painted[at] != blank[at]
            })
        })
    }

    #[test]
    fn text_lands_inside_the_region_and_nowhere_else() {
        for region in [
            Rect::new(100, 80, 480, 360),
            Rect::new(100, 80, 260, 400),
            Rect::new(50, 50, 600, 120),
            Rect::new(100, 80, 90, 60),
        ] {
            let painted = paint(region);
            assert!(painted != blank(), "{region:?} drew nothing");
            assert!(!changed_outside(&painted, region), "{region:?} spilled");
        }
    }

    #[test]
    fn a_region_with_no_room_draws_nothing() {
        let painted = paint(Rect::new(100, 80, 200, 5));
        assert!(painted == blank());
    }

    #[test]
    fn a_roomy_region_shows_more_than_a_cramped_one() {
        let count = |region| {
            let blank = blank();
            paint(region)
                .iter()
                .zip(&blank)
                .filter(|(a, b)| a != b)
                .count()
        };
        assert!(count(Rect::new(20, 20, 640, 440)) > count(Rect::new(20, 20, 640, 50)));
    }

    #[test]
    fn live_readings_never_change_the_card() {
        let region = Rect::new(20, 20, 640, 440);
        let busy = MonitorSample {
            memory: 90.0,
            temperature: 80.0,
            network_rx: 5.0,
            ..sample(99.0)
        };
        assert!(paint_sample(region, &sample(1.0)) == paint_sample(region, &busy));
    }
}

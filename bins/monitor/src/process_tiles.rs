//! [`ProcessTiles`] -- paints the process-group tiles and the "more" tile.

use platform::draw::{Canvas, Color, Rect};

use crate::palette::MonitorPalette;
use crate::size_label::SizeLabel;
use crate::text_layout::TextLayout;
use crate::tile_look::TileLook;
use crate::tile_metrics::TileMetrics;
use crate::typography::Typography;

/// What a process tile shows, already smoothed.
#[derive(Clone, Copy, Debug)]
pub struct ProcessFace<'a> {
    pub name: &'a str,
    pub memory_kb: u64,
    pub cpu: f64,
    pub count: usize,
    pub heat: u8,
}

pub struct ProcessTiles<'a> {
    palette: &'a MonitorPalette,
    metrics: TileMetrics,
}

impl<'a> ProcessTiles<'a> {
    const SECONDARY_BLEND: u8 = 170;

    #[must_use]
    pub const fn new(palette: &'a MonitorPalette, metrics: TileMetrics) -> Self {
        Self { palette, metrics }
    }

    pub fn paint(
        &self,
        canvas: &mut Canvas<'_>,
        typography: &mut Typography,
        cell: Rect,
        face: &ProcessFace<'_>,
    ) {
        let body = TileLook::body(cell, self.metrics.gutter, face.heat);
        let fill = TileLook::fill(self.palette.surface, self.palette.cpu, face.heat);
        let lines = [
            face.name.to_owned(),
            SizeLabel::kb(face.memory_kb),
            Self::activity(face),
        ];
        self.card(canvas, typography, body, fill, &lines);
    }

    pub fn paint_more(
        &self,
        canvas: &mut Canvas<'_>,
        typography: &mut Typography,
        cell: Rect,
        groups: usize,
        memory_kb: u64,
    ) {
        let body = cell.inset(self.metrics.gutter);
        let lines = [
            format!("+{groups} more"),
            SizeLabel::kb(memory_kb),
            String::new(),
        ];
        self.card(canvas, typography, body, self.palette.surface, &lines);
    }

    fn activity(face: &ProcessFace<'_>) -> String {
        if face.count > 1 {
            format!("{:.0}% cpu · {} procs", face.cpu, face.count)
        } else {
            format!("{:.0}% cpu", face.cpu)
        }
    }

    fn card(
        &self,
        canvas: &mut Canvas<'_>,
        typography: &mut Typography,
        body: Rect,
        fill: Color,
        lines: &[String; 3],
    ) {
        if body.w <= 0 || body.h <= 0 {
            return;
        }
        let palette = self.palette;
        canvas.fill_round_rect(body, fill, self.metrics.radius);
        canvas.draw_round_rect(body, palette.border, self.metrics.radius);
        let ink = TileLook::ink(fill, palette.foreground, palette.background);
        let quiet = fill.blend_opaque_color(ink, Self::SECONDARY_BLEND);
        let pad = self.metrics.pad;
        let room = body.w - pad * 2;
        let tall = body.h >= typography.title.line_height() * 4;
        let mut y = body.y + pad;
        let limit = body.bottom() - pad / 2;

        let heading = if tall {
            &mut typography.title
        } else {
            &mut typography.subtitle
        };
        if y + heading.line_height() > limit {
            return;
        }
        let name = TextLayout::truncate_prefix(&lines[0], heading, room);
        heading.draw_text(canvas, &name, body.x + pad, y, ink, fill);
        y += heading.line_height();

        for text in &lines[1..] {
            let tier = &mut typography.normal;
            if text.is_empty() || y + tier.line_height() > limit {
                break;
            }
            let fitted = TextLayout::truncate_prefix(text, tier, room);
            tier.draw_text(canvas, &fitted, body.x + pad, y, quiet, fill);
            y += tier.line_height();
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::manual_assert_eq,
    reason = "assert_eq on pixel buffers prints every pixel on failure"
)]
mod tests {
    use super::{ProcessFace, ProcessTiles};
    use crate::palette::MonitorPalette;
    use crate::tile_look::TileHeat;
    use crate::tile_metrics::TileMetrics;
    use crate::typography::Typography;
    use platform::draw::{Canvas, Rect};

    const W: i32 = 500;
    const H: i32 = 400;

    fn face(heat: u8) -> ProcessFace<'static> {
        ProcessFace {
            name: "firefox-with-a-very-long-name-that-cannot-fit",
            memory_kb: 2_000_000,
            cpu: 41.0,
            count: 5,
            heat,
        }
    }

    fn paint(cell: Rect, heat: u8) -> Vec<u32> {
        let palette = MonitorPalette::builtin();
        let mut typography = Typography::embedded([32.0, 14.0, 13.0, 11.0]);
        let metrics = TileMetrics::new(typography.title.line_height());
        let mut buf = vec![0u32; (W * H) as usize];
        let mut canvas = Canvas::new(&mut buf, W, H);
        canvas.clear(palette.background);
        ProcessTiles::new(&palette, metrics).paint(&mut canvas, &mut typography, cell, &face(heat));
        buf
    }

    fn blank() -> Vec<u32> {
        let mut buf = vec![0u32; (W * H) as usize];
        Canvas::new(&mut buf, W, H).clear(MonitorPalette::builtin().background);
        buf
    }

    fn pixel(buf: &[u32], x: i32, y: i32) -> u32 {
        buf[usize::try_from(y * W + x).expect("in range")]
    }

    #[test]
    fn a_tile_paints_inside_its_cell_only() {
        let cell = Rect::new(100, 100, 240, 180);
        let painted = paint(cell, TileHeat::STEPS);
        let blank = blank();
        for y in 0..H {
            for x in 0..W {
                if !cell.contains(platform::draw::Point::new(x, y)) {
                    assert_eq!(pixel(&painted, x, y), pixel(&blank, x, y), "({x},{y})");
                }
            }
        }
        assert!(painted != blank);
    }

    #[test]
    fn a_busier_tile_is_larger_and_brighter() {
        let cell = Rect::new(100, 100, 240, 180);
        let cold = paint(cell, 0);
        let hot = paint(cell, TileHeat::STEPS);
        let lit = |buf: &[u32]| {
            let blank = blank();
            buf.iter().zip(&blank).filter(|(a, b)| a != b).count()
        };
        assert!(lit(&hot) > lit(&cold));
        assert_ne!(pixel(&hot, 110, 240), pixel(&cold, 110, 240));
    }

    #[test]
    fn a_degenerate_cell_draws_nothing() {
        assert!(paint(Rect::new(10, 10, 5, 5), 8) == blank());
    }

    #[test]
    fn the_more_tile_draws_inside_its_cell() {
        let palette = MonitorPalette::builtin();
        let mut typography = Typography::embedded([32.0, 14.0, 13.0, 11.0]);
        let metrics = TileMetrics::new(typography.title.line_height());
        let cell = Rect::new(100, 100, 200, 120);
        let mut buf = vec![0u32; (W * H) as usize];
        let mut canvas = Canvas::new(&mut buf, W, H);
        canvas.clear(palette.background);
        ProcessTiles::new(&palette, metrics).paint_more(
            &mut canvas,
            &mut typography,
            cell,
            12,
            3_000_000,
        );
        assert!(buf != blank());
        assert_eq!(pixel(&buf, 50, 50), pixel(&blank(), 50, 50));
    }
}

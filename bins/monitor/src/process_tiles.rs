//! [`ProcessTiles`] -- paints the process-group tiles and the "more" tile:
//! the name sized to the tile, and the numbers only while hovered.

use platform::draw::{Canvas, Color, Rect};

use crate::palette::MonitorPalette;
use crate::size_label::SizeLabel;
use crate::text_layout::TextLayout;
use crate::tile_growth::TileGrowth;
use crate::tile_look::TileLook;
use crate::tile_metrics::TileMetrics;
use crate::tile_text::TileText;
use crate::typography::Typography;

/// What a process tile shows, already smoothed.
#[derive(Clone, Copy, Debug)]
pub struct ProcessFace<'a> {
    pub name: &'a str,
    pub memory_kb: u64,
    pub cpu: f64,
    pub count: usize,
    pub heat: u8,
    pub hovered: bool,
    pub grown: u8,
}

/// What the "more" tile shows.
#[derive(Clone, Copy, Debug)]
pub struct MoreFace {
    pub groups: usize,
    pub memory_kb: u64,
    pub hovered: bool,
    pub grown: u8,
}

struct Card<'a> {
    body: Rect,
    fill: Color,
    name: &'a str,
    details: &'a [String],
    grown: u8,
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
        let details = if face.hovered {
            Self::details(face)
        } else {
            Vec::new()
        };
        let card = Card {
            body,
            fill,
            name: face.name,
            details: &details,
            grown: face.grown,
        };
        self.card(canvas, typography, &card);
    }

    pub fn paint_more(
        &self,
        canvas: &mut Canvas<'_>,
        typography: &mut Typography,
        cell: Rect,
        face: &MoreFace,
    ) {
        let details = if face.hovered {
            vec![SizeLabel::kb(face.memory_kb)]
        } else {
            Vec::new()
        };
        let name = format!("+{} more", face.groups);
        let card = Card {
            body: cell.inset(self.metrics.gutter),
            fill: self.palette.surface,
            name: &name,
            details: &details,
            grown: face.grown,
        };
        self.card(canvas, typography, &card);
    }

    fn details(face: &ProcessFace<'_>) -> Vec<String> {
        let activity = if face.count > 1 {
            format!("{:.0}% cpu · {} procs", face.cpu, face.count)
        } else {
            format!("{:.0}% cpu", face.cpu)
        };
        vec![SizeLabel::kb(face.memory_kb), activity]
    }

    /// The card at the size it has grown to, with its text sized for the
    /// full card so the size does not change as it lands, and fading in once
    /// the card is nearly full size.
    fn card(&self, canvas: &mut Canvas<'_>, typography: &mut Typography, card: &Card<'_>) {
        let whole = card.body;
        let shown = TileGrowth::cell(whole, card.grown);
        if shown.w <= 0 || shown.h <= 0 {
            return;
        }
        let palette = self.palette;
        let fill = card.fill;
        let radius = self.metrics.radius.min(shown.w / 2).min(shown.h / 2);
        canvas.fill_round_rect_glass(shown, fill, radius);
        canvas.draw_round_rect(shown, palette.border, radius);
        let alpha = TileGrowth::text(card.grown);
        if alpha == 0 {
            return;
        }
        let ink = TileLook::ink(fill, palette.foreground, palette.background);
        let quiet = fill.blend_opaque_color(ink, Self::SECONDARY_BLEND);
        let ink = fill.blend_opaque_color(ink, alpha);
        let quiet = fill.blend_opaque_color(quiet, alpha);
        let pad = self.metrics.pad.min(whole.h / 6).min(whole.w / 6);
        let ladder = &mut typography.ladder;
        let text = TileText::fit(
            ladder,
            card.name,
            whole.w - pad * 2,
            whole.h - pad * 2,
            card.details.len(),
        );
        let room = shown.w - pad * 2;
        let mut y = shown.y + pad;
        let limit = shown.bottom() - pad;

        let heading = ladder.step(text.name_step);
        if y + heading.line_height() > shown.bottom() {
            return;
        }
        let fitted = TextLayout::truncate_prefix(card.name, heading, room);
        heading.draw_text(canvas, &fitted, shown.x + pad, y, ink, fill);
        y += heading.line_height();

        let tier = ladder.step(text.detail_step);
        for line in card.details {
            if y + tier.line_height() > limit {
                break;
            }
            let fitted = TextLayout::truncate_prefix(line, tier, room);
            tier.draw_text(canvas, &fitted, shown.x + pad, y, quiet, fill);
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
    use super::{MoreFace, ProcessFace, ProcessTiles};
    use crate::palette::MonitorPalette;
    use crate::tile_look::TileHeat;
    use crate::tile_metrics::TileMetrics;
    use crate::typography::Typography;
    use platform::draw::{Canvas, Rect};

    const W: i32 = 500;
    const H: i32 = 400;

    fn face(heat: u8, hovered: bool) -> ProcessFace<'static> {
        ProcessFace {
            name: "firefox-with-a-very-long-name-that-cannot-fit",
            memory_kb: 2_000_000,
            cpu: 41.0,
            count: 5,
            heat,
            hovered,
            grown: u8::MAX,
        }
    }

    fn paint(cell: Rect, heat: u8) -> Vec<u32> {
        paint_face(cell, &face(heat, false))
    }

    fn paint_face(cell: Rect, face: &ProcessFace<'_>) -> Vec<u32> {
        let palette = MonitorPalette::builtin();
        let mut typography = Typography::embedded([32.0, 14.0, 13.0, 11.0]);
        let metrics = TileMetrics::new(typography.title.line_height());
        let mut buf = vec![0u32; (W * H) as usize];
        let mut canvas = Canvas::new(&mut buf, W, H);
        canvas.clear(palette.background);
        ProcessTiles::new(&palette, metrics).paint(&mut canvas, &mut typography, cell, face);
        buf
    }

    fn lit_rows(buf: &[u32], cell: Rect) -> usize {
        let inner = cell.inset(12);
        (inner.y..inner.bottom())
            .filter(|&y| {
                let first = pixel(buf, inner.x, y);
                (inner.x..inner.right()).any(|x| pixel(buf, x, y) != first)
            })
            .count()
    }

    #[test]
    fn numbers_show_only_while_hovered() {
        let cell = Rect::new(20, 20, 400, 300);
        let quiet = paint_face(cell, &face(0, false));
        let hovered = paint_face(cell, &face(0, true));
        assert!(quiet != hovered);
        assert!(lit_rows(&hovered, cell) > lit_rows(&quiet, cell));
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
    fn a_growing_tile_starts_as_nothing_and_stays_inside_its_full_card() {
        let cell = Rect::new(100, 100, 240, 180);
        let none = paint_face(
            cell,
            &ProcessFace {
                grown: 0,
                ..face(0, false)
            },
        );
        assert!(none == blank());
        let half = paint_face(
            cell,
            &ProcessFace {
                grown: 128,
                ..face(0, false)
            },
        );
        let blank = blank();
        let inner = Rect::new(150, 135, 140, 110);
        for y in 0..H {
            for x in 0..W {
                if !inner.contains(platform::draw::Point::new(x, y)) {
                    assert_eq!(pixel(&half, x, y), pixel(&blank, x, y), "({x},{y})");
                }
            }
        }
        assert!(half != blank);
    }

    #[test]
    fn a_degenerate_cell_draws_nothing() {
        assert!(paint(Rect::new(10, 10, 2, 2), 8) == blank());
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
            &MoreFace {
                groups: 12,
                memory_kb: 3_000_000,
                hovered: true,
                grown: u8::MAX,
            },
        );
        assert!(buf != blank());
        assert_eq!(pixel(&buf, 50, 50), pixel(&blank(), 50, 50));
    }
}

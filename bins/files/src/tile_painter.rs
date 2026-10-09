//! [`TilePainter`] -- draws the spiral's tiles from data the window gathered,
//! so painting needs no access to the browser.

use platform::draw::{Canvas, Color, Rect};
use statusbar::TextRenderer;

use crate::row_kind::RowKind;
use crate::theme::Theme;
use crate::typography::Typography;

/// What a tile stands for, as far as its colours are concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileFace {
    Item(RowKind),
    Overflow,
}

/// One tile's strings and states for a frame.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaintedTile {
    pub rect: Rect,
    pub slot: usize,
    pub face: TileFace,
    pub name: String,
    pub detail: String,
    pub name_color: Color,
    pub selected: bool,
    pub hovered: bool,
    pub drop_target: bool,
}

/// The rename box's contents, when it sits over one of the tiles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileRename {
    pub tile: Rect,
    pub text: String,
    pub before_caret: String,
    pub valid: bool,
}

pub struct TilePainter<'a> {
    theme: &'a Theme,
    type_scale: &'a mut Typography,
}

impl<'a> TilePainter<'a> {
    pub const fn new(theme: &'a Theme, type_scale: &'a mut Typography) -> Self {
        Self { theme, type_scale }
    }

    pub fn paint(&mut self, canvas: &mut Canvas<'_>, tiles: &[PaintedTile]) {
        for tile in tiles {
            self.paint_tile(canvas, tile);
        }
    }

    fn paint_tile(&mut self, canvas: &mut Canvas<'_>, tile: &PaintedTile) {
        let theme = self.theme;
        let rect = self.inner(tile.rect);
        if rect.w <= 0 || rect.h <= 0 {
            return;
        }
        let radius = theme.spacing.radius() * 2;
        let resting = self.resting(tile);
        let surface = self.fill(tile).unwrap_or(resting);
        match self.fill(tile) {
            Some(color) => canvas.fill_round_rect(rect, color, radius),
            None => canvas.fill_round_rect_glass(rect, resting, radius),
        }
        let outline = match tile.face {
            TileFace::Item(kind) if tile.drop_target => kind.selection_fill(theme),
            TileFace::Overflow if tile.hovered => theme.accent,
            _ => theme.sidebar_border,
        };
        canvas.draw_round_rect(rect, outline, radius);

        let pad = theme.spacing.m();
        let room = rect.w - pad * 2;
        let name_tier = if rect.h >= theme.spacing.tile_min() * 2 {
            &mut self.type_scale.title
        } else {
            &mut self.type_scale.body
        };
        let name = Self::fit(name_tier, &tile.name, room);
        let name_y = rect.y + pad;
        name_tier.draw_text(
            canvas,
            &name,
            rect.x + pad,
            name_y,
            tile.name_color,
            surface,
        );
        let below = name_y + name_tier.line_height();

        let detail_tier = &mut self.type_scale.detail;
        if below + detail_tier.line_height() <= rect.bottom() - pad / 2 {
            let detail = Self::fit(detail_tier, &tile.detail, room);
            let color = if tile.selected {
                theme.text
            } else {
                theme.muted
            };
            detail_tier.draw_text(canvas, &detail, rect.x + pad, below, color, surface);
        }
    }

    /// The tile's resting glass: the theme fill its colouring slot names.
    const fn resting(&self, tile: &PaintedTile) -> Color {
        self.theme.tile_fills[tile.slot % self.theme.tile_fills.len()]
    }

    /// The solid fill for a tile's state, or `None` for the resting glass.
    const fn fill(&self, tile: &PaintedTile) -> Option<Color> {
        let theme = self.theme;
        match tile.face {
            TileFace::Item(kind) if tile.selected => Some(kind.selection_fill(theme)),
            TileFace::Item(kind) if tile.hovered => Some(kind.hover_fill(theme)),
            TileFace::Overflow if tile.hovered => Some(theme.accent_surface),
            _ => None,
        }
    }

    /// The tile's cell less a gutter on every side, so neighbours read as
    /// separate cards.
    fn inner(&self, cell: Rect) -> Rect {
        let gap = self.theme.spacing.gutter();
        Rect::new(
            cell.x + gap,
            cell.y + gap,
            cell.w - gap * 2,
            cell.h - gap * 2,
        )
    }

    /// The tile's label area, where the rename box is drawn.
    fn label_box(&self, tile: Rect) -> Rect {
        let rect = self.inner(tile);
        let gap = self.theme.spacing.xs();
        let height = self.type_scale.body.line_height() + gap * 2;
        Rect::new(
            rect.x + gap,
            rect.y + gap,
            (rect.w - gap * 2).max(0),
            height.min(rect.h),
        )
    }

    pub fn paint_rename(&mut self, canvas: &mut Canvas<'_>, rename: &TileRename) {
        let theme = self.theme;
        let rect = self.label_box(rename.tile);
        let radius = theme.spacing.radius();
        canvas.fill_round_rect(rect, theme.filter_bg, radius);
        let border = if rename.valid {
            theme.focus_ring
        } else {
            theme.error
        };
        canvas.draw_round_rect(rect, border, radius);
        let body = &mut self.type_scale.body;
        let inner_x = rect.x + theme.spacing.s();
        let text_y = rect.y + (rect.h - body.line_height()) / 2;
        body.draw_text(
            canvas,
            &rename.text,
            inner_x,
            text_y,
            theme.text,
            theme.filter_bg,
        );
        let caret_x = inner_x + body.measure(&rename.before_caret);
        canvas.fill_rect(
            Rect::new(caret_x, text_y, theme.spacing.rule(), body.line_height()),
            theme.caret,
        );
    }

    /// `text`, cut with an ellipsis until it fits `width` pixels.
    fn fit(tier: &mut TextRenderer, text: &str, width: i32) -> String {
        if width <= 0 {
            return String::new();
        }
        if tier.measure(text) <= width {
            return text.to_owned();
        }
        let mut kept: Vec<char> = text.chars().collect();
        while !kept.is_empty() {
            kept.pop();
            let candidate: String = kept.iter().chain(std::iter::once(&'…')).collect();
            if tier.measure(&candidate) <= width {
                return candidate;
            }
        }
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{PaintedTile, TileFace, TilePainter, TileRename};
    use crate::row_kind::RowKind;
    use crate::theme::Theme;
    use crate::typography::Typography;
    use platform::draw::{Canvas, Rect};

    fn setup() -> (Theme, Typography) {
        let cfg = config::Config::default();
        (Theme::from_config(&cfg), Typography::load(&cfg, 1.0))
    }

    fn tile(rect: Rect) -> PaintedTile {
        PaintedTile {
            rect,
            slot: 0,
            face: TileFace::Item(RowKind::Leaf),
            name: "report.pdf".to_owned(),
            detail: "12 KiB".to_owned(),
            name_color: Color::WHITE,
            selected: false,
            hovered: false,
            drop_target: false,
        }
    }

    use platform::draw::Color;

    fn paint(tiles: &[PaintedTile]) -> Vec<u32> {
        let (theme, mut type_scale) = setup();
        let mut buf = vec![0u32; 400 * 300];
        {
            let mut canvas = Canvas::new(&mut buf, 400, 300);
            canvas.clear(theme.background);
            TilePainter::new(&theme, &mut type_scale).paint(&mut canvas, tiles);
        }
        buf
    }

    fn pixel(buf: &[u32], x: i32, y: i32) -> u32 {
        buf[usize::try_from(y * 400 + x).expect("in range")]
    }

    #[test]
    fn a_tile_paints_inside_its_cell_and_nowhere_else() {
        let cell = Rect::new(100, 100, 150, 100);
        let blank = paint(&[]);
        let painted = paint(&[tile(cell)]);
        assert_eq!(pixel(&painted, 50, 50), pixel(&blank, 50, 50));
        assert_eq!(pixel(&painted, 260, 150), pixel(&blank, 260, 150));
        assert_ne!(painted, blank);
    }

    #[test]
    fn each_slot_paints_its_own_fill() {
        let cell = Rect::new(0, 0, 200, 150);
        let centre = |slot: usize| {
            let mut placed = tile(cell);
            placed.slot = slot;
            pixel(&paint(&[placed]), 100, 140)
        };
        let fills: Vec<u32> = (0..4).map(centre).collect();
        for (i, a) in fills.iter().enumerate() {
            for b in &fills[i + 1..] {
                assert_ne!(a, b);
            }
        }
        assert_eq!(centre(4), centre(0), "slots wrap");
    }

    #[test]
    fn selection_changes_the_fill() {
        let cell = Rect::new(0, 0, 200, 150);
        let plain = paint(&[tile(cell)]);
        let mut chosen = tile(cell);
        chosen.selected = true;
        assert_ne!(pixel(&paint(&[chosen]), 100, 140), pixel(&plain, 100, 140));
    }

    #[test]
    fn hovering_the_overflow_tile_lights_it() {
        let cell = Rect::new(0, 0, 200, 150);
        let mut more = tile(cell);
        more.face = TileFace::Overflow;
        let resting = paint(&[more.clone()]);
        more.hovered = true;
        assert_ne!(pixel(&paint(&[more]), 100, 140), pixel(&resting, 100, 140));
    }

    #[test]
    fn a_degenerate_cell_draws_nothing() {
        let blank = paint(&[]);
        assert_eq!(paint(&[tile(Rect::new(10, 10, 4, 4))]), blank);
    }

    #[test]
    fn long_names_are_cut_to_the_tile() {
        let (_, mut type_scale) = setup();
        let tier = &mut type_scale.body;
        let long = "a-very-long-file-name-that-cannot-fit.txt";
        let cut = TilePainter::fit(tier, long, 80);
        assert!(cut.ends_with('…'), "{cut}");
        assert!(tier.measure(&cut) <= 80);
        assert_eq!(TilePainter::fit(tier, "ok", 80), "ok");
        assert_eq!(TilePainter::fit(tier, long, 0), "");
    }

    #[test]
    fn the_rename_box_draws_over_the_tile() {
        let (theme, mut type_scale) = setup();
        let cell = Rect::new(0, 0, 200, 150);
        let mut before = vec![0u32; 400 * 300];
        let mut after = before.clone();
        for (buf, rename) in [(&mut before, false), (&mut after, true)] {
            let mut canvas = Canvas::new(buf, 400, 300);
            let mut painter = TilePainter::new(&theme, &mut type_scale);
            painter.paint(&mut canvas, &[tile(cell)]);
            if rename {
                painter.paint_rename(
                    &mut canvas,
                    &TileRename {
                        tile: cell,
                        text: "new".to_owned(),
                        before_caret: "new".to_owned(),
                        valid: true,
                    },
                );
            }
        }
        assert_ne!(before, after);
    }
}

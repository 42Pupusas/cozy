//! [`TilePainter`] -- draws the spiral's tiles from data the window gathered,
//! so painting needs no access to the browser.
//!
//! A tile answers the pointer with motion rather than a new fill: it rises
//! off the field with a soft shadow beneath, its border brightens, and its
//! action buttons fade in. Each of those is a level from 0 to 255 supplied
//! by the caller, so the painter holds no clock and draws any frame of the
//! glide on demand.

use platform::draw::{Canvas, Color, Rect};
use statusbar::TextRenderer;

use crate::action_icon::ActionIcon;
use crate::row_kind::RowKind;
use crate::spacing::Spacing;
use crate::theme::Theme;
use crate::tile_action::TileAction;
use crate::tile_geometry::TileGeometry;
use crate::typography::Typography;

/// What a tile stands for, as far as its colours are concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileFace {
    Item(RowKind),
    Overflow,
}

/// One tile's strings and states for a frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaintedTile {
    pub rect: Rect,
    pub slot: usize,
    pub face: TileFace,
    pub name: String,
    pub detail: String,
    pub name_color: Color,
    pub selected: bool,
    pub drop_target: bool,
    pub raise: u8,
    pub reveal: u8,
    pub glow: [u8; TileAction::ALL.len()],
    pub press: [u8; TileAction::ALL.len()],
}

/// The rename box's contents, when it sits over one of the tiles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileRename {
    pub tile: Rect,
    pub text: String,
    pub before_caret: String,
    pub valid: bool,
    pub raise: u8,
}

pub struct TilePainter<'a> {
    theme: &'a Theme,
    type_scale: &'a mut Typography,
}

impl<'a> TilePainter<'a> {
    /// How far the border travels from its resting colour toward the text
    /// colour at full rise, out of 255.
    const BORDER_BRIGHTENING: u16 = 170;
    /// Stacked translucent layers that make up the shadow, and the strength
    /// of each at full rise.
    const SHADOW_LAYERS: i32 = 3;
    const SHADOW_ALPHA: u16 = 34;
    /// The wash behind an action button once revealed, and the extra it gains
    /// under the pointer.
    const BUTTON_WASH: u16 = 22;
    const BUTTON_GLOW: u16 = 40;
    /// The accent wash of a button held down.
    const BUTTON_PRESS: u16 = 110;

    pub const fn new(theme: &'a Theme, type_scale: &'a mut Typography) -> Self {
        Self { theme, type_scale }
    }

    /// Paint `tiles`, the most raised last so a rising tile and its shadow
    /// land over its neighbours.
    pub fn paint(&mut self, canvas: &mut Canvas<'_>, tiles: &[PaintedTile]) {
        let mut order: Vec<&PaintedTile> = tiles.iter().collect();
        order.sort_by_key(|tile| tile.raise);
        for tile in order {
            self.paint_tile(canvas, tile);
        }
    }

    fn paint_tile(&mut self, canvas: &mut Canvas<'_>, tile: &PaintedTile) {
        let theme = self.theme;
        let geometry = TileGeometry::of(tile.rect, theme.spacing, self.type_scale.tile_lines());
        let resting = geometry.face;
        if resting.w <= 0 || resting.h <= 0 {
            return;
        }
        let lift = Self::share(theme.spacing.tile_lift(), tile.raise);
        let rect = Rect::new(resting.x, resting.y - lift, resting.w, resting.h);
        let radius = theme.spacing.radius() * 2;
        self.paint_shadow(canvas, rect, radius, tile.raise);
        let glass = self.resting(tile);
        let surface = self.selected_fill(tile).unwrap_or(glass);
        match self.selected_fill(tile) {
            Some(color) => canvas.fill_round_rect(rect, color, radius),
            None => canvas.fill_round_rect_glass(rect, glass, radius),
        }
        canvas.draw_round_rect(rect, self.outline(tile), radius);

        let pad = theme.spacing.m();
        let room = rect.w - pad * 2;
        let name_tier = if geometry.roomy {
            &mut self.type_scale.title
        } else {
            &mut self.type_scale.body
        };
        let name = Self::fit(name_tier, &tile.name, room);
        name_tier.draw_text(
            canvas,
            &name,
            rect.x + pad,
            geometry.name_y - lift,
            tile.name_color,
            surface,
        );
        if let Some(detail_y) = geometry.detail_y {
            let detail_tier = &mut self.type_scale.detail;
            let detail = Self::fit(detail_tier, &tile.detail, room);
            let color = if tile.selected {
                theme.text
            } else {
                theme.muted
            };
            detail_tier.draw_text(
                canvas,
                &detail,
                rect.x + pad,
                detail_y - lift,
                color,
                surface,
            );
        }
        self.paint_actions(canvas, tile, &geometry, surface);
    }

    /// How far the buttons of a tile sit from their resting place: up with
    /// the tile as it rises, and down by the lift while they are still
    /// appearing, so they settle into position as they fade in.
    ///
    /// The one definition the painter and the pointer handler share.
    #[must_use]
    pub fn button_shift(spacing: Spacing, raise: u8, reveal: u8) -> i32 {
        Self::share(spacing.tile_lift(), u8::MAX - reveal) - Self::share(spacing.tile_lift(), raise)
    }

    /// A soft edge under a rising tile: the tile's shape, dropped by the
    /// lift, laid down in growing translucent layers. The tile covers its
    /// own part of it, leaving a fringe at the sides and below.
    fn paint_shadow(&self, canvas: &mut Canvas<'_>, rect: Rect, radius: i32, raise: u8) {
        let alpha = Self::scaled(Self::SHADOW_ALPHA, raise);
        if alpha == 0 {
            return;
        }
        let drop = self.theme.spacing.tile_lift();
        let base = Rect::new(rect.x, rect.y + drop, rect.w, rect.h);
        for grow in 0..Self::SHADOW_LAYERS {
            canvas.blend_round_rect(base.inset(-grow), Color::BLACK, radius + grow, alpha);
        }
    }

    /// The four buttons, faded in by `reveal` and rising into place as they
    /// appear. The one under the pointer grows and washes brighter; held
    /// down, it settles back to its size and takes the accent colour.
    fn paint_actions(
        &self,
        canvas: &mut Canvas<'_>,
        tile: &PaintedTile,
        geometry: &TileGeometry,
        surface: Color,
    ) {
        if tile.reveal == 0 || !matches!(tile.face, TileFace::Item(_)) {
            return;
        }
        let theme = self.theme;
        let shift = Self::button_shift(theme.spacing, tile.raise, tile.reveal);
        let radius = theme.spacing.radius();
        for (action, button) in geometry.bar.buttons() {
            let glow = tile.glow[action.index()];
            let press = tile.press[action.index()];
            let rect = Rect::new(button.x, button.y + shift, button.w, button.h);
            let swell = Self::share(theme.spacing.action_grow(), glow)
                - Self::share(theme.spacing.action_grow(), press);
            let rect = rect.inset(-swell);
            let wash = Self::scaled(Self::BUTTON_WASH, tile.reveal)
                .saturating_add(Self::scaled(Self::BUTTON_GLOW, glow));
            canvas.blend_round_rect(rect, theme.text, radius + swell, wash);
            canvas.blend_round_rect(
                rect,
                theme.accent,
                radius + swell,
                Self::scaled(Self::BUTTON_PRESS, press),
            );
            let dim = surface.blend_opaque_color(theme.muted, tile.reveal);
            let lit = dim.blend_opaque_color(theme.text, glow);
            let ink = lit.blend_opaque_color(theme.accent, press);
            ActionIcon::new(*action, rect, ink).paint(canvas);
        }
    }

    /// The border: its resting colour, drawn toward the text colour as the
    /// tile rises. A tile a drag would drop into keeps the selection colour.
    fn outline(&self, tile: &PaintedTile) -> Color {
        let theme = self.theme;
        match tile.face {
            TileFace::Item(kind) if tile.drop_target => kind.selection_fill(theme),
            _ => theme.sidebar_border.blend_opaque_color(
                theme.text,
                Self::scaled(Self::BORDER_BRIGHTENING, tile.raise),
            ),
        }
    }

    /// The tile's resting glass: the theme fill its colouring slot names.
    const fn resting(&self, tile: &PaintedTile) -> Color {
        self.theme.tile_fills[tile.slot % self.theme.tile_fills.len()]
    }

    /// The solid fill of a selected tile, or `None` for the resting glass.
    /// Pointing at a tile never changes its fill.
    const fn selected_fill(&self, tile: &PaintedTile) -> Option<Color> {
        match tile.face {
            TileFace::Item(kind) if tile.selected => Some(kind.selection_fill(self.theme)),
            _ => None,
        }
    }

    /// `level` of `whole` pixels, rounded: how far a glide has carried.
    fn share(whole: i32, level: u8) -> i32 {
        (whole * i32::from(level) + 127) / 255
    }

    /// `level` of `whole`, as an alpha.
    fn scaled(whole: u16, level: u8) -> u8 {
        u8::try_from(whole * u16::from(level) / 255).unwrap_or(u8::MAX)
    }

    /// The tile's label area, where the rename box is drawn.
    fn label_box(&self, tile: Rect, raise: u8) -> Rect {
        let spacing = self.theme.spacing;
        let rect = spacing.tile_face(tile);
        let lift = Self::share(spacing.tile_lift(), raise);
        let gap = spacing.xs();
        let height = self.type_scale.body.line_height() + gap * 2;
        Rect::new(
            rect.x + gap,
            rect.y + gap - lift,
            (rect.w - gap * 2).max(0),
            height.min(rect.h),
        )
    }

    pub fn paint_rename(&mut self, canvas: &mut Canvas<'_>, rename: &TileRename) {
        let theme = self.theme;
        let rect = self.label_box(rename.tile, rename.raise);
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
    use crate::tile_geometry::TileGeometry;
    use crate::typography::Typography;
    use platform::draw::{Canvas, Color, Rect};

    const WIDTH: i32 = 400;
    const HEIGHT: i32 = 300;

    struct Scene {
        theme: Theme,
        type_scale: Typography,
    }

    impl Scene {
        fn new() -> Self {
            let cfg = config::Config::default();
            Self {
                theme: Theme::from_config(&cfg),
                type_scale: Typography::load(&cfg, 1.0),
            }
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
                drop_target: false,
                raise: 0,
                reveal: 0,
                glow: [0; 4],
                press: [0; 4],
            }
        }

        fn raised(rect: Rect, raise: u8) -> PaintedTile {
            PaintedTile {
                raise,
                ..Self::tile(rect)
            }
        }

        fn paint(&mut self, tiles: &[PaintedTile]) -> Vec<u32> {
            let mut buf = vec![0u32; (WIDTH * HEIGHT) as usize];
            {
                let mut canvas = Canvas::new(&mut buf, WIDTH, HEIGHT);
                canvas.clear(self.theme.background);
                TilePainter::new(&self.theme, &mut self.type_scale).paint(&mut canvas, tiles);
            }
            buf
        }

        fn pixel(buf: &[u32], x: i32, y: i32) -> u32 {
            buf[usize::try_from(y * WIDTH + x).expect("in range")]
        }

        fn brightness(pixel: u32) -> u32 {
            (pixel >> 16 & 0xff) + (pixel >> 8 & 0xff) + (pixel & 0xff)
        }

        fn geometry(&self, cell: Rect) -> TileGeometry {
            TileGeometry::of(cell, self.theme.spacing, self.type_scale.tile_lines())
        }
    }

    #[test]
    fn a_tile_paints_inside_its_cell_and_nowhere_else() {
        let mut scene = Scene::new();
        let cell = Rect::new(100, 100, 150, 100);
        let blank = scene.paint(&[]);
        let painted = scene.paint(&[Scene::tile(cell)]);
        assert_eq!(Scene::pixel(&painted, 50, 50), Scene::pixel(&blank, 50, 50));
        assert_eq!(
            Scene::pixel(&painted, 260, 150),
            Scene::pixel(&blank, 260, 150)
        );
        assert_ne!(painted, blank);
    }

    #[test]
    fn a_fully_raised_tile_with_buttons_still_stays_inside_its_cell() {
        let mut scene = Scene::new();
        let cell = Rect::new(100, 80, 200, 150);
        let blank = scene.paint(&[]);
        let mut tile = Scene::raised(cell, 255);
        tile.reveal = 255;
        tile.glow = [255; 4];
        let painted = scene.paint(&[tile]);
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                if !cell.contains(platform::draw::Point::new(x, y)) {
                    assert_eq!(
                        Scene::pixel(&painted, x, y),
                        Scene::pixel(&blank, x, y),
                        "a raised tile reached ({x}, {y}) outside its cell"
                    );
                }
            }
        }
    }

    #[test]
    fn each_slot_paints_its_own_fill() {
        let mut scene = Scene::new();
        let cell = Rect::new(0, 0, 200, 150);
        let mut centre = |slot: usize| {
            let mut placed = Scene::tile(cell);
            placed.slot = slot;
            Scene::pixel(&scene.paint(&[placed]), 100, 140)
        };
        let fills: Vec<u32> = (0..4).map(&mut centre).collect();
        for (i, a) in fills.iter().enumerate() {
            for b in &fills[i + 1..] {
                assert_ne!(a, b);
            }
        }
        assert_eq!(centre(4), centre(0), "slots wrap");
    }

    #[test]
    fn selection_changes_the_fill() {
        let mut scene = Scene::new();
        let cell = Rect::new(0, 0, 200, 150);
        let plain = scene.paint(&[Scene::tile(cell)]);
        let mut chosen = Scene::tile(cell);
        chosen.selected = true;
        assert_ne!(
            Scene::pixel(&scene.paint(&[chosen]), 100, 140),
            Scene::pixel(&plain, 100, 140)
        );
    }

    #[test]
    fn pointing_at_a_tile_does_not_change_its_fill() {
        let mut scene = Scene::new();
        let cell = Rect::new(0, 0, 200, 150);
        let resting = scene.paint(&[Scene::tile(cell)]);
        let mut pointed = Scene::raised(cell, 255);
        pointed.reveal = 255;
        let lifted = scene.paint(&[pointed]);
        let lift = scene.theme.spacing.tile_lift();
        assert_eq!(
            Scene::pixel(&lifted, 100, 140 - lift),
            Scene::pixel(&resting, 100, 140),
            "the same fill sits where the face has moved to"
        );
    }

    #[test]
    fn a_raised_tile_moves_up_and_a_resting_one_does_not() {
        let mut scene = Scene::new();
        let cell = Rect::new(100, 100, 200, 150);
        let lift = scene.theme.spacing.tile_lift();
        let top = scene.geometry(cell).face.y;
        let blank = scene.paint(&[]);
        let resting = scene.paint(&[Scene::tile(cell)]);
        let raised = scene.paint(&[Scene::raised(cell, 255)]);
        let probe = (200, top - lift);
        assert_eq!(
            Scene::pixel(&resting, probe.0, probe.1),
            Scene::pixel(&blank, probe.0, probe.1)
        );
        assert_ne!(
            Scene::pixel(&raised, probe.0, probe.1),
            Scene::pixel(&blank, probe.0, probe.1),
            "the raised face reaches above where the resting one starts"
        );
    }

    #[test]
    fn the_rise_is_gradual() {
        let mut scene = Scene::new();
        let cell = Rect::new(100, 100, 200, 150);
        let frames: Vec<Vec<u32>> = [0u8, 60, 130, 200, 255]
            .iter()
            .map(|&raise| scene.paint(&[Scene::raised(cell, raise)]))
            .collect();
        let changed = frames.windows(2).filter(|pair| pair[0] != pair[1]).count();
        assert!(changed >= 3, "only {changed} of 4 steps changed the frame");
    }

    #[test]
    fn a_raised_border_is_brighter() {
        let mut scene = Scene::new();
        let cell = Rect::new(100, 100, 200, 150);
        let face = scene.geometry(cell).face;
        let lift = scene.theme.spacing.tile_lift();
        let resting = scene.paint(&[Scene::tile(cell)]);
        let raised = scene.paint(&[Scene::raised(cell, 255)]);
        let at_rest = Scene::pixel(&resting, face.x + face.w / 2, face.y);
        let hot = Scene::pixel(&raised, face.x + face.w / 2, face.y - lift);
        assert!(
            Scene::brightness(hot) > Scene::brightness(at_rest),
            "border {hot:06x} is not brighter than {at_rest:06x}"
        );
    }

    #[test]
    fn a_raised_tile_casts_a_shadow_below_it() {
        let mut scene = Scene::new();
        let cell = Rect::new(100, 100, 200, 150);
        let face = scene.geometry(cell).face;
        let blank = scene.paint(&[]);
        let raised = scene.paint(&[Scene::raised(cell, 255)]);
        let beneath = (face.x + face.w / 2, face.bottom());
        assert!(
            Scene::brightness(Scene::pixel(&raised, beneath.0, beneath.1))
                < Scene::brightness(Scene::pixel(&blank, beneath.0, beneath.1)),
            "the strip under a raised tile is not darkened"
        );
    }

    #[test]
    fn a_resting_tile_casts_no_shadow() {
        let mut scene = Scene::new();
        let cell = Rect::new(100, 100, 200, 150);
        let face = scene.geometry(cell).face;
        let blank = scene.paint(&[]);
        let resting = scene.paint(&[Scene::tile(cell)]);
        let beneath = (face.x + face.w / 2, face.bottom());
        assert_eq!(
            Scene::pixel(&resting, beneath.0, beneath.1),
            Scene::pixel(&blank, beneath.0, beneath.1)
        );
    }

    #[test]
    fn the_overflow_tile_rises_but_never_shows_buttons() {
        let mut scene = Scene::new();
        let cell = Rect::new(0, 0, 300, 200);
        let mut more = Scene::tile(cell);
        more.face = TileFace::Overflow;
        let resting = scene.paint(&[more.clone()]);
        more.raise = 255;
        let raised = scene.paint(&[more.clone()]);
        assert_ne!(raised, resting);
        more.reveal = 255;
        assert_eq!(
            scene.paint(&[more]),
            raised,
            "an overflow tile has no actions"
        );
    }

    #[test]
    fn revealed_buttons_draw_and_hidden_ones_do_not() {
        let mut scene = Scene::new();
        let cell = Rect::new(50, 50, 300, 200);
        let hidden = scene.paint(&[Scene::tile(cell)]);
        let mut shown = Scene::tile(cell);
        shown.reveal = 255;
        let revealed = scene.paint(&[shown.clone()]);
        assert_ne!(revealed, hidden);
        let buttons = scene.geometry(cell).bar.buttons().to_vec();
        assert_eq!(buttons.len(), 4, "a 300 x 200 tile has room for four");
        let inside = |buf: &[u32], rect: Rect| {
            (rect.y..rect.bottom())
                .flat_map(|y| (rect.x..rect.right()).map(move |x| (x, y)))
                .filter(|&(x, y)| Scene::pixel(buf, x, y) != Scene::pixel(&hidden, x, y))
                .count()
        };
        for (action, rect) in &buttons {
            assert!(inside(&revealed, *rect) > 8, "{action:?} drew nothing");
        }
        shown.reveal = 100;
        let partial = scene.paint(&[shown]);
        assert_ne!(partial, hidden);
        assert_ne!(partial, revealed);
    }

    #[test]
    fn the_button_under_the_pointer_is_lit_more_than_its_neighbours() {
        let mut scene = Scene::new();
        let cell = Rect::new(50, 50, 300, 200);
        let mut shown = Scene::tile(cell);
        shown.reveal = 255;
        let plain = scene.paint(&[shown.clone()]);
        shown.glow = [0, 255, 0, 0];
        let lit = scene.paint(&[shown]);
        let buttons = scene.geometry(cell).bar.buttons().to_vec();
        let differs = |rect: Rect| {
            (rect.y..rect.bottom())
                .flat_map(|y| (rect.x..rect.right()).map(move |x| (x, y)))
                .any(|(x, y)| Scene::pixel(&lit, x, y) != Scene::pixel(&plain, x, y))
        };
        assert!(differs(buttons[1].1), "the lit button did not change");
        for index in [0, 2, 3] {
            assert!(!differs(buttons[index].1), "button {index} changed too");
        }
    }

    #[test]
    fn a_degenerate_cell_draws_nothing() {
        let mut scene = Scene::new();
        let blank = scene.paint(&[]);
        assert_eq!(scene.paint(&[Scene::tile(Rect::new(10, 10, 4, 4))]), blank);
    }

    #[test]
    fn the_most_raised_tile_is_painted_last() {
        let mut scene = Scene::new();
        let left = Rect::new(20, 40, 200, 150);
        let right = Rect::new(190, 40, 200, 150);
        let one = scene.paint(&[Scene::raised(left, 255), Scene::tile(right)]);
        let other = scene.paint(&[Scene::tile(right), Scene::raised(left, 255)]);
        assert_eq!(one, other, "input order must not decide which is on top");
    }

    #[test]
    fn long_names_are_cut_to_the_tile() {
        let mut scene = Scene::new();
        let tier = &mut scene.type_scale.body;
        let long = "a-very-long-file-name-that-cannot-fit.txt";
        let cut = TilePainter::fit(tier, long, 80);
        assert!(cut.ends_with('…'), "{cut}");
        assert!(tier.measure(&cut) <= 80);
        assert_eq!(TilePainter::fit(tier, "ok", 80), "ok");
        assert_eq!(TilePainter::fit(tier, long, 0), "");
    }

    #[test]
    fn the_rename_box_draws_over_the_tile_and_follows_its_rise() {
        let mut scene = Scene::new();
        let cell = Rect::new(0, 0, 200, 150);
        let mut frames = Vec::new();
        for raise in [0u8, 255] {
            let mut buf = vec![0u32; (WIDTH * HEIGHT) as usize];
            let mut canvas = Canvas::new(&mut buf, WIDTH, HEIGHT);
            let mut painter = TilePainter::new(&scene.theme, &mut scene.type_scale);
            painter.paint(&mut canvas, &[Scene::raised(cell, raise)]);
            painter.paint_rename(
                &mut canvas,
                &TileRename {
                    tile: cell,
                    text: "new".to_owned(),
                    before_caret: "new".to_owned(),
                    valid: true,
                    raise,
                },
            );
            frames.push(buf);
        }
        let plain = scene.paint(&[Scene::tile(cell)]);
        assert_ne!(frames[0], plain, "the box is drawn");
        assert_ne!(frames[0], frames[1], "and moves with the tile");
    }
}

//! [`TileHeat`] and [`TileLook`] -- how a process tile's CPU use shows: a
//! busier tile fills more of its cell and takes a brighter fill.

use platform::draw::{Color, Rect};

pub struct TileHeat;

impl TileHeat {
    pub const STEPS: u8 = 16;

    /// Group CPU at which a tile is half hot.
    const KNEE_PERCENT: f32 = 25.0;

    /// CPU percentage (summed over cores) as a heat in `0..=STEPS`.
    #[must_use]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn of(cpu_percent: f32) -> u8 {
        let load = cpu_percent.max(0.0);
        let share = load / (load + Self::KNEE_PERCENT);
        (share * f32::from(Self::STEPS)).round() as u8
    }
}

pub struct TileLook;

impl TileLook {
    /// An idle tile fills this share of its cell; a fully hot one fills all
    /// of it.
    const IDLE_SCALE_PERMILLE: i32 = 880;

    const FULL_BLEND: i32 = 200;

    const LIGHT_FILL_LUMINANCE: u32 = 140;

    #[must_use]
    pub fn body(cell: Rect, gutter: i32, heat: u8) -> Rect {
        let base = cell.inset(gutter);
        let (width, height) = (base.w.max(0), base.h.max(0));
        let steps = i32::from(TileHeat::STEPS);
        let heat = i32::from(heat).min(steps);
        let permille =
            Self::IDLE_SCALE_PERMILLE + (1000 - Self::IDLE_SCALE_PERMILLE) * heat / steps;
        let (w, h) = (width * permille / 1000, height * permille / 1000);
        Rect::new(base.x + (width - w) / 2, base.y + (height - h) / 2, w, h)
    }

    #[must_use]
    pub fn fill(surface: Color, accent: Color, heat: u8) -> Color {
        let steps = i32::from(TileHeat::STEPS);
        let alpha = Self::FULL_BLEND * i32::from(heat).min(steps) / steps;
        surface.blend_opaque_color(accent, u8::try_from(alpha).unwrap_or(u8::MAX))
    }

    #[must_use]
    pub fn luminance(color: Color) -> u32 {
        (u32::from(color.r) * 299 + u32::from(color.g) * 587 + u32::from(color.b) * 114) / 1000
    }

    /// `dark` over a light fill, `light` over a dark one.
    #[must_use]
    pub fn ink(fill: Color, light: Color, dark: Color) -> Color {
        if Self::luminance(fill) > Self::LIGHT_FILL_LUMINANCE {
            dark
        } else {
            light
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{TileHeat, TileLook};
    use platform::draw::{Color, Rect};

    const CELL: Rect = Rect::new(100, 50, 300, 210);

    #[test]
    fn heat_rises_with_cpu_and_stays_in_range() {
        let steps: Vec<u8> = [0.0, 1.0, 25.0, 100.0, 800.0, 1e9]
            .iter()
            .map(|&cpu| TileHeat::of(cpu))
            .collect();
        assert_eq!(steps[0], 0);
        assert_eq!(steps[2], TileHeat::STEPS / 2);
        assert!(steps.windows(2).all(|pair| pair[0] <= pair[1]), "{steps:?}");
        assert!(steps.iter().all(|&s| s <= TileHeat::STEPS));
        assert_eq!(TileHeat::of(-5.0), 0);
        assert_eq!(TileHeat::of(f32::NAN), 0);
    }

    #[test]
    fn a_hot_tile_fills_its_cell_less_the_gutter() {
        assert_eq!(TileLook::body(CELL, 6, TileHeat::STEPS), CELL.inset(6));
    }

    #[test]
    fn an_idle_tile_is_most_of_a_hot_one_and_centred() {
        let hot = TileLook::body(CELL, 6, TileHeat::STEPS);
        let idle = TileLook::body(CELL, 6, 0);
        assert!(
            (idle.w * 1000 - hot.w * 880).abs() <= 1000,
            "{idle:?} {hot:?}"
        );
        assert!(
            (idle.h * 1000 - hot.h * 880).abs() <= 1000,
            "{idle:?} {hot:?}"
        );
        assert!(((idle.x - hot.x) - (hot.right() - idle.right())).abs() <= 1);
        assert!(((idle.y - hot.y) - (hot.bottom() - idle.bottom())).abs() <= 1);
    }

    #[test]
    fn tiles_grow_monotonically_with_heat() {
        let widths: Vec<i32> = (0..=TileHeat::STEPS)
            .map(|heat| TileLook::body(CELL, 6, heat).w)
            .collect();
        assert!(
            widths.windows(2).all(|pair| pair[0] <= pair[1]),
            "{widths:?}"
        );
    }

    #[test]
    fn a_degenerate_cell_has_an_empty_body() {
        let body = TileLook::body(Rect::new(0, 0, 4, 4), 6, 8);
        assert!(body.w == 0 && body.h == 0);
    }

    #[test]
    fn hotter_tiles_are_brighter() {
        let surface = Color::rgb(0x18, 0x16, 0x16);
        let accent = Color::rgb(0x8a, 0x9a, 0x7b);
        assert_eq!(TileLook::fill(surface, accent, 0), surface);
        let luminances: Vec<u32> = (0..=TileHeat::STEPS)
            .map(|heat| TileLook::luminance(TileLook::fill(surface, accent, heat)))
            .collect();
        assert!(luminances.windows(2).all(|pair| pair[0] <= pair[1]));
        assert!(luminances[16] > luminances[0] + 40);
    }

    #[test]
    fn ink_contrasts_with_the_fill() {
        let light = Color::rgb(0xf2, 0xf2, 0xf3);
        let dark = Color::rgb(0x0a, 0x0a, 0x0b);
        assert_eq!(
            TileLook::ink(Color::rgb(0x18, 0x16, 0x16), light, dark),
            light
        );
        assert_eq!(
            TileLook::ink(Color::rgb(0xc0, 0xd0, 0xb0), light, dark),
            dark
        );
    }
}

//! [`FixedBand`] -- the short, wide strip along the top that holds the info
//! card and the four charts, so the rest of the window is left to processes.

use platform::draw::Rect;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FixedBand {
    area: Rect,
    height: i32,
}

impl FixedBand {
    const HEIGHT_DIVISOR: i32 = 5;
    const UNITS: [i32; 5] = [2, 1, 1, 1, 1];

    /// A band across the top of `area`, a fifth of its height but never
    /// shorter than `floor` nor taller than the area.
    #[must_use]
    pub fn new(area: Rect, floor: i32) -> Self {
        let area = Self::clamped(area);
        let height = (area.h / Self::HEIGHT_DIVISOR).max(floor).min(area.h);
        Self { area, height }
    }

    /// A band that takes all of `area`, for when there is nothing below it.
    #[must_use]
    pub fn filling(area: Rect) -> Self {
        let area = Self::clamped(area);
        Self {
            height: area.h,
            area,
        }
    }

    fn clamped(area: Rect) -> Rect {
        Rect::new(area.x, area.y, area.w.max(0), area.h.max(0))
    }

    /// What the band leaves for the process tiles.
    #[must_use]
    pub const fn rest(&self) -> Rect {
        Rect::new(
            self.area.x,
            self.area.y + self.height,
            self.area.w,
            self.area.h - self.height,
        )
    }

    /// The info card first, then the CPU, memory, temperature and network
    /// charts, left to right. The card is twice as wide as a chart.
    #[must_use]
    pub fn cells(&self) -> [Rect; 5] {
        let total: i32 = Self::UNITS.iter().sum();
        let mut cells = [Rect::new(0, 0, 0, 0); 5];
        let mut units = 0;
        let mut left = self.area.x;
        for (cell, share) in cells.iter_mut().zip(Self::UNITS) {
            units += share;
            let right = if units == total {
                self.area.right()
            } else {
                self.area.x + self.area.w * units / total
            };
            *cell = Rect::new(left, self.area.y, right - left, self.height);
            left = right;
        }
        cells
    }
}

#[cfg(test)]
mod tests {
    use super::FixedBand;
    use platform::draw::Rect;

    const AREA: Rect = Rect::new(18, 18, 1300, 800);

    #[test]
    fn the_band_and_the_rest_cover_the_area_exactly() {
        let band = FixedBand::new(AREA, 84);
        let rest = band.rest();
        let cells = band.cells();
        let across: i32 = cells.iter().map(|c| c.w).sum();
        assert_eq!(across, AREA.w);
        assert_eq!(cells[0].h + rest.h, AREA.h);
        assert_eq!(cells[0].bottom(), rest.y);
        assert_eq!(cells[4].right(), AREA.right());
    }

    #[test]
    fn the_band_is_a_fifth_of_a_tall_window() {
        let band = FixedBand::new(AREA, 84);
        assert_eq!(band.cells()[0].h, AREA.h / 5);
    }

    #[test]
    fn the_band_keeps_the_minimum_side_in_a_short_window() {
        let band = FixedBand::new(Rect::new(0, 0, 900, 400), 84);
        assert_eq!(band.cells()[0].h, 84);
    }

    #[test]
    fn a_filling_band_takes_everything() {
        let band = FixedBand::filling(AREA);
        assert_eq!(band.cells()[0].h, AREA.h);
        assert_eq!(band.rest().h, 0);
    }

    #[test]
    fn the_band_never_outgrows_the_area() {
        let band = FixedBand::new(Rect::new(0, 0, 200, 50), 84);
        assert_eq!(band.cells()[0].h, 50);
        assert_eq!(band.rest().h, 0);
    }

    #[test]
    fn the_info_card_is_wider_than_a_chart() {
        let cells = FixedBand::new(AREA, 84).cells();
        assert!(cells[0].w > cells[1].w);
        assert!(
            cells[1..]
                .windows(2)
                .all(|pair| pair[0].w.abs_diff(pair[1].w) <= 1)
        );
    }
}

//! [`SpiralSide`] -- one edge of the room left around the source, and how a
//! strip is cut from it.

use platform::draw::Rect;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpiralSide {
    Left,
    Top,
    Right,
    Bottom,
}

impl SpiralSide {
    pub const ALL: [Self; 4] = [Self::Left, Self::Top, Self::Right, Self::Bottom];

    /// The side after this one, turning clockwise.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Left => Self::Top,
            Self::Top => Self::Right,
            Self::Right => Self::Bottom,
            Self::Bottom => Self::Left,
        }
    }

    /// How much room lies between this edge of `rest` and the same edge of
    /// `source`.
    #[must_use]
    pub const fn gap(self, rest: Rect, source: Rect) -> i32 {
        let gap = match self {
            Self::Left => source.x - rest.x,
            Self::Top => source.y - rest.y,
            Self::Right => rest.right() - source.right(),
            Self::Bottom => rest.bottom() - source.bottom(),
        };
        if gap > 0 { gap } else { 0 }
    }

    /// How long a strip along this edge of `rest` is.
    #[must_use]
    pub const fn length(self, rest: Rect) -> i32 {
        match self {
            Self::Left | Self::Right => rest.h,
            Self::Top | Self::Bottom => rest.w,
        }
    }

    /// The strip `thickness` deep along this edge of `rest`.
    #[must_use]
    pub const fn strip(self, rest: Rect, thickness: i32) -> Rect {
        match self {
            Self::Left => Rect::new(rest.x, rest.y, thickness, rest.h),
            Self::Top => Rect::new(rest.x, rest.y, rest.w, thickness),
            Self::Right => Rect::new(rest.right() - thickness, rest.y, thickness, rest.h),
            Self::Bottom => Rect::new(rest.x, rest.bottom() - thickness, rest.w, thickness),
        }
    }

    /// What is left of `rest` once the strip `thickness` deep is cut away.
    #[must_use]
    pub const fn remainder(self, rest: Rect, thickness: i32) -> Rect {
        match self {
            Self::Left => Rect::new(rest.x + thickness, rest.y, rest.w - thickness, rest.h),
            Self::Top => Rect::new(rest.x, rest.y + thickness, rest.w, rest.h - thickness),
            Self::Right => Rect::new(rest.x, rest.y, rest.w - thickness, rest.h),
            Self::Bottom => Rect::new(rest.x, rest.y, rest.w, rest.h - thickness),
        }
    }

    /// The part of `strip` from `start` to `end` along its length, laid in
    /// the direction a clockwise turn travels along this edge.
    #[must_use]
    pub const fn piece(self, strip: Rect, start: i32, end: i32) -> Rect {
        match self {
            Self::Left => Rect::new(strip.x, strip.bottom() - end, strip.w, end - start),
            Self::Top => Rect::new(strip.x + start, strip.y, end - start, strip.h),
            Self::Right => Rect::new(strip.x, strip.y + start, strip.w, end - start),
            Self::Bottom => Rect::new(strip.right() - end, strip.y, end - start, strip.h),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SpiralSide;
    use platform::draw::Rect;

    const REST: Rect = Rect::new(0, 0, 1000, 600);
    const SOURCE: Rect = Rect::new(300, 200, 200, 100);

    #[test]
    fn the_gaps_are_the_room_on_each_side_of_the_source() {
        let gaps = SpiralSide::ALL.map(|side| side.gap(REST, SOURCE));
        assert_eq!(gaps, [300, 200, 500, 300]);
    }

    #[test]
    fn a_source_past_an_edge_leaves_no_gap_there() {
        let source = Rect::new(-20, 0, 100, 100);
        assert_eq!(SpiralSide::Left.gap(REST, source), 0);
    }

    #[test]
    fn a_strip_and_its_remainder_tile_the_rest() {
        for side in SpiralSide::ALL {
            let strip = side.strip(REST, 50);
            let rest = side.remainder(REST, 50);
            let area = |r: Rect| r.w * r.h;
            assert_eq!(area(strip) + area(rest), area(REST), "{side:?}");
            let apart = strip.right() <= rest.x
                || rest.right() <= strip.x
                || strip.bottom() <= rest.y
                || rest.bottom() <= strip.y;
            assert!(apart, "{side:?}: {strip:?} overlaps {rest:?}");
        }
    }

    #[test]
    fn a_strip_never_reaches_the_source_when_no_deeper_than_its_gap() {
        for side in SpiralSide::ALL {
            let strip = side.strip(REST, side.gap(REST, SOURCE));
            let apart = strip.right() <= SOURCE.x
                || SOURCE.right() <= strip.x
                || strip.bottom() <= SOURCE.y
                || SOURCE.bottom() <= strip.y;
            assert!(apart, "{side:?}");
        }
    }

    #[test]
    fn pieces_run_clockwise_and_cover_the_strip() {
        let top = SpiralSide::Top.strip(REST, 50);
        assert_eq!(SpiralSide::Top.piece(top, 0, 400).x, 0);
        let bottom = SpiralSide::Bottom.strip(REST, 50);
        assert_eq!(SpiralSide::Bottom.piece(bottom, 0, 400).right(), 1000);
        let left = SpiralSide::Left.strip(REST, 50);
        assert_eq!(SpiralSide::Left.piece(left, 0, 100).bottom(), 600);
        let right = SpiralSide::Right.strip(REST, 50);
        assert_eq!(SpiralSide::Right.piece(right, 0, 100).y, 0);
    }

    #[test]
    fn the_turn_visits_every_side_and_comes_back() {
        let mut side = SpiralSide::Left;
        for _ in 0..4 {
            side = side.next();
        }
        assert_eq!(side, SpiralSide::Left);
    }
}

//! [`RoundedSections`] -- the sections that show one solid rect.
//!
//! A square rect is one section; a rounded one is a cross of three solids
//! and four corner masks. A rounded rect always yields the same seven slots, so a
//! tile's sections stay with it while its radius or size changes.

use super::logical_rect::LogicalRect;
use super::quarter::Quarter;
use super::section_fill::SectionFill;
use super::section_state::SectionState;

pub struct RoundedSections;

impl RoundedSections {
    pub const ROUNDED_SLOTS: usize = 7;

    /// The sections for `rect` filled with `argb`, corners of `radius`
    /// logical pixels drawn from masks of `radius * scale` buffer pixels.
    pub fn expand(
        rect: LogicalRect,
        argb: u32,
        radius: i32,
        scale: i32,
        out: &mut Vec<SectionState>,
    ) {
        if radius <= 0 {
            out.push(SectionState {
                rect,
                fill: SectionFill::Solid(argb),
            });
            return;
        }
        let r = radius.min(rect.w / 2).min(rect.h / 2).max(0);
        let solid = |x, y, w, h| SectionState {
            rect: LogicalRect { x, y, w, h },
            fill: SectionFill::Solid(argb),
        };
        let LogicalRect { x, y, w, h } = rect;
        out.push(solid(x + r, y, w - 2 * r, r));
        out.push(solid(x, y + r, w, h - 2 * r));
        out.push(solid(x + r, y + h - r, w - 2 * r, r));
        let side = u16::try_from((r * scale.max(1)).max(0)).unwrap_or(u16::MAX);
        for (quarter, cx, cy) in [
            (Quarter::TopLeft, x, y),
            (Quarter::TopRight, x + w - r, y),
            (Quarter::BottomRight, x + w - r, y + h - r),
            (Quarter::BottomLeft, x, y + h - r),
        ] {
            out.push(SectionState {
                rect: LogicalRect {
                    x: cx,
                    y: cy,
                    w: r,
                    h: r,
                },
                fill: SectionFill::Corner {
                    argb,
                    side,
                    quarter,
                },
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LogicalRect, RoundedSections};
    use crate::scene::section_fill::SectionFill;
    use crate::scene::section_state::SectionState;

    const RECT: LogicalRect = LogicalRect {
        x: 10,
        y: 20,
        w: 100,
        h: 60,
    };

    fn expand(radius: i32) -> Vec<SectionState> {
        let mut out = Vec::new();
        RoundedSections::expand(RECT, 0xff11_2233, radius, 2, &mut out);
        out
    }

    fn area(states: &[SectionState]) -> i32 {
        states.iter().map(|s| s.rect.w * s.rect.h).sum()
    }

    #[test]
    fn a_square_rect_is_one_solid() {
        let out = expand(0);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].rect, RECT);
    }

    #[test]
    fn a_rounded_rect_tiles_its_bounds_without_overlap() {
        let out = expand(8);
        assert_eq!(out.len(), RoundedSections::ROUNDED_SLOTS);
        assert_eq!(area(&out), RECT.w * RECT.h);
        for (i, a) in out.iter().enumerate() {
            for b in &out[i + 1..] {
                let overlap_w =
                    (a.rect.x + a.rect.w).min(b.rect.x + b.rect.w) - a.rect.x.max(b.rect.x);
                let overlap_h =
                    (a.rect.y + a.rect.h).min(b.rect.y + b.rect.h) - a.rect.y.max(b.rect.y);
                assert!(overlap_w <= 0 || overlap_h <= 0, "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn corner_masks_are_sized_in_buffer_pixels() {
        let out = expand(8);
        let sides: Vec<u16> = out
            .iter()
            .filter_map(|s| match s.fill {
                SectionFill::Corner { side, .. } => Some(side),
                SectionFill::Solid(_) => None,
            })
            .collect();
        assert_eq!(sides, [16; 4]);
    }

    #[test]
    fn the_radius_is_capped_at_half_the_short_side() {
        let out = expand(500);
        assert_eq!(out.len(), RoundedSections::ROUNDED_SLOTS);
        assert_eq!(out[3].rect.w, RECT.h / 2);
        assert_eq!(out[1].rect.h, 0);
        assert_eq!(area(&out), RECT.w * RECT.h);
    }
}

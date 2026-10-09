//! [`SolidFrame`] -- what a section-composed window shows for one frame, in
//! physical pixels: a background colour and the solid rects above it, back
//! to front.

use crate::draw::{Color, Rect};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SolidRect {
    pub rect: Rect,
    pub color: Color,
    /// Corner radius in physical pixels; 0 is square.
    pub radius: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SolidFrame {
    pub background: Color,
    pub rects: Vec<SolidRect>,
}

impl SolidFrame {
    #[must_use]
    pub const fn new(background: Color) -> Self {
        Self {
            background,
            rects: Vec::new(),
        }
    }

    pub fn push(&mut self, rect: Rect, color: Color) {
        self.push_rounded(rect, color, 0);
    }

    pub fn push_rounded(&mut self, rect: Rect, color: Color, radius: u16) {
        self.rects.push(SolidRect {
            rect,
            color,
            radius,
        });
    }

    /// The premultiplied ARGB word a solid buffer of `color` holds.
    #[must_use]
    pub fn argb(color: Color) -> u32 {
        color.premultiplied_argb8888(color.a)
    }
}

#[cfg(test)]
mod tests {
    use super::SolidFrame;
    use crate::draw::Color;

    #[test]
    fn an_opaque_colour_packs_with_full_alpha() {
        assert_eq!(SolidFrame::argb(Color::rgb(0x10, 0x20, 0x30)), 0xff10_2030);
    }

    #[test]
    fn a_translucent_colour_is_premultiplied() {
        let argb = SolidFrame::argb(Color::rgba(200, 100, 50, 128));
        assert_eq!(argb >> 24, 128);
        assert_eq!((argb >> 16) & 0xff, 200 * 128 / 255);
    }
}

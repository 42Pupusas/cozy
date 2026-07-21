//! `StatusNotifier` tray-icon pixels and their software-canvas renderer.
//!
//! The `StatusNotifierItem` protocol exposes `IconPixmap` as one or more raw
//! ARGB32 pixmaps. Keeping their validation, size selection, and painting in
//! this module means the D-Bus watcher only has to obtain typed icon data; it
//! never needs to know about the bar's framebuffer or Wayland scaling.

use platform::draw::Canvas;

/// One `IconPixmap` entry supplied by a `StatusNotifierItem`.
///
/// The protocol stores pixels as 32-bit ARGB values in row-major order. The
/// vector is deliberately kept in wire order: `Canvas` uses the same packed
/// layout for its opaque XRGB path, and its ARGB path accepts the same channel
/// order after the icon's alpha has been composited by [`TrayIcon::paint`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayPixmap {
    width: i32,
    height: i32,
    argb: Vec<u32>,
}

impl TrayPixmap {
    /// Build a validated pixmap. Invalid dimensions or a mismatched pixel
    /// count are rejected instead of letting a misbehaving tray client cause
    /// an out-of-bounds read during bar painting.
    #[must_use]
    pub fn new(width: i32, height: i32, argb: Vec<u32>) -> Option<Self> {
        if width <= 0 || height <= 0 {
            return None;
        }
        let pixels = usize::try_from(width.checked_mul(height)?).ok()?;
        (argb.len() == pixels).then_some(Self {
            width,
            height,
            argb,
        })
    }

    /// Pixel width advertised by the tray item.
    #[must_use]
    pub const fn width(&self) -> i32 {
        self.width
    }

    /// Pixel height advertised by the tray item.
    #[must_use]
    pub const fn height(&self) -> i32 {
        self.height
    }

    /// Return the pixel at `(x, y)`. Coordinates are guaranteed in range by
    /// callers inside this module, so this remains a private indexing detail.
    fn pixel_at(&self, x: i32, y: i32) -> u32 {
        let index = usize::try_from(y * self.width + x).unwrap_or(0);
        self.argb[index]
    }
}

/// An item's normal or attention icon, potentially with several resolution alternatives.
///
/// The renderer chooses the smallest icon that can satisfy the requested
/// square, avoiding needless downsampling of a huge pixmap while still
/// preferring sharp enough artwork on `HiDPI` outputs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrayIcon {
    pixmaps: Vec<TrayPixmap>,
}

impl TrayIcon {
    /// Make an icon from the protocol's pixmap alternatives.
    #[must_use]
    pub const fn new(pixmaps: Vec<TrayPixmap>) -> Self {
        Self { pixmaps }
    }

    /// Whether this icon contains no paintable pixmap.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.pixmaps.is_empty()
    }

    /// Paint the best available pixmap into a `size × size` square at `(x,y)`.
    ///
    /// `StatusNotifier` pixmaps have straight ARGB alpha. The bar can itself be
    /// translucent, but tray artwork must remain opaque after it is blended
    /// over the already-cleared bar background; composing source-over here
    /// avoids black boxes around semi-transparent application logos.
    pub fn paint(&self, canvas: &mut Canvas<'_>, x: i32, y: i32, size: i32) {
        let Some(source) = self.best_for(size) else {
            return;
        };
        let target = size.max(1);
        let width = source.width.max(1);
        let height = source.height.max(1);
        // Read geometry before taking the canvas's mutable pixel borrow.
        let canvas_width = canvas.width();
        let canvas_height = canvas.height();
        let pixels = canvas.pixels_mut();

        for dy in 0..target {
            let py = y + dy;
            if py < 0 || py >= canvas_height {
                continue;
            }
            let sy = (dy * height / target).min(height - 1);
            for dx in 0..target {
                let px = x + dx;
                if px < 0 || px >= canvas_width {
                    continue;
                }
                let sx = (dx * width / target).min(width - 1);
                let src = source.pixel_at(sx, sy);
                let alpha = (src >> 24) as u8;
                if alpha == 0 {
                    continue;
                }
                let index = usize::try_from(py * canvas_width + px).unwrap_or(0);
                let dst = pixels[index];
                pixels[index] = Self::over(src, dst, alpha);
            }
        }
    }

    /// Choose the smallest square-ish pixmap at least `size` pixels wide or
    /// tall; if all are smaller, use the largest. `StatusNotifier` items often
    /// provide 16, 22, and 32 pixel variants, so this gives the expected icon
    /// at normal and `HiDPI` bar scales without a configuration knob.
    fn best_for(&self, size: i32) -> Option<&TrayPixmap> {
        let requested = size.max(1);
        self.pixmaps
            .iter()
            .filter(|p| p.width >= requested || p.height >= requested)
            .min_by_key(|p| p.width.max(p.height))
            .or_else(|| self.pixmaps.iter().max_by_key(|p| p.width.max(p.height)))
    }

    /// Composite one straight-ARGB source pixel over a packed destination.
    /// The destination's top byte is ignored on opaque bars and is retained
    /// on translucent ones; tray icons themselves are intentionally output as
    /// opaque ink, just like bar text glyphs.
    fn over(src: u32, dst: u32, alpha: u8) -> u32 {
        let a = u32::from(alpha);
        let inv = 255 - a;
        let r = ((src >> 16 & 0xff) * a + (dst >> 16 & 0xff) * inv) / 255;
        let g = ((src >> 8 & 0xff) * a + (dst >> 8 & 0xff) * inv) / 255;
        let b = ((src & 0xff) * a + (dst & 0xff) * inv) / 255;
        (r << 16) | (g << 8) | b
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform::draw::Color;

    #[test]
    fn pixmap_rejects_bad_dimensions_and_lengths() {
        assert!(TrayPixmap::new(0, 1, vec![]).is_none());
        assert!(TrayPixmap::new(2, 2, vec![0; 3]).is_none());
        assert!(TrayPixmap::new(2, 2, vec![0; 4]).is_some());
    }

    #[test]
    fn paint_selects_the_smallest_sufficient_pixmap() {
        let small = TrayPixmap::new(16, 16, vec![0xffff_0000; 256]).unwrap();
        let large = TrayPixmap::new(32, 32, vec![0xff00_ff00; 1024]).unwrap();
        let icon = TrayIcon::new(vec![small, large]);
        let mut pixels = vec![Color::BLACK.to_xrgb8888(); 24 * 24];
        let mut canvas = Canvas::new(&mut pixels, 24, 24);
        icon.paint(&mut canvas, 0, 0, 20);
        assert_eq!(pixels[0], Color::rgb(0, 255, 0).to_xrgb8888());
    }

    #[test]
    fn paint_alpha_composites_without_a_black_box() {
        let pixmap = TrayPixmap::new(1, 1, vec![0x80ff_0000]).unwrap();
        let icon = TrayIcon::new(vec![pixmap]);
        let mut pixels = vec![Color::rgb(0, 0, 255).to_xrgb8888()];
        let mut canvas = Canvas::new(&mut pixels, 1, 1);
        icon.paint(&mut canvas, 0, 0, 1);
        assert_eq!(pixels[0], 0x0080_007f);
    }
}

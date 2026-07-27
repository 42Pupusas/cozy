//! How a decoded wallpaper image is fit into an output's pixel dimensions
//! -- swaybg's `-m`/`mode` flag (`stretch`/`fit`/`fill`/`center`/`tile`),
//! reimplemented as one pure function per variant over plain RGBA8 buffers.
//!
//! Deliberately free of any Wayland/`platform` type: every variant here is
//! `(source pixels, source size) -> (target size) -> target pixels`, so it
//! is unit-testable with synthetic images and needs no live compositor --
//! same "isolable core" split as [`crate::WallpaperImage::load`]. The
//! eventual `bins/wallpaper` binary is the only thing that ever calls
//! [`ScaleMode::blit`] with a real decoded file and a real output size.

use crate::WallpaperImage;

/// Which of swaybg's five fit modes to apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaleMode {
    /// Scale both axes independently to exactly fill the target,
    /// distorting the aspect ratio if source and target differ.
    Stretch,
    /// Scale uniformly so the whole image is visible, letterboxed
    /// (padded with `pad`) on whichever axis has slack.
    Fit,
    /// Scale uniformly so the image covers the whole target with no
    /// padding, cropping whichever axis overflows.
    Fill,
    /// No scaling: center the image at 1:1, padding (or cropping, if the
    /// image is larger than the target) around it.
    Center,
    /// No scaling: repeat the image at 1:1 across the whole target.
    Tile,
}

impl ScaleMode {
    /// Render `image` into a `target_width * target_height` RGBA8 buffer
    /// per this mode, padding any uncovered area with `pad`.
    ///
    /// Returns a flat `target_width * target_height * 4`-byte buffer,
    /// row-major, matching [`WallpaperImage::rgba`]'s layout -- the
    /// caller (`bins/wallpaper`) blits this straight into its `wl_shm`
    /// backbuffer with a single `copy_from_slice` per row, or a whole-
    /// buffer one if the target is byte-identical to the shm layout.
    #[must_use]
    pub fn blit(
        self,
        image: &WallpaperImage,
        target_width: u32,
        target_height: u32,
        pad: [u8; 4],
    ) -> Vec<u8> {
        let mut out = vec![0u8; target_width as usize * target_height as usize * 4];
        for px in out.chunks_exact_mut(4) {
            px.copy_from_slice(&pad);
        }
        if target_width == 0 || target_height == 0 || image.width == 0 || image.height == 0 {
            return out;
        }

        match self {
            Self::Stretch => Self::blit_stretch(image, target_width, target_height, &mut out),
            Self::Fit => Self::blit_scaled(image, target_width, target_height, &mut out, true),
            Self::Fill => Self::blit_scaled(image, target_width, target_height, &mut out, false),
            Self::Center => {
                Self::blit_unscaled(image, target_width, target_height, &mut out, false);
            }
            Self::Tile => Self::blit_unscaled(image, target_width, target_height, &mut out, true),
        }
        out
    }

    /// Nearest-neighbor resample, independently per axis -- source and
    /// target aspect ratios needn't match; whichever differs more just
    /// looks stretched, exactly like swaybg's own `stretch` mode.
    fn blit_stretch(image: &WallpaperImage, tw: u32, th: u32, out: &mut [u8]) {
        for y in 0..th {
            let sy = y * image.height / th;
            for x in 0..tw {
                let sx = x * image.width / tw;
                Self::copy_pixel(image, sx, sy, out, x, y, tw);
            }
        }
    }

    /// Uniform-scale resample: one scale factor shared by both axes,
    /// chosen so the image either fits entirely inside the target
    /// (`letterbox = true`, swaybg's `fit`) or entirely covers it
    /// (`letterbox = false`, swaybg's `fill`), then centered.
    fn blit_scaled(image: &WallpaperImage, tw: u32, th: u32, out: &mut [u8], letterbox: bool) {
        let scale_x = f64::from(tw) / f64::from(image.width);
        let scale_y = f64::from(th) / f64::from(image.height);
        let scale = if letterbox {
            scale_x.min(scale_y)
        } else {
            scale_x.max(scale_y)
        };

        // Scaled image size, and the offset that centers it in the target
        // (negative when it overflows -- `fill` crops by walking past the
        // target's edges on the oversized axis).
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let scaled_w = (f64::from(image.width) * scale).round() as i64;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let scaled_h = (f64::from(image.height) * scale).round() as i64;
        let off_x = (i64::from(tw) - scaled_w) / 2;
        let off_y = (i64::from(th) - scaled_h) / 2;

        for y in 0..th {
            let dst_y = i64::from(y) - off_y;
            if dst_y < 0 || dst_y >= scaled_h {
                continue;
            }
            // `dst_y` is bounded by `scaled_h`, itself derived from a
            // real output/monitor pixel dimension times a scale factor --
            // nowhere near i64's 52-bit-mantissa-losing range, so the
            // widen-to-f64 for the division is exact in every realistic
            // case; the two casts below are the same deliberate, reviewed
            // pattern as `platform::draw`'s float<->int pixel conversions.
            #[allow(clippy::cast_precision_loss)]
            let row_pos = dst_y as f64;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let sy = ((row_pos / scale) as u32).min(image.height - 1);
            for x in 0..tw {
                let dst_x = i64::from(x) - off_x;
                if dst_x < 0 || dst_x >= scaled_w {
                    continue;
                }
                #[allow(clippy::cast_precision_loss)]
                let col_pos = dst_x as f64;
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let sx = ((col_pos / scale) as u32).min(image.width - 1);
                Self::copy_pixel(image, sx, sy, out, x, y, tw);
            }
        }
    }

    /// No resampling at all: place the image at native size, either once
    /// centered (`center`, swaybg's `center` mode) or repeated across the
    /// whole target (`tile`, swaybg's `tile` mode).
    fn blit_unscaled(image: &WallpaperImage, tw: u32, th: u32, out: &mut [u8], tile: bool) {
        let off_x = (i64::from(tw) - i64::from(image.width)) / 2;
        let off_y = (i64::from(th) - i64::from(image.height)) / 2;

        for y in 0..th {
            let sy = if tile {
                y % image.height
            } else {
                let dy = i64::from(y) - off_y;
                if dy < 0 || dy >= i64::from(image.height) {
                    continue;
                }
                // Already range-checked against `image.height` (a real
                // `u32` to begin with) just above, so this narrowing back
                // to `u32` can't actually truncate or lose sign.
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                {
                    dy as u32
                }
            };
            for x in 0..tw {
                let sx = if tile {
                    x % image.width
                } else {
                    let dx = i64::from(x) - off_x;
                    if dx < 0 || dx >= i64::from(image.width) {
                        continue;
                    }
                    // Same reasoning as `dy` above: already range-checked
                    // against `image.width`, so this can't truncate or
                    // lose sign.
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    {
                        dx as u32
                    }
                };
                Self::copy_pixel(image, sx, sy, out, x, y, tw);
            }
        }
    }

    /// Copy one RGBA8 pixel from `image` at `(sx, sy)` into `out` (a flat
    /// `tw`-wide buffer) at `(dx, dy)`. Shared by every blit variant above
    /// so the row-major index arithmetic exists exactly once.
    fn copy_pixel(
        image: &WallpaperImage,
        sx: u32,
        sy: u32,
        out: &mut [u8],
        dx: u32,
        dy: u32,
        tw: u32,
    ) {
        let src = (sy as usize * image.width as usize + sx as usize) * 4;
        let dst = (dy as usize * tw as usize + dx as usize) * 4;
        out[dst..dst + 4].copy_from_slice(&image.rgba[src..src + 4]);
    }
}

#[cfg(test)]
mod tests {
    use super::ScaleMode;
    use crate::WallpaperImage;

    /// A 2x2 image: top-left red, top-right green, bottom-left blue,
    /// bottom-right white -- distinct enough that any mixed-up axis or
    /// off-by-one in the blit math produces a visibly wrong pixel.
    fn tiny_image() -> WallpaperImage {
        WallpaperImage {
            width: 2,
            height: 2,
            rgba: vec![
                255, 0, 0, 255, // (0,0) red
                0, 255, 0, 255, // (1,0) green
                0, 0, 255, 255, // (0,1) blue
                255, 255, 255, 255, // (1,1) white
            ],
        }
    }

    fn pixel_at(buf: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
        let i = (y as usize * w as usize + x as usize) * 4;
        buf[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn stretch_fills_every_target_pixel_with_no_padding() {
        let img = tiny_image();
        let out = ScaleMode::Stretch.blit(&img, 4, 4, [0, 0, 0, 0]);
        // Each quadrant of the 4x4 target should be a solid block of the
        // corresponding source pixel's color under nearest-neighbor stretch.
        assert_eq!(pixel_at(&out, 4, 0, 0), [255, 0, 0, 255]);
        assert_eq!(pixel_at(&out, 4, 3, 0), [0, 255, 0, 255]);
        assert_eq!(pixel_at(&out, 4, 0, 3), [0, 0, 255, 255]);
        assert_eq!(pixel_at(&out, 4, 3, 3), [255, 255, 255, 255]);
    }

    #[test]
    fn center_places_image_at_native_size_with_padding_around_it() {
        let img = tiny_image();
        let pad = [10, 20, 30, 255];
        let out = ScaleMode::Center.blit(&img, 6, 6, pad);
        // A 2x2 image centered in a 6x6 target lands at offset (2, 2).
        assert_eq!(pixel_at(&out, 6, 2, 2), [255, 0, 0, 255]);
        assert_eq!(pixel_at(&out, 6, 3, 3), [255, 255, 255, 255]);
        // Corner of the target is untouched padding.
        assert_eq!(pixel_at(&out, 6, 0, 0), pad);
    }

    #[test]
    fn tile_repeats_the_image_across_the_whole_target() {
        let img = tiny_image();
        let out = ScaleMode::Tile.blit(&img, 4, 4, [0, 0, 0, 0]);
        // The 2x2 source repeats twice on each axis: (2,2) is a second
        // copy of (0,0)'s red.
        assert_eq!(pixel_at(&out, 4, 0, 0), [255, 0, 0, 255]);
        assert_eq!(pixel_at(&out, 4, 2, 0), [255, 0, 0, 255]);
        assert_eq!(pixel_at(&out, 4, 2, 2), [255, 0, 0, 255]);
    }

    #[test]
    fn fit_letterboxes_a_narrower_target_on_the_wide_axis() {
        // Source is 2x2 (square); target is wider than tall, so `fit`
        // scales to the height (the binding constraint) and pads left/
        // right rather than cropping.
        let img = tiny_image();
        let pad = [1, 2, 3, 255];
        let out = ScaleMode::Fit.blit(&img, 8, 4, pad);
        // Scale factor is 4/2 = 2, so the scaled image is 4x4, centered
        // in an 8-wide target -> starts at x=2.
        assert_eq!(pixel_at(&out, 8, 0, 0), pad);
        assert_eq!(pixel_at(&out, 8, 2, 0), [255, 0, 0, 255]);
    }

    #[test]
    fn fill_crops_a_narrower_target_on_the_wide_axis() {
        // Same geometry as the fit test, but `fill` scales to *cover* the
        // whole 8x4 target (scale factor 4, image becomes 8x8) and crops
        // the top/bottom rather than padding -- no padding color should
        // appear anywhere in the output.
        let img = tiny_image();
        let out = ScaleMode::Fill.blit(&img, 8, 4, [9, 9, 9, 9]);
        assert!(out.chunks_exact(4).all(|px| px != [9, 9, 9, 9]));
    }

    #[test]
    fn empty_target_dimensions_return_without_panicking() {
        let img = tiny_image();
        let out = ScaleMode::Stretch.blit(&img, 0, 5, [0, 0, 0, 0]);
        assert!(out.is_empty());
    }
}

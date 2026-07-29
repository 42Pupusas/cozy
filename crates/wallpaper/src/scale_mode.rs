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
        self.blit_into(image, target_width, target_height, pad, &mut out);
        out
    }

    /// Render into caller-owned RGBA8 storage.
    ///
    /// Reusing this buffer avoids a heap allocation and lets a compositor
    /// client keep one frame-sized scratch surface across redraws. The slice
    /// must contain exactly `target_width * target_height * 4` bytes; keeping
    /// that contract explicit prevents silently producing a truncated frame.
    pub fn blit_into(
        self,
        image: &WallpaperImage,
        target_width: u32,
        target_height: u32,
        pad: [u8; 4],
        out: &mut [u8],
    ) {
        self.blit_into_inner(image, target_width, target_height, pad, out);
    }

    fn blit_into_inner(
        self,
        image: &WallpaperImage,
        target_width: u32,
        target_height: u32,
        pad: [u8; 4],
        out: &mut [u8],
    ) {
        let output_len = target_width as usize * target_height as usize * 4;
        assert_eq!(
            out.len(),
            output_len,
            "wallpaper output buffer has the wrong size"
        );

        // Stretch, fill, and tile overwrite every destination pixel. Avoid
        // first painting the padding colour in those modes: that pass was a
        // second full-frame memory write before the real blit began. Fit and
        // center require the padding pass because they intentionally leave
        // uncovered pixels untouched, including when the pad is transparent
        // black and the caller supplied a previously used buffer.
        if target_width == 0 || target_height == 0 || image.width == 0 || image.height == 0 {
            for px in out.chunks_exact_mut(4) {
                px.copy_from_slice(&pad);
            }
            return;
        }
        if self.leaves_uncovered_pixels(image, target_width, target_height) {
            for px in out.chunks_exact_mut(4) {
                px.copy_from_slice(&pad);
            }
        }

        match self {
            Self::Stretch => Self::blit_stretch(image, target_width, target_height, out),
            Self::Fit => Self::blit_scaled(image, target_width, target_height, out, true),
            Self::Fill => Self::blit_scaled(image, target_width, target_height, out, false),
            Self::Center => Self::blit_unscaled(image, target_width, target_height, out, false),
            Self::Tile => Self::blit_unscaled(image, target_width, target_height, out, true),
        }
    }

    /// Whether this mode leaves any destination pixels untouched.
    ///
    /// Reused buffers only need a padding pass when uncovered pixels really
    /// exist. In particular, a `Fit` render with matching aspect ratios covers
    /// the target completely, so clearing the full frame first only doubles
    /// its memory traffic.
    fn leaves_uncovered_pixels(self, image: &WallpaperImage, tw: u32, th: u32) -> bool {
        match self {
            Self::Fit => {
                let scale = (f64::from(tw) / f64::from(image.width))
                    .min(f64::from(th) / f64::from(image.height));
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let scaled_width = (f64::from(image.width) * scale).round() as u32;
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let scaled_height = (f64::from(image.height) * scale).round() as u32;
                scaled_width < tw || scaled_height < th
            }
            Self::Center => image.width < tw || image.height < th,
            Self::Stretch | Self::Fill | Self::Tile => false,
        }
    }

    /// Nearest-neighbor resample, independently per axis -- source and
    /// target aspect ratios needn't match; whichever differs more just
    /// looks stretched, exactly like swaybg's own `stretch` mode.
    fn blit_stretch(image: &WallpaperImage, tw: u32, th: u32, out: &mut [u8]) {
        // Walk the nearest-neighbour mapping with integer error accumulators.
        // The old x * source / target expression performed a division for
        // every pixel; the equivalent Bresenham-style walk keeps the exact
        // floor mapping while replacing those divisions with additions and a
        // predictable threshold branch.
        let mut y_accumulator = 0_u64;
        let mut sy = 0_u32;
        for y in 0..th {
            let mut x_accumulator = 0_u64;
            let mut sx = 0_u32;
            for x in 0..tw {
                Self::copy_pixel(image, sx, sy, out, x, y, tw);
                x_accumulator += u64::from(image.width);
                while x_accumulator >= u64::from(tw) {
                    x_accumulator -= u64::from(tw);
                    sx += 1;
                }
            }
            y_accumulator += u64::from(image.height);
            while y_accumulator >= u64::from(th) {
                y_accumulator -= u64::from(th);
                sy += 1;
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
        if scaled_w <= 0 || scaled_h <= 0 {
            return;
        }

        // The scaled image intersects the target in one contiguous rectangle.
        // Mapping only that rectangle avoids sparse `Option` maps and keeps
        // letterbox/crop tests out of the hot pixel loop.
        let first_x = off_x.max(0);
        let last_x = (off_x + scaled_w).min(i64::from(tw));
        let first_y = off_y.max(0);
        let last_y = (off_y + scaled_h).min(i64::from(th));
        if first_x >= last_x || first_y >= last_y {
            return;
        }

        let first_x = usize::try_from(first_x).expect("visible x origin fits usize");
        let last_x = usize::try_from(last_x).expect("visible x limit fits usize");
        let first_y = usize::try_from(first_y).expect("visible y origin fits usize");
        let last_y = usize::try_from(last_y).expect("visible y limit fits usize");

        let mut source_x_bytes = Vec::with_capacity(last_x - first_x);
        for x in first_x..last_x {
            #[allow(clippy::cast_precision_loss)]
            let target_x = i64::try_from(x).expect("target x fits i64");
            #[allow(clippy::cast_precision_loss)]
            let col_pos = (target_x - off_x) as f64;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let sx = ((col_pos / scale) as u32).min(image.width - 1);
            source_x_bytes.push(sx as usize * 4);
        }
        let mut source_row_bytes = Vec::with_capacity(last_y - first_y);
        for y in first_y..last_y {
            #[allow(clippy::cast_precision_loss)]
            let target_y = i64::try_from(y).expect("target y fits i64");
            #[allow(clippy::cast_precision_loss)]
            let row_pos = (target_y - off_y) as f64;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let sy = ((row_pos / scale) as u32).min(image.height - 1);
            source_row_bytes.push(sy as usize * image.width as usize * 4);
        }

        // The maps contain byte offsets so the hot loop only adds the source
        // row and column positions. Walking destination pixels as slices also
        // removes repeated destination row-major index calculations.
        let row_bytes = tw as usize * 4;
        let first_column_byte = first_x * 4;
        let last_column_byte = last_x * 4;
        for (source_row, destination_row) in source_row_bytes.into_iter().zip(
            out.chunks_exact_mut(row_bytes)
                .skip(first_y)
                .take(last_y - first_y),
        ) {
            let destination_pixels =
                destination_row[first_column_byte..last_column_byte].chunks_exact_mut(4);
            for (source_column, destination_pixel) in source_x_bytes.iter().zip(destination_pixels)
            {
                let source = source_row + source_column;
                destination_pixel.copy_from_slice(&image.rgba[source..source + 4]);
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
    fn stretch_matches_floor_mapping_for_non_even_dimensions() {
        let image = WallpaperImage {
            width: 3,
            height: 2,
            rgba: vec![
                10, 0, 0, 255, 20, 0, 0, 255, 30, 0, 0, 255, 40, 0, 0, 255, 50, 0, 0, 255, 60, 0,
                0, 255,
            ],
        };
        let out = ScaleMode::Stretch.blit(&image, 5, 3, [0, 0, 0, 0]);
        let expected = [10, 10, 20, 20, 30, 10, 10, 20, 20, 30, 40, 40, 50, 50, 60];
        for (pixel, expected_red) in expected.into_iter().enumerate() {
            assert_eq!(out[pixel * 4], expected_red);
        }
    }

    #[test]
    fn reused_buffer_clears_zero_padding_in_fit_mode() {
        let img = tiny_image();
        let mut out = vec![0xaa; 8 * 4 * 4];
        ScaleMode::Fit.blit_into(&img, 8, 4, [0, 0, 0, 0], &mut out);

        assert_eq!(pixel_at(&out, 8, 0, 0), [0, 0, 0, 0]);
        assert_eq!(pixel_at(&out, 8, 2, 0), [255, 0, 0, 255]);
    }

    #[test]
    fn fit_with_matching_aspect_ratio_overwrites_reused_buffer() {
        let img = tiny_image();
        let mut out = vec![0xaa; 4 * 4 * 4];
        ScaleMode::Fit.blit_into(&img, 4, 4, [17, 18, 19, 20], &mut out);

        assert!(out.chunks_exact(4).all(|pixel| pixel != [0xaa; 4]));
        assert!(out.chunks_exact(4).all(|pixel| pixel != [17, 18, 19, 20]));
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

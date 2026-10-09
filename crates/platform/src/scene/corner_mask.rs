//! [`CornerMask`] -- the top-left corner of a rounded rect as premultiplied
//! ARGB pixels: the fill inside the arc, transparent outside it, and
//! antialiased along it.

pub struct CornerMask;

impl CornerMask {
    const SUBSAMPLES: u32 = 4;

    /// Fill `out` (`side`² pixels, row-major) with an arc of radius `side`
    /// centred on the bottom-right corner of the square.
    pub fn paint(out: &mut [u32], side: usize, argb: u32) {
        let n = Self::SUBSAMPLES;
        let full = n * n;
        #[allow(clippy::cast_precision_loss)]
        let radius = side as f32;
        for (y, row) in out.chunks_exact_mut(side).take(side).enumerate() {
            for (x, pixel) in row.iter_mut().enumerate() {
                let mut inside = 0;
                for sy in 0..n {
                    for sx in 0..n {
                        #[allow(clippy::cast_precision_loss)]
                        let dx = radius - (x as f32 + (sx as f32 + 0.5) / n as f32);
                        #[allow(clippy::cast_precision_loss)]
                        let dy = radius - (y as f32 + (sy as f32 + 0.5) / n as f32);
                        if dx * dx + dy * dy <= radius * radius {
                            inside += 1;
                        }
                    }
                }
                *pixel = Self::scaled(argb, inside, full);
            }
        }
    }

    fn scaled(argb: u32, inside: u32, full: u32) -> u32 {
        if inside == full {
            return argb;
        }
        argb.to_le_bytes().iter().rev().fold(0, |word, &channel| {
            (word << 8) | (u32::from(channel) * inside / full)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::CornerMask;

    const FILL: u32 = 0xff40_8020;

    fn mask(side: usize) -> Vec<u32> {
        let mut out = vec![0; side * side];
        CornerMask::paint(&mut out, side, FILL);
        out
    }

    #[test]
    fn the_far_corner_is_empty_and_the_inner_corner_is_filled() {
        let side = 12;
        let out = mask(side);
        assert_eq!(out[0], 0);
        assert_eq!(out[side * side - 1], FILL);
    }

    #[test]
    fn edge_pixels_are_premultiplied_partial_coverage() {
        let side = 12;
        let partial: Vec<u32> = mask(side)
            .into_iter()
            .filter(|&p| p != 0 && p != FILL)
            .collect();
        assert!(!partial.is_empty());
        for p in partial {
            let [b, g, r, a] = p.to_le_bytes();
            assert!(a < 0xff && r <= a && g <= a && b <= a, "{p:#010x}");
        }
    }

    #[test]
    fn the_mask_is_symmetric_about_its_diagonal() {
        let side = 9;
        let out = mask(side);
        for y in 0..side {
            for x in 0..side {
                assert_eq!(out[y * side + x], out[x * side + y]);
            }
        }
    }
}

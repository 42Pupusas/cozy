//! Carries a window image across a resize of its shm pool.

/// A copy of one frame, held only while the pool is reshaped.
///
/// A resize moves every slot but the first to a new offset and may remap the
/// pool, so the old image has to leave the pool before the reshape and be
/// painted back after. The copy is dropped as soon as it is restored.
pub struct FrameCarry {
    pixels: Vec<u32>,
    width: usize,
    height: usize,
}

impl FrameCarry {
    #[must_use]
    pub fn capture(source: &[u32], width: i32, height: i32) -> Self {
        let width = usize::try_from(width).unwrap_or(0);
        let height = usize::try_from(height).unwrap_or(0);
        let len = (width * height).min(source.len());
        Self {
            pixels: source[..len].to_vec(),
            width,
            height: len.checked_div(width).unwrap_or(0),
        }
    }

    /// Paint the carried image top-left aligned into a `width` × `height`
    /// frame, filling everything it does not cover with `bg`.
    pub fn paint_onto(&self, target: &mut [u32], width: i32, height: i32, bg: u32) {
        let width = usize::try_from(width).unwrap_or(0);
        let height = usize::try_from(height).unwrap_or(0);
        let copy_w = self.width.min(width);
        let copy_h = self.height.min(height);
        for (row, line) in target.chunks_mut(width.max(1)).take(height).enumerate() {
            if row < copy_h {
                let src = &self.pixels[row * self.width..row * self.width + copy_w];
                line[..copy_w].copy_from_slice(src);
                line[copy_w..].fill(bg);
            } else {
                line.fill(bg);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grow_keeps_the_image_top_left_and_fills_the_rest() {
        let carry = FrameCarry::capture(&[1, 2, 3, 4], 2, 2);
        let mut target = vec![0; 9];
        carry.paint_onto(&mut target, 3, 3, 9);
        assert_eq!(target, vec![1, 2, 9, 3, 4, 9, 9, 9, 9]);
    }

    #[test]
    fn shrink_crops_to_the_new_size() {
        let carry = FrameCarry::capture(&[1, 2, 3, 4, 5, 6, 7, 8, 9], 3, 3);
        let mut target = vec![0; 4];
        carry.paint_onto(&mut target, 2, 2, 0);
        assert_eq!(target, vec![1, 2, 4, 5]);
    }

    #[test]
    fn a_short_source_carries_only_its_whole_rows() {
        let carry = FrameCarry::capture(&[1, 2, 3], 2, 2);
        let mut target = vec![0; 4];
        carry.paint_onto(&mut target, 2, 2, 7);
        assert_eq!(target, vec![1, 2, 7, 7]);
    }
}

//! [`TextLadder`] -- a run of text sizes from small to large, so tile text
//! can grow with its tile.

use statusbar::TextRenderer;

pub struct TextLadder {
    steps: Vec<TextRenderer>,
}

impl TextLadder {
    const RATIO: f32 = 1.25;

    /// Sizes from `smallest` up by a fixed ratio, ending at or just past
    /// `largest`.
    #[must_use]
    pub fn sizes(smallest: f32, largest: f32) -> Vec<f32> {
        let smallest = if smallest.is_finite() && smallest > 0.0 {
            smallest
        } else {
            1.0
        };
        let mut sizes = vec![smallest];
        let mut size = smallest;
        while size < largest && sizes.len() < 16 {
            size *= Self::RATIO;
            sizes.push(size);
        }
        sizes
    }

    #[must_use]
    pub fn embedded(smallest: f32, largest: f32) -> Self {
        Self::new(
            Self::sizes(smallest, largest)
                .into_iter()
                .map(|px| TextRenderer::load(None, px))
                .collect(),
        )
    }

    /// Steps in rising size; never empty.
    #[must_use]
    pub fn new(mut steps: Vec<TextRenderer>) -> Self {
        if steps.is_empty() {
            steps.push(TextRenderer::Bitmap);
        }
        Self { steps }
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.steps.len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    #[must_use]
    pub fn line_height(&self, step: usize) -> i32 {
        self.steps[step.min(self.steps.len() - 1)].line_height()
    }

    pub fn step(&mut self, step: usize) -> &mut TextRenderer {
        let last = self.steps.len() - 1;
        &mut self.steps[step.min(last)]
    }
}

#[cfg(test)]
mod tests {
    use super::TextLadder;

    #[test]
    fn sizes_rise_from_the_smallest_past_the_largest() {
        let sizes = TextLadder::sizes(11.0, 35.0);
        assert!((sizes[0] - 11.0).abs() < 1e-3);
        assert!(sizes.windows(2).all(|pair| pair[1] > pair[0]));
        assert!(*sizes.last().expect("sizes") >= 35.0);
        assert!(sizes[sizes.len() - 2] < 35.0);
    }

    #[test]
    fn a_bad_range_still_gives_one_size() {
        assert_eq!(TextLadder::sizes(f32::NAN, 0.0).len(), 1);
        assert_eq!(TextLadder::sizes(12.0, 4.0).len(), 1);
    }

    #[test]
    fn steps_past_the_end_clamp_to_the_largest() {
        let mut ladder = TextLadder::embedded(11.0, 30.0);
        let last = ladder.len() - 1;
        assert_eq!(ladder.line_height(99), ladder.line_height(last));
        let _ = ladder.step(99).measure("x");
        assert!(!TextLadder::new(Vec::new()).is_empty());
    }
}

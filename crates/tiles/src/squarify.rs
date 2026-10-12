//! [`Squarify`] -- how many weights the next treemap strip takes.

/// A strip laid along a side of length `side`, where one unit of weight
/// covers `scale` square pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Squarify {
    side: f64,
    scale: f64,
}

impl Squarify {
    #[must_use]
    pub const fn new(side: f64, scale: f64) -> Self {
        Self { side, scale }
    }

    /// How many of the leading `weights`, largest first, the strip takes:
    /// the count after which adding another cell would make the worst aspect
    /// ratio worse.
    #[must_use]
    pub fn run(&self, weights: &[f64]) -> usize {
        let Some(&first) = weights.first() else {
            return 0;
        };
        if self.side <= 0.0 || !(self.scale.is_finite() && self.scale > 0.0) {
            return weights.len();
        }
        let lead = first * self.scale;
        let mut sum = lead;
        let mut worst = self.worst(sum, lead, lead);
        let mut taken = 1;
        for weight in &weights[1..] {
            let area = weight * self.scale;
            let grown = sum + area;
            let ratio = self.worst(grown, lead, area);
            if ratio > worst {
                break;
            }
            sum = grown;
            worst = ratio;
            taken += 1;
        }
        taken
    }

    fn worst(&self, sum: f64, largest: f64, smallest: f64) -> f64 {
        let side = self.side.powi(2);
        let strip = sum.powi(2);
        (side * largest / strip).max(strip / (side * smallest))
    }
}

#[cfg(test)]
mod tests {
    use super::Squarify;

    #[test]
    fn nothing_offered_takes_nothing() {
        assert_eq!(Squarify::new(100.0, 1.0).run(&[]), 0);
    }

    #[test]
    fn a_single_weight_is_taken() {
        assert_eq!(Squarify::new(100.0, 1.0).run(&[5.0]), 1);
    }

    #[test]
    fn equal_squares_fill_a_side_they_divide_evenly() {
        assert_eq!(Squarify::new(400.0, 10_000.0).run(&[1.0; 8]), 4);
    }

    #[test]
    fn a_degenerate_side_or_scale_takes_everything() {
        assert_eq!(Squarify::new(0.0, 1.0).run(&[1.0, 2.0, 3.0]), 3);
        assert_eq!(Squarify::new(10.0, f64::INFINITY).run(&[1.0, 2.0]), 2);
    }
}

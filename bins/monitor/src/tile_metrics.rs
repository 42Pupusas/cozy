//! [`TileMetrics`] -- the spacing every tile shares, derived from one line
//! height so it follows the user's font size and display scale.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileMetrics {
    pub margin: i32,
    pub gutter: i32,
    pub radius: i32,
    pub pad: i32,
    pub min_side: i32,
}

impl TileMetrics {
    const MIN_LINE: i32 = 8;

    #[must_use]
    pub fn new(line_height: i32) -> Self {
        let line = line_height.max(Self::MIN_LINE);
        Self {
            margin: line,
            gutter: (line / 7).max(2),
            radius: (line / 2).max(6),
            pad: (line * 2 / 3).max(10),
            min_side: (line * 7).max(112),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TileMetrics;

    #[test]
    fn padding_always_clears_the_rounded_corners() {
        for line in [0, 8, 14, 20, 28, 40, 80] {
            let metrics = TileMetrics::new(line);
            assert!(metrics.pad >= metrics.radius, "{line}: {metrics:?}");
            assert!(metrics.min_side > metrics.pad * 2, "{line}: {metrics:?}");
        }
    }

    #[test]
    fn spacing_grows_with_the_font() {
        let small = TileMetrics::new(14);
        let large = TileMetrics::new(40);
        assert!(large.gutter > small.gutter);
        assert!(large.pad > small.pad);
        assert!(large.min_side > small.min_side);
    }
}

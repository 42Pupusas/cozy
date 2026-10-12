//! [`TileMetrics`] -- the spacing every tile shares, derived from one line
//! height so it follows the user's font size and display scale.

use crate::tile_minimum::TileMinimum;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileMetrics {
    pub margin: i32,
    pub gutter: i32,
    pub radius: i32,
    pub pad: i32,
    pub minimum: TileMinimum,
}

impl TileMetrics {
    const MIN_LINE: i32 = 8;

    #[must_use]
    pub fn new(line_height: i32) -> Self {
        let line = line_height.max(Self::MIN_LINE);
        let pad = (line * 2 / 3).max(10);
        let gutter = (line / 16).max(1);
        Self {
            margin: (line / 3).max(2),
            gutter,
            radius: (line / 2).max(6),
            pad,
            minimum: TileMinimum {
                width: line * 13 / 2 + gutter * 4,
                height: line * 3 + pad * 3 / 2 + gutter * 4,
                band: (line * 5).max(84),
            },
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
            assert!(
                metrics.minimum.width > metrics.pad * 2,
                "{line}: {metrics:?}"
            );
            assert!(
                metrics.minimum.height > metrics.pad * 2,
                "{line}: {metrics:?}"
            );
        }
    }

    #[test]
    fn tiles_sit_close_together() {
        let metrics = TileMetrics::new(40);
        assert!(metrics.gutter * 2 <= 40 / 8, "{metrics:?}");
        assert!(metrics.margin <= 40 / 3, "{metrics:?}");
    }

    #[test]
    fn spacing_grows_with_the_font() {
        let small = TileMetrics::new(14);
        let large = TileMetrics::new(40);
        assert!(large.gutter > small.gutter);
        assert!(large.margin > small.margin);
        assert!(large.pad > small.pad);
        assert!(large.minimum.width > small.minimum.width);
        assert!(large.minimum.height > small.minimum.height);
    }

    #[test]
    fn a_process_tile_holds_three_lines_and_is_wider_than_tall() {
        for line in [14, 18, 28, 40] {
            let metrics = TileMetrics::new(line);
            let body = metrics.minimum.height - metrics.gutter * 4;
            assert!(
                body >= metrics.pad + metrics.pad / 2 + line * 3,
                "{metrics:?}"
            );
            assert!(
                metrics.minimum.width > metrics.minimum.height,
                "{metrics:?}"
            );
            assert!(metrics.minimum.width < line * 7, "{metrics:?}");
        }
    }
}

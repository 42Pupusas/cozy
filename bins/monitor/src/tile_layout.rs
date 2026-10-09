//! [`TileLayout`] -- decides which tiles the window shows and where.
//!
//! A short band across the top holds the info card and one tile per chart.
//! Below it, a tile per process group sized by its share of memory, and one
//! "more" tile standing for the groups that no longer fit.

use platform::draw::{Point, Rect};

use crate::fixed_band::FixedBand;
use crate::group_smoother::Share;
use crate::weighted_spiral::WeightedSpiral;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChartKind {
    Cpu,
    Memory,
    Temperature,
    Network,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TileKind {
    Info,
    Chart(ChartKind),
    Process(String),
    More { count: usize },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlacedTile {
    pub kind: TileKind,
    pub rect: Rect,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Arrangement {
    pub tiles: Vec<PlacedTile>,
    pub shown: usize,
}

impl Arrangement {
    #[must_use]
    pub fn at(&self, point: Point) -> Option<&PlacedTile> {
        self.tiles.iter().find(|tile| tile.rect.contains(point))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TileLayout {
    area: Rect,
    min_side: i32,
}

impl TileLayout {
    const DECAY: f64 = 0.9;
    const MAX_PROCESS_TILES: usize = 48;

    #[must_use]
    pub const fn new(area: Rect, min_side: i32) -> Self {
        Self { area, min_side }
    }

    /// The tiles for `shares` (largest first), showing as many groups as stay
    /// legible and folding the rest into one "more" tile. Sizes follow rank
    /// alone, each tile keeping a fixed fraction of its predecessor's area, so
    /// one dominant group cannot take over the field.
    #[must_use]
    pub fn arrange(&self, shares: &[Share]) -> Arrangement {
        let band = if shares.is_empty() {
            FixedBand::filling(self.area)
        } else {
            FixedBand::new(self.area, self.min_side)
        };
        let mut tiles = Self::band_tiles(&band);
        let spiral = WeightedSpiral::new(band.rest());
        let mut shown = shares.len().min(Self::MAX_PROCESS_TILES);
        loop {
            let kinds = Self::kinds(shares, shown);
            let weights = Self::weights(kinds.len());
            if shown == 0 || spiral.legible(&weights, self.min_side) {
                tiles.extend(
                    kinds
                        .into_iter()
                        .zip(spiral.cells(&weights))
                        .map(|(kind, rect)| PlacedTile { kind, rect }),
                );
                return Arrangement { tiles, shown };
            }
            shown -= 1;
        }
    }

    fn band_tiles(band: &FixedBand) -> Vec<PlacedTile> {
        let kinds = [
            TileKind::Info,
            TileKind::Chart(ChartKind::Cpu),
            TileKind::Chart(ChartKind::Memory),
            TileKind::Chart(ChartKind::Temperature),
            TileKind::Chart(ChartKind::Network),
        ];
        kinds
            .into_iter()
            .zip(band.cells())
            .map(|(kind, rect)| PlacedTile { kind, rect })
            .collect()
    }

    fn kinds(shares: &[Share], shown: usize) -> Vec<TileKind> {
        let mut kinds: Vec<TileKind> = shares[..shown]
            .iter()
            .map(|share| TileKind::Process(share.name.clone()))
            .collect();
        let tail = shares.len() - shown;
        if tail > 0 {
            kinds.push(TileKind::More { count: tail });
        }
        kinds
    }

    fn weights(count: usize) -> Vec<f64> {
        std::iter::successors(Some(1.0), |weight| Some(weight * Self::DECAY))
            .take(count)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{Arrangement, ChartKind, TileKind, TileLayout};
    use crate::group_smoother::Share;
    use platform::draw::{Point, Rect};

    const AREA: Rect = Rect::new(18, 18, 1300, 800);

    fn shares(n: usize) -> Vec<Share> {
        let mut left = 1.0;
        (0..n)
            .map(|i| {
                let share = left * 0.4;
                left -= share;
                Share {
                    name: format!("p{i}"),
                    share,
                }
            })
            .collect()
    }

    fn count(arrangement: &Arrangement, wanted: impl Fn(&TileKind) -> bool) -> usize {
        arrangement
            .tiles
            .iter()
            .filter(|tile| wanted(&tile.kind))
            .count()
    }

    #[test]
    fn the_info_card_and_every_chart_are_always_present() {
        for n in [0, 1, 8, 60] {
            let arranged = TileLayout::new(AREA, 100).arrange(&shares(n));
            assert_eq!(count(&arranged, |k| *k == TileKind::Info), 1, "n={n}");
            for chart in [
                ChartKind::Cpu,
                ChartKind::Memory,
                ChartKind::Temperature,
                ChartKind::Network,
            ] {
                assert_eq!(count(&arranged, |k| *k == TileKind::Chart(chart)), 1);
            }
        }
    }

    #[test]
    fn tiles_cover_the_area_exactly_without_overlap() {
        let arranged = TileLayout::new(AREA, 100).arrange(&shares(12));
        let total: i64 = arranged
            .tiles
            .iter()
            .map(|t| i64::from(t.rect.w) * i64::from(t.rect.h))
            .sum();
        assert_eq!(total, i64::from(AREA.w) * i64::from(AREA.h));
        for (i, a) in arranged.tiles.iter().enumerate() {
            for b in &arranged.tiles[i + 1..] {
                let across = a.rect.right().min(b.rect.right()) - a.rect.x.max(b.rect.x);
                let down = a.rect.bottom().min(b.rect.bottom()) - a.rect.y.max(b.rect.y);
                assert!(across <= 0 || down <= 0, "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn a_heavier_group_gets_a_larger_tile() {
        let arranged = TileLayout::new(AREA, 1).arrange(&shares(6));
        let area_of = |name: &str| {
            let tile = arranged
                .tiles
                .iter()
                .find(|t| t.kind == TileKind::Process(name.to_owned()))
                .expect("shown");
            i64::from(tile.rect.w) * i64::from(tile.rect.h)
        };
        assert!(area_of("p0") > area_of("p1"));
        assert!(area_of("p1") > area_of("p2"));
    }

    #[test]
    fn what_does_not_fit_collapses_into_one_more_tile() {
        let all = shares(60);
        let arranged = TileLayout::new(AREA, 110).arrange(&all);
        assert!(arranged.shown < all.len());
        let more: Vec<usize> = arranged
            .tiles
            .iter()
            .filter_map(|t| match t.kind {
                TileKind::More { count } => Some(count),
                _ => None,
            })
            .collect();
        assert_eq!(more, [all.len() - arranged.shown]);
        assert_eq!(
            count(&arranged, |k| matches!(k, TileKind::Process(_))),
            arranged.shown
        );
    }

    #[test]
    fn the_more_tile_is_the_smallest_tile() {
        let arranged = TileLayout::new(AREA, 60).arrange(&shares(60));
        let more = arranged
            .tiles
            .iter()
            .find(|t| matches!(t.kind, TileKind::More { .. }))
            .expect("more tile");
        let area = |t: &super::PlacedTile| i64::from(t.rect.w) * i64::from(t.rect.h);
        assert!(
            arranged
                .tiles
                .iter()
                .filter(|t| matches!(t.kind, TileKind::Process(_)))
                .all(|t| area(t) >= area(more))
        );
    }

    #[test]
    fn every_tile_is_legible_when_anything_could_be_dropped() {
        let arranged = TileLayout::new(AREA, 110).arrange(&shares(60));
        assert!(arranged.shown > 0);
        assert!(
            arranged
                .tiles
                .iter()
                .all(|t| t.rect.w >= 110 && t.rect.h >= 110)
        );
    }

    #[test]
    fn a_roomy_window_shows_every_group_with_no_more_tile() {
        let arranged = TileLayout::new(AREA, 60).arrange(&shares(3));
        assert_eq!(arranged.shown, 3);
        assert_eq!(count(&arranged, |k| matches!(k, TileKind::More { .. })), 0);
    }

    #[test]
    fn a_window_too_small_for_anything_still_shows_the_fixed_tiles() {
        let arranged = TileLayout::new(Rect::new(0, 0, 200, 150), 120).arrange(&shares(10));
        assert_eq!(arranged.shown, 0);
        assert_eq!(count(&arranged, |k| *k == TileKind::Info), 1);
        assert_eq!(count(&arranged, |k| matches!(k, TileKind::More { .. })), 1);
    }

    #[test]
    fn nothing_running_leaves_just_the_fixed_tiles() {
        let arranged = TileLayout::new(AREA, 100).arrange(&[]);
        assert_eq!(arranged.tiles.len(), 5);
    }

    #[test]
    fn a_point_finds_the_tile_under_it() {
        let arranged = TileLayout::new(AREA, 100).arrange(&shares(4));
        let first = &arranged.tiles[0];
        let inside = Point::new(first.rect.x + 1, first.rect.y + 1);
        assert_eq!(arranged.at(inside).map(|t| &t.kind), Some(&first.kind));
        assert!(arranged.at(Point::new(-5, -5)).is_none());
    }

    #[test]
    fn the_fixed_tiles_sit_in_a_short_band_above_the_processes() {
        let arranged = TileLayout::new(AREA, 110).arrange(&shares(12));
        let band_bottom = arranged.tiles[..5]
            .iter()
            .map(|t| t.rect.bottom())
            .max()
            .expect("band");
        assert!(band_bottom - AREA.y <= AREA.h / 4);
        assert!(arranged.tiles[5..].iter().all(|t| t.rect.y >= band_bottom));
    }

    #[test]
    fn sizes_follow_rank_not_memory() {
        let shares: Vec<Share> = [0.9, 0.05, 0.03, 0.02]
            .into_iter()
            .enumerate()
            .map(|(i, share)| Share {
                name: format!("p{i}"),
                share,
            })
            .collect();
        let arranged = TileLayout::new(AREA, 1).arrange(&shares);
        let areas: Vec<i64> = arranged.tiles[5..]
            .iter()
            .map(|t| i64::from(t.rect.w) * i64::from(t.rect.h))
            .collect();
        assert!(areas[0] * 100 < areas.iter().sum::<i64>() * 40, "{areas:?}");
    }

    fn skewed(n: usize) -> Vec<Share> {
        let raw: Vec<f64> = (0..n)
            .map(|i| 1.0 / (f64::from(u32::try_from(i).unwrap_or(0)) + 1.0).powf(1.3))
            .collect();
        let total: f64 = raw.iter().sum();
        raw.iter()
            .enumerate()
            .map(|(i, r)| Share {
                name: format!("p{i}"),
                share: r / total,
            })
            .collect()
    }

    #[test]
    fn a_dominant_group_stays_well_under_half_the_process_area() {
        let arranged = TileLayout::new(AREA, 112).arrange(&skewed(40));
        let processes: Vec<i64> = arranged
            .tiles
            .iter()
            .filter(|t| matches!(t.kind, TileKind::Process(_)))
            .map(|t| i64::from(t.rect.w) * i64::from(t.rect.h))
            .collect();
        let whole: i64 = processes.iter().sum();
        let largest = processes.iter().copied().max().expect("processes");
        assert!(largest * 100 < whole * 30, "{largest} of {whole}");
    }

    #[test]
    fn a_skewed_system_shows_many_groups() {
        let arranged = TileLayout::new(AREA, 112).arrange(&skewed(40));
        assert!(arranged.shown >= 16, "{}", arranged.shown);
    }
}

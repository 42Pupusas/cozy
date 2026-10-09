//! [`TileField`] -- the tile presentation's state: the size ranking and how
//! far along it the field has been scrolled.

use std::path::PathBuf;

use platform::draw::{Point, Rect};

use crate::browser::Browser;
use crate::dir_sizes::DirSizes;
use crate::spiral::{MinCell, SpiralLayout};
use crate::tile_coloring::TileColoring;
use crate::tile_rank::TileRank;

/// A tile's entry, the cell it occupies, and which of the theme's tile fills
/// it takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlacedTile {
    pub row: usize,
    pub rect: Rect,
    pub slot: usize,
}

/// The tiles in view, the innermost cell kept for the card, and whether
/// scrolling could bring in more at either end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arrangement {
    pub tiles: Vec<PlacedTile>,
    pub card: Option<Rect>,
    pub later: bool,
    pub earlier: bool,
}

/// The entries in view in rank order, for the keyboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub rows: Vec<usize>,
    pub later: bool,
    pub earlier: bool,
}

/// The browser state the ranking was last built from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Synced {
    listing: u64,
    refreshes: u64,
    sizes: u64,
}

pub struct TileField {
    sizes: DirSizes,
    rank: TileRank,
    /// How many of the largest entries the field has scrolled past.
    offset: usize,
    folder: Option<PathBuf>,
    synced: Option<Synced>,
}

impl TileField {
    #[must_use]
    pub fn new(wake: waker::Waker) -> Self {
        Self {
            sizes: DirSizes::new(wake),
            rank: TileRank::default(),
            offset: 0,
            folder: None,
            synced: None,
        }
    }

    /// Fold in directory totals that arrived. Returns whether any did.
    pub fn poll(&mut self) -> bool {
        self.sizes.poll()
    }

    #[must_use]
    pub const fn sizes(&self) -> &DirSizes {
        &self.sizes
    }

    /// Re-rank after the browser's listing or the measured sizes changed.
    /// Another folder starts the field back at its largest entries and
    /// begins measuring its folders.
    pub fn sync(&mut self, browser: &Browser) {
        let previous = self.synced;
        let refreshes = browser.refresh_count();
        let listing = browser.listing_revision();
        if previous.is_some_and(|seen| seen.refreshes != refreshes) {
            self.sizes.invalidate(browser.path());
        }
        let entries = || (0..browser.len()).filter_map(|row| browser.entry(row));
        let relisted =
            previous.is_none_or(|seen| seen.listing != listing || seen.refreshes != refreshes);
        if relisted {
            self.sizes.follow(entries());
        }
        if self.folder.as_deref() != Some(browser.path()) {
            self.offset = 0;
            self.folder = Some(browser.path().to_path_buf());
        }
        let now = Synced {
            listing,
            refreshes,
            sizes: self.sizes.revision(),
        };
        if previous != Some(now) {
            let sizes: Vec<u64> = entries().map(|entry| self.sizes.of(entry)).collect();
            self.rank = TileRank::new(&sizes);
        }
        self.synced = Some(now);
    }

    /// The tiles in view and their cells, plus the card's cell.
    ///
    /// `reserve`, when given, is the least room the pinned card needs. The
    /// card then takes the spiral's innermost cell and the tiles share the
    /// cells outside it.
    #[must_use]
    pub fn arrange(&self, area: Rect, min_side: i32, reserve: Option<MinCell>) -> Arrangement {
        let from = self.start();
        let spiral = SpiralLayout::new(area, min_side);
        let rest = self.rank.len().saturating_sub(from);
        let Some(need) = reserve else {
            let rows = self.rank.window(from, spiral.capacity(rest));
            let cells = spiral.cells(rows.len());
            return self.arrangement(from, rows, cells, None);
        };
        let spiral = spiral.with_prominent_last().with_last_height(need.h);
        let cells = if rest == 0 {
            1
        } else {
            spiral.capacity_ending_in(rest + 1, need).max(2)
        };
        let rows = self.rank.window(from, cells - 1);
        let mut cells = spiral.cells(rows.len() + 1);
        let card = cells.pop();
        self.arrangement(from, rows, cells, card)
    }

    fn arrangement(
        &self,
        from: usize,
        rows: &[usize],
        cells: Vec<Rect>,
        card: Option<Rect>,
    ) -> Arrangement {
        Arrangement {
            tiles: Self::place(rows, cells),
            card,
            later: from + rows.len() < self.rank.len(),
            earlier: from > 0,
        }
    }

    fn place(rows: &[usize], cells: Vec<Rect>) -> Vec<PlacedTile> {
        let slots = TileColoring::of(&cells).slots();
        rows.iter()
            .zip(cells)
            .zip(slots)
            .map(|((&row, rect), slot)| PlacedTile { row, rect, slot })
            .collect()
    }

    /// The tiles in view and their cells.
    #[must_use]
    pub fn layout(&self, area: Rect, min_side: i32, reserve: Option<MinCell>) -> Vec<PlacedTile> {
        self.arrange(area, min_side, reserve).tiles
    }

    /// The cell the pinned card occupies, when one is reserved.
    #[must_use]
    pub fn card_cell(&self, area: Rect, min_side: i32, reserve: Option<MinCell>) -> Option<Rect> {
        self.arrange(area, min_side, reserve).card
    }

    /// The entries in view in rank order, and whether more lie past either
    /// end.
    #[must_use]
    pub fn page(&self, area: Rect, min_side: i32, reserve: Option<MinCell>) -> Page {
        let arranged = self.arrange(area, min_side, reserve);
        Page {
            rows: arranged.tiles.iter().map(|placed| placed.row).collect(),
            later: arranged.later,
            earlier: arranged.earlier,
        }
    }

    /// The entry whose tile is under `at`, if any.
    #[must_use]
    pub fn hit(
        &self,
        area: Rect,
        min_side: i32,
        reserve: Option<MinCell>,
        at: Point,
    ) -> Option<usize> {
        self.layout(area, min_side, reserve)
            .into_iter()
            .find(|placed| placed.rect.contains(at))
            .map(|placed| placed.row)
    }

    /// Scroll `steps` tiles: positive drops the largest tiles from view and
    /// brings in smaller ones, negative does the reverse. Stops at either
    /// end of the ranking. Returns whether the view moved.
    pub fn slide(
        &mut self,
        area: Rect,
        min_side: i32,
        reserve: Option<MinCell>,
        steps: i32,
    ) -> bool {
        let before = self.start();
        self.offset = before;
        for _ in 0..steps.unsigned_abs() {
            if steps > 0 {
                if !self.arrange(area, min_side, reserve).later {
                    break;
                }
                self.offset += 1;
            } else if self.offset > 0 {
                self.offset -= 1;
            } else {
                break;
            }
        }
        self.offset != before
    }

    fn start(&self) -> usize {
        self.offset.min(self.rank.len().saturating_sub(1))
    }
}

#[cfg(test)]
mod tests {
    use super::TileField;
    use crate::browser::Browser;
    use crate::spiral::MinCell;
    use crate::test_support::TempDir;
    use crate::tile_coloring::TileColoring;
    use platform::draw::{Point, Rect};

    const AREA: Rect = Rect::new(0, 0, 1300, 800);

    fn field() -> (TileField, waker::Waker) {
        let wake = waker::Waker::new().expect("eventfd");
        (TileField::new(wake.clone()), wake)
    }

    fn names(field: &TileField, browser: &Browser, area: Rect, min: i32) -> Vec<String> {
        field
            .layout(area, min, None)
            .iter()
            .map(|placed| browser.entry(placed.row).expect("entry").name.clone())
            .collect()
    }

    fn settle(field: &mut TileField, wake: &waker::Waker, browser: &Browser) {
        loop {
            field.sync(browser);
            let pending = (0..browser.len())
                .filter_map(|row| browser.entry(row))
                .any(|entry| field.sizes().is_pending(entry));
            if !pending {
                return;
            }
            assert!(wake.wait(5000), "the size tally never signalled");
            wake.drain();
            field.poll();
        }
    }

    #[test]
    fn files_are_laid_out_largest_first() {
        let dir = TempDir::new("tile_field_files");
        dir.create_file("small", &[0; 10]);
        dir.create_file("large", &[0; 900]);
        dir.create_file("medium", &[0; 300]);
        let browser = Browser::open(dir.path(), 800, 600, 28).expect("open");
        let (mut field, _wake) = field();
        field.sync(&browser);
        assert_eq!(
            names(&field, &browser, AREA, 1),
            ["large", "medium", "small"]
        );
    }

    #[test]
    fn folders_rerank_when_their_totals_arrive() {
        let dir = TempDir::new("tile_field_folders");
        dir.create_file("file", &[0; 50]);
        dir.create_file("folder/inner", &[0; 5000]);
        let browser = Browser::open(dir.path(), 800, 600, 28).expect("open");
        let (mut field, wake) = field();
        field.sync(&browser);
        assert_eq!(names(&field, &browser, AREA, 1)[0], "file");
        settle(&mut field, &wake, &browser);
        assert_eq!(names(&field, &browser, AREA, 1)[0], "folder");
    }

    fn crowded() -> (TileField, Browser, TempDir) {
        let dir = TempDir::new("tile_field_crowded");
        for i in 0..40 {
            dir.create_file(&format!("f{i:02}"), &vec![0; 100 + i]);
        }
        let browser = Browser::open(dir.path(), 800, 600, 28).expect("open");
        let (mut field, _wake) = field();
        field.sync(&browser);
        (field, browser, dir)
    }

    #[test]
    fn a_crowded_folder_shows_the_largest_and_can_scroll_on() {
        let (field, browser, _dir) = crowded();
        let page = field.page(AREA, 120, None);
        assert!(page.rows.len() < 40, "forty files cannot fit at this size");
        assert!(page.later && !page.earlier);
        assert_eq!(browser.entry(page.rows[0]).expect("entry").name, "f39");
    }

    #[test]
    fn one_step_drops_the_largest_and_brings_in_the_next_smaller() {
        let (mut field, _browser, _dir) = crowded();
        let before = field.page(AREA, 120, None).rows;
        assert!(field.slide(AREA, 120, None, 1));
        let after = field.page(AREA, 120, None).rows;
        assert_eq!(before.len(), after.len());
        assert_eq!(&after[..after.len() - 1], &before[1..]);
        assert_ne!(after.last(), before.last());
        assert_eq!(field.offset, 1);
    }

    #[test]
    fn stepping_back_undoes_a_step() {
        let (mut field, _browser, _dir) = crowded();
        let before = field.page(AREA, 120, None).rows;
        field.slide(AREA, 120, None, 3);
        assert!(field.slide(AREA, 120, None, -3));
        assert_eq!(field.page(AREA, 120, None).rows, before);
    }

    #[test]
    fn scrolling_stops_at_either_end() {
        let (mut field, _browser, _dir) = crowded();
        assert!(!field.slide(AREA, 120, None, -1), "already at the top");
        assert!(!field.slide(AREA, 120, None, 0));
        assert!(field.slide(AREA, 120, None, 500));
        let last = field.page(AREA, 120, None);
        assert!(!last.later && last.earlier);
        assert_eq!(last.rows.last(), Some(&0), "the smallest ends the view");
        let full = field.arrange(AREA, 120, None).tiles.len();
        assert_eq!(full, last.rows.len(), "the view stays full at the end");
        assert!(!field.slide(AREA, 120, None, 1));
        assert!(field.slide(AREA, 120, None, -500));
        assert_eq!(field.offset, 0);
    }

    #[test]
    fn every_entry_is_seen_while_scrolling() {
        let (mut field, _browser, _dir) = crowded();
        let mut seen = field.page(AREA, 120, None).rows;
        while field.slide(AREA, 120, None, 1) {
            seen.extend(field.page(AREA, 120, None).rows);
        }
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen, (0..40).collect::<Vec<_>>());
    }

    #[test]
    fn another_folder_starts_back_at_the_largest() {
        let dir = TempDir::new("tile_field_relist");
        for i in 0..40 {
            dir.create_file(&format!("f{i:02}"), &[0; 10]);
        }
        dir.create_dir("sub");
        let mut browser = Browser::open(dir.path(), 800, 600, 28).expect("open");
        let (mut field, _wake) = field();
        field.sync(&browser);
        field.slide(AREA, 120, None, 5);
        assert_eq!(field.offset, 5);
        browser.go_to(&dir.path().join("sub"));
        field.sync(&browser);
        assert_eq!(field.offset, 0);
    }

    #[test]
    fn a_changed_listing_in_the_same_folder_keeps_the_place() {
        let (mut field, mut browser, dir) = crowded();
        field.slide(AREA, 120, None, 4);
        dir.create_file("zz-new", &[0; 5]);
        assert!(browser.poll_listing());
        field.sync(&browser);
        assert_eq!(field.offset, 4);
    }

    const NEED: MinCell = MinCell { w: 150, h: 200 };

    #[test]
    fn the_card_takes_the_innermost_cell_and_outgrows_the_small_tiles() {
        let (field, _browser, _dir) = crowded();
        let arranged = field.arrange(AREA, 120, Some(NEED));
        let card = arranged.card.expect("a reserved card gets a cell");
        assert!(card.w >= NEED.w && card.h >= NEED.h, "{card:?}");
        let areas: Vec<i64> = arranged
            .tiles
            .iter()
            .map(|p| i64::from(p.rect.w) * i64::from(p.rect.h))
            .collect();
        let card_area = i64::from(card.w) * i64::from(card.h);
        let smallest = areas.iter().min().copied().expect("tiles");
        let largest = areas.iter().max().copied().expect("tiles");
        assert!(card_area > smallest, "the card outgrows the smallest tile");
        assert!(card_area < largest, "the card does not lead the field");
        assert!(arranged.tiles.iter().all(|p| p.rect != card));
    }

    #[test]
    fn bordering_tiles_never_share_a_fill_slot() {
        for reserve in [None, Some(NEED)] {
            let (field, _browser, _dir) = crowded();
            let placed = field.arrange(AREA, 120, reserve).tiles;
            assert!(placed.len() > 4, "enough tiles to need several slots");
            for (i, a) in placed.iter().enumerate() {
                for b in &placed[i + 1..] {
                    if TileColoring::borders(a.rect, b.rect) {
                        assert_ne!(a.slot, b.slot, "{:?} borders {:?}", a.rect, b.rect);
                    }
                }
            }
        }
    }

    #[test]
    fn the_card_costs_tiles_but_nothing_overlaps_it() {
        let (field, _browser, _dir) = crowded();
        let with_card = field.arrange(AREA, 120, Some(NEED));
        let without = field.arrange(AREA, 120, None);
        assert!(with_card.tiles.len() <= without.tiles.len());
        let card = with_card.card.expect("card");
        for placed in &with_card.tiles {
            let r = placed.rect;
            let apart = r.right() <= card.x
                || card.right() <= r.x
                || r.bottom() <= card.y
                || card.bottom() <= r.y;
            assert!(apart, "{r:?} overlaps {card:?}");
        }
    }

    #[test]
    fn an_empty_directory_gives_the_card_the_whole_area() {
        let dir = TempDir::new("tile_field_card_empty");
        let browser = Browser::open(dir.path(), 800, 600, 28).expect("open");
        let (mut field, _wake) = field();
        field.sync(&browser);
        let arranged = field.arrange(AREA, 120, Some(NEED));
        assert!(arranged.tiles.is_empty());
        assert_eq!(arranged.card, Some(AREA));
    }

    #[test]
    fn a_single_entry_leaves_the_card_the_rest() {
        let dir = TempDir::new("tile_field_card_one");
        dir.create_file("a", &[0; 10]);
        let browser = Browser::open(dir.path(), 800, 600, 28).expect("open");
        let (mut field, _wake) = field();
        field.sync(&browser);
        let arranged = field.arrange(AREA, 120, Some(NEED));
        assert_eq!(arranged.tiles.len(), 1);
        assert!(arranged.card.is_some());
        assert!(!arranged.later && !arranged.earlier);
    }

    #[test]
    fn no_reserve_means_no_card_cell() {
        let (field, _browser, _dir) = crowded();
        assert_eq!(field.card_cell(AREA, 120, None), None);
    }

    #[test]
    fn nothing_is_hit_outside_the_area() {
        let dir = TempDir::new("tile_field_miss");
        dir.create_file("a", &[0; 10]);
        let browser = Browser::open(dir.path(), 800, 600, 28).expect("open");
        let (mut field, _wake) = field();
        field.sync(&browser);
        assert_eq!(field.hit(AREA, 1, None, Point::new(5000, 5)), None);
        assert_eq!(field.hit(AREA, 1, None, Point::new(5, 5)), Some(0));
    }
}

//! [`TileField`] -- the tile presentation's state: the size ranking, which
//! overflow page is open, and a folder press waiting to become a click.

use platform::draw::{Point, Rect};

use crate::browser::Browser;
use crate::dir_sizes::DirSizes;
use crate::spiral::{MinCell, SpiralLayout};
use crate::tile_coloring::TileColoring;
use crate::tile_rank::{Tile, TileRank};

/// A tile, the cell it occupies, and which of the theme's tile fills it
/// takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlacedTile {
    pub tile: Tile,
    pub rect: Rect,
    pub slot: usize,
}

/// The tiles of the open page, and the innermost cell kept for the card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arrangement {
    pub tiles: Vec<PlacedTile>,
    pub card: Option<Rect>,
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
    /// Rank offsets of the overflow pages opened so far, innermost last.
    pages: Vec<usize>,
    synced: Option<Synced>,
    /// A folder tile pressed without modifiers, entered on release unless
    /// the press turns into a drag.
    armed: Option<usize>,
    /// When a release last entered a folder, so the second half of a
    /// habitual double-click does not land on a tile inside it.
    entered_ms: Option<u64>,
}

impl TileField {
    #[must_use]
    pub fn new(wake: waker::Waker) -> Self {
        Self {
            sizes: DirSizes::new(wake),
            rank: TileRank::default(),
            pages: Vec::new(),
            synced: None,
            armed: None,
            entered_ms: None,
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
    /// A new listing also closes any open overflow page and starts measuring
    /// its folders.
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
        if previous.is_none_or(|seen| seen.listing != listing) {
            self.pages.clear();
            self.armed = None;
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

    /// The tiles of the open page and their cells, plus the card's cell.
    ///
    /// `reserve`, when given, is the least room the pinned card needs. The
    /// card then takes the spiral's innermost cell -- the last one cut, after
    /// the overflow tile -- and the tiles share the cells outside it.
    #[must_use]
    pub fn arrange(&self, area: Rect, min_side: i32, reserve: Option<MinCell>) -> Arrangement {
        let from = self.page_start();
        let spiral = SpiralLayout::new(area, min_side);
        let rest = self.rank.len().saturating_sub(from);
        let Some(need) = reserve else {
            let tiles = self.rank.page(from, spiral.capacity(rest));
            let cells = spiral.cells(tiles.len());
            return Arrangement {
                tiles: Self::place(tiles, cells),
                card: None,
            };
        };
        let spiral = spiral.with_prominent_last().with_last_height(need.h);
        let cells = if rest == 0 {
            1
        } else {
            spiral.capacity_ending_in(rest + 1, need).max(2)
        };
        let tiles = self.rank.page(from, cells - 1);
        let mut cells = spiral.cells(tiles.len() + 1);
        let card = cells.pop();
        Arrangement {
            tiles: Self::place(tiles, cells),
            card,
        }
    }

    fn place(tiles: Vec<Tile>, cells: Vec<Rect>) -> Vec<PlacedTile> {
        let slots = TileColoring::of(&cells).slots();
        tiles
            .into_iter()
            .zip(cells)
            .zip(slots)
            .map(|((tile, rect), slot)| PlacedTile { tile, rect, slot })
            .collect()
    }

    /// The tiles of the open page and their cells.
    #[must_use]
    pub fn layout(&self, area: Rect, min_side: i32, reserve: Option<MinCell>) -> Vec<PlacedTile> {
        self.arrange(area, min_side, reserve).tiles
    }

    /// The cell the pinned card occupies, when one is reserved.
    #[must_use]
    pub fn card_cell(&self, area: Rect, min_side: i32, reserve: Option<MinCell>) -> Option<Rect> {
        self.arrange(area, min_side, reserve).card
    }

    /// The open page's entries in rank order, and whether it ends in an
    /// overflow tile.
    #[must_use]
    pub fn page(
        &self,
        area: Rect,
        min_side: i32,
        reserve: Option<MinCell>,
    ) -> (Vec<usize>, Option<usize>) {
        let mut rows = Vec::new();
        let mut more = None;
        for placed in self.layout(area, min_side, reserve) {
            match placed.tile {
                Tile::Item(row) => rows.push(row),
                Tile::More { from, .. } => more = Some(from),
            }
        }
        (rows, more)
    }

    /// The tile under `at`, if any.
    #[must_use]
    pub fn hit(
        &self,
        area: Rect,
        min_side: i32,
        reserve: Option<MinCell>,
        at: Point,
    ) -> Option<Tile> {
        self.layout(area, min_side, reserve)
            .into_iter()
            .find(|placed| placed.rect.contains(at))
            .map(|placed| placed.tile)
    }

    /// Open the overflow page starting at rank `from`.
    pub fn open_more(&mut self, from: usize) {
        if from > self.page_start() && from < self.rank.len() {
            self.pages.push(from);
        }
    }

    /// Step one page deeper for a positive wheel `lines`, or back out for a
    /// negative one. Returns whether the page changed.
    pub fn wheel(
        &mut self,
        area: Rect,
        min_side: i32,
        reserve: Option<MinCell>,
        lines: i32,
    ) -> bool {
        match lines.signum() {
            1 => self.page(area, min_side, reserve).1.is_some_and(|from| {
                let before = self.depth();
                self.open_more(from);
                self.depth() != before
            }),
            -1 => self.close_page(),
            _ => false,
        }
    }

    /// Close the innermost overflow page. Returns whether one was open.
    pub fn close_page(&mut self) -> bool {
        self.pages.pop().is_some()
    }

    /// How many overflow pages deep the field is.
    #[must_use]
    pub const fn depth(&self) -> usize {
        self.pages.len()
    }

    fn page_start(&self) -> usize {
        self.pages.last().copied().unwrap_or(0)
    }

    pub const fn arm(&mut self, row: usize) {
        self.armed = Some(row);
    }

    pub const fn disarm(&mut self) {
        self.armed = None;
    }

    /// The armed folder, consumed.
    pub const fn take_armed(&mut self) -> Option<usize> {
        self.armed.take()
    }

    /// Visible-entry indices, largest first.
    #[must_use]
    pub fn ranked(&self) -> &[usize] {
        self.rank.order()
    }

    pub const fn entered(&mut self, now_ms: u64) {
        self.entered_ms = Some(now_ms);
    }

    /// Whether a press at `now_ms` follows a folder entry closely enough to
    /// be the rest of a double-click. Asking consumes the entry.
    pub fn swallows(&mut self, now_ms: u64, within_ms: u64) -> bool {
        self.entered_ms
            .take()
            .is_some_and(|then| now_ms.saturating_sub(then) <= within_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::TileField;
    use crate::browser::Browser;
    use crate::spiral::MinCell;
    use crate::test_support::TempDir;
    use crate::tile_coloring::TileColoring;
    use crate::tile_rank::Tile;
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
            .map(|placed| match placed.tile {
                Tile::Item(row) => browser.entry(row).expect("entry").name.clone(),
                Tile::More { count, .. } => format!("+{count}"),
            })
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

    #[test]
    fn overflow_pages_open_and_close() {
        let dir = TempDir::new("tile_field_pages");
        for i in 0..40 {
            dir.create_file(&format!("f{i:02}"), &vec![0; 100 + i]);
        }
        let browser = Browser::open(dir.path(), 800, 600, 28).expect("open");
        let (mut field, _wake) = field();
        field.sync(&browser);
        let first = field.layout(AREA, 120, None);
        let Some(Tile::More { from, count }) = first.last().map(|placed| placed.tile) else {
            panic!("forty files cannot fit at this size: {first:?}");
        };
        assert_eq!(from + count, 40);
        let at = Point::new(
            first.last().expect("tile").rect.x + 1,
            first.last().expect("tile").rect.y + 1,
        );
        assert_eq!(
            field.hit(AREA, 120, None, at),
            Some(Tile::More { from, count })
        );

        field.open_more(from);
        assert_eq!(field.depth(), 1);
        let second = names(&field, &browser, AREA, 120);
        assert_eq!(second[0], format!("f{:02}", 39 - from));
        assert!(field.close_page());
        assert!(!field.close_page());
    }

    #[test]
    fn the_wheel_turns_pages_and_stops_at_either_end() {
        let dir = TempDir::new("tile_field_wheel");
        for i in 0..40 {
            dir.create_file(&format!("f{i:02}"), &vec![0; 100 + i]);
        }
        let browser = Browser::open(dir.path(), 800, 600, 28).expect("open");
        let (mut field, _wake) = field();
        field.sync(&browser);
        assert!(
            !field.wheel(AREA, 120, None, -1),
            "nothing to close at the top"
        );
        assert!(!field.wheel(AREA, 120, None, 0));
        let mut turned = 0;
        while field.wheel(AREA, 120, None, 3) {
            turned += 1;
            assert!(turned < 40, "the wheel must reach the last page");
        }
        assert!(turned > 0);
        assert_eq!(field.depth(), turned);
        assert!(
            field.page(AREA, 120, None).1.is_none(),
            "the last page has no overflow"
        );
        assert!(field.wheel(AREA, 120, None, -1));
        assert_eq!(field.depth(), turned - 1);
    }

    #[test]
    fn a_new_listing_closes_open_pages() {
        let dir = TempDir::new("tile_field_relist");
        for i in 0..40 {
            dir.create_file(&format!("f{i:02}"), &[0; 10]);
        }
        dir.create_dir("sub");
        let mut browser = Browser::open(dir.path(), 800, 600, 28).expect("open");
        let (mut field, _wake) = field();
        field.sync(&browser);
        let Some(Tile::More { from, .. }) = field.layout(AREA, 120, None).last().map(|p| p.tile)
        else {
            panic!("expected overflow");
        };
        field.open_more(from);
        field.arm(0);
        browser.go_to(&dir.path().join("sub"));
        field.sync(&browser);
        assert_eq!(field.depth(), 0);
        assert_eq!(field.take_armed(), None);
    }

    #[test]
    fn an_open_more_that_does_not_advance_is_refused() {
        let dir = TempDir::new("tile_field_refuse");
        dir.create_file("a", &[0; 10]);
        let browser = Browser::open(dir.path(), 800, 600, 28).expect("open");
        let (mut field, _wake) = field();
        field.sync(&browser);
        field.open_more(0);
        field.open_more(7);
        assert_eq!(field.depth(), 0);
    }

    #[test]
    fn a_press_soon_after_entering_is_swallowed_once() {
        let (mut field, _wake) = field();
        assert!(!field.swallows(1000, 400));
        field.entered(1000);
        assert!(field.swallows(1300, 400));
        assert!(!field.swallows(1310, 400));
        field.entered(1000);
        assert!(!field.swallows(1500, 400));
    }

    #[test]
    fn the_ranking_lists_every_visible_entry() {
        let dir = TempDir::new("tile_field_ranked");
        dir.create_file("a", &[0; 1]);
        dir.create_file("b", &[0; 9]);
        let browser = Browser::open(dir.path(), 800, 600, 28).expect("open");
        let (mut field, _wake) = field();
        field.sync(&browser);
        assert_eq!(field.ranked(), [1, 0]);
    }

    #[test]
    fn the_page_lists_its_entries_and_where_the_overflow_starts() {
        let dir = TempDir::new("tile_field_page");
        for i in 0..40 {
            dir.create_file(&format!("f{i:02}"), &vec![0; 100 + i]);
        }
        let browser = Browser::open(dir.path(), 800, 600, 28).expect("open");
        let (mut field, _wake) = field();
        field.sync(&browser);
        let (rows, more) = field.page(AREA, 120, None);
        let from = more.expect("forty files overflow");
        assert_eq!(rows.len(), from);
        assert_eq!(rows[0], 39, "the largest file leads");
        let (all, none) = field.page(AREA, 1, None);
        assert!(none.is_none() || all.len() < 40);
    }

    const NEED: MinCell = MinCell { w: 150, h: 200 };

    fn crowded_field(tag: &str) -> (TileField, Browser, TempDir) {
        let dir = TempDir::new(tag);
        for i in 0..40 {
            dir.create_file(&format!("f{i:02}"), &vec![0; 100 + i]);
        }
        let browser = Browser::open(dir.path(), 800, 600, 28).expect("open");
        let (mut field, _wake) = field();
        field.sync(&browser);
        (field, browser, dir)
    }

    #[test]
    fn the_card_takes_the_innermost_cell_and_outgrows_the_small_tiles() {
        let (field, _browser, _dir) = crowded_field("tile_field_card_last");
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
            let (field, _browser, _dir) = crowded_field("tile_field_slots");
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
    fn the_overflow_tile_is_the_last_tile_before_the_card() {
        let (field, _browser, _dir) = crowded_field("tile_field_card_more");
        let arranged = field.arrange(AREA, 120, Some(NEED));
        assert!(matches!(
            arranged.tiles.last().map(|p| p.tile),
            Some(Tile::More { .. })
        ));
    }

    #[test]
    fn the_card_costs_tiles_but_nothing_overlaps_it() {
        let (field, _browser, _dir) = crowded_field("tile_field_card_cost");
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
    }

    #[test]
    fn no_reserve_means_no_card_cell() {
        let (field, _browser, _dir) = crowded_field("tile_field_no_card");
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
        assert_eq!(
            field.hit(AREA, 1, None, Point::new(5, 5)),
            Some(Tile::Item(0))
        );
    }
}

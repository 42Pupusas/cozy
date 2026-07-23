//! The set of pixel rectangles a single dashboard paint actually touched.
//!
//! The monitor paints onto a persistent backbuffer that a translucent window
//! then premultiplies and copies into the compositor's shm slots. Both of
//! those post-paint passes are strictly linear in pixel count, so doing them
//! over the whole window when a steady-state frame mutates only ~0.15% of it
//! (measured: the four chart cards scroll ~1px columns and a few core bars
//! move) is ~660x wasted work every frame -- invisible in a release build's
//! wall clock but catastrophic in debug, and pure busywork either way.
//!
//! [`DamageList`] is how the painter reports the rectangles it genuinely drew,
//! so the platform layer can premultiply and upload only those. It is a small
//! fixed-capacity set (no per-frame allocation on the hot path): a dashboard
//! frame damages a handful of regions -- four chart edges, a clutch of core-bar
//! columns, and on a full repaint the whole window as one rect -- so the cap is
//! generous but bounded. When the true region count would exceed the cap the
//! list *coarsens* to a single bounding rectangle rather than dropping damage,
//! which is always safe (it can only over-copy, never under-copy) and keeps the
//! worst case identical to today's full-window behaviour.

use platform::draw::Rect;

/// Maximum distinct regions retained before coarsening to one bounding rect.
/// Sized for the dashboard's real damage shape: 4 chart cards + up to a
/// typical core count of bars + a little slack, all on the stack.
const MAX_REGIONS: usize = 32;

/// A bounded collection of damaged rectangles for one frame's paint.
///
/// Empty means "nothing changed" (the caller may skip premultiply/upload
/// entirely); a single full-window rect means "treat as a full repaint".
#[derive(Clone, Debug)]
pub struct DamageList {
    regions: [Rect; MAX_REGIONS],
    len: usize,
    /// Once the region count would exceed `MAX_REGIONS`, the list collapses
    /// into `regions[0]` as a bounding box of everything pushed so far and
    /// stays collapsed. Prevents unbounded growth while never losing coverage.
    coarsened: bool,
}

impl DamageList {
    /// An empty damage list -- nothing was drawn.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            regions: [Rect::new(0, 0, 0, 0); MAX_REGIONS],
            len: 0,
            coarsened: false,
        }
    }

    /// A damage list covering exactly one rectangle -- the whole window on a
    /// full repaint, or any single known-dirty region.
    #[must_use]
    pub fn single(rect: Rect) -> Self {
        let mut list = Self::new();
        list.push(rect);
        list
    }

    /// Record that `rect` was drawn this frame. Empty rects are ignored;
    /// once capacity is exceeded the whole list coarsens to a bounding box so
    /// coverage is never lost, only precision.
    pub fn push(&mut self, rect: Rect) {
        if rect.w <= 0 || rect.h <= 0 {
            return;
        }
        if self.coarsened {
            self.regions[0] = Self::union(self.regions[0], rect);
            return;
        }
        if self.len == MAX_REGIONS {
            // Collapse everything to a single bounding rect and fold `rect` in.
            let mut bound = self.regions[0];
            for r in &self.regions[1..self.len] {
                bound = Self::union(bound, *r);
            }
            self.regions[0] = Self::union(bound, rect);
            self.len = 1;
            self.coarsened = true;
            return;
        }
        self.regions[self.len] = rect;
        self.len += 1;
    }

    /// Fold every region of `other` into this list, preserving the same
    /// capacity/coarsening discipline. Lets a parent painter merge the damage
    /// its child widgets each reported into one frame-level list.
    pub fn extend(&mut self, other: &Self) {
        for &r in other.regions() {
            self.push(r);
        }
    }

    /// The damaged rectangles. Each is guaranteed non-empty.
    #[must_use]
    pub fn regions(&self) -> &[Rect] {
        &self.regions[..self.len]
    }

    /// Whether nothing was damaged.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The bounding box over every damaged region, or an empty rect if none.
    /// Retained as the coarse fallback for any consumer that needs a single
    /// rect (and exercised by the unit tests); the shipped present path uses
    /// the per-region set directly, so the non-test build sees no caller.
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn bounding(&self) -> Rect {
        if self.len == 0 {
            return Rect::new(0, 0, 0, 0);
        }
        let mut bound = self.regions[0];
        for r in &self.regions[1..self.len] {
            bound = Self::union(bound, *r);
        }
        bound
    }

    /// The number of distinct pixel rows any region touches, over a window of
    /// `height` rows. This is the cost proxy that actually matters for the
    /// present bill: the shipped shm path is row-band granular (it uploads
    /// every full-width row a rect spans, ignoring x/width), so two thin
    /// chart strips at different y-bands cost their combined *row* coverage,
    /// not their tiny combined area. Overlapping row spans are counted once.
    /// Returned as a fraction 0..=1 of the window height for direct display.
    #[must_use]
    #[allow(
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "height is guarded > 0 above; row counts are far below f64 mantissa limits"
    )]
    pub fn row_coverage_fraction(&self, height: i32) -> f64 {
        if height <= 0 || self.len == 0 {
            return 0.0;
        }
        // A small stack bitmap of touched rows would be ideal, but the window
        // is a few hundred rows tall and this runs once per painted frame
        // under a profiling flag, so a boolean vec is fine and clear.
        let h = height as usize;
        let mut touched = vec![false; h];
        for r in &self.regions[..self.len] {
            let top = r.y.max(0) as usize;
            let bottom = (r.bottom().max(r.y) as usize).min(h);
            for row in touched.iter_mut().take(bottom).skip(top) {
                *row = true;
            }
        }
        let count = touched.iter().filter(|&&t| t).count();
        count as f64 / h as f64
    }

    /// Convert into the platform layer's damage type so `tick_with_painter`
    /// can premultiply and upload only these regions. An empty list becomes
    /// `Damage::none()` (skip all post-paint work); a coarsened single-region
    /// list is still just its rects -- only an explicit full repaint uses
    /// `Damage::full()`, chosen by the painter, not inferred here.
    #[must_use]
    pub fn to_platform(&self) -> platform::draw::Damage {
        if self.is_empty() {
            platform::draw::Damage::none()
        } else {
            platform::draw::Damage::regions(self.regions().to_vec())
        }
    }

    /// Smallest axis-aligned rect covering both inputs. Assumes both are
    /// non-empty (guaranteed by `push`).
    fn union(a: Rect, b: Rect) -> Rect {
        let x = a.x.min(b.x);
        let y = a.y.min(b.y);
        let right = a.right().max(b.right());
        let bottom = a.bottom().max(b.bottom());
        Rect::new(x, y, right - x, bottom - y)
    }
}

impl Default for DamageList {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{DamageList, MAX_REGIONS};
    use platform::draw::Rect;

    #[test]
    fn empty_list_reports_empty() {
        let d = DamageList::new();
        assert!(d.is_empty());
        assert_eq!(d.regions().len(), 0);
        assert_eq!(d.bounding(), Rect::new(0, 0, 0, 0));
    }

    #[test]
    fn zero_area_rects_are_ignored() {
        let mut d = DamageList::new();
        d.push(Rect::new(5, 5, 0, 10));
        d.push(Rect::new(5, 5, 10, 0));
        d.push(Rect::new(5, 5, -3, 10));
        assert!(d.is_empty());
    }

    #[test]
    fn distinct_regions_are_retained_individually() {
        let mut d = DamageList::new();
        d.push(Rect::new(0, 0, 10, 10));
        d.push(Rect::new(100, 100, 5, 5));
        assert_eq!(d.regions().len(), 2);
        assert_eq!(d.regions()[0], Rect::new(0, 0, 10, 10));
        assert_eq!(d.regions()[1], Rect::new(100, 100, 5, 5));
    }

    #[test]
    fn bounding_covers_all_regions() {
        let mut d = DamageList::new();
        d.push(Rect::new(10, 20, 5, 5)); // -> right 15, bottom 25
        d.push(Rect::new(100, 0, 10, 200)); // -> right 110, bottom 200
        // Bounding: x 10, y 0, right 110, bottom 200.
        assert_eq!(d.bounding(), Rect::new(10, 0, 100, 200));
    }

    #[test]
    fn exceeding_capacity_coarsens_to_a_single_covering_rect() {
        let mut d = DamageList::new();
        // Push one more than capacity, all disjoint 1px cells on a diagonal.
        for i in 0..=MAX_REGIONS as i32 {
            d.push(Rect::new(i * 10, i * 10, 1, 1));
        }
        // Must have collapsed to a single region that still covers the extremes.
        assert_eq!(d.regions().len(), 1);
        let last = MAX_REGIONS as i32 * 10;
        let covering = d.regions()[0];
        assert!(covering.x <= 0 && covering.y <= 0);
        assert!(covering.right() >= last + 1 && covering.bottom() >= last + 1);
    }

    #[test]
    fn single_constructor_holds_exactly_that_rect() {
        let r = Rect::new(3, 4, 20, 30);
        let d = DamageList::single(r);
        assert_eq!(d.regions(), &[r]);
        assert_eq!(d.bounding(), r);
    }

    #[test]
    fn row_coverage_counts_separated_bands_and_ignores_width() {
        // Two thin strips (width 2) at different vertical bands: the shm path
        // uploads every row each touches regardless of width, so a 100-tall
        // window with rows 0..10 and 50..60 touched is 20 rows = 20% coverage.
        // This is the metric that reveals chart bands as the present cost.
        let mut d = DamageList::new();
        d.push(Rect::new(0, 0, 2, 10));
        d.push(Rect::new(300, 50, 2, 10));
        assert!((d.row_coverage_fraction(100) - 0.20).abs() < 1e-9);
    }

    #[test]
    fn row_coverage_counts_overlapping_rows_once() {
        // Overlapping vertical spans must not double-count: rows 0..10 and
        // 5..15 cover 15 distinct rows, not 20.
        let mut d = DamageList::new();
        d.push(Rect::new(0, 0, 2, 10));
        d.push(Rect::new(50, 5, 2, 10));
        assert!((d.row_coverage_fraction(100) - 0.15).abs() < 1e-9);
    }

    #[test]
    fn row_coverage_empty_and_degenerate_height_are_zero() {
        assert_eq!(DamageList::new().row_coverage_fraction(100), 0.0);
        let d = DamageList::single(Rect::new(0, 0, 10, 10));
        assert_eq!(d.row_coverage_fraction(0), 0.0);
    }
}

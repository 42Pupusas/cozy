//! Scrollbar thumb geometry and drag handling.
//!
//! The scrollbar owns no scroll position of its own. It is a *view* onto a
//! [`ListViewport`], which stays the single source of truth -- two copies of
//! "where are we scrolled to" would inevitably drift apart, and the bug that
//! produces (thumb in one place, list in another) is maddening to chase.
//! The only state here is the in-progress drag.
//!
//! ### Why the thumb has a minimum length
//! A purely proportional thumb (`track * viewport / content`) collapses to
//! sub-pixel height on a large directory -- at 100k rows it is a fraction of
//! a pixel, so there is nothing left to grab. Clamping to a minimum keeps it
//! usable.
//!
//! That clamp is also the module's one real trap. Once the thumb has a
//! minimum length, the thumb *travels* over `track - thumb`, not over the
//! whole track, so the naive mapping
//!
//! ```text
//! offset = position / track * content        // WRONG
//! ```
//!
//! overshoots: it reaches maximum scroll before the thumb reaches the bottom
//! of the track, and the last rows become unreachable by dragging. Both
//! directions of the mapping here divide by `track - thumb` instead, so
//! dragging the thumb flush with the bottom lands exactly on `max_scroll` and
//! the round trip is stable.

use crate::ListViewport;

/// The thumb's position and size within the track, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Thumb {
    /// Distance from the top of the track to the top of the thumb.
    pub offset: i32,
    /// The thumb's length along the track.
    pub length: i32,
}

/// A vertical scrollbar driven by a [`ListViewport`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scrollbar {
    /// The track's length in pixels -- normally the viewport height.
    track: i32,
    /// Shortest the thumb may become, so it stays grabbable on a huge list.
    min_thumb: i32,
    /// While dragging, how far below the thumb's top edge the pointer first
    /// grabbed it. Holding this is what makes the thumb move *with* the
    /// cursor instead of snapping its top edge to it on the first motion.
    drag_grab: Option<i32>,
}

impl Scrollbar {
    /// Default shortest thumb, in pixels. Roughly a comfortable click target.
    pub const DEFAULT_MIN_THUMB: i32 = 24;

    /// A scrollbar for a track of `track` pixels.
    #[must_use]
    pub fn new(track: i32) -> Self {
        Self {
            track: track.max(0),
            min_thumb: Self::DEFAULT_MIN_THUMB,
            drag_grab: None,
        }
    }

    /// Overrides the minimum thumb length (builder style).
    #[must_use]
    pub fn with_min_thumb(mut self, min_thumb: i32) -> Self {
        self.min_thumb = min_thumb.max(1);
        self
    }

    /// Updates the track length, e.g. on a window resize.
    pub fn set_track(&mut self, track: i32) {
        self.track = track.max(0);
    }

    /// The track's length in pixels.
    #[must_use]
    pub const fn track(&self) -> i32 {
        self.track
    }

    /// Whether a drag is currently in progress.
    #[must_use]
    pub const fn is_dragging(&self) -> bool {
        self.drag_grab.is_some()
    }

    /// Whether the scrollbar should be shown at all: only when the content
    /// actually overflows. Drawing a full-length thumb on a short list is
    /// visual noise that says nothing.
    #[must_use]
    pub fn is_needed(&self, viewport: &ListViewport) -> bool {
        self.track > 0 && viewport.max_scroll() > 0
    }

    /// The thumb's length for `viewport`, clamped to the minimum and to the
    /// track.
    #[must_use]
    fn thumb_length(&self, viewport: &ListViewport) -> i32 {
        let content = viewport.content_height();
        if content <= 0 {
            return self.track;
        }
        // Proportional length, in i64 so `track * height` cannot overflow on
        // a tall track.
        let proportional =
            i64::from(self.track) * i64::from(viewport.height()) / i64::from(content);
        let proportional = i32::try_from(proportional).unwrap_or(self.track);
        proportional.clamp(self.min_thumb.min(self.track), self.track)
    }

    /// How far the thumb can travel: the track minus the thumb itself.
    ///
    /// This -- not the track length -- is the denominator for both directions
    /// of the position mapping. See the module docs.
    #[must_use]
    fn travel(&self, viewport: &ListViewport) -> i32 {
        (self.track - self.thumb_length(viewport)).max(0)
    }

    /// The thumb's current geometry for `viewport`.
    #[must_use]
    pub fn thumb(&self, viewport: &ListViewport) -> Thumb {
        let length = self.thumb_length(viewport);
        let max_scroll = viewport.max_scroll();
        let offset = if max_scroll <= 0 {
            0
        } else {
            let travel = i64::from(self.travel(viewport));
            let scrolled = i64::from(viewport.offset()) * travel / i64::from(max_scroll);
            i32::try_from(scrolled).unwrap_or(0)
        };
        Thumb { offset, length }
    }

    /// Whether `y` (track-relative) falls on the thumb.
    #[must_use]
    pub fn hits_thumb(&self, viewport: &ListViewport, y: i32) -> bool {
        let thumb = self.thumb(viewport);
        y >= thumb.offset && y < thumb.offset.saturating_add(thumb.length)
    }

    /// Converts a thumb top-edge position into a scroll offset, dividing by
    /// the travel distance so a thumb flush with the bottom of the track maps
    /// exactly to `max_scroll`.
    #[must_use]
    fn offset_for_thumb(&self, viewport: &ListViewport, thumb_top: i32) -> i32 {
        let travel = self.travel(viewport);
        if travel <= 0 {
            return 0;
        }
        let scrolled =
            i64::from(thumb_top) * i64::from(viewport.max_scroll()) / i64::from(travel);
        i32::try_from(scrolled).unwrap_or_else(|_| viewport.max_scroll())
    }

    /// Begins a drag at track-relative `y`.
    ///
    /// Pressing *on* the thumb grabs it where it was clicked. Pressing on the
    /// empty track jumps so the thumb centres on the cursor and then grabs
    /// it, so a click-and-drag from the track keeps working in one gesture
    /// rather than needing a second press. Returns whether the offset moved.
    pub fn press(&mut self, viewport: &mut ListViewport, y: i32) -> bool {
        if self.hits_thumb(viewport, y) {
            let thumb = self.thumb(viewport);
            self.drag_grab = Some(y - thumb.offset);
            return false;
        }
        let length = self.thumb_length(viewport);
        let centred = y - length / 2;
        self.drag_grab = Some(length / 2);
        let target = self.offset_for_thumb(viewport, centred.clamp(0, self.travel(viewport)));
        viewport.scroll_to(target)
    }

    /// Continues a drag to track-relative `y`. A no-op unless [`Self::press`]
    /// started one. Returns whether the offset moved.
    pub fn drag(&mut self, viewport: &mut ListViewport, y: i32) -> bool {
        let Some(grab) = self.drag_grab else {
            return false;
        };
        let thumb_top = (y - grab).clamp(0, self.travel(viewport));
        let target = self.offset_for_thumb(viewport, thumb_top);
        viewport.scroll_to(target)
    }

    /// Ends any drag in progress.
    pub const fn release(&mut self) {
        self.drag_grab = None;
    }
}

#[cfg(test)]
mod tests {
    use super::{Scrollbar, Thumb};
    use crate::ListViewport;

    /// 100 rows of 20px (2000px content) in a 200px viewport and track.
    fn setup() -> (Scrollbar, ListViewport) {
        (Scrollbar::new(200), ListViewport::new(20, 100, 200))
    }

    #[test]
    fn a_thumb_is_proportional_to_the_visible_fraction() {
        // min_thumb of 1 so the proportional maths is what is under test --
        // the default 24px minimum would clamp this 20px result.
        let bar = Scrollbar::new(200).with_min_thumb(1);
        let v = ListViewport::new(20, 100, 200);
        // 200 visible of 2000 content = one tenth of a 200px track.
        assert_eq!(bar.thumb(&v), Thumb { offset: 0, length: 20 });
    }

    #[test]
    fn the_thumb_sits_at_the_top_when_unscrolled() {
        let (bar, v) = setup();
        assert_eq!(bar.thumb(&v).offset, 0);
    }

    #[test]
    fn the_thumb_sits_flush_with_the_bottom_at_max_scroll() {
        let (bar, mut v) = setup();
        v.scroll_to(v.max_scroll());
        let thumb = bar.thumb(&v);
        assert_eq!(
            thumb.offset + thumb.length,
            bar.track(),
            "thumb must reach the end of the track exactly"
        );
    }

    #[test]
    fn a_short_list_needs_no_scrollbar() {
        let bar = Scrollbar::new(200);
        let v = ListViewport::new(20, 3, 200); // 60px of content
        assert!(!bar.is_needed(&v));
    }

    #[test]
    fn an_overflowing_list_needs_a_scrollbar() {
        let (bar, v) = setup();
        assert!(bar.is_needed(&v));
    }

    #[test]
    fn a_zero_length_track_is_never_needed() {
        let bar = Scrollbar::new(0);
        let v = ListViewport::new(20, 1000, 200);
        assert!(!bar.is_needed(&v));
    }

    #[test]
    fn an_empty_list_yields_a_full_length_thumb() {
        let bar = Scrollbar::new(200);
        let v = ListViewport::new(20, 0, 200);
        assert_eq!(bar.thumb(&v), Thumb { offset: 0, length: 200 });
    }

    #[test]
    fn the_thumb_never_shrinks_below_the_minimum() {
        // 100k rows would give a sub-pixel proportional thumb.
        let bar = Scrollbar::new(600);
        let v = ListViewport::new(24, 100_000, 600);
        let thumb = bar.thumb(&v);
        assert_eq!(thumb.length, Scrollbar::DEFAULT_MIN_THUMB);
        assert!(thumb.length > 0, "a thumb must stay grabbable");
    }

    #[test]
    fn the_minimum_thumb_is_overridable() {
        let bar = Scrollbar::new(600).with_min_thumb(40);
        let v = ListViewport::new(24, 100_000, 600);
        assert_eq!(bar.thumb(&v).length, 40);
    }

    #[test]
    fn a_minimum_longer_than_the_track_cannot_overflow_it() {
        let bar = Scrollbar::new(10).with_min_thumb(100);
        let v = ListViewport::new(20, 1000, 10);
        let thumb = bar.thumb(&v);
        assert!(thumb.length <= bar.track());
        assert!(thumb.offset + thumb.length <= bar.track());
    }

    #[test]
    fn a_clamped_thumb_still_reaches_the_bottom_at_max_scroll() {
        // The trap: with a minimum-length thumb, dividing by the track
        // instead of the travel distance would leave the last rows
        // unreachable by dragging.
        let bar = Scrollbar::new(600);
        let mut v = ListViewport::new(24, 100_000, 600);
        v.scroll_to(v.max_scroll());
        let thumb = bar.thumb(&v);
        assert_eq!(thumb.offset + thumb.length, 600);
    }

    #[test]
    fn dragging_a_clamped_thumb_to_the_bottom_reaches_max_scroll() {
        let mut bar = Scrollbar::new(600);
        let mut v = ListViewport::new(24, 100_000, 600);
        bar.press(&mut v, 0);
        bar.drag(&mut v, 10_000); // far past the end
        assert_eq!(v.offset(), v.max_scroll(), "last rows must be reachable");
    }

    #[test]
    fn the_thumb_position_round_trips_through_a_drag() {
        // Position -> offset -> position must be stable, or the thumb
        // visibly creeps as you drag it.
        let mut bar = Scrollbar::new(600);
        let mut v = ListViewport::new(24, 100_000, 600);
        for target in [0, 50, 137, 300, 500, 576] {
            // Rewind and grab the thumb where it actually is. Pressing a
            // fixed y across iterations would land on the empty track once
            // the thumb had moved, silently testing the centring path
            // instead of the drag path.
            v.scroll_to(0);
            let grab = bar.thumb(&v).offset;
            bar.press(&mut v, grab);
            bar.drag(&mut v, target);
            let thumb = bar.thumb(&v);
            assert!(
                (thumb.offset - target).abs() <= 1,
                "thumb at {} after dragging to {target}",
                thumb.offset
            );
            bar.release();
        }
    }

    #[test]
    fn hit_testing_finds_the_thumb_and_misses_the_track() {
        let (bar, v) = setup();
        // Derived from the thumb rather than hardcoded, so the minimum-length
        // clamp cannot silently invalidate the boundary being probed.
        let thumb = bar.thumb(&v);
        assert!(bar.hits_thumb(&v, thumb.offset));
        assert!(bar.hits_thumb(&v, thumb.offset + thumb.length - 1));
        assert!(
            !bar.hits_thumb(&v, thumb.offset + thumb.length),
            "just past the thumb is track"
        );
        assert!(!bar.hits_thumb(&v, 150));
    }

    #[test]
    fn pressing_the_thumb_does_not_move_it() {
        // Grabbing must not jump the list -- only dragging scrolls.
        let (mut bar, mut v) = setup();
        v.scroll_to(500);
        let before = bar.thumb(&v).offset;
        assert!(!bar.press(&mut v, before + 5));
        assert_eq!(v.offset(), 500);
        assert!(bar.is_dragging());
    }

    #[test]
    fn pressing_the_thumb_keeps_the_grab_point_under_the_cursor() {
        // Without remembering the grab offset, the thumb's top edge would
        // snap to the cursor on the first motion event.
        let (mut bar, mut v) = setup();
        v.scroll_to(1000);
        let thumb_before = bar.thumb(&v);
        let grab_at = thumb_before.offset + 7;
        bar.press(&mut v, grab_at);
        bar.drag(&mut v, grab_at + 30);
        let thumb_after = bar.thumb(&v);
        assert!(
            (thumb_after.offset - (thumb_before.offset + 30)).abs() <= 1,
            "thumb moved with the cursor, not snapped to it"
        );
    }

    #[test]
    fn pressing_the_empty_track_centres_the_thumb_on_the_cursor() {
        let (mut bar, mut v) = setup();
        assert!(bar.press(&mut v, 100));
        let thumb = bar.thumb(&v);
        let centre = thumb.offset + thumb.length / 2;
        assert!((centre - 100).abs() <= 1, "thumb centred at {centre}");
    }

    #[test]
    fn pressing_the_track_leaves_a_drag_armed_for_one_gesture() {
        let (mut bar, mut v) = setup();
        bar.press(&mut v, 150);
        assert!(bar.is_dragging(), "press-and-drag from the track must work");
        bar.drag(&mut v, 160);
        let thumb = bar.thumb(&v);
        assert!((thumb.offset + thumb.length / 2 - 160).abs() <= 1);
    }

    #[test]
    fn dragging_without_pressing_does_nothing() {
        let (mut bar, mut v) = setup();
        assert!(!bar.drag(&mut v, 100));
        assert_eq!(v.offset(), 0);
    }

    #[test]
    fn dragging_after_release_does_nothing() {
        let (mut bar, mut v) = setup();
        bar.press(&mut v, 0);
        bar.release();
        assert!(!bar.drag(&mut v, 150));
        assert_eq!(v.offset(), 0);
    }

    #[test]
    fn a_drag_clamps_at_both_ends() {
        let (mut bar, mut v) = setup();
        bar.press(&mut v, 0);
        assert!(!bar.drag(&mut v, -500), "already at the top");
        assert_eq!(v.offset(), 0);
        bar.drag(&mut v, 10_000);
        assert_eq!(v.offset(), v.max_scroll());
    }

    #[test]
    fn the_thumb_stays_inside_the_track_at_every_offset() {
        // Invariant sweep: a thumb that pokes out of the track paints over
        // whatever is next to the scrollbar.
        let bar = Scrollbar::new(600);
        let mut v = ListViewport::new(24, 100_000, 600);
        for offset in (0..=v.max_scroll()).step_by(9_973) {
            v.scroll_to(offset);
            let thumb = bar.thumb(&v);
            assert!(thumb.offset >= 0, "offset {offset}: thumb above the track");
            assert!(
                thumb.offset + thumb.length <= bar.track(),
                "offset {offset}: thumb past the track end"
            );
        }
    }

    #[test]
    fn resizing_the_track_rescales_the_thumb() {
        // min_thumb of 1 again: the point here is that the length tracks the
        // track, which the default clamp would mask at this size.
        let mut bar = Scrollbar::new(200).with_min_thumb(1);
        let v = ListViewport::new(20, 100, 200);
        assert_eq!(bar.thumb(&v).length, 20);
        bar.set_track(400);
        assert_eq!(bar.thumb(&v).length, 40);
    }

    #[test]
    fn a_scrollbar_and_its_viewport_never_disagree() {
        // The scrollbar holds no offset of its own; this pins that down.
        let (mut bar, mut v) = setup();
        bar.press(&mut v, 5);
        bar.drag(&mut v, 90);
        let from_bar = bar.offset_for_thumb(&v, bar.thumb(&v).offset);
        assert!(
            (from_bar - v.offset()).abs() <= 20,
            "thumb {} vs viewport {}",
            from_bar,
            v.offset()
        );
    }
}

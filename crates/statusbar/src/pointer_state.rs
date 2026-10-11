//! Pointer input state for [`super::BarWindow`]: `wl_pointer` position
//! tracking, click detection, and scroll accumulation.
//!
//! Extracted because it's a genuinely self-contained subsystem — turning
//! raw `wl_pointer.{enter,motion,button,axis}` events into the two things
//! `BarWindow`'s caller actually wants (a hit-test position, and drained
//! click/scroll queues) doesn't touch the swap chain, layer-surface
//! protocol, or module rendering, only pointer geometry and button/axis
//! bookkeeping.

use platform::draw::Point;
use platform::event::RawEvent;
use platform::id::{PointerId, SurfaceId};
use platform::protocol::{ButtonState, PointerButton, PointerEvent};

/// Tracks the bar's `wl_pointer` object (once the seat advertises one) plus
/// everything derived from its events: last known position, a pending
/// click, and queued scroll deltas.
#[derive(Default)]
pub struct PointerState {
    pointer: Option<PointerId>,
    /// Latest known pointer position, physical pixels, surface-local
    /// (`(0,0)` at the bar's top-left) -- updated on Enter/Motion, read by
    /// [`PointerState::pos`] so the caller can hit-test which module
    /// segment a scroll landed on.
    pos: Point,
    /// Pending scroll deltas from `wl_pointer.axis` (vertical only), drained
    /// by [`PointerState::take_scroll`] once per `tick`. One entry per axis
    /// event; positive = wheel down, negative = wheel up (same sign
    /// convention as `platform::TermWindow`'s scroll handling).
    pending_scroll: Vec<i32>,
    /// Sub-pixel remainder accumulator, in raw `wl_fixed` 24.8 units: a
    /// touchpad or hi-res mouse wheel routinely sends many axis events
    /// whose magnitude is under one whole pixel, and `Fixed::to_pixels`
    /// truncates -- converting per-event before accumulating silently
    /// discarded those (a slow/gentle scroll could produce nothing but
    /// zeros, forever). This carries the truncated fraction forward so it
    /// isn't lost, added back in before the next event's own truncation.
    scroll_fixed_remainder: i32,
    /// Set on a left-button *press*, cleared and reported on the matching
    /// *release* -- only a clean press-then-release at (roughly) the same
    /// spot counts as a click, same convention as everything else that
    /// distinguishes a click from the start of a drag. A bar has nothing to
    /// drag, so this never needs to track motion, just press/release
    /// pairing.
    left_pressed: bool,
    /// The surface a qualifying left-click landed on, drained by
    /// [`PointerState::take_click`] once per `tick`.
    pending_click: Option<SurfaceId>,
    /// The bar and an open panel popup share this pointer; positions are
    /// local to whichever surface it is over.
    focus: Option<SurfaceId>,
    /// Serial of the latest left press, the one a popup grab must cite.
    press_serial: Option<u32>,
}

impl PointerState {
    /// No pointer object yet -- waiting on `wl_seat.capabilities`.
    pub fn new() -> Self {
        Self::default()
    }

    /// The `wl_pointer` object id, once the seat has advertised one.
    #[must_use]
    pub const fn id(&self) -> Option<PointerId> {
        self.pointer
    }

    /// Record the `wl_pointer` object obtained from `wl_seat.get_pointer`.
    pub const fn set_id(&mut self, pointer: PointerId) {
        self.pointer = Some(pointer);
    }

    /// Latest known pointer position, physical pixels, surface-local
    /// (`(0,0)` at the bar's top-left).
    #[must_use]
    pub const fn pos(&self) -> Point {
        self.pos
    }

    /// Drain and return every scroll delta queued since the last call.
    /// Positive = wheel down/away, negative = wheel up/toward -- one entry
    /// per `wl_pointer.axis` event, already converted from `wl_fixed` to
    /// pixels (a typical detent is ~15).
    pub fn take_scroll(&mut self) -> Vec<i32> {
        std::mem::take(&mut self.pending_scroll)
    }

    /// Consume a pending left-click (press + matching release since the
    /// last call), reporting the surface it landed on.
    pub const fn take_click(&mut self) -> Option<SurfaceId> {
        self.pending_click.take()
    }

    #[must_use]
    pub const fn focus(&self) -> Option<SurfaceId> {
        self.focus
    }

    #[must_use]
    pub const fn press_serial(&self) -> Option<u32> {
        self.press_serial
    }

    /// Handle one `wl_pointer` event (already confirmed to be
    /// `ev.is(self.id())` by the caller). Only Enter/Motion/Button/Axis
    /// carry anything a bar cares about; everything else (Frame,
    /// `AxisSource`, ...) is ignored.
    pub fn on_event(&mut self, ev: &RawEvent) {
        match ev.event::<PointerEvent>() {
            Some(PointerEvent::Enter) => {
                self.focus = Some(SurfaceId(ev.arg(1)));
                self.pos = ev.point(2, 3);
            }
            Some(PointerEvent::Leave) => {
                self.focus = None;
                self.left_pressed = false;
            }
            Some(PointerEvent::Motion) => {
                self.pos = ev.point(1, 2);
            }
            Some(PointerEvent::Button) => self.on_button(ev),
            Some(PointerEvent::Axis) => self.on_axis(ev),
            _ => {}
        }
    }

    /// Args: (serial, time, button, state). A click is a press immediately
    /// followed by a release with nothing else of interest in between -- a
    /// bar has no drag/selection to distinguish, so press just arms it and
    /// release (while armed) reports it.
    const fn on_button(&mut self, ev: &RawEvent) {
        let button = PointerButton::from_code(ev.arg(2));
        let state = ButtonState::from_raw(ev.arg(3));
        if matches!(
            (button, state),
            (Some(PointerButton::Left), Some(ButtonState::Pressed))
        ) {
            self.press_serial = Some(ev.arg(0));
        }
        self.apply_button(button, state);
    }

    /// The actual press/release state machine, split out from
    /// [`Self::on_button`]'s wire-arg decoding so tests can drive it
    /// directly without constructing a real wire-encoded `RawEvent`.
    const fn apply_button(&mut self, button: Option<PointerButton>, state: Option<ButtonState>) {
        match (button, state) {
            (Some(PointerButton::Left), Some(ButtonState::Pressed)) => {
                self.left_pressed = true;
            }
            (Some(PointerButton::Left), Some(ButtonState::Released)) => {
                if self.left_pressed {
                    self.pending_click = self.focus;
                }
                self.left_pressed = false;
            }
            _ => {}
        }
    }

    /// Axis args: (time, axis, value). axis 0 = vertical; value is
    /// `wl_fixed` (24.8) -- positive = wheel down, negative = wheel up, same
    /// convention as `platform::TermWindow`'s scroll handling.
    ///
    /// Touchpad/hi-res-wheel scrolling sends many small axis events,
    /// routinely under one whole pixel each (`wl_fixed`'s 8 fractional bits
    /// mean e.g. 0.3px is a real, distinct value on the wire). Converting
    /// each event to pixels independently via `to_pixels()` (plain
    /// truncating integer division) rounded every such sub-pixel event down
    /// to exactly 0 and threw it away -- a slow, gentle scroll gesture could
    /// legitimately produce nothing but zeros forever, which is why
    /// brightness/volume scrolling felt broken or wildly inconsistent
    /// depending on scroll speed. Instead accumulate in raw fixed-point
    /// units and only truncate to pixels once enough has built up, carrying
    /// the leftover fraction forward.
    fn on_axis(&mut self, ev: &RawEvent) {
        let axis = ev.arg(1);
        if axis == 0 {
            self.accumulate_axis(ev.fixed(2).0);
        }
    }

    /// The raw fixed-point accumulation, split out from [`Self::on_axis`]'s
    /// wire-arg decoding for the same test-ability reason as
    /// [`Self::apply_button`].
    fn accumulate_axis(&mut self, fixed_value: i32) {
        let raw = self.scroll_fixed_remainder + fixed_value;
        let px = raw / 256;
        self.scroll_fixed_remainder = raw - px * 256;
        if px != 0 {
            self.pending_scroll.push(px);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_POINTER: PointerId = PointerId(42);

    #[test]
    fn new_has_no_pointer_id() {
        let p = PointerState::new();
        assert!(p.id().is_none());
    }

    #[test]
    fn set_id_records_it() {
        let mut p = PointerState::new();
        p.set_id(TEST_POINTER);
        assert_eq!(p.id(), Some(TEST_POINTER));
    }

    #[test]
    fn take_scroll_drains_and_resets() {
        let mut p = PointerState::new();
        p.pending_scroll.push(5);
        p.pending_scroll.push(-3);
        assert_eq!(p.take_scroll(), vec![5, -3]);
        assert!(p.take_scroll().is_empty());
    }

    const BAR: SurfaceId = SurfaceId(7);
    const POPUP: SurfaceId = SurfaceId(9);

    fn wire(opcode: PointerEvent, args: [u32; 4]) -> RawEvent {
        RawEvent::Wire {
            object_id: TEST_POINTER.0,
            opcode: opcode as u16,
            args,
            nargs: 4,
            bytes: Box::new([]),
        }
    }

    fn enter(surface: SurfaceId) -> RawEvent {
        wire(PointerEvent::Enter, [1, surface.0, 10 * 256, 20 * 256])
    }

    fn button(serial: u32, state: ButtonState) -> RawEvent {
        wire(
            PointerEvent::Button,
            [serial, 0, PointerButton::Left as u32, state as u32],
        )
    }

    #[test]
    fn take_click_drains_and_resets() {
        let mut p = PointerState::new();
        p.pending_click = Some(BAR);
        assert_eq!(p.take_click(), Some(BAR));
        assert_eq!(p.take_click(), None);
    }

    #[test]
    fn a_click_reports_the_surface_it_landed_on() {
        let mut p = PointerState::new();
        p.on_event(&enter(POPUP));
        p.on_event(&button(4, ButtonState::Pressed));
        p.on_event(&button(5, ButtonState::Released));
        assert_eq!(p.take_click(), Some(POPUP));
        assert_eq!(p.focus(), Some(POPUP));
    }

    #[test]
    fn the_press_serial_is_kept_for_a_popup_grab() {
        let mut p = PointerState::new();
        p.on_event(&enter(BAR));
        p.on_event(&button(41, ButtonState::Pressed));
        p.on_event(&button(42, ButtonState::Released));
        assert_eq!(p.press_serial(), Some(41));
    }

    #[test]
    fn a_press_carried_across_surfaces_is_no_click() {
        let mut p = PointerState::new();
        p.on_event(&enter(BAR));
        p.on_event(&button(1, ButtonState::Pressed));
        p.on_event(&wire(PointerEvent::Leave, [2, BAR.0, 0, 0]));
        p.on_event(&enter(POPUP));
        p.on_event(&button(3, ButtonState::Released));
        assert_eq!(p.take_click(), None);
    }

    #[test]
    fn button_press_then_release_yields_a_click() {
        let mut p = PointerState::new();
        p.on_event(&enter(BAR));
        p.apply_button(Some(PointerButton::Left), Some(ButtonState::Pressed));
        assert_eq!(p.take_click(), None);
        p.apply_button(Some(PointerButton::Left), Some(ButtonState::Released));
        assert_eq!(p.take_click(), Some(BAR));
    }

    #[test]
    fn release_without_press_is_not_a_click() {
        let mut p = PointerState::new();
        p.apply_button(Some(PointerButton::Left), Some(ButtonState::Released));
        assert_eq!(p.take_click(), None);
    }

    #[test]
    fn right_button_is_ignored() {
        let mut p = PointerState::new();
        p.apply_button(Some(PointerButton::Right), Some(ButtonState::Pressed));
        p.apply_button(Some(PointerButton::Right), Some(ButtonState::Released));
        assert_eq!(p.take_click(), None);
    }

    #[test]
    fn gentle_scroll_accumulates_before_producing_a_step() {
        let mut p = PointerState::new();
        // Each sub-pixel nudge alone truncates to 0; several should
        // eventually accumulate past one whole pixel.
        for _ in 0..10 {
            p.accumulate_axis(30); // 30/256 px per event
        }
        let total: i32 = p.take_scroll().iter().sum();
        assert!(total > 0, "expected accumulated scroll, got {total}");
    }

    #[test]
    fn single_small_event_stays_pending_not_dropped() {
        let mut p = PointerState::new();
        p.accumulate_axis(30); // 30/256 px, truncates to 0
        assert!(p.take_scroll().is_empty());
        assert_eq!(p.scroll_fixed_remainder, 30);
    }

    #[test]
    fn negative_axis_value_produces_negative_step() {
        let mut p = PointerState::new();
        p.accumulate_axis(-300); // -300/256 = -1.17 -> -1 px, remainder -44
        assert_eq!(p.take_scroll(), vec![-1]);
    }
}

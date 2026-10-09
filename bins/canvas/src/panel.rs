//! [`Panel`] -- the floating, movable tool palette.
//!
//! Replaces the old fixed top bar. The panel is a small card that sits
//! above the canvas, is dragged by its grip, and reports which tool a click
//! landed on. Geometry only: the window paints it and feeds it pointer
//! positions.
//!
//! Coordinates are physical pixels throughout, because the panel is chrome
//! rather than content -- it does not pan or zoom with the canvas.

use crate::handle::CursorShape;
use crate::session::Tool;

/// A rectangle in panel space, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Slot {
    /// Whether `p` falls inside this slot.
    #[must_use]
    pub const fn contains(self, p: (i32, i32)) -> bool {
        p.0 >= self.x && p.0 < self.x + self.w && p.1 >= self.y && p.1 < self.y + self.h
    }

    /// The slot inset by `d` on every side, for painting an inner glyph.
    #[must_use]
    pub const fn inset(self, d: i32) -> Self {
        Self {
            x: self.x + d,
            y: self.y + d,
            w: self.w - d * 2,
            h: self.h - d * 2,
        }
    }

    /// The slot's centre.
    #[cfg(test)]
    #[must_use]
    pub const fn center(self) -> (i32, i32) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }
}

/// What a press on the panel started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelHit {
    /// The grip: the press begins a panel drag.
    Grip,
    /// A tool button.
    Tool(Tool),
}

/// The floating tool palette.
///
/// Position resets to the default corner each launch, deliberately: the
/// panel is a transient control surface, not part of the document.
#[derive(Debug, Clone)]
pub struct Panel {
    /// Top-left corner, in physical pixels.
    x: i32,
    y: i32,
    /// Scale-derived button pitch.
    button: i32,
    pad: i32,
    /// Set while the user drags the panel, holding the grab offset.
    dragging: Option<(i32, i32)>,
}

impl Panel {
    /// Button edge length at scale 1, in physical pixels.
    const BUTTON: i32 = 34;
    /// Inner padding around the button column.
    const PAD: i32 = 6;
    /// Height of the drag grip strip along the top.
    const GRIP: i32 = 14;
    /// Distance from the window edge the panel starts at.
    const MARGIN: i32 = 16;

    /// A panel at its default position, sized for `scale`.
    #[must_use]
    pub fn new(scale: i32) -> Self {
        let s = scale.max(1);
        Self {
            x: Self::MARGIN * s,
            y: Self::MARGIN * s,
            button: Self::BUTTON * s,
            pad: Self::PAD * s,
            dragging: None,
        }
    }

    /// The grip strip's height in physical pixels.
    #[must_use]
    pub const fn grip_height(&self) -> i32 {
        // The grip scales with the padding rather than the button so it
        // stays a thin strip at every density.
        Self::GRIP * self.pad / Self::PAD
    }

    /// The panel's outer rectangle.
    #[must_use]
    pub fn bounds(&self) -> Slot {
        let count = i32::try_from(Tool::ALL.len()).unwrap_or(1);
        Slot {
            x: self.x,
            y: self.y,
            w: self.button + self.pad * 2,
            h: self.grip_height() + count * self.button + self.pad * 2,
        }
    }

    /// The grip strip, which drags the whole panel.
    #[must_use]
    pub fn grip(&self) -> Slot {
        let b = self.bounds();
        Slot {
            x: b.x,
            y: b.y,
            w: b.w,
            h: self.grip_height(),
        }
    }

    /// The slot for the tool at `index`, or `None` past the end.
    #[must_use]
    pub fn slot(&self, index: usize) -> Option<Slot> {
        let n = i32::try_from(index).ok()?;
        (index < Tool::ALL.len()).then(|| Slot {
            x: self.x + self.pad,
            y: self.y + self.grip_height() + self.pad + n * self.button,
            w: self.button,
            h: self.button,
        })
    }

    /// Every tool paired with its slot, for painting.
    pub fn slots(&self) -> impl Iterator<Item = (Tool, Slot)> + '_ {
        Tool::ALL
            .iter()
            .enumerate()
            .filter_map(|(i, tool)| self.slot(i).map(|s| (*tool, s)))
    }

    /// What `p` lands on, or `None` when the point misses the panel.
    #[must_use]
    pub fn hit(&self, p: (i32, i32)) -> Option<PanelHit> {
        if !self.bounds().contains(p) {
            return None;
        }
        if self.grip().contains(p) {
            return Some(PanelHit::Grip);
        }
        for (tool, slot) in self.slots() {
            if slot.contains(p) {
                return Some(PanelHit::Tool(tool));
            }
        }
        // Inside the card but between buttons: swallow the click so it does
        // not fall through and draw on the canvas underneath.
        Some(PanelHit::Grip)
    }

    /// The cursor the panel wants at `p`, or `None` if the panel is not
    /// under the pointer at all.
    #[must_use]
    pub fn cursor_at(&self, p: (i32, i32)) -> Option<CursorShape> {
        if self.dragging.is_some() {
            return Some(CursorShape::Grabbing);
        }
        match self.hit(p)? {
            PanelHit::Grip => Some(CursorShape::Grab),
            PanelHit::Tool(_) => Some(CursorShape::Default),
        }
    }

    /// Whether the panel is being dragged right now.
    #[must_use]
    pub const fn is_dragging(&self) -> bool {
        self.dragging.is_some()
    }

    /// Begin a drag if `p` is on the grip. Returns whether it started.
    pub fn press(&mut self, p: (i32, i32)) -> bool {
        if self.grip().contains(p) {
            self.dragging = Some((p.0 - self.x, p.1 - self.y));
            return true;
        }
        false
    }

    /// Continue a drag, keeping the panel inside `(width, height)`.
    /// Returns whether the panel moved.
    pub fn drag(&mut self, p: (i32, i32), width: i32, height: i32) -> bool {
        let Some((ox, oy)) = self.dragging else {
            return false;
        };
        let before = (self.x, self.y);
        self.x = p.0 - ox;
        self.y = p.1 - oy;
        self.clamp(width, height);
        (self.x, self.y) != before
    }

    /// End any drag in progress.
    pub const fn release(&mut self) {
        self.dragging = None;
    }

    /// Keep the panel wholly on screen after a drag or a window resize.
    ///
    /// A panel dragged mostly off the edge and then abandoned would be
    /// unreachable, so it is pulled back inside every time.
    pub fn clamp(&mut self, width: i32, height: i32) {
        let b = self.bounds();
        self.x = self.x.clamp(0, (width - b.w).max(0));
        self.y = self.y.clamp(0, (height - b.h).max(0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A panel at scale 1, positioned away from the origin so that tests
    /// distinguish panel-space from window-space mistakes.
    fn panel() -> Panel {
        let mut p = Panel::new(1);
        p.x = 100;
        p.y = 100;
        p
    }

    #[test]
    fn a_fresh_panel_sits_at_the_default_margin() {
        let p = Panel::new(1);
        let b = p.bounds();
        assert_eq!((b.x, b.y), (Panel::MARGIN, Panel::MARGIN));
    }

    #[test]
    fn a_hidpi_panel_is_bigger_than_a_scale_one_panel() {
        let one = Panel::new(1).bounds();
        let two = Panel::new(2).bounds();
        assert!(two.w > one.w && two.h > one.h);
    }

    #[test]
    fn there_is_a_slot_for_every_tool_and_no_more() {
        let p = panel();
        assert_eq!(p.slots().count(), Tool::ALL.len());
        assert!(p.slot(Tool::ALL.len()).is_none());
    }

    #[test]
    fn the_slots_run_down_the_panel_without_overlapping() {
        let p = panel();
        let slots: Vec<Slot> = p.slots().map(|(_, s)| s).collect();
        for pair in slots.windows(2) {
            assert!(
                pair[0].y + pair[0].h <= pair[1].y,
                "slots overlap: {:?} then {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn every_slot_stays_inside_the_panel() {
        let p = panel();
        let b = p.bounds();
        for (tool, s) in p.slots() {
            assert!(
                s.x >= b.x && s.x + s.w <= b.x + b.w,
                "{tool:?} escaped in x"
            );
            assert!(
                s.y >= b.y && s.y + s.h <= b.y + b.h,
                "{tool:?} escaped in y"
            );
        }
    }

    #[test]
    fn the_grip_sits_above_every_button() {
        let p = panel();
        let grip = p.grip();
        for (_, s) in p.slots() {
            assert!(grip.y + grip.h <= s.y);
        }
    }

    #[test]
    fn clicking_a_slot_reports_its_tool() {
        let p = panel();
        for (tool, s) in p.slots() {
            assert_eq!(p.hit(s.center()), Some(PanelHit::Tool(tool)));
        }
    }

    #[test]
    fn clicking_the_grip_reports_the_grip() {
        let p = panel();
        assert_eq!(p.hit(p.grip().center()), Some(PanelHit::Grip));
    }

    #[test]
    fn clicking_outside_the_panel_reports_nothing() {
        let p = panel();
        assert_eq!(p.hit((0, 0)), None);
        assert_eq!(p.hit((9999, 9999)), None);
    }

    #[test]
    fn a_click_between_buttons_is_swallowed_not_passed_through() {
        let p = panel();
        // The left padding strip: inside the card, on no button.
        let inside_padding = (p.bounds().x + 1, p.slot(0).unwrap().center().1);
        assert!(
            p.hit(inside_padding).is_some(),
            "a gap inside the panel must not reach the canvas"
        );
    }

    #[test]
    fn the_grip_offers_a_grab_cursor_and_a_button_does_not() {
        let p = panel();
        assert_eq!(p.cursor_at(p.grip().center()), Some(CursorShape::Grab));
        assert_eq!(
            p.cursor_at(p.slot(0).unwrap().center()),
            Some(CursorShape::Default)
        );
        assert_eq!(p.cursor_at((0, 0)), None);
    }

    #[test]
    fn dragging_the_grip_moves_the_panel() {
        let mut p = panel();
        let grip = p.grip().center();
        assert!(p.press(grip));
        assert!(p.is_dragging());
        assert!(p.drag((grip.0 + 40, grip.1 + 25), 1000, 1000));
        let b = p.bounds();
        assert_eq!((b.x, b.y), (140, 125));
    }

    #[test]
    fn a_press_on_a_button_does_not_start_a_drag() {
        let mut p = panel();
        assert!(!p.press(p.slot(0).unwrap().center()));
        assert!(!p.is_dragging());
    }

    #[test]
    fn dragging_without_a_press_does_nothing() {
        let mut p = panel();
        assert!(!p.drag((500, 500), 1000, 1000));
    }

    #[test]
    fn releasing_ends_the_drag() {
        let mut p = panel();
        p.press(p.grip().center());
        p.release();
        assert!(!p.is_dragging());
        assert!(!p.drag((500, 500), 1000, 1000));
    }

    #[test]
    fn a_live_drag_shows_the_grabbing_cursor_anywhere() {
        let mut p = panel();
        p.press(p.grip().center());
        assert_eq!(p.cursor_at((0, 0)), Some(CursorShape::Grabbing));
    }

    #[test]
    fn the_panel_cannot_be_dragged_off_screen() {
        let mut p = panel();
        let grip = p.grip().center();
        p.press(grip);
        p.drag((-9999, -9999), 800, 600);
        let b = p.bounds();
        assert!(b.x >= 0 && b.y >= 0, "escaped top-left: {b:?}");
        p.drag((9999, 9999), 800, 600);
        let b = p.bounds();
        assert!(
            b.x + b.w <= 800 && b.y + b.h <= 600,
            "escaped bottom-right: {b:?}"
        );
    }

    #[test]
    fn a_window_smaller_than_the_panel_pins_it_to_the_origin() {
        let mut p = panel();
        p.clamp(10, 10);
        let b = p.bounds();
        assert_eq!((b.x, b.y), (0, 0), "must not clamp to a negative position");
    }

    #[test]
    fn shrinking_the_window_pulls_a_far_panel_back_into_view() {
        let mut p = panel();
        p.press(p.grip().center());
        p.drag((900, 700), 1000, 800);
        p.clamp(400, 300);
        let b = p.bounds();
        assert!(b.x + b.w <= 400 && b.y + b.h <= 300);
    }

    #[test]
    fn a_slot_inset_stays_within_the_slot() {
        let s = Slot {
            x: 10,
            y: 20,
            w: 30,
            h: 40,
        };
        let i = s.inset(4);
        assert_eq!((i.x, i.y, i.w, i.h), (14, 24, 22, 32));
    }

    #[test]
    fn slot_containment_excludes_the_far_edges() {
        let s = Slot {
            x: 0,
            y: 0,
            w: 10,
            h: 10,
        };
        assert!(s.contains((0, 0)));
        assert!(s.contains((9, 9)));
        assert!(!s.contains((10, 10)));
    }
}

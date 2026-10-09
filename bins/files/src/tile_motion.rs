//! [`TileMotion`] -- the glides a tile makes as the pointer arrives: it
//! rises, its action buttons appear, the button under the pointer grows and
//! lights, and a button held down takes the press colour.

use crate::fade::Fades;
use crate::hover::{Pane, Spot};
use crate::tile_action::TileAction;

/// Which tile a glide belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileKey {
    Item(usize),
    Overflow,
}

pub struct TileMotion {
    raise: Fades<TileKey>,
    reveal: Fades<TileKey>,
    glow: Fades<(usize, TileAction)>,
    press: Fades<(usize, TileAction)>,
    chosen: Fades<usize>,
    tray: Fades<usize>,
}

impl TileMotion {
    const LIVE_FROM: u8 = 128;

    #[must_use]
    pub const fn new() -> Self {
        Self {
            raise: Fades::new(),
            reveal: Fades::new(),
            glow: Fades::new(),
            press: Fades::new(),
            chosen: Fades::new(),
            tray: Fades::new(),
        }
    }

    /// Point the press glide at the button held down, if any.
    pub fn press_on(&mut self, held: Option<(usize, TileAction)>, now_ms: u64) {
        let held: Vec<(usize, TileAction)> = held.into_iter().collect();
        self.press.aim(&held, now_ms);
    }

    /// Point the glides at what is under the pointer, at the keyboard
    /// cursor's tile and at the selected tiles.
    ///
    /// A tile rises for any of them, and for the tray open under it, but
    /// only a pointer brings the buttons, since a key cannot press them. A
    /// selected tile also takes the strong border. `actions` is false while a
    /// name is being edited, when the buttons would sit over the box.
    pub fn aim(
        &mut self,
        spot: Option<Spot>,
        cursor: Option<usize>,
        selected: &[usize],
        tray: Option<usize>,
        actions: bool,
        now_ms: u64,
    ) {
        let pointed = match spot {
            Some(Spot::Item(Pane::Tiles, row) | Spot::Action(row, _)) => Some(TileKey::Item(row)),
            Some(Spot::Overflow) => Some(TileKey::Overflow),
            _ => None,
        };
        let raised: Vec<TileKey> = pointed
            .into_iter()
            .chain(cursor.map(TileKey::Item))
            .chain(selected.iter().copied().map(TileKey::Item))
            .chain(tray.map(TileKey::Item))
            .collect();
        self.raise.aim(&raised, now_ms);
        self.chosen.aim(selected, now_ms);
        let open: Vec<usize> = tray.into_iter().collect();
        self.tray.aim(&open, now_ms);

        let shown: Vec<TileKey> = pointed
            .into_iter()
            .chain(tray.map(TileKey::Item))
            .filter(|key| actions && matches!(key, TileKey::Item(_)))
            .collect();
        self.reveal.aim(&shown, now_ms);

        let lit: Vec<(usize, TileAction)> = match spot {
            Some(Spot::Action(row, action)) if actions => vec![(row, action)],
            _ => Vec::new(),
        };
        self.glow.aim(&lit, now_ms);
    }

    pub fn advance(&mut self, now_ms: u64) {
        self.raise.advance(now_ms);
        self.reveal.advance(now_ms);
        self.glow.advance(now_ms);
        self.press.advance(now_ms);
        self.chosen.advance(now_ms);
        self.tray.advance(now_ms);
    }

    /// How far the tray under the tile at `row` has opened.
    #[must_use]
    pub fn tray(&self, row: usize) -> u8 {
        self.tray.level(&row)
    }

    /// Whether the tray at `row` is open enough to be pressed.
    #[must_use]
    pub fn tray_is_live(&self, row: usize) -> bool {
        self.tray(row) >= Self::LIVE_FROM
    }

    #[must_use]
    pub fn chosen(&self, row: usize) -> u8 {
        self.chosen.level(&row)
    }

    #[must_use]
    pub fn press(&self, row: usize, action: TileAction) -> u8 {
        self.press.level(&(row, action))
    }

    #[must_use]
    pub fn raise(&self, key: TileKey) -> u8 {
        self.raise.level(&key)
    }

    #[must_use]
    pub fn reveal(&self, key: TileKey) -> u8 {
        self.reveal.level(&key)
    }

    #[must_use]
    pub fn glow(&self, row: usize, action: TileAction) -> u8 {
        self.glow.level(&(row, action))
    }

    /// Whether the buttons of the tile at `row` are visible enough to be
    /// pressed. A click during the first moments of the fade selects the tile
    /// instead of doing something the user could not yet see.
    #[must_use]
    pub fn buttons_are_live(&self, row: usize) -> bool {
        self.reveal(TileKey::Item(row)) >= Self::LIVE_FROM
    }

    /// Whether another frame is owed to carry a glide on.
    #[must_use]
    pub fn is_animating(&self) -> bool {
        self.raise.is_animating()
            || self.reveal.is_animating()
            || self.glow.is_animating()
            || self.press.is_animating()
            || self.chosen.is_animating()
            || self.tray.is_animating()
    }
}

impl Default for TileMotion {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{TileKey, TileMotion};
    use crate::hover::{Pane, Spot};
    use crate::tile_action::TileAction;
    use crate::toolbar::Target;

    const FRAME_MS: u64 = 16;

    trait Frames {
        fn run(&mut self, from_ms: u64, frames: u64);
    }

    impl Frames for TileMotion {
        fn run(&mut self, from_ms: u64, frames: u64) {
            for frame in 1..=frames {
                self.advance(from_ms + frame * FRAME_MS);
            }
        }
    }

    fn over(row: usize) -> Spot {
        Spot::Item(Pane::Tiles, row)
    }

    #[test]
    fn a_hovered_tile_rises_and_shows_its_buttons_but_lights_none() {
        let mut motion = TileMotion::new();
        motion.aim(Some(over(2)), None, &[], None, true, 0);
        motion.run(0, 20);
        assert_eq!(motion.raise(TileKey::Item(2)), 255);
        assert_eq!(motion.reveal(TileKey::Item(2)), 255);
        assert_eq!(motion.glow(2, TileAction::Cut), 0);
        assert_eq!(motion.raise(TileKey::Item(3)), 0);
        assert!(!motion.is_animating());
    }

    #[test]
    fn nothing_moves_on_the_first_frame_and_then_it_does() {
        let mut motion = TileMotion::new();
        motion.aim(Some(over(0)), None, &[], None, true, 1000);
        assert!(motion.is_animating());
        assert_eq!(motion.raise(TileKey::Item(0)), 0);
        motion.advance(1032);
        assert!(motion.raise(TileKey::Item(0)) > 0);
    }

    #[test]
    fn a_button_under_the_pointer_lights_while_its_tile_stays_raised() {
        let mut motion = TileMotion::new();
        motion.aim(
            Some(Spot::Action(1, TileAction::Copy)),
            None,
            &[],
            None,
            true,
            0,
        );
        motion.run(0, 20);
        assert_eq!(motion.glow(1, TileAction::Copy), 255);
        assert_eq!(motion.glow(1, TileAction::Cut), 0);
        assert_eq!(motion.raise(TileKey::Item(1)), 255);
        assert_eq!(motion.reveal(TileKey::Item(1)), 255);
    }

    #[test]
    fn a_held_button_takes_the_press_colour_and_lets_go_smoothly() {
        let mut motion = TileMotion::new();
        let held = Some((1, TileAction::Copy));
        motion.press_on(held, 0);
        assert_eq!(motion.press(1, TileAction::Copy), 0, "it starts from rest");
        motion.run(0, 20);
        assert_eq!(motion.press(1, TileAction::Copy), 255);
        assert_eq!(motion.press(1, TileAction::Cut), 0);
        motion.press_on(None, 400);
        motion.run(400, 3);
        let letting_go = motion.press(1, TileAction::Copy);
        assert!(
            letting_go > 0 && letting_go < 255,
            "mid-release: {letting_go}"
        );
        motion.run(448, 20);
        assert_eq!(motion.press(1, TileAction::Copy), 0);
        assert!(!motion.is_animating());
    }

    #[test]
    fn the_keyboard_cursor_raises_its_tile_without_buttons() {
        let mut motion = TileMotion::new();
        motion.aim(None, Some(4), &[], None, true, 0);
        motion.run(0, 20);
        assert_eq!(motion.raise(TileKey::Item(4)), 255);
        assert_eq!(motion.reveal(TileKey::Item(4)), 0);
    }

    #[test]
    fn buttons_become_pressable_only_once_mostly_visible() {
        let mut motion = TileMotion::new();
        motion.aim(Some(over(0)), None, &[], None, true, 0);
        assert!(!motion.buttons_are_live(0), "not at the first frame");
        motion.run(0, 20);
        assert!(motion.buttons_are_live(0));
        assert!(!motion.buttons_are_live(1), "another tile's are not");
    }

    #[test]
    fn the_overflow_tile_rises_but_has_no_buttons() {
        let mut motion = TileMotion::new();
        motion.aim(Some(Spot::Overflow), None, &[], None, true, 0);
        motion.run(0, 20);
        assert_eq!(motion.raise(TileKey::Overflow), 255);
        assert_eq!(motion.reveal(TileKey::Overflow), 0);
    }

    #[test]
    fn buttons_stay_hidden_while_a_name_is_being_edited() {
        let mut motion = TileMotion::new();
        motion.aim(
            Some(Spot::Action(0, TileAction::Rename)),
            None,
            &[],
            None,
            false,
            0,
        );
        motion.run(0, 20);
        assert_eq!(motion.reveal(TileKey::Item(0)), 0);
        assert_eq!(motion.glow(0, TileAction::Rename), 0);
        assert_eq!(motion.raise(TileKey::Item(0)), 255, "the tile still rises");
    }

    #[test]
    fn moving_off_lets_everything_fall_and_settle() {
        let mut motion = TileMotion::new();
        motion.aim(
            Some(Spot::Action(0, TileAction::Copy)),
            None,
            &[],
            None,
            true,
            0,
        );
        motion.run(0, 20);
        motion.aim(None, None, &[], None, true, 400);
        motion.run(400, 20);
        assert_eq!(motion.raise(TileKey::Item(0)), 0);
        assert_eq!(motion.reveal(TileKey::Item(0)), 0);
        assert_eq!(motion.glow(0, TileAction::Copy), 0);
        assert!(!motion.is_animating());
    }

    #[test]
    fn a_control_in_the_card_raises_no_tile() {
        let mut motion = TileMotion::new();
        motion.aim(Some(Spot::Control(Target::Home)), None, &[], None, true, 0);
        assert!(!motion.is_animating());
    }

    #[test]
    fn crossing_to_a_neighbour_hands_the_rise_over() {
        let mut motion = TileMotion::new();
        motion.aim(Some(over(0)), None, &[], None, true, 0);
        motion.run(0, 20);
        motion.aim(Some(over(1)), None, &[], None, true, 400);
        motion.run(400, 3);
        let left = motion.raise(TileKey::Item(0));
        let entered = motion.raise(TileKey::Item(1));
        assert!(left < 255 && left > 0, "the old tile is mid-fall: {left}");
        assert!(entered > 0 && entered < 255, "the new tile is mid-rise");
    }
}

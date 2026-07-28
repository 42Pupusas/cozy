//! Semantic modifier state, decoded from `wl_keyboard.modifiers`.
//!
//! The compositor sends three raw XKB bitmasks (depressed/latched/locked)
//! plus a layout group. Those are *indices into the loaded keymap*, not
//! anything a widget wants to reason about: no caller should be writing
//! `mods.depressed & 0x04` to ask "is Ctrl held".
//!
//! [`ModifierState`] does that decode once, in one place, and exposes the
//! five modifiers a GUI actually branches on. The raw masks stay available
//! for the one legitimate consumer that needs them verbatim -- an XKB
//! translator, which owns what they mean for the current keymap and must
//! not be second-guessed.

/// The XKB modifier indices, as bit positions in the wire masks.
///
/// These are fixed by the standard keymap layout every compositor ships:
/// Shift=0, Lock=1, Control=2, Mod1(Alt)=3, Mod2(NumLock)=4, Mod3=5,
/// Mod4(Super)=6, Mod5(AltGr)=7. Named here rather than spelled as bare
/// hex at each test site.
mod mask {
    pub const SHIFT: u32 = 1 << 0;
    pub const LOCK: u32 = 1 << 1;
    pub const CONTROL: u32 = 1 << 2;
    pub const ALT: u32 = 1 << 3;
    pub const NUM: u32 = 1 << 4;
    pub const SUPER: u32 = 1 << 6;
}

/// Which modifiers are currently active.
///
/// Copy + cheap: this is carried inside every key and pointer event.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ModifierState {
    /// Raw depressed mask, as received.
    depressed: u32,
    /// Raw latched mask (a one-shot modifier, e.g. sticky Shift).
    latched: u32,
    /// Raw locked mask (a toggle, e.g. Caps Lock).
    locked: u32,
    /// Layout group index (which layout of a multi-layout keymap is live).
    group: u32,
}

impl ModifierState {
    /// Record a `wl_keyboard.modifiers` event.
    #[must_use]
    pub const fn from_wire(depressed: u32, latched: u32, locked: u32, group: u32) -> Self {
        Self {
            depressed,
            latched,
            locked,
            group,
        }
    }

    /// The union of every way a modifier can be active.
    ///
    /// A modifier counts as held whether it is physically down, latched by a
    /// previous one-shot press, or locked on. Accessibility "sticky keys"
    /// users produce Ctrl+C entirely through *latched* bits and never set
    /// the depressed one, so reading `depressed` alone silently breaks every
    /// shortcut for them.
    #[must_use]
    const fn active(self) -> u32 {
        self.depressed | self.latched | self.locked
    }

    /// Shift is held.
    #[must_use]
    pub const fn shift(self) -> bool {
        self.active() & mask::SHIFT != 0
    }

    /// Control is held.
    #[must_use]
    pub const fn ctrl(self) -> bool {
        self.active() & mask::CONTROL != 0
    }

    /// Alt (Mod1) is held.
    #[must_use]
    pub const fn alt(self) -> bool {
        self.active() & mask::ALT != 0
    }

    /// Super/Meta/"the logo key" (Mod4) is held.
    #[must_use]
    pub const fn super_key(self) -> bool {
        self.active() & mask::SUPER != 0
    }

    /// Caps Lock is on.
    ///
    /// Only the *locked* mask counts: physically holding the Caps key down
    /// while it is toggled off should not read as capitals.
    #[must_use]
    pub const fn caps_lock(self) -> bool {
        self.locked & mask::LOCK != 0
    }

    /// Num Lock is on. Locked-only, for the same reason as Caps.
    #[must_use]
    pub const fn num_lock(self) -> bool {
        self.locked & mask::NUM != 0
    }

    /// True when no modifier that changes a keystroke's meaning is active.
    ///
    /// Deliberately ignores the lock modifiers: a plain letter typed with
    /// Caps Lock on is still an unmodified keystroke as far as shortcut
    /// dispatch is concerned, it just arrives as a different character.
    #[must_use]
    pub const fn is_plain(self) -> bool {
        !self.shift() && !self.ctrl() && !self.alt() && !self.super_key()
    }

    /// The raw masks, in `wl_keyboard.modifiers` order, for handing to an
    /// XKB translator via `xkb_state_update_mask`.
    ///
    /// Returned as a tuple rather than as four public fields so callers
    /// cannot casually read one mask and hand-roll their own decode -- the
    /// mistake this whole type exists to prevent.
    #[must_use]
    pub const fn raw(self) -> (u32, u32, u32, u32) {
        (self.depressed, self.latched, self.locked, self.group)
    }

    /// The active layout group.
    #[must_use]
    pub const fn group(self) -> u32 {
        self.group
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_state_has_nothing_held() {
        let m = ModifierState::default();
        assert!(!m.shift());
        assert!(!m.ctrl());
        assert!(!m.alt());
        assert!(!m.super_key());
        assert!(!m.caps_lock());
        assert!(!m.num_lock());
        assert!(m.is_plain());
    }

    #[test]
    fn each_modifier_decodes_from_its_own_bit() {
        let shift = ModifierState::from_wire(mask::SHIFT, 0, 0, 0);
        assert!(shift.shift() && !shift.ctrl() && !shift.alt() && !shift.super_key());

        let ctrl = ModifierState::from_wire(mask::CONTROL, 0, 0, 0);
        assert!(ctrl.ctrl() && !ctrl.shift() && !ctrl.alt() && !ctrl.super_key());

        let alt = ModifierState::from_wire(mask::ALT, 0, 0, 0);
        assert!(alt.alt() && !alt.shift() && !alt.ctrl() && !alt.super_key());

        let sup = ModifierState::from_wire(mask::SUPER, 0, 0, 0);
        assert!(sup.super_key() && !sup.shift() && !sup.ctrl() && !sup.alt());
    }

    #[test]
    fn combinations_are_reported_together() {
        let m = ModifierState::from_wire(mask::CONTROL | mask::SHIFT, 0, 0, 0);
        assert!(m.ctrl() && m.shift());
        assert!(!m.alt() && !m.super_key());
        assert!(!m.is_plain());
    }

    /// Sticky-keys users generate every shortcut through the latched mask.
    /// Reading only `depressed` would leave them unable to use the app at
    /// all, so this is a guard against a very quiet accessibility failure.
    #[test]
    fn a_latched_modifier_counts_as_held() {
        let m = ModifierState::from_wire(0, mask::CONTROL, 0, 0);
        assert!(m.ctrl(), "a latched (sticky) Ctrl must register as held");
        assert!(!m.is_plain());
    }

    #[test]
    fn a_locked_modifier_counts_as_held() {
        let m = ModifierState::from_wire(0, 0, mask::CONTROL, 0);
        assert!(m.ctrl());
    }

    /// Caps and Num are toggles: holding the physical key down is not the
    /// same as the lock being engaged.
    #[test]
    fn lock_modifiers_read_only_the_locked_mask() {
        let held_not_locked = ModifierState::from_wire(mask::LOCK | mask::NUM, 0, 0, 0);
        assert!(!held_not_locked.caps_lock());
        assert!(!held_not_locked.num_lock());

        let locked = ModifierState::from_wire(0, 0, mask::LOCK | mask::NUM, 0);
        assert!(locked.caps_lock());
        assert!(locked.num_lock());
    }

    /// Caps Lock changes which character arrives, not whether the keystroke
    /// is a shortcut -- `is_plain` must not be disturbed by it.
    #[test]
    fn caps_lock_alone_is_still_a_plain_keystroke() {
        let m = ModifierState::from_wire(0, 0, mask::LOCK, 0);
        assert!(m.caps_lock());
        assert!(m.is_plain(), "Caps Lock must not read as a modified chord");
    }

    #[test]
    fn raw_masks_survive_the_round_trip_for_the_xkb_translator() {
        let m = ModifierState::from_wire(1, 2, 4, 3);
        assert_eq!(m.raw(), (1, 2, 4, 3));
        assert_eq!(m.group(), 3);
    }

    /// The layout group is carried, not folded into the modifier bits: a
    /// keymap's second layout must not look like a held modifier.
    #[test]
    fn a_layout_group_is_not_mistaken_for_a_modifier() {
        let m = ModifierState::from_wire(0, 0, 0, 1);
        assert!(m.is_plain());
        assert_eq!(m.group(), 1);
    }
}

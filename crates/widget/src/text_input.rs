//! A single-line editable text field.
//!
//! `bins/launcher` and `bins/statusbar` both hand-roll their query field as a
//! bare `String` with `push`/`pop`, which means no cursor: you can only ever
//! edit the end. This is the shared replacement -- the filter box in
//! `bins/files` needs a real caret, and so will the rename field and the
//! location bar.
//!
//! ### Byte offsets, char boundaries
//! The caret is stored as a **byte** offset into the `String`, not a
//! character index. Byte offsets are what `String::insert`/`remove` and every
//! slice operation actually want, so storing a char index would mean an O(n)
//! `char_indices` walk on every keystroke to convert.
//!
//! The catch is that a byte offset can land mid-character, and slicing a
//! `String` there panics. Filenames are full of multi-byte text -- accented
//! Latin, CJK, emoji -- so this is a real crash, not a theoretical one. Every
//! movement here therefore steps whole characters via `is_char_boundary`, and
//! the invariant "the caret is always on a boundary" is what the tests hammer
//! hardest.
//!
//! Grapheme clusters are deliberately *not* handled: a family emoji or a
//! combining accent is several `char`s, and backspace will eat one at a time.
//! Doing better needs the Unicode segmentation tables, which is a job for
//! `crates/text_layout`, not a 200-line widget.

/// A single-line text field with a caret.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextInput {
    /// The current contents.
    text: String,
    /// Caret position as a byte offset into `text`. Always on a char
    /// boundary, and always `<= text.len()`.
    caret: usize,
}

impl TextInput {
    /// An empty field.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            text: String::new(),
            caret: 0,
        }
    }

    /// A field pre-filled with `text`, caret at the end -- the natural start
    /// for a rename box, where the user usually wants to append or clear.
    #[must_use]
    pub fn with_text(text: impl Into<String>) -> Self {
        let text = text.into();
        let caret = text.len();
        Self { text, caret }
    }

    /// The current contents.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The caret's byte offset.
    #[must_use]
    pub const fn caret(&self) -> usize {
        self.caret
    }

    /// Whether the field is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The text before the caret -- what a renderer measures to place the
    /// caret pixel-accurately.
    #[must_use]
    pub fn before_caret(&self) -> &str {
        &self.text[..self.caret]
    }

    /// The text after the caret.
    #[must_use]
    pub fn after_caret(&self) -> &str {
        &self.text[self.caret..]
    }

    /// Replaces the contents, putting the caret at the end.
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.caret = self.text.len();
    }

    /// Empties the field. Returns whether anything was there.
    pub fn clear(&mut self) -> bool {
        let had = !self.text.is_empty();
        self.text.clear();
        self.caret = 0;
        had
    }

    /// Inserts `c` at the caret and steps past it.
    ///
    /// Control characters are rejected rather than inserted: a keyboard
    /// decoder that reports Return or Tab as text would otherwise embed a
    /// `\n` in a filename, which no filesystem wants and no single-line field
    /// can render.
    pub fn insert(&mut self, c: char) -> bool {
        if c.is_control() {
            return false;
        }
        self.text.insert(self.caret, c);
        self.caret += c.len_utf8();
        true
    }

    /// Inserts a whole string at the caret (a paste), skipping control
    /// characters. Returns whether anything was inserted.
    pub fn insert_str(&mut self, s: &str) -> bool {
        let filtered: String = s.chars().filter(|c| !c.is_control()).collect();
        if filtered.is_empty() {
            return false;
        }
        self.text.insert_str(self.caret, &filtered);
        self.caret += filtered.len();
        true
    }

    /// The byte offset one character before `from`, or `None` at the start.
    ///
    /// Walks back over continuation bytes rather than subtracting 1, which
    /// is what keeps the caret off the middle of a multi-byte character.
    fn prev_boundary(&self, from: usize) -> Option<usize> {
        if from == 0 {
            return None;
        }
        let mut at = from - 1;
        while at > 0 && !self.text.is_char_boundary(at) {
            at -= 1;
        }
        Some(at)
    }

    /// The byte offset one character after `from`, or `None` at the end.
    fn next_boundary(&self, from: usize) -> Option<usize> {
        if from >= self.text.len() {
            return None;
        }
        let mut at = from + 1;
        while at < self.text.len() && !self.text.is_char_boundary(at) {
            at += 1;
        }
        Some(at)
    }

    /// Deletes the character before the caret (Backspace). Returns whether
    /// anything was deleted.
    pub fn backspace(&mut self) -> bool {
        let Some(at) = self.prev_boundary(self.caret) else {
            return false;
        };
        self.text.replace_range(at..self.caret, "");
        self.caret = at;
        true
    }

    /// Deletes the character at the caret (Delete). Returns whether anything
    /// was deleted.
    pub fn delete(&mut self) -> bool {
        let Some(to) = self.next_boundary(self.caret) else {
            return false;
        };
        self.text.replace_range(self.caret..to, "");
        true
    }

    /// Moves the caret one character left. Returns whether it moved.
    pub fn move_left(&mut self) -> bool {
        match self.prev_boundary(self.caret) {
            Some(at) => {
                self.caret = at;
                true
            }
            None => false,
        }
    }

    /// Moves the caret one character right. Returns whether it moved.
    pub fn move_right(&mut self) -> bool {
        match self.next_boundary(self.caret) {
            Some(at) => {
                self.caret = at;
                true
            }
            None => false,
        }
    }

    /// Moves the caret to the start (Home). Returns whether it moved.
    pub const fn move_home(&mut self) -> bool {
        let moved = self.caret != 0;
        self.caret = 0;
        moved
    }

    /// Moves the caret to the end (End). Returns whether it moved.
    pub const fn move_end(&mut self) -> bool {
        let moved = self.caret != self.text.len();
        self.caret = self.text.len();
        moved
    }

    /// Whether `at` is a word character. Underscore counts as one so
    /// `some_long_name` is crossed in a single word-jump, which is how
    /// editors treat identifiers and how filenames are usually written.
    fn is_word(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }

    /// Moves the caret left by one word (Ctrl-Left).
    ///
    /// Skips any separators immediately to the left first, then the word
    /// itself -- so a caret sitting after `foo   ` lands at the start of
    /// `foo` rather than stopping in the whitespace.
    pub fn move_word_left(&mut self) -> bool {
        let start = self.caret;
        while let Some(at) = self.prev_boundary(self.caret) {
            if self.text[at..].chars().next().is_some_and(Self::is_word) {
                break;
            }
            self.caret = at;
        }
        while let Some(at) = self.prev_boundary(self.caret) {
            if !self.text[at..].chars().next().is_some_and(Self::is_word) {
                break;
            }
            self.caret = at;
        }
        self.caret != start
    }

    /// Moves the caret right by one word (Ctrl-Right).
    pub fn move_word_right(&mut self) -> bool {
        let start = self.caret;
        while self.text[self.caret..]
            .chars()
            .next()
            .is_some_and(Self::is_word)
        {
            match self.next_boundary(self.caret) {
                Some(at) => self.caret = at,
                None => break,
            }
        }
        while self.text[self.caret..]
            .chars()
            .next()
            .is_some_and(|c| !Self::is_word(c))
        {
            match self.next_boundary(self.caret) {
                Some(at) => self.caret = at,
                None => break,
            }
        }
        self.caret != start
    }

    /// Deletes the word before the caret (Ctrl-Backspace / Ctrl-W).
    pub fn delete_word_left(&mut self) -> bool {
        let end = self.caret;
        if !self.move_word_left() {
            return false;
        }
        self.text.replace_range(self.caret..end, "");
        true
    }

    /// Deletes from the caret to the end of the line (Ctrl-K).
    pub fn delete_to_end(&mut self) -> bool {
        if self.caret >= self.text.len() {
            return false;
        }
        self.text.truncate(self.caret);
        true
    }

    /// Deletes from the start of the line to the caret (Ctrl-U).
    pub fn delete_to_start(&mut self) -> bool {
        if self.caret == 0 {
            return false;
        }
        self.text.replace_range(..self.caret, "");
        self.caret = 0;
        true
    }

    /// Puts the caret at byte offset `at`, snapping *down* to the nearest
    /// char boundary.
    ///
    /// Snapping rather than rejecting matters for click-to-position: a hit
    /// test converts a pixel x into a byte offset and can easily land inside
    /// a multi-byte character, and refusing to move the caret there would
    /// make clicks near CJK text feel dead.
    pub fn set_caret(&mut self, at: usize) {
        let mut at = at.min(self.text.len());
        while at > 0 && !self.text.is_char_boundary(at) {
            at -= 1;
        }
        self.caret = at;
    }
}

#[cfg(test)]
mod tests {
    use super::TextInput;

    /// Every operation must leave the caret on a char boundary and in range;
    /// violating either makes the next slice panic.
    fn assert_invariants(input: &TextInput) {
        assert!(
            input.caret() <= input.text().len(),
            "caret {} past end of {:?}",
            input.caret(),
            input.text()
        );
        assert!(
            input.text().is_char_boundary(input.caret()),
            "caret {} splits a character in {:?}",
            input.caret(),
            input.text()
        );
        // The two halves must reconstitute the whole.
        assert_eq!(
            format!("{}{}", input.before_caret(), input.after_caret()),
            input.text()
        );
    }

    #[test]
    fn a_fresh_input_is_empty_with_the_caret_at_zero() {
        let input = TextInput::new();
        assert!(input.is_empty());
        assert_eq!(input.caret(), 0);
        assert_invariants(&input);
    }

    #[test]
    fn with_text_puts_the_caret_at_the_end() {
        let input = TextInput::with_text("hello");
        assert_eq!(input.text(), "hello");
        assert_eq!(input.caret(), 5);
        assert_invariants(&input);
    }

    #[test]
    fn typing_appends_and_advances_the_caret() {
        let mut input = TextInput::new();
        for c in "abc".chars() {
            assert!(input.insert(c));
        }
        assert_eq!(input.text(), "abc");
        assert_eq!(input.caret(), 3);
        assert_invariants(&input);
    }

    #[test]
    fn typing_inserts_at_the_caret_not_the_end() {
        // The whole reason this type exists: launcher's push/pop cannot.
        let mut input = TextInput::with_text("ac");
        input.move_left();
        input.insert('b');
        assert_eq!(input.text(), "abc");
        assert_eq!(input.caret(), 2);
        assert_invariants(&input);
    }

    #[test]
    fn control_characters_are_refused() {
        // A decoder reporting Return as text must not embed a newline in a
        // filename.
        let mut input = TextInput::new();
        assert!(!input.insert('\n'));
        assert!(!input.insert('\t'));
        assert!(!input.insert('\u{7f}'));
        assert!(input.is_empty());
    }

    #[test]
    fn backspace_removes_the_character_before_the_caret() {
        let mut input = TextInput::with_text("abc");
        assert!(input.backspace());
        assert_eq!(input.text(), "ab");
        assert_eq!(input.caret(), 2);
        assert_invariants(&input);
    }

    #[test]
    fn backspace_at_the_start_does_nothing() {
        let mut input = TextInput::with_text("abc");
        input.move_home();
        assert!(!input.backspace());
        assert_eq!(input.text(), "abc");
    }

    #[test]
    fn delete_removes_the_character_at_the_caret() {
        let mut input = TextInput::with_text("abc");
        input.move_home();
        assert!(input.delete());
        assert_eq!(input.text(), "bc");
        assert_eq!(input.caret(), 0, "caret stays put on delete");
        assert_invariants(&input);
    }

    #[test]
    fn delete_at_the_end_does_nothing() {
        let mut input = TextInput::with_text("abc");
        assert!(!input.delete());
        assert_eq!(input.text(), "abc");
    }

    #[test]
    fn backspace_over_a_multibyte_character_removes_it_whole() {
        // The crash this module exists to prevent: naive caret -= 1 would
        // land mid-character and panic on the next slice.
        let mut input = TextInput::with_text("café");
        assert_eq!(input.caret(), 5, "é is two bytes");
        assert!(input.backspace());
        assert_eq!(input.text(), "caf");
        assert_invariants(&input);
    }

    #[test]
    fn backspace_over_an_emoji_removes_it_whole() {
        let mut input = TextInput::with_text("hi🐾");
        assert_eq!(input.caret(), 6, "the paw is four bytes");
        assert!(input.backspace());
        assert_eq!(input.text(), "hi");
        assert_invariants(&input);
    }

    #[test]
    fn arrows_step_whole_multibyte_characters() {
        let mut input = TextInput::with_text("日本語");
        assert_eq!(input.caret(), 9, "three 3-byte characters");
        input.move_left();
        assert_eq!(input.caret(), 6);
        input.move_left();
        assert_eq!(input.caret(), 3);
        assert_invariants(&input);
        input.move_right();
        assert_eq!(input.caret(), 6);
        assert_invariants(&input);
    }

    #[test]
    fn walking_a_mixed_script_string_never_splits_a_character() {
        // Sweeps the invariant across every caret position, both directions.
        let mut input = TextInput::with_text("a→é🐾ん_z");
        while input.move_left() {
            assert_invariants(&input);
        }
        assert_eq!(input.caret(), 0);
        while input.move_right() {
            assert_invariants(&input);
        }
        assert_eq!(input.caret(), input.text().len());
    }

    #[test]
    fn deleting_every_character_from_the_middle_stays_valid() {
        let mut input = TextInput::with_text("aé🐾ん");
        input.set_caret(3);
        while input.backspace() {
            assert_invariants(&input);
        }
        while input.delete() {
            assert_invariants(&input);
        }
        assert!(input.is_empty());
    }

    #[test]
    fn arrows_stop_at_the_ends() {
        let mut input = TextInput::with_text("ab");
        input.move_home();
        assert!(!input.move_left());
        input.move_end();
        assert!(!input.move_right());
    }

    #[test]
    fn home_and_end_report_whether_they_moved() {
        let mut input = TextInput::with_text("abc");
        assert!(input.move_home());
        assert!(!input.move_home());
        assert!(input.move_end());
        assert!(!input.move_end());
    }

    #[test]
    fn pasting_inserts_at_the_caret() {
        let mut input = TextInput::with_text("ad");
        input.move_left();
        assert!(input.insert_str("bc"));
        assert_eq!(input.text(), "abcd");
        assert_eq!(input.caret(), 3);
        assert_invariants(&input);
    }

    #[test]
    fn pasting_strips_control_characters() {
        // Pasting a line from a terminal must not smuggle in a newline.
        let mut input = TextInput::new();
        assert!(input.insert_str("a\nb\tc"));
        assert_eq!(input.text(), "abc");
        assert_invariants(&input);
    }

    #[test]
    fn pasting_only_control_characters_inserts_nothing() {
        let mut input = TextInput::new();
        assert!(!input.insert_str("\n\t"));
        assert!(input.is_empty());
    }

    #[test]
    fn pasting_multibyte_text_keeps_the_caret_valid() {
        let mut input = TextInput::with_text("ab");
        input.move_home();
        input.insert_str("日本");
        assert_eq!(input.text(), "日本ab");
        assert_eq!(input.caret(), 6);
        assert_invariants(&input);
    }

    #[test]
    fn clear_empties_and_reports() {
        let mut input = TextInput::with_text("abc");
        assert!(input.clear());
        assert!(input.is_empty());
        assert_eq!(input.caret(), 0);
        assert!(!input.clear(), "clearing an empty field is a no-op");
    }

    #[test]
    fn word_left_skips_separators_then_the_word() {
        let mut input = TextInput::with_text("foo   bar");
        assert!(input.move_word_left());
        assert_eq!(input.before_caret(), "foo   ", "lands at the start of bar");
        assert!(input.move_word_left());
        assert_eq!(input.caret(), 0);
        assert_invariants(&input);
    }

    #[test]
    fn word_right_crosses_the_word_then_its_trailing_separators() {
        let mut input = TextInput::with_text("foo   bar");
        input.move_home();
        assert!(input.move_word_right());
        assert_eq!(input.before_caret(), "foo   ");
        assert_invariants(&input);
    }

    #[test]
    fn underscores_do_not_break_a_word_jump() {
        // some_long_name should be one jump, as in every editor.
        let mut input = TextInput::with_text("some_long_name");
        assert!(input.move_word_left());
        assert_eq!(input.caret(), 0);
    }

    #[test]
    fn word_jumps_stop_at_the_ends() {
        let mut input = TextInput::with_text("word");
        input.move_home();
        assert!(!input.move_word_left());
        input.move_end();
        assert!(!input.move_word_right());
    }

    #[test]
    fn word_jumps_handle_multibyte_words() {
        let mut input = TextInput::with_text("héllo wörld");
        assert!(input.move_word_left());
        assert_eq!(input.after_caret(), "wörld");
        assert_invariants(&input);
    }

    #[test]
    fn delete_word_left_removes_the_previous_word() {
        let mut input = TextInput::with_text("one two three");
        assert!(input.delete_word_left());
        assert_eq!(input.text(), "one two ");
        assert_invariants(&input);
    }

    #[test]
    fn delete_word_left_at_the_start_does_nothing() {
        let mut input = TextInput::with_text("abc");
        input.move_home();
        assert!(!input.delete_word_left());
        assert_eq!(input.text(), "abc");
    }

    #[test]
    fn delete_word_left_from_the_middle_keeps_the_tail() {
        // Caret sits just after "two". Only the word goes -- both spaces
        // that surrounded it stay, which is what every editor does and what
        // makes the operation exactly undo one word-left jump.
        let mut input = TextInput::with_text("one two three");
        input.set_caret(7);
        assert!(input.delete_word_left());
        assert_eq!(input.text(), "one  three");
        assert_invariants(&input);
    }

    #[test]
    fn delete_word_left_from_the_next_word_takes_the_separator_with_it() {
        // Caret at the start of "three": the jump crosses the space first,
        // so the kill swallows " two" and leaves a single clean gap.
        let mut input = TextInput::with_text("one two three");
        input.set_caret(8);
        assert!(input.delete_word_left());
        assert_eq!(input.text(), "one three");
        assert_invariants(&input);
    }

    #[test]
    fn delete_to_end_truncates_at_the_caret() {
        let mut input = TextInput::with_text("keep this");
        input.set_caret(4);
        assert!(input.delete_to_end());
        assert_eq!(input.text(), "keep");
        assert_invariants(&input);
    }

    #[test]
    fn delete_to_start_removes_the_head() {
        let mut input = TextInput::with_text("drop keep");
        input.set_caret(5);
        assert!(input.delete_to_start());
        assert_eq!(input.text(), "keep");
        assert_eq!(input.caret(), 0);
        assert_invariants(&input);
    }

    #[test]
    fn line_kills_at_their_no_op_ends_report_false() {
        let mut input = TextInput::with_text("abc");
        assert!(!input.delete_to_end(), "caret already at the end");
        input.move_home();
        assert!(!input.delete_to_start(), "caret already at the start");
    }

    #[test]
    fn set_caret_snaps_down_to_a_char_boundary() {
        // Click-to-position: a pixel hit test can land mid-character, and
        // refusing to move would make clicks near CJK text feel dead.
        let mut input = TextInput::with_text("日本語");
        input.set_caret(4); // inside the second character
        assert_eq!(input.caret(), 3);
        assert_invariants(&input);
    }

    #[test]
    fn set_caret_clamps_past_the_end() {
        let mut input = TextInput::with_text("ab");
        input.set_caret(999);
        assert_eq!(input.caret(), 2);
        assert_invariants(&input);
    }

    #[test]
    fn set_caret_to_every_byte_offset_stays_on_a_boundary() {
        let mut input = TextInput::with_text("a→é🐾ん");
        for at in 0..=input.text().len() + 3 {
            input.set_caret(at);
            assert_invariants(&input);
        }
    }

    #[test]
    fn a_realistic_edit_session_holds_the_invariant_throughout() {
        // Types a filename, corrects it mid-string, jumps by word, pastes.
        let mut input = TextInput::new();
        for c in "reporte_añual🐾.txt".chars() {
            input.insert(c);
            assert_invariants(&input);
        }
        input.move_word_left();
        assert_invariants(&input);
        input.delete_word_left();
        assert_invariants(&input);
        input.insert_str("final");
        assert_invariants(&input);
        input.move_home();
        input.delete();
        assert_invariants(&input);
        input.move_end();
        while input.backspace() {
            assert_invariants(&input);
        }
        assert!(input.is_empty());
    }
}

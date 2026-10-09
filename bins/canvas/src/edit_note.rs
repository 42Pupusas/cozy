//! [`NoteEditor`] -- multiline text editing with a caret, for note cards.
//!
//! `widget::TextInput` is single-line by design (filenames, filter
//! queries). A note is deliberately multiline: Enter makes a new line,
//! arrows move between lines, and the caret is a `(line, column)` pair.
//! The storage and the invariant tests follow `TextInput`'s byte-offset
//! pattern, lifted to lines.
//!
//! Selection spans live in [`crate::text_range`]; this module owns how
//! editing *changes* them.

use crate::text_range::{Caret, TextRange};

/// An editable multiline text buffer with a caret and an optional
/// selection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NoteEditor {
    /// One `String` per line. Always at least one line, even when empty.
    lines: Vec<String>,
    /// Caret line index.
    pub line: usize,
    /// Caret column, a **byte** offset into `lines[line]`, always on a
    /// char boundary.
    pub col: usize,
    /// The fixed end of a selection, set while one is being made. The
    /// moving end is always the caret, so shift+arrow grows the correct
    /// side no matter which way the selection was made.
    anchor: Option<Caret>,
}

impl NoteEditor {
    /// An empty editor, caret at the start.
    #[must_use]
    pub fn new() -> Self {
        Self {
            lines: vec![String::new()],
            line: 0,
            col: 0,
            anchor: None,
        }
    }

    /// An editor pre-filled with `text` (lines split on `\n`), caret at
    /// the end.
    #[cfg(test)]
    #[must_use]
    pub fn with_text(text: &str) -> Self {
        let mut editor = Self::new();
        editor.set_text(text);
        editor
    }

    /// Replace the whole contents, caret to the end.
    pub fn set_text(&mut self, text: &str) {
        self.lines = text.split('\n').map(str::to_owned).collect();
        self.line = self.lines.len() - 1;
        self.col = self.lines[self.line].len();
        // The old selection indexed the old text; keeping it would leave an
        // anchor pointing into a line that no longer exists.
        self.anchor = None;
    }

    /// The full text, lines rejoined with `\n`.
    #[must_use]
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// The line count (at least 1).
    #[must_use]
    pub const fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// One line's contents.
    #[must_use]
    pub fn line(&self, index: usize) -> &str {
        self.lines[index.min(self.lines.len() - 1)].as_str()
    }

    /// The caret's position.
    #[must_use]
    pub const fn caret(&self) -> Caret {
        Caret::new(self.line, self.col)
    }

    /// The active selection, or `None` when nothing is selected.
    ///
    /// An anchor equal to the caret is reported as no selection: the user
    /// pressed shift and moved nothing.
    #[must_use]
    pub fn selection(&self) -> Option<TextRange> {
        let anchor = self.anchor?;
        let range = TextRange::ordered(anchor, self.caret());
        (!range.is_empty()).then_some(range)
    }

    /// Begin (or continue) a selection anchored at the caret.
    ///
    /// Called before a shift-modified move: the anchor is only planted the
    /// first time, so holding shift through several moves keeps growing one
    /// selection instead of restarting it.
    pub const fn anchor_selection(&mut self) {
        if self.anchor.is_none() {
            self.anchor = Some(self.caret());
        }
    }

    /// Drop any selection, leaving the caret where it is.
    pub const fn clear_selection(&mut self) {
        self.anchor = None;
    }

    /// Select the entire buffer, caret at the end.
    pub fn select_all(&mut self) {
        self.anchor = Some(Caret::START);
        self.line = self.lines.len() - 1;
        self.col = self.lines[self.line].len();
    }

    /// The selected text, or `None` when nothing is selected.
    #[must_use]
    pub fn selected_text(&self) -> Option<String> {
        let range = self.selection()?;
        if range.is_single_line() {
            let (from, to) = range.columns_on(range.start.line, self.line_len(range.start.line))?;
            return Some(self.lines[range.start.line][from..to].to_owned());
        }
        let mut out = String::new();
        for line in range.start.line..=range.end.line {
            let Some((from, to)) = range.columns_on(line, self.line_len(line)) else {
                continue;
            };
            if line > range.start.line {
                out.push('\n');
            }
            out.push_str(&self.lines[line][from..to]);
        }
        Some(out)
    }

    /// Delete the selection, leaving the caret where it began. Returns
    /// whether anything was removed.
    pub fn delete_selection(&mut self) -> bool {
        let Some(range) = self.selection() else {
            return false;
        };
        let start_len = self.line_len(range.start.line);
        let end_len = self.line_len(range.end.line);
        let head = self.lines[range.start.line][..range.start.col.min(start_len)].to_owned();
        let tail = self.lines[range.end.line][range.end.col.min(end_len)..].to_owned();
        // Replace the whole span with `head + tail`, collapsing every line
        // it touched into one.
        self.lines.drain(range.start.line..=range.end.line);
        self.lines.insert(range.start.line, head + &tail);
        self.line = range.start.line;
        self.col = range.start.col.min(self.lines[self.line].len());
        self.anchor = None;
        true
    }

    /// Insert `text` at the caret, replacing any selection.
    ///
    /// Newlines split lines exactly as typing Enter does, so pasting
    /// multi-line text behaves like typing it.
    pub fn insert_str(&mut self, text: &str) {
        self.delete_selection();
        for c in text.chars() {
            // `\r` alone, or the `\r` of a CRLF, would otherwise be dropped
            // by the control-character rule and lose the line break.
            if c == '\r' {
                continue;
            }
            self.insert(c);
        }
    }

    /// The byte length of `index`'s line, clamped to a real line.
    fn line_len(&self, index: usize) -> usize {
        self.lines[index.min(self.lines.len() - 1)].len()
    }

    /// Insert a character at the caret, replacing any selection. `\n`
    /// splits the line (Enter); other control characters are rejected, same
    /// rule as `widget::TextInput`.
    pub fn insert(&mut self, c: char) {
        self.delete_selection();
        if c == '\n' {
            let rest = self.lines[self.line].split_off(self.col);
            self.lines.insert(self.line + 1, rest);
            self.line += 1;
            self.col = 0;
            return;
        }
        if c.is_control() {
            return;
        }
        self.lines[self.line].insert(self.col, c);
        self.col += c.len_utf8();
    }

    /// Backspace: delete the selection if there is one, else the character
    /// before the caret, joining lines at a line start.
    pub fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.col > 0 {
            let at = self.prev_boundary().expect("col > 0 guarantees one");
            self.lines[self.line].replace_range(at..self.col, "");
            self.col = at;
            return;
        }
        if self.line > 0 {
            let joined = self.lines.remove(self.line);
            self.line -= 1;
            self.col = self.lines[self.line].len();
            self.lines[self.line].push_str(&joined);
        }
    }

    /// Delete: remove the selection if there is one, else the character at
    /// the caret, or join the next line up at a line end.
    pub fn delete(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.col < self.lines[self.line].len() {
            let to = self.next_boundary().expect("col < len guarantees one");
            self.lines[self.line].replace_range(self.col..to, "");
            return;
        }
        if self.line + 1 < self.lines.len() {
            let joined = self.lines.remove(self.line + 1);
            self.lines[self.line].push_str(&joined);
        }
    }

    /// Move the caret left, wrapping to the previous line's end.
    pub fn move_left(&mut self) {
        if self.col > 0 {
            self.col = self.prev_boundary().expect("col > 0 guarantees one");
        } else if self.line > 0 {
            self.line -= 1;
            self.col = self.lines[self.line].len();
        }
    }

    /// Move the caret right, wrapping to the next line's start.
    pub fn move_right(&mut self) {
        if self.col < self.lines[self.line].len() {
            self.col = self.next_boundary().expect("col < len guarantees one");
        } else if self.line + 1 < self.lines.len() {
            self.line += 1;
            self.col = 0;
        }
    }

    /// Move the caret up one line, keeping the column as a char count.
    pub fn move_up(&mut self) {
        if self.line == 0 {
            self.col = 0;
            return;
        }
        let goal = self.col_as_chars();
        self.line -= 1;
        self.place_col(goal);
    }

    /// Move the caret down one line, keeping the column as a char count.
    pub fn move_down(&mut self) {
        if self.line + 1 >= self.lines.len() {
            self.col = self.lines[self.line].len();
            return;
        }
        let goal = self.col_as_chars();
        self.line += 1;
        self.place_col(goal);
    }

    /// Move to the start of the current line (Home).
    pub const fn move_home(&mut self) {
        self.col = 0;
    }

    /// Move to the end of the current line (End).
    pub fn move_end(&mut self) {
        self.col = self.lines[self.line].len();
    }

    /// Place the caret from a click: `line` clamped, `col` a byte offset
    /// snapped down to a char boundary.
    #[cfg(test)]
    pub fn click(&mut self, line: usize, col: usize) {
        self.line = line.min(self.lines.len() - 1);
        let len = self.lines[self.line].len();
        let mut col = col.min(len);
        while col > 0 && !self.lines[self.line].is_char_boundary(col) {
            col -= 1;
        }
        self.col = col;
    }

    /// Put the caret at the `goal`-th character of the current line, or at
    /// the line end when the line is shorter.
    ///
    /// `goal` must be measured on the previous line *before* the line index
    /// moves, otherwise a shorter target line slices out of bounds.
    fn place_col(&mut self, goal: usize) {
        let line = &self.lines[self.line];
        self.col = line
            .char_indices()
            .nth(goal)
            .map_or(line.len(), |(offset, _)| offset);
    }

    /// The caret column as a character count.
    fn col_as_chars(&self) -> usize {
        self.lines[self.line][..self.col].chars().count()
    }

    /// The byte offset one char before the caret's column, or `None`.
    fn prev_boundary(&self) -> Option<usize> {
        let text = &self.lines[self.line];
        let mut at = self.col.checked_sub(1)?;
        while at > 0 && !text.is_char_boundary(at) {
            at -= 1;
        }
        Some(at)
    }

    /// The byte offset one char after the caret's column, or `None`.
    fn next_boundary(&self) -> Option<usize> {
        let text = &self.lines[self.line];
        let mut at = self.col + 1;
        while at < text.len() && !text.is_char_boundary(at) {
            at += 1;
        }
        (at <= text.len()).then_some(at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_invariants(editor: &NoteEditor) {
        assert!(!editor.lines.is_empty(), "at least one line must exist");
        assert!(editor.line < editor.lines.len(), "caret line out of range");
        let text = &editor.lines[editor.line];
        assert!(
            editor.col <= text.len(),
            "caret col {} past line len {}",
            editor.col,
            text.len()
        );
        assert!(
            text.is_char_boundary(editor.col),
            "caret col {} splits a character",
            editor.col
        );
    }

    #[test]
    fn a_fresh_editor_is_one_empty_line() {
        let editor = NoteEditor::new();
        assert_eq!(editor.text(), "");
        assert_eq!(editor.line_count(), 1);
        assert_invariants(&editor);
    }

    #[test]
    fn enter_splits_the_line_at_the_caret() {
        let mut editor = NoteEditor::with_text("hello");
        for _ in 0..3 {
            editor.move_left();
        }
        editor.insert('\n');
        assert_eq!(editor.text(), "he\nllo");
        assert_eq!(editor.line_count(), 2);
        assert_eq!((editor.line, editor.col), (1, 0));
        assert_invariants(&editor);
    }

    #[test]
    fn a_fresh_editor_has_no_selection() {
        assert!(NoteEditor::with_text("abc").selection().is_none());
    }

    #[test]
    fn an_anchor_without_movement_is_not_a_selection() {
        // Pressing shift and moving nothing must not select anything.
        let mut e = NoteEditor::with_text("abc");
        e.anchor_selection();
        assert!(e.selection().is_none());
        assert!(e.selected_text().is_none());
    }

    #[test]
    fn anchoring_then_moving_selects_the_span() {
        let mut e = NoteEditor::with_text("abcdef");
        e.move_home();
        e.anchor_selection();
        e.move_right();
        e.move_right();
        assert_eq!(e.selected_text().as_deref(), Some("ab"));
    }

    #[test]
    fn a_backward_selection_yields_the_same_text() {
        let mut e = NoteEditor::with_text("abcdef");
        e.anchor_selection();
        e.move_left();
        e.move_left();
        assert_eq!(e.selected_text().as_deref(), Some("ef"));
    }

    #[test]
    fn anchoring_twice_keeps_growing_one_selection() {
        // Holding shift across several moves must not restart the anchor.
        let mut e = NoteEditor::with_text("abcdef");
        e.move_home();
        e.anchor_selection();
        e.move_right();
        e.anchor_selection();
        e.move_right();
        assert_eq!(e.selected_text().as_deref(), Some("ab"));
    }

    #[test]
    fn select_all_covers_every_line() {
        let mut e = NoteEditor::with_text("ab\ncd");
        e.select_all();
        assert_eq!(e.selected_text().as_deref(), Some("ab\ncd"));
    }

    #[test]
    fn a_multiline_selection_keeps_its_line_breaks() {
        let mut e = NoteEditor::with_text("abc\ndef\nghi");
        e.line = 0;
        e.col = 1;
        e.anchor_selection();
        e.line = 2;
        e.col = 2;
        assert_eq!(e.selected_text().as_deref(), Some("bc\ndef\ngh"));
    }

    #[test]
    fn deleting_a_selection_removes_exactly_it() {
        let mut e = NoteEditor::with_text("abcdef");
        e.move_home();
        e.anchor_selection();
        e.move_right();
        e.move_right();
        assert!(e.delete_selection());
        assert_eq!(e.text(), "cdef");
        assert_eq!((e.line, e.col), (0, 0), "the caret lands where it began");
        assert!(e.selection().is_none(), "the selection is consumed");
        assert_invariants(&e);
    }

    #[test]
    fn deleting_a_multiline_selection_joins_the_ends() {
        let mut e = NoteEditor::with_text("abc\ndef\nghi");
        e.line = 0;
        e.col = 1;
        e.anchor_selection();
        e.line = 2;
        e.col = 2;
        assert!(e.delete_selection());
        assert_eq!(e.text(), "ai");
        assert_eq!(e.line_count(), 1, "the spanned lines collapse into one");
        assert_invariants(&e);
    }

    #[test]
    fn deleting_without_a_selection_reports_nothing() {
        let mut e = NoteEditor::with_text("abc");
        assert!(!e.delete_selection());
        assert_eq!(e.text(), "abc");
    }

    #[test]
    fn typing_replaces_the_selection() {
        let mut e = NoteEditor::with_text("abcdef");
        e.move_home();
        e.anchor_selection();
        e.move_right();
        e.move_right();
        e.insert('X');
        assert_eq!(e.text(), "Xcdef");
        assert_invariants(&e);
    }

    #[test]
    fn backspace_deletes_the_selection_rather_than_one_character() {
        let mut e = NoteEditor::with_text("abcdef");
        e.select_all();
        e.backspace();
        assert_eq!(e.text(), "");
        assert_invariants(&e);
    }

    #[test]
    fn delete_removes_the_selection_rather_than_one_character() {
        let mut e = NoteEditor::with_text("abcdef");
        e.move_home();
        e.anchor_selection();
        e.move_right();
        e.delete();
        assert_eq!(e.text(), "bcdef");
        assert_invariants(&e);
    }

    #[test]
    fn pasting_inserts_at_the_caret() {
        let mut e = NoteEditor::with_text("ad");
        e.move_home();
        e.move_right();
        e.insert_str("bc");
        assert_eq!(e.text(), "abcd");
        assert_invariants(&e);
    }

    #[test]
    fn pasting_replaces_a_selection() {
        let mut e = NoteEditor::with_text("abcdef");
        e.select_all();
        e.insert_str("xy");
        assert_eq!(e.text(), "xy");
        assert_invariants(&e);
    }

    #[test]
    fn pasting_multiline_text_creates_lines() {
        let mut e = NoteEditor::new();
        e.insert_str("ab\ncd");
        assert_eq!(e.line_count(), 2);
        assert_eq!(e.text(), "ab\ncd");
        assert_invariants(&e);
    }

    #[test]
    fn pasting_crlf_does_not_leave_stray_carriage_returns() {
        // A clipboard from another app may well carry CRLF.
        let mut e = NoteEditor::new();
        e.insert_str("ab\r\ncd");
        assert_eq!(e.text(), "ab\ncd");
        assert!(!e.text().contains('\r'));
    }

    #[test]
    fn pasting_multibyte_text_keeps_the_caret_valid() {
        let mut e = NoteEditor::new();
        e.insert_str("日本語");
        assert_eq!(e.text(), "日本語");
        assert_invariants(&e);
    }

    #[test]
    fn a_selection_over_multibyte_text_slices_whole_characters() {
        let mut e = NoteEditor::with_text("日本語");
        e.move_home();
        e.anchor_selection();
        e.move_right();
        assert_eq!(e.selected_text().as_deref(), Some("日"));
        e.delete_selection();
        assert_eq!(e.text(), "本語");
        assert_invariants(&e);
    }

    #[test]
    fn replacing_the_text_drops_a_stale_selection() {
        // The anchor indexed the old buffer; keeping it could point past
        // the end of a shorter one.
        let mut e = NoteEditor::with_text("abcdef");
        e.select_all();
        e.set_text("x");
        assert!(e.selection().is_none());
        assert_invariants(&e);
    }

    #[test]
    fn clearing_the_selection_leaves_the_text_and_caret_alone() {
        let mut e = NoteEditor::with_text("abc");
        e.select_all();
        let caret = (e.line, e.col);
        e.clear_selection();
        assert!(e.selection().is_none());
        assert_eq!(e.text(), "abc");
        assert_eq!((e.line, e.col), caret);
    }

    #[test]
    fn backspace_at_a_line_start_joins_lines() {
        let mut editor = NoteEditor::with_text("ab\ncd");
        editor.move_home();
        assert_eq!((editor.line, editor.col), (1, 0));
        editor.backspace();
        assert_eq!(editor.text(), "abcd");
        assert_eq!((editor.line, editor.col), (0, 2), "caret sits at the seam");
        assert_invariants(&editor);
    }

    #[test]
    fn backspace_over_a_multibyte_character_removes_it_whole() {
        let mut editor = NoteEditor::with_text("café");
        editor.backspace();
        assert_eq!(editor.text(), "caf");
        assert_invariants(&editor);
    }

    #[test]
    fn delete_at_a_line_end_joins_the_next_line() {
        let mut editor = NoteEditor::with_text("ab\ncd");
        editor.move_up();
        editor.move_end();
        editor.delete();
        assert_eq!(editor.text(), "abcd");
        assert_invariants(&editor);
    }

    #[test]
    fn arrows_wrap_between_lines() {
        let mut editor = NoteEditor::with_text("ab\ncd");
        editor.move_home();
        editor.move_left();
        assert_eq!((editor.line, editor.col), (0, 2), "wrapped to line 0 end");
        editor.move_end();
        editor.move_right();
        assert_eq!((editor.line, editor.col), (1, 0), "wrapped to line 1 start");
        assert_invariants(&editor);
    }

    #[test]
    fn vertical_moves_clamp_to_shorter_lines() {
        let mut editor = NoteEditor::with_text("abcdef\nxy");
        editor.move_up();
        editor.move_end();
        assert_eq!((editor.line, editor.col), (0, 6));
        editor.move_down();
        assert_eq!(
            (editor.line, editor.col),
            (1, 2),
            "clamped to the short line"
        );
        // The caret has no goal column: the clamp is remembered, so moving
        // back up returns to the clamped column, not the original one.
        editor.move_up();
        assert_eq!((editor.line, editor.col), (0, 2));
        assert_invariants(&editor);
    }

    #[test]
    fn vertical_moves_past_the_ends_stick() {
        let mut editor = NoteEditor::with_text("ab\ncd");
        editor.move_up();
        assert_eq!((editor.line, editor.col), (0, 2), "column carried up");
        editor.move_home();
        editor.move_up();
        assert_eq!((editor.line, editor.col), (0, 0), "no line above");
        editor.move_end();
        editor.move_down();
        editor.move_down();
        assert_eq!((editor.line, editor.col), (1, 2), "no line below");
        assert_invariants(&editor);
    }

    #[test]
    fn control_characters_are_refused() {
        let mut editor = NoteEditor::new();
        editor.insert('\t');
        editor.insert('\u{7f}');
        assert_eq!(editor.text(), "");
    }

    #[test]
    fn click_places_the_caret_on_a_boundary() {
        let mut editor = NoteEditor::with_text("日本\n語");
        editor.click(1, 4);
        assert_eq!(editor.line, 1);
        assert_eq!(editor.col, 3, "snapped down into the character");
        editor.click(99, 99);
        assert_eq!(editor.line, 1);
        assert_eq!(editor.col, 3, "clamped to the last line's end");
        assert_invariants(&editor);
    }

    #[test]
    fn the_caret_starts_at_the_last_character() {
        let editor = NoteEditor::with_text("ab\ncd");
        assert_eq!((editor.line, editor.col), (1, 2));
    }

    #[test]
    fn a_realistic_editing_session_holds_the_invariants() {
        let mut editor = NoteEditor::new();
        for c in "shopping list".chars() {
            editor.insert(c);
            assert_invariants(&editor);
        }
        editor.insert('\n');
        for c in "milk 爪 Jay's".chars() {
            editor.insert(c);
            assert_invariants(&editor);
        }
        editor.move_home();
        editor.move_up();
        editor.move_end();
        editor.backspace();
        assert_invariants(&editor);
        editor.delete();
        assert_invariants(&editor);
        while !editor.text().is_empty() {
            editor.move_home();
            editor.delete();
        }
        assert_invariants(&editor);
    }
}

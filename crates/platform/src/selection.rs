//! Text-selection model for the terminal window.
//!
//! Owns everything about *what* is selected and the on-screen content it's
//! selected from — deliberately free of Wayland, rendering, and clipboard
//! concerns so it can be reasoned about (and tested) in isolation. The window
//! keeps the protocol/clipboard plumbing and asks this type for the pure
//! answers: the selected text, which viewport rows a highlight touches, and
//! how a scroll or drag moves the anchor.
//!
//! ## Coordinates
//!
//! Endpoints are `(col, row)` where `row` is a **signed viewport row**: it goes
//! negative when the selected text scrolls off the top into scrollback, which
//! keeps the selection logically glued to its text (scroll back down and it's
//! still selected). Consumers clamp to the visible range before drawing.

#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]

/// A `(col, row)` selection endpoint; `row` is a signed viewport row.
type Endpoint = (u16, i32);

/// Selection state plus the mirror of on-screen content it selects from.
///
/// Split out of `TermWindow` so the selection model — anchor tracking, drag
/// semantics, scroll-following, and text extraction — lives in one testable
/// place. The window drives it from pointer/keyboard events and reads back the
/// range, the mirror, and the highlighted rows when painting.
///
/// Named `SelectionTracker` (not `Selection`) to stay distinct from
/// [`grid::Selection`], which is the pure `(start, end)` value type in absolute
/// buffer coordinates. This is the stateful UI-thread *controller* that tracks
/// a drag in signed viewport rows and follows scrolling.
#[derive(Default)]
pub struct SelectionTracker {
    /// Active selection as `(anchor, head)` endpoints, or `None` when nothing
    /// is selected. The anchor is the fixed end; the head is what a drag or
    /// shift+click moves.
    range: Option<(Endpoint, Endpoint)>,
    /// Mirror of the latest grid content the renderer has drawn, used to
    /// extract selection text and locate the highlight — entirely on the UI
    /// thread, no round-trip to the parser-owned grid. Indexed `[row][col]`,
    /// only the visible viewport.
    mirror: Vec<Vec<grid::Cell>>,
    /// Current viewport offset (lines scrolled into scrollback; 0 = live
    /// bottom). Tracked so a wheel-scroll shifts the selection with the text.
    view_offset: i32,
    /// Whether the current press has moved enough to count as a drag. A press
    /// then release with no motion is a *click*, which clears the selection; a
    /// drag sets it. Set on press, tested on release.
    drag_moved: bool,
    /// Whether the left mouse button is currently held. Motion only extends a
    /// selection while this is true — releasing ends the drag for good (the
    /// selection itself stays visible).
    left_held: bool,
}

impl SelectionTracker {
    /// A fresh, empty selection.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The current `(anchor, head)` endpoints, if any.
    #[must_use]
    pub fn range(&self) -> Option<(Endpoint, Endpoint)> {
        self.range
    }

    /// Whether a selection is currently active.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.range.is_some()
    }

    /// Whether the left button is currently held with a selection drag in
    /// progress. Used by the window to drive edge-of-screen autoscroll only
    /// while an actual drag is live (not e.g. after release, or before any
    /// press).
    #[must_use]
    pub fn is_dragging(&self) -> bool {
        self.left_held
    }

    /// The mirrored on-screen content, indexed `[row][col]`.
    #[must_use]
    pub fn mirror(&self) -> &[Vec<grid::Cell>] {
        &self.mirror
    }

    /// Clear the selection if one is active. Returns `true` when something was
    /// actually cleared (so the caller knows to mark the window dirty).
    pub fn clear(&mut self) -> bool {
        if self.range.is_some() {
            self.range = None;
            true
        } else {
            false
        }
    }

    /// Shift+click range extension: keep the anchor fixed and move the head to
    /// `cell`, so repeated shift+clicks (or a shift+click then a drag) keep
    /// growing/shrinking the same range.
    fn extend(&mut self, cell: Endpoint) {
        if let Some((anchor, _head)) = self.range {
            self.range = Some((anchor, cell));
        } else {
            self.range = Some((cell, cell));
        }
    }

    /// Add `d` to both endpoints' rows (signed; rows may go negative = the
    /// selected text is above the visible area, still anchored). Used with
    /// opposite signs for the two scroll kinds:
    ///   • content scroll  → text moved up   → `shift(-delta)`
    ///   • viewport scroll → text appears to move down → `shift(+delta)`
    /// Never clears — the selection stays glued to its text.
    fn shift(&mut self, d: i32) {
        let Some(((ac, ar), (bc, br))) = self.range else {
            return;
        };
        self.range = Some(((ac, ar + d), (bc, br + d)));
    }

    /// Handle a left-button press at `cell` (with `shift` held or not).
    /// Mirrors kitty/xterm semantics:
    ///   • shift + existing selection → extend the range to `cell`;
    ///   • bare press with a selection visible → dismiss-click, clear it and
    ///     start nothing;
    ///   • bare press on empty space → begin a potential selection whose fate
    ///     (real selection vs. no-op click) is decided by whether the pointer
    ///     moves before release.
    ///
    /// Always marks the window dirty via the caller.
    pub fn press_left(&mut self, cell: Endpoint, shift: bool) {
        self.left_held = true;
        if shift && self.range.is_some() {
            self.extend(cell);
            self.drag_moved = true;
        } else if self.range.is_some() {
            self.range = None;
            self.drag_moved = false;
        } else {
            self.range = Some((cell, cell));
            self.drag_moved = false;
        }
    }

    /// Handle a left-button release. If the pointer never moved since the
    /// press, this was a bare click — clear whatever we tentatively anchored.
    pub fn release_left(&mut self) {
        self.left_held = false;
        if !self.drag_moved {
            self.range = None;
        }
    }

    /// Extend the selection head to `cell` during a drag. No-op unless the
    /// button is held and `cell` differs from the anchor. Returns `true` when
    /// the range actually changed (so the caller marks the window dirty).
    pub fn drag_to(&mut self, cell: Endpoint) -> bool {
        if !self.left_held {
            return false;
        }
        if let Some((anchor, _)) = self.range {
            if cell != anchor {
                self.range = Some((anchor, cell));
                self.drag_moved = true;
                return true;
            }
        }
        false
    }

    /// Fold a fresh dirty snapshot into the mirror and scroll-follow the
    /// selection. Handles the two scroll kinds (viewport wheel-scroll and
    /// content push) and keeps the mirror in sync with the latest drawn rows.
    pub fn ingest_snapshot(&mut self, snap: &grid::DirtySnapshot) {
        // Wheel-scroll of the viewport: the content is fixed but the view
        // window moved. Scrolling UP into history (offset grows) makes the
        // text appear to move DOWN, so the selection moves down too — the
        // opposite sign of a content scroll. Reset the mirror (a full-frame
        // snapshot repaints every visible row below).
        if let Some(new_off) = snap.viewport_offset() {
            let new_off = i32::from(new_off);
            // Distinguish a genuine user wheel-scroll from an output-driven
            // snap back to the bottom (offset -> 0 with new content). Only the
            // former shifts the selection with the text; the latter is just
            // the view jumping home, so the selection keeps its rows (the
            // content-scroll branch below shifts it for the output).
            let is_snap_to_bottom = new_off == 0 && snap.content_delta > 0;
            let view_delta = new_off - self.view_offset;
            self.view_offset = new_off;
            if view_delta != 0 && !is_snap_to_bottom {
                self.shift(view_delta);
            }
            self.mirror.clear();
        } else if self.view_offset != 0 {
            // A live-bottom snapshot while we thought we were scrolled — output
            // snapped the view back to the bottom without a viewport frame.
            // Resync so the next wheel-scroll shifts from the right base.
            self.view_offset = 0;
        }
        // Content scrolled up (new output pushed lines into scrollback): the
        // text physically moved up, so the selection moves up too. Rows that
        // scrolled off the top simply go negative (text is now in scrollback)
        // — the selection stays anchored to it.
        let delta = i32::from(snap.content_delta);
        if delta > 0 {
            self.shift(-delta);
            if !self.mirror.is_empty() {
                let d = (delta as usize).min(self.mirror.len());
                self.mirror.drain(..d);
            }
        }
        // Keep the cell mirror in sync so selection/highlight/extraction always
        // reflect the latest drawn content. `clone_from` (rather than
        // `row.cells.clone()` + assign) reuses the existing row Vec's heap
        // allocation when its capacity already fits — the overwhelmingly common
        // case, since a row's width doesn't change between snapshots — instead
        // of allocating a fresh Vec and dropping the old one on every single
        // dirty row of every snapshot.
        for row in &snap.rows {
            let r = usize::from(row.row);
            if self.mirror.len() <= r {
                self.mirror.resize(r + 1, Vec::new());
            }
            self.mirror[r].clone_from(&row.cells);
        }
    }

    /// The viewport rows the current selection touches, clamped to the
    /// `rows_visible` on-screen rows (empty if there's no selection or it's
    /// entirely scrolled off-screen). Off-screen rows need no repaint.
    #[must_use]
    pub fn visible_rows(&self, rows_visible: i32) -> Vec<u16> {
        let Some(((_, ar), (_, br))) = self.range else {
            return Vec::new();
        };
        let lo = ar.min(br).max(0);
        let hi = ar.max(br).min(rows_visible - 1);
        if lo > hi {
            return Vec::new();
        }
        (lo..=hi).map(|r| r as u16).collect()
    }

    /// Extract the currently selected text from the cell mirror (viewport
    /// coords). `None` when there's no non-empty selection.
    #[must_use]
    pub fn selected_text(&self) -> Option<String> {
        let ((ac, ar), (bc, br)) = self.range?;
        if (ac, ar) == (bc, br) {
            return None;
        }
        let ((sc, sr), (ec, er)) = if (ar, ac) <= (br, bc) {
            ((ac, ar), (bc, br))
        } else {
            ((bc, br), (ac, ar))
        };
        let mut out = String::new();
        for row in sr..=er {
            // Rows above/below the visible area (negative or past the mirror)
            // hold no extractable text — skip them; the selection itself stays
            // anchored and yields its text again once scrolled back on-screen.
            if row < 0 {
                continue;
            }
            let line = self.mirror.get(row as usize)?;
            let c0 = if row == sr { usize::from(sc) } else { 0 };
            let c1 = if row == er {
                usize::from(ec)
            } else {
                line.len().saturating_sub(1)
            };
            let mut s = String::new();
            let end = c1.min(line.len().saturating_sub(1));
            for cell in &line[c0..=end] {
                if !cell.spacer {
                    s.push(cell.ch);
                }
            }
            out.push_str(s.trim_end());
            if row != er {
                out.push('\n');
            }
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grid::{Cell, CursorInfo, DirtyRow, DirtySnapshot, ScrollBand, SnapshotKind};

    /// A row of plain cells from a string, row-indexed `row`.
    fn text_row(row: u16, s: &str) -> DirtyRow {
        let cells = s
            .chars()
            .map(|ch| Cell {
                ch,
                ..Cell::default()
            })
            .collect();
        DirtyRow { row, cells }
    }

    /// An incremental snapshot carrying `rows`, with the given content-scroll
    /// delta and no pixel-scroll band.
    fn incremental(rows: Vec<DirtyRow>, content_delta: u16) -> DirtySnapshot {
        DirtySnapshot {
            rows,
            cols: 80,
            cursor: CursorInfo::default(),
            cursor_changed: false,
            content_delta,
            kind: SnapshotKind::Incremental { scroll_span: None },
        }
    }

    /// A full-frame viewport snapshot at `offset` lines into scrollback.
    fn viewport(rows: Vec<DirtyRow>, offset: u16, content_delta: u16) -> DirtySnapshot {
        DirtySnapshot {
            rows,
            cols: 80,
            cursor: CursorInfo::default(),
            cursor_changed: false,
            content_delta,
            kind: SnapshotKind::Viewport {
                offset,
                shift: None,
            },
        }
    }

    /// A selection with two mirrored rows of text and a range spanning them.
    fn with_two_rows() -> SelectionTracker {
        let mut sel = SelectionTracker::new();
        sel.ingest_snapshot(&incremental(
            vec![text_row(0, "hello world"), text_row(1, "second line")],
            0,
        ));
        sel
    }

    #[test]
    fn new_selection_is_empty() {
        let sel = SelectionTracker::new();
        assert!(!sel.is_active());
        assert_eq!(sel.selected_text(), None);
        assert!(sel.visible_rows(24).is_empty());
    }

    #[test]
    fn press_on_empty_space_begins_a_potential_selection() {
        let mut sel = with_two_rows();
        sel.press_left((2, 0), false);
        // A zero-width anchor yields no text yet, but the range exists.
        assert!(sel.is_active());
        assert_eq!(sel.selected_text(), None);
    }

    #[test]
    fn drag_extends_and_yields_text() {
        let mut sel = with_two_rows();
        sel.press_left((0, 0), false);
        assert!(sel.drag_to((5, 0)));
        assert_eq!(sel.selected_text().as_deref(), Some("hello"));
    }

    #[test]
    fn is_dragging_tracks_button_state() {
        let mut sel = with_two_rows();
        assert!(!sel.is_dragging());
        sel.press_left((0, 0), false);
        assert!(sel.is_dragging());
        sel.drag_to((5, 0));
        assert!(sel.is_dragging());
        sel.release_left();
        assert!(!sel.is_dragging());
    }

    #[test]
    fn drag_without_press_is_ignored() {
        let mut sel = with_two_rows();
        // No button held -> motion must not extend anything.
        assert!(!sel.drag_to((5, 0)));
        assert!(!sel.is_active());
    }

    #[test]
    fn bare_click_clears_on_release() {
        let mut sel = with_two_rows();
        sel.press_left((2, 0), false);
        sel.release_left();
        assert!(!sel.is_active());
    }

    #[test]
    fn drag_survives_release() {
        let mut sel = with_two_rows();
        sel.press_left((0, 0), false);
        sel.drag_to((5, 0));
        sel.release_left();
        assert!(sel.is_active());
        assert_eq!(sel.selected_text().as_deref(), Some("hello"));
    }

    #[test]
    fn press_while_selection_visible_is_a_dismiss_click() {
        let mut sel = with_two_rows();
        sel.press_left((0, 0), false);
        sel.drag_to((5, 0));
        sel.release_left();
        assert!(sel.is_active());
        // A fresh press on empty space (no shift) dismisses the selection and
        // starts nothing.
        sel.press_left((3, 1), false);
        assert!(!sel.is_active());
    }

    #[test]
    fn shift_click_extends_from_the_anchor() {
        let mut sel = with_two_rows();
        sel.press_left((0, 0), false);
        sel.drag_to((5, 0)); // "hello"
        sel.release_left();
        // Shift+click past the end grows the same range to the end of row 0.
        sel.press_left((10, 0), true);
        assert_eq!(sel.selected_text().as_deref(), Some("hello world"));
    }

    #[test]
    fn multi_row_selection_joins_with_newline() {
        let mut sel = with_two_rows();
        sel.press_left((6, 0), false); // start at "world"
        sel.drag_to((6, 1)); // through "second"
        assert_eq!(sel.selected_text().as_deref(), Some("world\nsecond"));
    }

    #[test]
    fn clear_reports_whether_anything_was_active() {
        let mut sel = with_two_rows();
        assert!(!sel.clear());
        sel.press_left((0, 0), false);
        sel.drag_to((5, 0));
        assert!(sel.clear());
        assert!(!sel.is_active());
    }

    #[test]
    fn visible_rows_clamps_to_the_screen() {
        let mut sel = with_two_rows();
        sel.press_left((0, 0), false);
        sel.drag_to((0, 1));
        assert_eq!(sel.visible_rows(24), vec![0, 1]);
        // A one-row screen clips the span to its single visible row.
        assert_eq!(sel.visible_rows(1), vec![0]);
    }

    #[test]
    fn content_scroll_shifts_the_selection_up_with_its_text() {
        let mut sel = with_two_rows();
        sel.press_left((0, 1), false);
        sel.drag_to((6, 1)); // "second" on row 1
        // Two lines of new output push the text up by two rows.
        sel.ingest_snapshot(&incremental(vec![], 2));
        // The selection followed its text into scrollback (now row -1).
        assert_eq!(sel.range().unwrap().0 .1, -1);
        // Still glued to its text; the row is off-screen so no visible rows.
        assert!(sel.visible_rows(24).is_empty());
    }

    #[test]
    fn viewport_scroll_shifts_the_selection_down() {
        let mut sel = with_two_rows();
        sel.press_left((0, 0), false);
        sel.drag_to((5, 0));
        // Wheel-scroll up into history by 3 lines: text appears to move down.
        sel.ingest_snapshot(&viewport(vec![], 3, 0));
        assert_eq!(sel.range().unwrap().0 .1, 3);
    }

    #[test]
    fn snap_to_bottom_suppresses_the_viewport_shift_but_not_the_content_shift() {
        let mut sel = with_two_rows();
        // Scroll up first so view_offset is non-zero (here 3).
        sel.ingest_snapshot(&viewport(vec![], 3, 0));
        sel.press_left((0, 0), false);
        sel.drag_to((5, 0));
        // Output arrives that snaps the view home (offset 0, content_delta>0).
        // The view jump (delta -3) must NOT drag the selection, but the one
        // line of new content still shifts it up by 1 (it stays glued to text).
        sel.ingest_snapshot(&viewport(vec![], 0, 1));
        assert_eq!(sel.range(), Some(((0, -1), (5, -1))));
    }

    #[test]
    fn mirror_carries_link_id_through_for_hover_lookup() {
        // TermWindow::link_id_at reads straight through `mirror()` -- verify
        // a snapshot's link_id survives the ingest so a hover-underline can
        // find the cell it's over.
        let mut sel = SelectionTracker::new();
        let cells = vec![
            Cell { ch: 'a', link_id: 0, ..Cell::default() },
            Cell { ch: 'b', link_id: 7, ..Cell::default() },
        ];
        sel.ingest_snapshot(&incremental(vec![DirtyRow { row: 0, cells }], 0));
        assert_eq!(sel.mirror()[0][0].link_id, 0);
        assert_eq!(sel.mirror()[0][1].link_id, 7);
    }

    #[test]
    fn zero_delta_scroll_band_snapshot_still_updates_the_mirror() {
        // A scroll_span carried alongside an incremental snapshot must not
        // disturb the model beyond content_delta; the mirror still updates.
        let mut sel = SelectionTracker::new();
        let snap = DirtySnapshot {
            rows: vec![text_row(0, "abc")],
            cols: 80,
            cursor: CursorInfo::default(),
            cursor_changed: false,
            content_delta: 0,
            kind: SnapshotKind::Incremental {
                scroll_span: Some(ScrollBand {
                    top: 0,
                    bot: 5,
                    delta: 1,
                }),
            },
        };
        sel.ingest_snapshot(&snap);
        sel.press_left((0, 0), false);
        assert!(sel.drag_to((3, 0)));
        assert_eq!(sel.selected_text().as_deref(), Some("abc"));
    }
}

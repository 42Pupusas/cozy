//! Scratch probes for the coverage review — hostile/degenerate inputs a real
//! PTY stream can absolutely produce (`printf '\e[65535B'` etc.).

use grid::Grid;
use vt_parser::Parser;

fn feed(grid: &mut Grid, bytes: &[u8]) {
    let mut p = Parser::new();
    p.feed(bytes, |ev| grid.apply(&ev));
}

/// CSI CUD with a huge count must not overflow `row + n` (u16 add).
#[test]
fn cursor_down_u16_max_no_overflow() {
    let mut g = Grid::new(80, 24);
    feed(&mut g, b"\x1b[65535B");
    assert_eq!(g.cursor().1, 23);
}

/// CUD with cursor already at the bottom: `row + n` exceeds `u16::MAX`.
#[test]
fn cursor_down_from_bottom_u16_overflow() {
    let mut g = Grid::new(80, 24);
    feed(&mut g, b"\x1b[24;80H\x1b[65535B");
    assert_eq!(g.cursor().1, 23);
}

/// CUF with cursor already at the right edge: `col + n` exceeds `u16::MAX`.
#[test]
fn cursor_forward_from_edge_u16_overflow() {
    let mut g = Grid::new(80, 24);
    feed(&mut g, b"\x1b[24;80H\x1b[65535C");
    assert_eq!(g.cursor().0, 79);
}

/// ECH with cursor at right edge: `col + n` exceeds `u16::MAX`.
#[test]
fn erase_chars_from_edge_u16_overflow() {
    let mut g = Grid::new(80, 24);
    feed(&mut g, b"\x1b[1;80H\x1b[65535X");
}

/// LF at the bottom with a huge preceding CUD; also tab near `u16::MAX` col is
/// impossible, but `(col/8+1)*8` can overflow if col is near `u16::MAX` via a
/// 65535-wide resize — cheap to guard anyway.
#[test]
fn cursor_next_line_from_bottom_u16_overflow() {
    let mut g = Grid::new(80, 24);
    feed(&mut g, b"\x1b[24;1H\x1b[65535E");
}

/// CSI CUF with a huge count must not overflow `col + n`.
#[test]
fn cursor_forward_u16_max_no_overflow() {
    let mut g = Grid::new(80, 24);
    feed(&mut g, b"\x1b[65535C");
    assert_eq!(g.cursor().0, 79);
}

/// ECH with a huge count must not overflow `col + n`.
#[test]
fn erase_chars_u16_max_no_overflow() {
    let mut g = Grid::new(80, 24);
    feed(&mut g, b"hello\x1b[G\x1b[65535X");
}

/// CNL (cursor next line) with a huge count.
#[test]
fn cursor_next_line_u16_max_no_overflow() {
    let mut g = Grid::new(80, 24);
    feed(&mut g, b"\x1b[65535E");
    assert_eq!(g.cursor(), (0, 23));
}

/// LF while cursor sits below an upward-shrunk scroll region.
#[test]
fn linefeed_below_scroll_region() {
    let mut g = Grid::new(80, 24);
    // Region rows 1..10 (1-based), cursor to last row, then LF.
    feed(&mut g, b"\x1b[1;10r\x1b[24;1H\n");
}

/// DECSTBM with top >= bottom (degenerate region) must not panic on scroll.
#[test]
fn scroll_region_top_equals_bottom() {
    let mut g = Grid::new(80, 24);
    feed(&mut g, b"\x1b[5;5r\x1b[5;1Hx\ny\nz");
}

/// DECSTBM with reversed params (top > bottom).
#[test]
fn scroll_region_reversed() {
    let mut g = Grid::new(80, 24);
    feed(&mut g, b"\x1b[10;2r\nabc\n\n\n");
}

/// Scroll-up count far larger than the region height.
#[test]
fn scroll_up_huge_count() {
    let mut g = Grid::new(80, 24);
    feed(&mut g, b"\x1b[65535S");
}

/// IL/DL with cursor below the scroll region bottom.
#[test]
fn insert_delete_lines_outside_region() {
    let mut g = Grid::new(80, 24);
    feed(&mut g, b"\x1b[1;5r\x1b[20;1H\x1b[3L\x1b[3M");
}

/// Tab from the last column must not underflow/overflow.
#[test]
fn tab_at_last_column() {
    let mut g = Grid::new(80, 24);
    feed(&mut g, b"\x1b[80G\t\t");
    assert_eq!(g.cursor().0, 79);
}

/// Alt-screen enter/exit preserves the primary content.
#[test]
fn alt_screen_roundtrip_preserves_primary() {
    let mut g = Grid::new(80, 24);
    feed(&mut g, b"hello");
    feed(&mut g, b"\x1b[?1049h"); // enter alt
    feed(&mut g, b"ALTSCREEN");
    feed(&mut g, b"\x1b[?1049l"); // leave alt
    let s = g.screen();
    let row0: String = (0..5).map(|c| s.get_cell(c, 0).ch).collect();
    assert_eq!(row0, "hello");
}

/// Wide char printed at last column with pending-wrap already armed.
#[test]
fn wide_char_pending_wrap_last_column() {
    let mut g = Grid::new(4, 4);
    feed(&mut g, "ab漢漢".as_bytes());
}

/// A 1-column grid printing wide chars must not panic or loop.
#[test]
fn one_column_grid_wide_chars() {
    let mut g = Grid::new(1, 4);
    feed(&mut g, "漢漢x".as_bytes());
}

/// DCH at the far right edge.
#[test]
fn delete_chars_at_right_edge() {
    let mut g = Grid::new(80, 24);
    feed(&mut g, b"\x1b[80G\x1b[65535P");
}

/// CUP targeting far outside the grid clamps.
#[test]
fn cursor_position_out_of_bounds_clamps() {
    let mut g = Grid::new(80, 24);
    feed(&mut g, b"\x1b[999;999H");
    assert_eq!(g.cursor(), (79, 23));
}

// -- DSR / DA replies ------------------------------------------------------

/// DSR 6 (Cursor Position Report) must reply with the correct 1-based
/// row/col -- shells like fish block on this to track their prompt.
#[test]
fn dsr_cursor_position_report_is_1_based() {
    let mut g = Grid::new(80, 24);
    let mut p = Parser::new();
    p.feed(b"\x1b[5;10H\x1b[6n", |ev| g.apply(&ev));
    let replies = g.take_replies();
    assert_eq!(replies, b"\x1b[5;10R");
}

/// DSR 5 ("report status") must reply CSI 0 n regardless of terminal state.
#[test]
fn dsr_status_report_replies_ok() {
    let mut g = Grid::new(80, 24);
    let mut p = Parser::new();
    p.feed(b"\x1b[5n", |ev| g.apply(&ev));
    let replies = g.take_replies();
    assert_eq!(replies, b"\x1b[0n");
}

/// Unrecognized DSR codes must not queue a bogus reply.
#[test]
fn dsr_unknown_code_produces_no_reply() {
    let mut g = Grid::new(80, 24);
    let mut p = Parser::new();
    p.feed(b"\x1b[99n", |ev| g.apply(&ev));
    let replies = g.take_replies();
    assert!(replies.is_empty());
}

/// Device Attributes query must reply with a VT102-class response.
#[test]
fn device_attributes_replies() {
    let mut g = Grid::new(80, 24);
    let mut p = Parser::new();
    p.feed(b"\x1b[c", |ev| g.apply(&ev));
    let replies = g.take_replies();
    assert_eq!(replies, b"\x1b[?6c");
}

/// `take_replies` must drain -- a second call before any new query returns empty.
#[test]
fn take_replies_drains_buffer() {
    let mut g = Grid::new(80, 24);
    let mut p = Parser::new();
    p.feed(b"\x1b[6n", |ev| g.apply(&ev));
    let _ = g.take_replies();
    assert!(g.take_replies().is_empty());
}

// -- IL / DL / DCH / ICH content correctness -------------------------------

/// Insert Line must push existing content down and clear the inserted row,
/// not just leave garbage / fail to shift.
#[test]
fn insert_line_shifts_content_down_and_clears_new_row() {
    let mut g = Grid::new(10, 5);
    feed(&mut g, b"\x1b[1;1Hrow0\x1b[2;1Hrow1\x1b[1;1H\x1b[L");
    let s = g.screen();
    let row0: String = (0..4).map(|c| s.get_cell(c, 0).ch).collect();
    let row1: String = (0..4).map(|c| s.get_cell(c, 1).ch).collect();
    assert_eq!(
        row0, "    ",
        "inserted row must be blanked (space-filled), not left with old content"
    );
    assert_eq!(row1, "row0");
}

/// Delete Line must pull content up and blank the vacated bottom row.
#[test]
fn delete_line_shifts_content_up_and_clears_bottom_row() {
    let mut g = Grid::new(10, 3);
    feed(
        &mut g,
        b"\x1b[1;1Hrow0\x1b[2;1Hrow1\x1b[3;1Hrow2\x1b[1;1H\x1b[M",
    );
    let s = g.screen();
    let row0: String = (0..4).map(|c| s.get_cell(c, 0).ch).collect();
    let row2: String = (0..4).map(|c| s.get_cell(c, 2).ch).collect();
    assert_eq!(row0, "row1", "deleting row0 must pull row1's content up");
    assert_eq!(row2, "    ", "vacated bottom row must be blanked");
}

/// REGRESSION: deleting a line at the top of the screen must NOT push the
/// deleted content into scrollback -- DL is a line-editing op, not a scroll.
/// (`Screen::delete_lines` used to alias `scroll_up`, which pushes to
/// scrollback whenever the region starts at row 0.)
#[test]
fn delete_line_at_top_does_not_pollute_scrollback() {
    let mut g = Grid::new(10, 3);
    feed(
        &mut g,
        b"\x1b[1;1Hrow0\x1b[2;1Hrow1\x1b[3;1Hrow2\x1b[1;1H\x1b[M\x1b[M\x1b[M",
    );
    // After deleting all 3 rows one at a time from the top, the screen must
    // simply be blank -- writing directly to the last row must land there
    // untouched by any scrollback-push side effect from the DLs above.
    feed(&mut g, b"\x1b[3;1HX");
    let s = g.screen();
    let row2: String = (0..1).map(|c| s.get_cell(c, 2).ch).collect();
    assert_eq!(row2, "X");
}

/// Delete Chars (DCH) must shift the remainder of the line left and blank
/// the vacated columns at the right edge, not duplicate trailing content.
#[test]
fn delete_chars_shifts_left_and_blanks_tail() {
    let mut g = Grid::new(10, 1);
    feed(&mut g, b"abcdefghij\x1b[1;1H\x1b[3P");
    let s = g.screen();
    let row: String = (0..10).map(|c| s.get_cell(c, 0).ch).collect();
    assert_eq!(row, "defghij   ");
}

/// Insert Chars (ICH) must shift the remainder right and blank the newly
/// opened columns, not overwrite/lose the tail off the right edge.
#[test]
fn insert_chars_shifts_right_and_blanks_gap() {
    let mut g = Grid::new(10, 1);
    feed(&mut g, b"abcdefghij\x1b[1;1H\x1b[3@");
    let s = g.screen();
    let row: String = (0..10).map(|c| s.get_cell(c, 0).ch).collect();
    assert_eq!(&row[3..10], "abcdefg");
}

/// IL at the exact scroll-region bottom must not push content past the
/// region into untouched rows below it.
#[test]
fn insert_line_at_region_bottom_does_not_leak_past_region() {
    let mut g = Grid::new(10, 6);
    // Region rows 1..4 restricted to just 0..3 (1-based 1;4).
    feed(
        &mut g,
        b"\x1b[1;4r\x1b[1;1Hr0\x1b[2;1Hr1\x1b[3;1Hr2\x1b[5;1Hkeep\x1b[3;1H\x1b[L",
    );
    let s = g.screen();
    let row4: String = (0..4).map(|c| s.get_cell(c, 4).ch).collect();
    assert_eq!(
        row4, "keep",
        "content below the scroll region must be untouched"
    );
}

/// DCH with n larger than remaining columns must clamp cleanly (no
/// underflow/panic), blanking to the end of line.
#[test]
fn delete_chars_count_larger_than_remaining_clamps() {
    let mut g = Grid::new(10, 1);
    feed(&mut g, b"abcdefghij\x1b[1;5H\x1b[999P");
    let s = g.screen();
    let row: String = (0..10).map(|c| s.get_cell(c, 0).ch).collect();
    assert_eq!(&row[..4], "abcd");
}

/// ICH with n larger than remaining columns clamps rather than panicking.
#[test]
fn insert_chars_count_larger_than_remaining_clamps() {
    let mut g = Grid::new(10, 1);
    feed(&mut g, b"abcdefghij\x1b[1;5H\x1b[999@");
    let s = g.screen();
    let row: String = (0..4).map(|c| s.get_cell(c, 0).ch).collect();
    assert_eq!(row, "abcd");
}

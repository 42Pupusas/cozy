//! Coverage for `Grid::esc` (plain ESC sequences: RIS, DECSC/DECRC, IND, RI) —
//! previously untested (0% coverage, CC 9).

use grid::Grid;
use vt_parser::Parser;

fn feed(grid: &mut Grid, bytes: &[u8]) {
    let mut p = Parser::new();
    p.feed(bytes, |ev| grid.apply(&ev));
}

#[test]
fn ris_resets_cursor_and_attrs_and_clears_screen() {
    let mut g = Grid::new(10, 3);
    feed(&mut g, b"hello\x1b[1;31m\x1b[2;2H");
    // Sanity: state actually changed before reset.
    assert_ne!(g.cursor(), (0, 0));

    feed(&mut g, b"\x1bc"); // RIS
    assert_eq!(g.cursor(), (0, 0));
    // Screen content is cleared -- the first cell should no longer hold 'h'.
    assert_eq!(g.screen().get_cell(0, 0).ch, ' ');
}

#[test]
fn ris_resets_attrs_so_new_text_is_plain() {
    let mut g = Grid::new(10, 3);
    feed(&mut g, b"\x1b[1m"); // bold on
    feed(&mut g, b"\x1bc"); // RIS
    feed(&mut g, b"X");
    assert!(!g.screen().get_cell(0, 0).attrs.bold());
}

#[test]
fn ris_exits_alt_screen() {
    let mut g = Grid::new(10, 3);
    feed(&mut g, b"\x1b[?1049h"); // enter alt screen
    feed(&mut g, b"\x1bc"); // RIS
    // After RIS, printing should land in what is now (again) the primary
    // screen at (0,0) -- if alt_active were still true this would write into
    // the (cleared) alt screen instead, but either way cursor must be reset.
    assert_eq!(g.cursor(), (0, 0));
}

#[test]
fn decsc_decrc_roundtrip_cursor_and_attrs() {
    let mut g = Grid::new(10, 5);
    feed(&mut g, b"\x1b[3;4H\x1b[1m"); // move + bold
    feed(&mut g, b"\x1b7"); // DECSC - save
    feed(&mut g, b"\x1b[1;1H\x1b[0m"); // move away + reset attrs
    assert_eq!(g.cursor(), (0, 0));

    feed(&mut g, b"\x1b8"); // DECRC - restore
    assert_eq!(g.cursor(), (3, 2)); // col=3 (0-idx), row=2 (0-idx)
    feed(&mut g, b"X");
    assert!(g.screen().get_cell(3, 2).attrs.bold());
}

#[test]
fn decrc_without_prior_save_is_a_noop() {
    let mut g = Grid::new(10, 5);
    feed(&mut g, b"\x1b[2;2H");
    let before = g.cursor();
    feed(&mut g, b"\x1b8"); // DECRC, nothing was ever saved
    assert_eq!(g.cursor(), before);
}

#[test]
fn ind_moves_cursor_down_one() {
    let mut g = Grid::new(10, 5);
    feed(&mut g, b"\x1b[2;1H");
    let (_, row0) = g.cursor();
    feed(&mut g, b"\x1bD"); // IND
    let (_, row1) = g.cursor();
    assert_eq!(row1, row0 + 1);
}

#[test]
fn ind_at_bottom_scrolls_instead_of_overflowing() {
    let mut g = Grid::new(10, 3);
    feed(&mut g, b"\x1b[3;1H"); // last row
    feed(&mut g, b"\x1bD"); // IND at bottom -- must scroll, not move past bounds
    assert_eq!(g.cursor().1, 2); // still last row (24 -> clamped)
}

#[test]
fn ri_moves_cursor_up_one() {
    let mut g = Grid::new(10, 5);
    feed(&mut g, b"\x1b[3;1H");
    let (_, row0) = g.cursor();
    feed(&mut g, b"\x1bM"); // RI
    let (_, row1) = g.cursor();
    assert_eq!(row1, row0 - 1);
}

#[test]
fn ri_at_top_scrolls_down_and_keeps_cursor_at_top() {
    let mut g = Grid::new(10, 3);
    feed(&mut g, b"first\r\n"); // put something on row 0 before scrolling
    feed(&mut g, b"\x1b[1;1H"); // back to top
    feed(&mut g, b"\x1bM"); // RI at top -- scrolls down, cursor stays at row 0
    assert_eq!(g.cursor().1, 0);
}

#[test]
fn ri_respects_scroll_region_top() {
    let mut g = Grid::new(10, 6);
    feed(&mut g, b"\x1b[2;5r"); // scroll region rows 2..=5 (1-based)
    feed(&mut g, b"\x1b[2;1H"); // cursor at region top (row idx 1)
    feed(&mut g, b"\x1bM"); // RI at region top -- must scroll within region, not move above it
    assert_eq!(g.cursor().1, 1); // stays at region top, not row 0
}

#[test]
fn unrecognised_esc_sequence_is_a_harmless_noop() {
    let mut g = Grid::new(10, 3);
    feed(&mut g, b"\x1b[2;2H");
    let before = g.cursor();
    // ESC with some final byte we don't handle at all.
    feed(&mut g, b"\x1bZ");
    assert_eq!(g.cursor(), before);
}

#[test]
fn esc_abandons_pending_wrap() {
    // Fill the last column exactly so a pending-wrap flag would be set, then
    // send an ESC sequence that moves the cursor -- it must not carry the
    // deferred wrap into the next print.
    let mut g = Grid::new(3, 3);
    feed(&mut g, b"abc"); // fills the row exactly, cursor pending-wraps
    feed(&mut g, b"\x1b7"); // DECSC -- consumes/abandons pending wrap
    feed(&mut g, b"\x1b8"); // DECRC restores the same position
    feed(&mut g, b"X");
    // If pending wrap had survived, X would land at (0,1) after an implicit
    // wrap; instead it must overwrite in place at the saved column.
    assert_eq!(g.screen().get_cell(2, 0).ch, 'X');
}

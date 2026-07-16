//! Direct coverage for `Grid::csi`'s branches that other test files don't
//! already hit: CUU/CUB/CPL, CHA, HVP, all four ED modes, mode set/reset
//! (both SM/RM and DEC private forms), and unknown CSI as a no-op.

use grid::Grid;
use vt_parser::Parser;

fn feed(grid: &mut Grid, bytes: &[u8]) {
    let mut p = Parser::new();
    p.feed(bytes, |ev| grid.apply(&ev));
}

#[test]
fn cursor_up_moves_and_clamps_at_top() {
    let mut g = Grid::new(80, 24);
    feed(&mut g, b"\x1b[10;1H\x1b[3A");
    assert_eq!(g.cursor(), (0, 6));
    // Clamp: can't go above row 0.
    feed(&mut g, b"\x1b[100A");
    assert_eq!(g.cursor(), (0, 0));
}

#[test]
fn cursor_back_moves_and_clamps_at_left() {
    let mut g = Grid::new(80, 24);
    feed(&mut g, b"\x1b[1;10H\x1b[3D");
    assert_eq!(g.cursor(), (6, 0));
    feed(&mut g, b"\x1b[100D");
    assert_eq!(g.cursor(), (0, 0));
}

#[test]
fn cursor_prev_line_moves_up_and_resets_col() {
    let mut g = Grid::new(80, 24);
    feed(&mut g, b"\x1b[10;10H\x1b[2F");
    assert_eq!(g.cursor(), (0, 7));
}

#[test]
fn cursor_prev_line_clamps_at_top() {
    let mut g = Grid::new(80, 24);
    feed(&mut g, b"\x1b[2;5H\x1b[100F");
    assert_eq!(g.cursor(), (0, 0));
}

#[test]
fn cursor_horiz_abs_moves_col_only_and_clamps() {
    let mut g = Grid::new(80, 24);
    feed(&mut g, b"\x1b[5;5H\x1b[40G");
    assert_eq!(g.cursor(), (39, 4));
    feed(&mut g, b"\x1b[9999G");
    assert_eq!(g.cursor(), (79, 4));
}

#[test]
fn horiz_vert_pos_is_an_alias_for_cup() {
    let mut g = Grid::new(80, 24);
    feed(&mut g, b"\x1b[12;34f");
    assert_eq!(g.cursor(), (33, 11));
}

#[test]
fn erase_display_mode_0_clears_cursor_to_end() {
    let mut g = Grid::new(10, 3);
    feed(&mut g, b"aaaaaaaaaa\r\nbbbbbbbbbb\r\ncccccccccc");
    feed(&mut g, b"\x1b[2;5H\x1b[0J");
    let screen = g.screen();
    // Row 0 untouched.
    assert_eq!(screen.get_cell(0, 0).ch, 'a');
    // Row 1: first 4 cols kept, rest erased.
    assert_eq!(screen.get_cell(3, 1).ch, 'b');
    assert_eq!(screen.get_cell(4, 1).ch, ' ');
    // Row 2 fully erased.
    assert_eq!(screen.get_cell(0, 2).ch, ' ');
}

#[test]
fn erase_display_mode_1_clears_start_to_cursor() {
    let mut g = Grid::new(10, 3);
    feed(&mut g, b"aaaaaaaaaa\r\nbbbbbbbbbb\r\ncccccccccc");
    feed(&mut g, b"\x1b[2;5H\x1b[1J");
    let screen = g.screen();
    // Row 0 fully erased.
    assert_eq!(screen.get_cell(0, 0).ch, ' ');
    assert_eq!(screen.get_cell(9, 0).ch, ' ');
    // Row 1: cols 0..=4 erased, rest kept.
    assert_eq!(screen.get_cell(4, 1).ch, ' ');
    assert_eq!(screen.get_cell(5, 1).ch, 'b');
    // Row 2 untouched.
    assert_eq!(screen.get_cell(0, 2).ch, 'c');
}

#[test]
fn erase_display_mode_2_and_3_clear_everything() {
    for mode in [b'2', b'3'] {
        let mut g = Grid::new(10, 3);
        feed(&mut g, b"aaaaaaaaaa\r\nbbbbbbbbbb\r\ncccccccccc");
        feed(&mut g, b"\x1b[2;5H");
        feed(&mut g, &[0x1b, b'[', mode, b'J']);
        let screen = g.screen();
        for row in 0..3 {
            for col in 0..10 {
                assert_eq!(screen.get_cell(col, row).ch, ' ');
            }
        }
    }
}

#[test]
fn erase_display_unknown_mode_is_noop() {
    let mut g = Grid::new(10, 3);
    feed(&mut g, b"aaaaaaaaaa");
    feed(&mut g, b"\x1b[9J");
    assert_eq!(g.screen().get_cell(0, 0).ch, 'a');
}

#[test]
fn set_mode_and_reset_mode_do_not_panic_and_are_distinct_from_dec_private() {
    let mut g = Grid::new(80, 24);
    // ANSI SM/RM (no `?` prefix) vs DEC private (`?` prefix) go through the
    // same match arms in `Grid::csi` but are parsed as different CsiActions
    // upstream; exercise both so the arm is covered either way.
    feed(&mut g, b"\x1b[4h"); // SM (insert mode) - just must not panic
    feed(&mut g, b"\x1b[4l"); // RM
    feed(&mut g, b"\x1b[?25h"); // DECSET show cursor
    feed(&mut g, b"\x1b[?25l"); // DECRST hide cursor
}

#[test]
fn unknown_csi_final_byte_is_a_harmless_noop() {
    let mut g = Grid::new(80, 24);
    feed(&mut g, b"hello");
    // `CSI 5 z` isn't a recognised final byte/sequence; must not disturb
    // existing content or panic.
    feed(&mut g, b"\x1b[5z");
    assert_eq!(g.screen().get_cell(0, 0).ch, 'h');
}

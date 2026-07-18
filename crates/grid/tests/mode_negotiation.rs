//! Coverage for the mode-negotiation wiring added on top of the parser's
//! DECSCUSR / Kitty-keyboard / tab-stop / xterm-modifyOtherKeys support:
//! mouse tracking (1000/1002/1003/1006), DECCKM/DECAWM/DECOM/1004, tab
//! stops (HTS/TBC/CHT/CBT), DECSCUSR, and the Kitty keyboard flag stack.

use grid::{CursorShape, Grid};
use input::{KeyboardMode, MouseMode};
use vt_parser::Parser;

fn feed(grid: &mut Grid, bytes: &[u8]) {
    let mut p = Parser::new();
    p.feed(bytes, |ev| grid.apply(&ev));
}

#[test]
fn mouse_mode_starts_off() {
    let g = Grid::new(80, 24);
    assert_eq!(g.mouse_mode(), MouseMode::Off);
}

#[test]
fn decset_1000_enables_normal_tracking() {
    let mut g = Grid::new(80, 24);
    feed(&mut g, b"\x1b[?1000h");
    assert_eq!(g.mouse_mode(), MouseMode::Normal);
    feed(&mut g, b"\x1b[?1000l");
    assert_eq!(g.mouse_mode(), MouseMode::Off);
}

#[test]
fn decset_1002_and_1003_select_motion_modes() {
    let mut g = Grid::new(80, 24);
    feed(&mut g, b"\x1b[?1002h");
    assert_eq!(g.mouse_mode(), MouseMode::ButtonMotion);
    feed(&mut g, b"\x1b[?1003h");
    assert_eq!(g.mouse_mode(), MouseMode::AnyEvent);
}

#[test]
fn decset_1006_layers_sgr_encoding_on_top_of_tracking() {
    let mut g = Grid::new(80, 24);
    feed(&mut g, b"\x1b[?1000h\x1b[?1006h");
    // Tracking granularity is unaffected by 1006 -- it's an independent flag.
    assert_eq!(g.mouse_mode(), MouseMode::Normal);
    assert!(g.sgr_mouse());
    // Disabling 1006 leaves the tracking mode as-is; only the encoding changes.
    feed(&mut g, b"\x1b[?1006l");
    assert_eq!(g.mouse_mode(), MouseMode::Normal);
    assert!(!g.sgr_mouse());
}

#[test]
fn disabling_a_non_active_tracking_mode_does_not_clobber_the_active_one() {
    let mut g = Grid::new(80, 24);
    feed(&mut g, b"\x1b[?1002h"); // app switches straight to button-motion
    feed(&mut g, b"\x1b[?1000l"); // stale ?1000l from an old init script
    assert_eq!(g.mouse_mode(), MouseMode::ButtonMotion);
}

#[test]
fn decckm_toggles_app_cursor() {
    let mut g = Grid::new(80, 24);
    assert!(!g.app_cursor());
    feed(&mut g, b"\x1b[?1h");
    assert!(g.app_cursor());
    feed(&mut g, b"\x1b[?1l");
    assert!(!g.app_cursor());
}

#[test]
fn decawm_off_disables_wrap_and_sticks_cursor_at_right_edge() {
    let mut g = Grid::new(10, 3);
    feed(&mut g, b"\x1b[?7l"); // DECAWM off
    feed(&mut g, b"0123456789ABC"); // would wrap onto row 1 with autowrap on
    assert_eq!(g.cursor(), (9, 0), "cursor sticks at the last column");
    // The last column keeps getting overwritten by each subsequent glyph
    // instead of wrapping.
    assert_eq!(g.screen().get_cell(9, 0).ch, 'C');
}

#[test]
fn decawm_on_by_default_still_wraps() {
    let mut g = Grid::new(10, 3);
    feed(&mut g, b"0123456789A");
    assert_eq!(g.cursor(), (1, 1));
}

#[test]
fn decom_homes_cursor_to_scroll_region_top_and_makes_cup_relative() {
    let mut g = Grid::new(80, 24);
    feed(&mut g, b"\x1b[5;20r"); // scroll region rows 5..=20 (1-based)
    feed(&mut g, b"\x1b[?6h"); // DECOM on -- re-homes cursor to region top
    assert_eq!(g.cursor(), (0, 4));
    // CUP row 1 now means "row 1 of the region" = absolute row 4 (0-based).
    feed(&mut g, b"\x1b[1;1H");
    assert_eq!(g.cursor(), (0, 4));
    // CUP row 3 = absolute row 6.
    feed(&mut g, b"\x1b[3;1H");
    assert_eq!(g.cursor(), (0, 6));
}

#[test]
fn decom_off_restores_absolute_addressing() {
    let mut g = Grid::new(80, 24);
    feed(&mut g, b"\x1b[5;20r\x1b[?6h\x1b[?6l");
    assert_eq!(g.cursor(), (0, 0));
    feed(&mut g, b"\x1b[10;1H");
    assert_eq!(g.cursor(), (0, 9));
}

#[test]
fn focus_events_flag_tracked() {
    let mut g = Grid::new(80, 24);
    assert!(!g.focus_events());
    feed(&mut g, b"\x1b[?1004h");
    assert!(g.focus_events());
    feed(&mut g, b"\x1b[?1004l");
    assert!(!g.focus_events());
}

#[test]
fn decscusr_sets_cursor_shape() {
    let mut g = Grid::new(80, 24);
    feed(&mut g, b"\x1b[3 q"); // blinking underline
    assert_eq!(g.cursor_shape(), CursorShape::Underline);
    feed(&mut g, b"\x1b[5 q"); // blinking bar
    assert_eq!(g.cursor_shape(), CursorShape::Bar);
    feed(&mut g, b"\x1b[1 q"); // blinking block
    assert_eq!(g.cursor_shape(), CursorShape::Block);
}

#[test]
fn hts_sets_a_custom_tab_stop() {
    let mut g = Grid::new(40, 3);
    feed(&mut g, b"\x1b[1;6H"); // column 5 (0-based)
    feed(&mut g, b"\x1bH"); // HTS at column 5
    feed(&mut g, b"\x1b[1;1H\t"); // tab from col 0 should land on col 5, not 8
    assert_eq!(g.cursor(), (5, 0));
}

#[test]
fn tbc_clears_all_tab_stops_so_tab_jumps_to_the_right_edge() {
    let mut g = Grid::new(20, 3);
    feed(&mut g, b"\x1b[3g"); // TBC all
    feed(&mut g, b"\t");
    assert_eq!(g.cursor(), (19, 0));
}

#[test]
fn cht_advances_by_multiple_tab_stops() {
    let mut g = Grid::new(40, 3);
    feed(&mut g, b"\x1b[2I"); // CHT 2 -- default stops at 8, 16
    assert_eq!(g.cursor(), (16, 0));
}

#[test]
fn cbt_retreats_by_a_tab_stop() {
    let mut g = Grid::new(40, 3);
    feed(&mut g, b"\x1b[1;20H"); // column 19 (0-based)
    feed(&mut g, b"\x1b[1Z"); // CBT 1 -- default stop at 16
    assert_eq!(g.cursor(), (16, 0));
}

#[test]
fn xterm_modify_other_keys_level_2_switches_keyboard_mode() {
    let mut g = Grid::new(80, 24);
    assert_eq!(g.keyboard_mode(), KeyboardMode::Legacy);
    feed(&mut g, b"\x1b[>4;2m");
    assert_eq!(g.keyboard_mode(), KeyboardMode::XtermModifyOtherKeys);
    feed(&mut g, b"\x1b[>4;0m");
    assert_eq!(g.keyboard_mode(), KeyboardMode::Legacy);
}

#[test]
fn kitty_keyboard_push_pop_switches_mode_and_flags() {
    let mut g = Grid::new(80, 24);
    feed(&mut g, b"\x1b[>5u"); // push flags=5
    assert_eq!(g.keyboard_mode(), KeyboardMode::Kitty);
    assert_eq!(g.kitty_flags(), 5);
    feed(&mut g, b"\x1b[<1u"); // pop 1 -- stack empties, mode falls back
    assert_eq!(g.keyboard_mode(), KeyboardMode::Legacy);
    assert_eq!(g.kitty_flags(), 0);
}

#[test]
fn kitty_keyboard_set_replace_or_and_and_modes() {
    let mut g = Grid::new(80, 24);
    feed(&mut g, b"\x1b[>1u"); // push flags=1
    feed(&mut g, b"\x1b[=2;2u"); // OR in flag 2 -> 3
    assert_eq!(g.kitty_flags(), 3);
    feed(&mut g, b"\x1b[=1;3u"); // AND out flag 1 -> 2
    assert_eq!(g.kitty_flags(), 2);
    feed(&mut g, b"\x1b[=9;1u"); // replace -> 9
    assert_eq!(g.kitty_flags(), 9);
}

#[test]
fn kitty_keyboard_query_replies_with_current_flags() {
    let mut g = Grid::new(80, 24);
    feed(&mut g, b"\x1b[>7u");
    feed(&mut g, b"\x1b[?u");
    let replies = g.take_replies();
    assert_eq!(replies, b"\x1b[?7u");
}

#[test]
fn dec_special_graphics_translates_line_drawing_bytes() {
    let mut g = Grid::new(20, 3);
    feed(&mut g, b"\x1b(0"); // designate G0 = DEC Special Graphics
    feed(&mut g, b"lqk"); // top-left corner, horizontal line, top-right corner
    assert_eq!(g.screen().get_cell(0, 0).ch, '\u{250c}');
    assert_eq!(g.screen().get_cell(1, 0).ch, '\u{2500}');
    assert_eq!(g.screen().get_cell(2, 0).ch, '\u{2510}');
}

#[test]
fn shift_out_in_switches_between_g1_and_g0() {
    let mut g = Grid::new(20, 3);
    feed(&mut g, b"\x1b)0"); // designate G1 = DEC Special Graphics
    feed(&mut g, b"\x0e"); // SO -- invoke G1
    feed(&mut g, b"q");
    assert_eq!(g.screen().get_cell(0, 0).ch, '\u{2500}');
    feed(&mut g, b"\x0f"); // SI -- back to G0 (plain ASCII)
    feed(&mut g, b"q");
    assert_eq!(g.screen().get_cell(1, 0).ch, 'q');
}

#[test]
fn designating_a_non_special_charset_is_plain_ascii() {
    let mut g = Grid::new(20, 3);
    feed(&mut g, b"\x1b(B"); // US-ASCII -- explicit, should be a no-op
    feed(&mut g, b"q");
    assert_eq!(g.screen().get_cell(0, 0).ch, 'q');
}

#[test]
fn ris_resets_charset_state() {
    let mut g = Grid::new(20, 3);
    feed(&mut g, b"\x1b(0\x0e"); // G0 = special graphics, SO to G1 (still ascii)
    feed(&mut g, b"\x1bc"); // RIS
    feed(&mut g, b"q");
    assert_eq!(g.screen().get_cell(0, 0).ch, 'q');
}

#[test]
fn bell_c0_sets_pending_flag_and_take_clears_it() {
    let mut g = Grid::new(20, 3);
    assert!(!g.take_bell());
    feed(&mut g, b"\x07");
    assert!(g.take_bell());
    assert!(!g.take_bell(), "take_bell must clear the flag");
}

#[test]
fn osc_title_is_captured_and_take_clears_it() {
    let mut g = Grid::new(20, 3);
    assert_eq!(g.take_title(), None);
    feed(&mut g, b"\x1b]0;hello world\x07");
    assert_eq!(g.take_title(), Some("hello world".to_string()));
    assert_eq!(g.take_title(), None, "take_title must clear the pending value");
}

#[test]
fn osc8_hyperlink_stamps_cells_with_a_link_id_resolvable_to_its_uri() {
    let mut g = Grid::new(20, 3);
    assert_eq!(g.screen().get_cell(0, 0).link_id, 0);
    feed(&mut g, b"\x1b]8;;https://example.com\x07link\x1b]8;;\x07plain");
    let linked = g.screen().get_cell(0, 0);
    assert_ne!(linked.link_id, 0);
    assert_eq!(g.link_uri(linked.link_id), Some("https://example.com"));
    // The trailing "plain" text after the close form carries no link.
    let unlinked = g.screen().get_cell(4, 0);
    assert_eq!(unlinked.link_id, 0);
}

#[test]
fn osc8_repeated_same_uri_reuses_the_link_id() {
    let mut g = Grid::new(20, 3);
    feed(&mut g, b"\x1b]8;;https://a\x07a\x1b]8;;\x07");
    let id1 = g.screen().get_cell(0, 0).link_id;
    feed(&mut g, b"\x1b]8;;https://a\x07b\x1b]8;;\x07");
    let id2 = g.screen().get_cell(1, 0).link_id;
    assert_eq!(id1, id2, "re-opening the same URI right after closing it should reuse the id");
}

#[test]
fn osc8_link_uri_is_none_for_id_zero_and_stale_ids() {
    let g = Grid::new(20, 3);
    assert_eq!(g.link_uri(0), None);
    assert_eq!(g.link_uri(999), None);
}

#[test]
fn ris_clears_the_link_table() {
    let mut g = Grid::new(20, 3);
    feed(&mut g, b"\x1b]8;;https://a\x07x\x1b]8;;\x07");
    let id = g.screen().get_cell(0, 0).link_id;
    assert!(g.link_uri(id).is_some());
    feed(&mut g, b"\x1bc"); // RIS
    assert_eq!(g.link_uri(id), None);
}

#[test]
fn osc52_clipboard_write_decodes_base64_and_is_taken_once() {
    let mut g = Grid::new(20, 3);
    assert_eq!(g.take_clipboard(), None);
    feed(&mut g, b"\x1b]52;c;aGVsbG8=\x07"); // "hello"
    assert_eq!(g.take_clipboard(), Some("hello".to_string()));
    assert_eq!(g.take_clipboard(), None, "take_clipboard must clear the pending value");
}

#[test]
fn osc52_query_is_not_answered() {
    let mut g = Grid::new(20, 3);
    feed(&mut g, b"\x1b]52;c;?\x07");
    assert_eq!(g.take_clipboard(), None);
}

#[test]
fn osc52_primary_selection_is_ignored() {
    let mut g = Grid::new(20, 3);
    feed(&mut g, b"\x1b]52;p;aGVsbG8=\x07");
    assert_eq!(g.take_clipboard(), None);
}

#[test]
fn osc52_empty_selector_defaults_to_clipboard() {
    let mut g = Grid::new(20, 3);
    feed(&mut g, b"\x1b]52;;aGVsbG8=\x07");
    assert_eq!(g.take_clipboard(), Some("hello".to_string()));
}

#[test]
fn osc10_query_replies_with_configured_foreground() {
    let mut g = Grid::new(20, 3);
    g.set_default_colors(grid::Color::Rgb(0xaa, 0xbb, 0xcc), grid::Color::Rgb(0x11, 0x22, 0x33));
    feed(&mut g, b"\x1b]10;?\x07");
    let replies = g.take_replies();
    assert_eq!(replies, b"\x1b]10;rgb:aaaa/bbbb/cccc\x1b\\");
}

#[test]
fn osc11_query_replies_with_configured_background() {
    let mut g = Grid::new(20, 3);
    g.set_default_colors(grid::Color::Rgb(0xaa, 0xbb, 0xcc), grid::Color::Rgb(0x11, 0x22, 0x33));
    feed(&mut g, b"\x1b]11;?\x07");
    let replies = g.take_replies();
    assert_eq!(replies, b"\x1b]11;rgb:1111/2222/3333\x1b\\");
}

#[test]
fn osc10_set_request_is_ignored_not_replied() {
    let mut g = Grid::new(20, 3);
    feed(&mut g, b"\x1b]10;rgb:ff/ff/ff\x07");
    assert!(g.take_replies().is_empty());
}

#[test]
fn osc10_defaults_are_sane_without_set_default_colors() {
    let mut g = Grid::new(20, 3);
    feed(&mut g, b"\x1b]10;?\x07");
    let replies = g.take_replies();
    assert!(!replies.is_empty(), "must reply even with no explicit theme set");
}

#[test]
fn combining_acute_accent_composes_onto_the_previous_letter() {
    let mut g = Grid::new(20, 3);
    feed(&mut g, "e\u{301}".as_bytes()); // 'e' + COMBINING ACUTE ACCENT
    assert_eq!(g.screen().get_cell(0, 0).ch, '\u{e9}'); // precomposed 'e' with acute
    assert_eq!(g.cursor(), (1, 0), "combining mark must not advance the cursor");
}

#[test]
fn combining_mark_with_no_table_entry_is_dropped_not_corrupting() {
    let mut g = Grid::new(20, 3);
    feed(&mut g, "z\u{323}".as_bytes()); // 'z' + COMBINING DOT BELOW (not in our table)
    assert_eq!(g.screen().get_cell(0, 0).ch, 'z', "base letter left untouched");
}

#[test]
fn combining_mark_at_column_zero_is_a_harmless_noop() {
    let mut g = Grid::new(20, 3);
    feed(&mut g, "\u{301}".as_bytes()); // bare combining mark, nothing before it
    assert_eq!(g.cursor(), (0, 0));
}

#[test]
fn decrqss_sgr_reports_current_attributes() {
    let mut g = Grid::new(20, 3);
    feed(&mut g, b"\x1b[1;31m"); // bold, fg red
    feed(&mut g, b"\x1bP$qm\x9c"); // DECRQSS m, terminated by bare ST
    let replies = g.take_replies();
    let s = std::str::from_utf8(&replies).unwrap();
    assert_eq!(s, "\x1bP1$r0;1;31m\x1b\\");
}

#[test]
fn decrqss_decstbm_reports_scroll_region() {
    let mut g = Grid::new(80, 24);
    feed(&mut g, b"\x1b[5;20r");
    feed(&mut g, b"\x1bP$qr\x9c");
    let replies = g.take_replies();
    let s = std::str::from_utf8(&replies).unwrap();
    assert_eq!(s, "\x1bP1$r5;20r\x1b\\");
}

#[test]
fn decrqss_decscusr_reports_cursor_style() {
    let mut g = Grid::new(20, 3);
    feed(&mut g, b"\x1b[4 q"); // steady underline
    feed(&mut g, b"\x1bP$q q\x9c");
    let replies = g.take_replies();
    let s = std::str::from_utf8(&replies).unwrap();
    assert_eq!(s, "\x1bP1$r4 q\x1b\\");
}

#[test]
fn decrqss_unsupported_request_gets_invalid_reply() {
    let mut g = Grid::new(20, 3);
    feed(&mut g, b"\x1bP$qX\x9c");
    let replies = g.take_replies();
    let s = std::str::from_utf8(&replies).unwrap();
    assert_eq!(s, "\x1bP0$r\x1b\\");
}

#[test]
fn rep_repeats_last_printed_char() {
    let mut g = Grid::new(20, 3);
    feed(&mut g, b"A\x1b[4b"); // print 'A', then REP x4 -> AAAAA
    for col in 0..5 {
        assert_eq!(g.screen().get_cell(col, 0).ch, 'A');
    }
    assert_eq!(g.screen().get_cell(5, 0).ch, ' ');
}

#[test]
fn rep_default_count_is_one() {
    let mut g = Grid::new(20, 3);
    feed(&mut g, b"x\x1b[b");
    assert_eq!(g.screen().get_cell(0, 0).ch, 'x');
    assert_eq!(g.screen().get_cell(1, 0).ch, 'x');
    assert_eq!(g.screen().get_cell(2, 0).ch, ' ');
}

#[test]
fn rep_with_no_preceding_print_is_a_noop() {
    let mut g = Grid::new(20, 3);
    // Cursor move breaks the chain -- REP right after a CUP has nothing to repeat.
    feed(&mut g, b"\x1b[2;5H\x1b[3b");
    assert_eq!(g.cursor(), (4, 1), "cursor unmoved, nothing printed");
}

#[test]
fn rep_after_control_byte_is_a_noop() {
    let mut g = Grid::new(20, 3);
    feed(&mut g, b"A\r\x1b[3b"); // print 'A', CR resets chain, REP no-ops
    assert_eq!(g.screen().get_cell(0, 0).ch, 'A');
    assert_eq!(g.screen().get_cell(1, 0).ch, ' ');
}

#[test]
fn ris_resets_all_negotiated_modes() {
    let mut g = Grid::new(80, 24);
    feed(&mut g, b"\x1b[?1000h\x1b[?1006h\x1b[?1h\x1b[?7l\x1b[?6h\x1b[?1004h\x1b[>5u");
    feed(&mut g, b"\x1bc"); // RIS
    assert_eq!(g.mouse_mode(), MouseMode::Off);
    assert!(!g.app_cursor());
    assert!(!g.focus_events());
    assert_eq!(g.keyboard_mode(), KeyboardMode::Legacy);
    // DECAWM back to on (default).
    let mut g2 = Grid::new(10, 3);
    feed(&mut g2, b"\x1b[?7l\x1bc0123456789A");
    assert_eq!(g2.cursor(), (1, 1), "autowrap restored by RIS");
}

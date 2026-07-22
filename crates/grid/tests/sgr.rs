//! Coverage for `Grid::apply_sgr` (CSI `m`) — every attribute on/off toggle,
//! every color mode, and the multi-attribute-in-one-sequence path.

use grid::{Color, Grid, UnderlineStyle};
use vt_parser::Parser;

fn feed(grid: &mut Grid, bytes: &[u8]) {
    let mut p = Parser::new();
    p.feed(bytes, |ev| grid.apply(&ev));
}

/// Print one char after applying `sgr_seq`, return its attrs.
fn attrs_after(sgr_seq: &[u8]) -> grid::Attrs {
    let mut g = Grid::new(10, 3);
    feed(&mut g, sgr_seq);
    feed(&mut g, b"X");
    g.screen().get_cell(0, 0).attrs
}

#[test]
fn bold_on() {
    assert!(attrs_after(b"\x1b[1m").bold());
}

#[test]
fn dim_on() {
    assert!(attrs_after(b"\x1b[2m").dim());
}

#[test]
fn italic_on() {
    assert!(attrs_after(b"\x1b[3m").italic());
}

#[test]
fn underline_on() {
    assert!(attrs_after(b"\x1b[4m").underline());
}

#[test]
fn blink_on() {
    assert!(attrs_after(b"\x1b[5m").blink());
}

#[test]
fn rapid_blink_also_sets_blink() {
    // SgrAttr::Blink | SgrAttr::RapidBlink share the same cell flag -- we
    // don't model blink speed separately.
    assert!(attrs_after(b"\x1b[6m").blink());
}

#[test]
fn inverse_on() {
    assert!(attrs_after(b"\x1b[7m").inverse());
}

#[test]
fn invisible_on() {
    assert!(attrs_after(b"\x1b[8m").invisible());
}

#[test]
fn strikethrough_on() {
    assert!(attrs_after(b"\x1b[9m").strikethrough());
}

#[test]
fn plain_underline_is_single_style() {
    let a = attrs_after(b"\x1b[4m");
    assert!(a.underline());
    assert_eq!(a.underline_style(), UnderlineStyle::Single);
}

#[test]
fn colon_underline_style_selects_curly() {
    let a = attrs_after(b"\x1b[4:3m");
    assert!(a.underline());
    assert_eq!(a.underline_style(), UnderlineStyle::Curly);
}

#[test]
fn colon_underline_style_zero_turns_underline_off() {
    let a = attrs_after(b"\x1b[4m\x1b[4:0m");
    assert!(!a.underline());
}

#[test]
fn underline_color_256_and_rgb_and_default() {
    let a = attrs_after(b"\x1b[58;5;9m");
    assert_eq!(a.underline_color, Color::Indexed(9));

    let a = attrs_after(b"\x1b[58;2;10;20;30m");
    assert_eq!(a.underline_color, Color::Rgb(10, 20, 30));

    let a = attrs_after(b"\x1b[58;5;9;59m");
    assert_eq!(a.underline_color, Color::Default);
}

#[test]
fn reset_clears_underline_style_and_color() {
    let a = attrs_after(b"\x1b[4:3;58;5;9;0m");
    assert!(!a.underline());
    assert_eq!(a.underline_style(), UnderlineStyle::Single);
    assert_eq!(a.underline_color, Color::Default);
}

#[test]
fn normal_intensity_clears_both_bold_and_dim() {
    let a = attrs_after(b"\x1b[1;2;22m");
    assert!(!a.bold());
    assert!(!a.dim());
}

#[test]
fn no_italic_clears_only_italic() {
    let a = attrs_after(b"\x1b[3;23m");
    assert!(!a.italic());
}

#[test]
fn no_underline_clears_only_underline() {
    let a = attrs_after(b"\x1b[4;24m");
    assert!(!a.underline());
}

#[test]
fn no_blink_clears_only_blink() {
    let a = attrs_after(b"\x1b[5;25m");
    assert!(!a.blink());
}

#[test]
fn no_inverse_clears_only_inverse() {
    let a = attrs_after(b"\x1b[7;27m");
    assert!(!a.inverse());
}

#[test]
fn no_invisible_clears_only_invisible() {
    let a = attrs_after(b"\x1b[8;28m");
    assert!(!a.invisible());
}

#[test]
fn no_strikethrough_clears_only_strikethrough() {
    let a = attrs_after(b"\x1b[9;29m");
    assert!(!a.strikethrough());
}

#[test]
fn reset_clears_everything_including_color() {
    let a = attrs_after(b"\x1b[1;3;4;31;42;0m");
    assert_eq!(a, grid::Attrs::default());
}

#[test]
fn fg_standard_color_0_through_7() {
    for n in 30u16..=37 {
        let seq = format!("\x1b[{n}m");
        let a = attrs_after(seq.as_bytes());
        assert_eq!(
            a.fg,
            Color::Indexed(u8::try_from(n - 30).unwrap()),
            "code {n}"
        );
    }
}

#[test]
fn bg_standard_color_0_through_7() {
    for n in 40u16..=47 {
        let seq = format!("\x1b[{n}m");
        let a = attrs_after(seq.as_bytes());
        assert_eq!(
            a.bg,
            Color::Indexed(u8::try_from(n - 40).unwrap()),
            "code {n}"
        );
    }
}

#[test]
fn fg_bright_color_90_through_97_offsets_by_8() {
    for n in 90u16..=97 {
        let seq = format!("\x1b[{n}m");
        let a = attrs_after(seq.as_bytes());
        assert_eq!(
            a.fg,
            Color::Indexed(u8::try_from(n - 90 + 8).unwrap()),
            "code {n}"
        );
    }
}

#[test]
fn bg_bright_color_100_through_107_offsets_by_8() {
    for n in 100u16..=107 {
        let seq = format!("\x1b[{n}m");
        let a = attrs_after(seq.as_bytes());
        assert_eq!(
            a.bg,
            Color::Indexed(u8::try_from(n - 100 + 8).unwrap()),
            "code {n}"
        );
    }
}

#[test]
fn fg_default_39_resets_only_fg() {
    let a = attrs_after(b"\x1b[31;39m");
    assert_eq!(a.fg, Color::Default);
}

#[test]
fn bg_default_49_resets_only_bg() {
    let a = attrs_after(b"\x1b[41;49m");
    assert_eq!(a.bg, Color::Default);
}

#[test]
fn fg_256_color() {
    let a = attrs_after(b"\x1b[38;5;200m");
    assert_eq!(a.fg, Color::Indexed(200));
}

#[test]
fn bg_256_color() {
    let a = attrs_after(b"\x1b[48;5;201m");
    assert_eq!(a.bg, Color::Indexed(201));
}

#[test]
fn fg_rgb_truecolor() {
    let a = attrs_after(b"\x1b[38;2;10;20;30m");
    assert_eq!(a.fg, Color::Rgb(10, 20, 30));
}

#[test]
fn bg_rgb_truecolor() {
    let a = attrs_after(b"\x1b[48;2;40;50;60m");
    assert_eq!(a.bg, Color::Rgb(40, 50, 60));
}

#[test]
fn multiple_attrs_in_one_sequence_all_apply() {
    let a = attrs_after(b"\x1b[1;3;4;7;38;2;1;2;3;48;5;9m");
    assert!(a.bold());
    assert!(a.italic());
    assert!(a.underline());
    assert!(a.inverse());
    assert_eq!(a.fg, Color::Rgb(1, 2, 3));
    assert_eq!(a.bg, Color::Indexed(9));
}

#[test]
fn empty_sgr_is_reset() {
    // `CSI m` with no params means `CSI 0 m`.
    let a = attrs_after(b"\x1b[1m\x1b[m");
    assert_eq!(a, grid::Attrs::default());
}

#[test]
fn attrs_persist_across_prints_until_changed() {
    let mut g = Grid::new(10, 3);
    feed(&mut g, b"\x1b[1mAB");
    let a0 = g.screen().get_cell(0, 0).attrs;
    let a1 = g.screen().get_cell(1, 0).attrs;
    assert!(a0.bold());
    assert!(a1.bold());
}

#[test]
fn attrs_do_not_leak_backward_to_already_printed_cells() {
    let mut g = Grid::new(10, 3);
    feed(&mut g, b"A\x1b[1mB");
    let a0 = g.screen().get_cell(0, 0).attrs;
    let a1 = g.screen().get_cell(1, 0).attrs;
    assert!(!a0.bold());
    assert!(a1.bold());
}

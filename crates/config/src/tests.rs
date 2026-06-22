#![cfg(test)]

use crate::{Action, Color, Config, CursorStyle};

fn parse(toml: &str) -> Config {
    Config::from_bytes(toml.as_bytes()).expect("parse failed")
}

fn fail(toml: &str) -> String {
    Config::from_bytes(toml.as_bytes())
        .unwrap_err()
        .to_string()
}

// ── Defaults ──────────────────────────────────────────────────────────────────

#[test]
fn empty_doc_gives_defaults() {
    let cfg = parse("");
    assert_eq!(cfg.term, "xterm-256color");
    assert_eq!(cfg.scrollback, 10_000);
    assert_eq!(cfg.font.size, 14);
    assert_eq!(cfg.cursor.style, CursorStyle::Block);
    assert!(cfg.cursor.blink);
    assert_eq!(cfg.cursor.blink_rate, 500);
    assert!(cfg.keybinds.is_empty());
}

// ── [general] ─────────────────────────────────────────────────────────────────

#[test]
fn general_shell_and_term() {
    let cfg = parse(
        "[general]\nshell = \"/usr/bin/zsh\"\nterm  = \"xterm-kitty\"\n",
    );
    assert_eq!(cfg.shell, "/usr/bin/zsh");
    assert_eq!(cfg.term,  "xterm-kitty");
}

#[test]
fn general_scrollback_zero() {
    let cfg = parse("[general]\nscrollback = 0\n");
    assert_eq!(cfg.scrollback, 0);
}

// ── [font] ────────────────────────────────────────────────────────────────────

#[test]
fn font_path_and_size() {
    let cfg = parse(
        "[font]\npath = \"/usr/share/fonts/TTF/Hack-Regular.ttf\"\nsize = 18\n",
    );
    assert_eq!(
        cfg.font.path.as_deref(),
        Some("/usr/share/fonts/TTF/Hack-Regular.ttf")
    );
    assert_eq!(cfg.font.size, 18);
}

#[test]
fn font_size_out_of_range() {
    let e = fail("[font]\nsize = 0\n");
    assert!(e.contains("font.size"), "got: {e}");
}

#[test]
fn font_size_upper_bound() {
    let e = fail("[font]\nsize = 1000\n");
    assert!(e.contains("font.size"), "got: {e}");
}

// ── [cursor] ──────────────────────────────────────────────────────────────────

#[test]
fn cursor_beam_no_blink() {
    let cfg = parse(
        "[cursor]\nstyle = \"beam\"\nblink = false\nblink_rate = 250\n",
    );
    assert_eq!(cfg.cursor.style, CursorStyle::Beam);
    assert!(!cfg.cursor.blink);
    assert_eq!(cfg.cursor.blink_rate, 250);
}

#[test]
fn cursor_underline() {
    let cfg = parse("[cursor]\nstyle = \"underline\"\n");
    assert_eq!(cfg.cursor.style, CursorStyle::Underline);
}

#[test]
fn cursor_bad_style() {
    let e = fail("[cursor]\nstyle = \"bar\"\n");
    assert!(e.contains("cursor.style"), "got: {e}");
}

// ── [colors] ──────────────────────────────────────────────────────────────────

#[test]
fn colors_foreground_background() {
    let cfg = parse(
        "[colors]\nforeground = \"#ffffff\"\nbackground = \"#000000\"\n",
    );
    assert_eq!(cfg.palette.foreground, Color::from_rgb(0xff, 0xff, 0xff));
    assert_eq!(cfg.palette.background, Color::from_rgb(0x00, 0x00, 0x00));
}

#[test]
fn colors_ansi_palette() {
    let cfg = parse(
        "[colors]\nred = \"#ff0000\"\nbright_red = \"#ff5555\"\n",
    );
    assert_eq!(cfg.palette.ansi(1), Color::from_rgb(0xff, 0x00, 0x00));
    assert_eq!(cfg.palette.ansi(9), Color::from_rgb(0xff, 0x55, 0x55));
}

#[test]
fn color_bad_hex() {
    let e = fail("[colors]\nforeground = \"zzz\"\n");
    assert!(e.contains("rrggbb") || e.contains("foreground"), "got: {e}");
}

#[test]
fn color_display_roundtrip() {
    let c = Color::from_rgb(0xab, 0x46, 0x42);
    let s = c.to_string();
    assert_eq!(Color::parse_hex(&s), Some(c));
}

#[test]
fn color_as_u32() {
    let c = Color::from_rgb(0x01, 0x02, 0x03);
    assert_eq!(c.as_u32(), 0x00_01_02_03);
}

// ── [keybinds] ────────────────────────────────────────────────────────────────

#[test]
fn keybinds_known_actions() {
    let cfg = parse(concat!(
        "[keybinds]\n",
        "\"Ctrl+Shift+C\" = \"copy\"\n",
        "\"Ctrl+Shift+V\" = \"paste\"\n",
        "\"Ctrl+Shift+U\" = \"scroll_up\"\n",
        "\"Ctrl+Shift+D\" = \"scroll_down\"\n",
        "\"Ctrl+T\"       = \"new_tab\"\n",
        "\"Ctrl+W\"       = \"close_tab\"\n",
    ));
    assert_eq!(cfg.keybinds.len(), 6);
    assert_eq!(cfg.keybinds[0].action, Action::Copy);
    assert_eq!(cfg.keybinds[1].action, Action::Paste);
    assert_eq!(cfg.keybinds[2].action, Action::ScrollUp);
    assert_eq!(cfg.keybinds[3].action, Action::ScrollDown);
    assert_eq!(cfg.keybinds[4].action, Action::NewTab);
    assert_eq!(cfg.keybinds[5].action, Action::CloseTab);
}

#[test]
fn keybinds_send_raw() {
    let cfg = parse("[keybinds]\n\"Alt+F4\" = \"\\u001b[1;5D\"\n");
    assert_eq!(cfg.keybinds[0].chord, "Alt+F4");
    assert!(
        matches!(&cfg.keybinds[0].action, Action::Send(s) if s.contains('\x1b')),
        "got: {:?}", cfg.keybinds[0].action
    );
}

// ── Full document ─────────────────────────────────────────────────────────────

#[test]
fn full_document() {
    let cfg = parse(concat!(
        "[general]\n",
        "shell      = \"/bin/fish\"\n",
        "term       = \"xterm-256color\"\n",
        "scrollback = 50000\n",
        "\n",
        "[font]\n",
        "path = \"/usr/share/fonts/TTF/JetBrainsMono-Regular.ttf\"\n",
        "size = 16\n",
        "\n",
        "[cursor]\n",
        "style      = \"beam\"\n",
        "blink      = true\n",
        "blink_rate = 400\n",
        "\n",
        "[colors]\n",
        "foreground = \"#d8d8d8\"\n",
        "background = \"#181818\"\n",
        "red        = \"#ab4642\"\n",
        "\n",
        "[keybinds]\n",
        "\"Ctrl+Shift+C\" = \"copy\"\n",
        "\"Ctrl+Shift+V\" = \"paste\"\n",
    ));

    assert_eq!(cfg.shell,      "/bin/fish");
    assert_eq!(cfg.scrollback, 50_000);
    assert_eq!(cfg.font.size,  16);
    assert_eq!(cfg.cursor.style, CursorStyle::Beam);
    assert_eq!(cfg.cursor.blink_rate, 400);
    assert_eq!(cfg.palette.red, Color::from_rgb(0xab, 0x46, 0x42));
    assert_eq!(cfg.keybinds.len(), 2);
}

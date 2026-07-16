#![cfg(test)]

use crate::{Action, Color, Config, ConfigError, CursorStyle};

/// Test-only helpers, kept as methods on `Config` rather than free functions
/// since they only ever parse (or fail to parse) a `Config`.
impl Config {
    fn parse(toml: &str) -> Self {
        Self::from_bytes(toml.as_bytes()).expect("parse failed")
    }

    fn fail(toml: &str) -> String {
        Self::from_bytes(toml.as_bytes()).unwrap_err().to_string()
    }
}

// ── Defaults ──────────────────────────────────────────────────────────────────

#[test]
fn empty_doc_gives_defaults() {
    let cfg = Config::parse("");
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
    let cfg = Config::parse("[general]\nshell = \"/usr/bin/zsh\"\nterm  = \"xterm-kitty\"\n");
    assert_eq!(cfg.shell, "/usr/bin/zsh");
    assert_eq!(cfg.term, "xterm-kitty");
}

#[test]
fn general_scrollback_zero() {
    let cfg = Config::parse("[general]\nscrollback = 0\n");
    assert_eq!(cfg.scrollback, 0);
}

// ── [font] ────────────────────────────────────────────────────────────────────

#[test]
fn font_path_and_size() {
    let cfg =
        Config::parse("[font]\npath = \"/usr/share/fonts/TTF/Hack-Regular.ttf\"\nsize = 18\n");
    assert_eq!(
        cfg.font.path.as_deref(),
        Some("/usr/share/fonts/TTF/Hack-Regular.ttf")
    );
    assert_eq!(cfg.font.size, 18);
}

#[test]
fn font_size_out_of_range() {
    let e = Config::fail("[font]\nsize = 0\n");
    assert!(e.contains("font.size"), "got: {e}");
}

#[test]
fn font_size_upper_bound() {
    let e = Config::fail("[font]\nsize = 1000\n");
    assert!(e.contains("font.size"), "got: {e}");
}

// ── [window] ───────────────────────────────────────────────────

#[test]
fn window_opacity_default_is_opaque() {
    let cfg = Config::parse("");
    assert!((cfg.window.opacity - 1.0).abs() < f64::EPSILON);
}

#[test]
fn window_opacity_fractional() {
    let cfg = Config::parse("[window]\nopacity = 0.80\n");
    assert!((cfg.window.opacity - 0.80).abs() < 1e-9);
}

#[test]
fn window_opacity_accepts_bare_integers() {
    // TOML `0` / `1` are valid floats in spirit; accept them like Alacritty does.
    let cfg = Config::parse("[window]\nopacity = 0\n");
    assert!((cfg.window.opacity - 0.0).abs() < f64::EPSILON);
}

#[test]
fn window_opacity_out_of_range() {
    let e = Config::fail("[window]\nopacity = 1.5\n");
    assert!(e.contains("window.opacity"), "got: {e}");
}

#[test]
fn window_opacity_negative_rejected() {
    let e = Config::fail("[window]\nopacity = -0.1\n");
    assert!(e.contains("window.opacity"), "got: {e}");
}

// ── [cursor] ──────────────────────────────────────────────────────────────────

#[test]
fn cursor_beam_no_blink() {
    let cfg = Config::parse("[cursor]\nstyle = \"beam\"\nblink = false\nblink_rate = 250\n");
    assert_eq!(cfg.cursor.style, CursorStyle::Beam);
    assert!(!cfg.cursor.blink);
    assert_eq!(cfg.cursor.blink_rate, 250);
}

#[test]
fn cursor_underline() {
    let cfg = Config::parse("[cursor]\nstyle = \"underline\"\n");
    assert_eq!(cfg.cursor.style, CursorStyle::Underline);
}

#[test]
fn cursor_bad_style() {
    let e = Config::fail("[cursor]\nstyle = \"bar\"\n");
    assert!(e.contains("cursor.style"), "got: {e}");
}

// ── [colors] ──────────────────────────────────────────────────────────────────

#[test]
fn colors_foreground_background() {
    let cfg = Config::parse("[colors]\nforeground = \"#ffffff\"\nbackground = \"#000000\"\n");
    assert_eq!(cfg.palette.foreground, Color::from_rgb(0xff, 0xff, 0xff));
    assert_eq!(cfg.palette.background, Color::from_rgb(0x00, 0x00, 0x00));
}

#[test]
fn colors_ansi_palette() {
    let cfg = Config::parse("[colors]\nred = \"#ff0000\"\nbright_red = \"#ff5555\"\n");
    assert_eq!(cfg.palette.ansi(1), Color::from_rgb(0xff, 0x00, 0x00));
    assert_eq!(cfg.palette.ansi(9), Color::from_rgb(0xff, 0x55, 0x55));
}

#[test]
fn colors_ansi_palette_all_16_indices_match_named_fields() {
    let cfg = Config::parse(concat!(
        "[colors]\n",
        "black = \"#000001\"\nred = \"#000002\"\ngreen = \"#000003\"\nyellow = \"#000004\"\n",
        "blue = \"#000005\"\nmagenta = \"#000006\"\ncyan = \"#000007\"\nwhite = \"#000008\"\n",
        "bright_black = \"#000009\"\nbright_red = \"#00000a\"\nbright_green = \"#00000b\"\n",
        "bright_yellow = \"#00000c\"\nbright_blue = \"#00000d\"\nbright_magenta = \"#00000e\"\n",
        "bright_cyan = \"#00000f\"\nbright_white = \"#000010\"\n",
    ));
    let p = &cfg.palette;
    assert_eq!(p.ansi(0), p.black);
    assert_eq!(p.ansi(1), p.red);
    assert_eq!(p.ansi(2), p.green);
    assert_eq!(p.ansi(3), p.yellow);
    assert_eq!(p.ansi(4), p.blue);
    assert_eq!(p.ansi(5), p.magenta);
    assert_eq!(p.ansi(6), p.cyan);
    assert_eq!(p.ansi(7), p.white);
    assert_eq!(p.ansi(8), p.bright_black);
    assert_eq!(p.ansi(9), p.bright_red);
    assert_eq!(p.ansi(10), p.bright_green);
    assert_eq!(p.ansi(11), p.bright_yellow);
    assert_eq!(p.ansi(12), p.bright_blue);
    assert_eq!(p.ansi(13), p.bright_magenta);
    assert_eq!(p.ansi(14), p.bright_cyan);
    assert_eq!(p.ansi(15), p.bright_white);
    // All 16 are distinct given distinct config values, confirming each
    // index truly reads its own field rather than aliasing a neighbor.
    let all: Vec<Color> = (0..16).map(|i| p.ansi(i)).collect();
    for i in 0..all.len() {
        for j in (i + 1)..all.len() {
            assert_ne!(all[i], all[j], "ansi({i}) and ansi({j}) collided");
        }
    }
}

#[test]
fn colors_ansi_palette_out_of_range_index_falls_back_to_foreground() {
    let cfg = Config::parse("[colors]\nforeground = \"#123456\"\n");
    for idx in [16u8, 17, 100, 255] {
        assert_eq!(cfg.palette.ansi(idx), cfg.palette.foreground, "idx {idx}");
    }
}

#[test]
fn color_bad_hex() {
    let e = Config::fail("[colors]\nforeground = \"zzz\"\n");
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
    let cfg = Config::parse(concat!(
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
    let cfg = Config::parse("[keybinds]\n\"Alt+F4\" = \"\\u001b[1;5D\"\n");
    assert_eq!(cfg.keybinds[0].chord, "Alt+F4");
    assert!(
        matches!(&cfg.keybinds[0].action, Action::Send(s) if s.contains('\x1b')),
        "got: {:?}",
        cfg.keybinds[0].action
    );
}

// ── Full document ─────────────────────────────────────────────────────────────

#[test]
fn full_document() {
    let cfg = Config::parse(concat!(
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

    assert_eq!(cfg.shell, "/bin/fish");
    assert_eq!(cfg.scrollback, 50_000);
    assert_eq!(cfg.font.size, 16);
    assert_eq!(cfg.cursor.style, CursorStyle::Beam);
    assert_eq!(cfg.cursor.blink_rate, 400);
    assert_eq!(cfg.palette.red, Color::from_rgb(0xab, 0x46, 0x42));
    assert_eq!(cfg.keybinds.len(), 2);
}

// ── ConfigError ───────────────────────────────────────────────────────────

#[test]
fn config_error_display_io() {
    let inner = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
    let e = ConfigError::Io(inner);
    assert_eq!(e.to_string(), "io error: no such file");
}

#[test]
fn config_error_display_invalid_value() {
    let e = ConfigError::InvalidValue {
        field: "font.size",
        reason: "must be > 0",
    };
    assert_eq!(e.to_string(), "invalid value for `font.size`: must be > 0");
}

#[test]
fn config_error_source_present_for_io_and_toml_only() {
    use std::error::Error as _;
    let io_err = ConfigError::Io(std::io::Error::new(std::io::ErrorKind::Other, "x"));
    assert!(io_err.source().is_some());

    let invalid = ConfigError::InvalidValue {
        field: "x",
        reason: "y",
    };
    assert!(invalid.source().is_none());
}

#[test]
fn config_error_from_io_error() {
    let inner = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
    let e: ConfigError = inner.into();
    assert!(matches!(e, ConfigError::Io(_)));
}

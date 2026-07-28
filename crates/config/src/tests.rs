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

#[test]
fn window_target_fps_default_is_60() {
    let cfg = Config::parse("");
    assert_eq!(cfg.window.target_fps, 60);
}

#[test]
fn window_target_fps_custom() {
    let cfg = Config::parse("[window]\ntarget_fps = 144\n");
    assert_eq!(cfg.window.target_fps, 144);
}

#[test]
fn window_target_fps_zero_rejected() {
    let e = Config::fail("[window]\ntarget_fps = 0\n");
    assert!(e.contains("window.target_fps"), "got: {e}");
}

#[test]
fn window_target_fps_upper_bound() {
    let e = Config::fail("[window]\ntarget_fps = 1001\n");
    assert!(e.contains("window.target_fps"), "got: {e}");
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

// ── [bar] ─────────────────────────────────────────────────────────────────

#[test]
fn bar_defaults_mirror_terminal_settings() {
    let cfg = Config::parse(
        "[font]\npath = \"/x/y.ttf\"\nsize = 16\n[colors]\nforeground = \"#111111\"\nbackground = \"#222222\"\n",
    );
    assert_eq!(cfg.bar_font_path(), Some("/x/y.ttf"));
    assert_eq!(cfg.bar_font_size(), 16);
    assert_eq!(cfg.bar_background(), Color::parse_hex("#222222").unwrap());
    assert_eq!(cfg.bar_foreground(), Color::parse_hex("#111111").unwrap());
    assert_eq!(cfg.bar_height(), 28);
}

#[test]
fn bar_overrides_win_over_mirrored_terminal_settings() {
    let cfg = Config::parse(
        "[font]\npath = \"/terminal.ttf\"\nsize = 16\n[colors]\nforeground = \"#111111\"\nbackground = \"#222222\"\n[bar]\nbackground = \"#abcdef\"\nforeground = \"#fedcba\"\nheight = 32\nfont_path = \"/bar.ttf\"\nfont_size = 11\n",
    );
    assert_eq!(cfg.bar_font_path(), Some("/bar.ttf"));
    assert_eq!(cfg.bar_font_size(), 11);
    assert_eq!(cfg.bar_background(), Color::parse_hex("#abcdef").unwrap());
    assert_eq!(cfg.bar_foreground(), Color::parse_hex("#fedcba").unwrap());
    assert_eq!(cfg.bar_height(), 32);
}

#[test]
fn bar_font_path_none_when_neither_bar_nor_terminal_font_configured() {
    let cfg = Config::parse("");
    assert_eq!(cfg.bar_font_path(), None);
}

#[test]
fn bar_bad_color_is_a_validation_error() {
    let err = Config::fail("[bar]\nbackground = \"not-a-color\"\n");
    assert!(err.contains("bar.background"));
}

#[test]
fn bar_height_out_of_range_rejected() {
    let err = Config::fail("[bar]\nheight = 0\n");
    assert!(err.contains("bar.height"));
}

#[test]
fn bar_emoji_and_opacity_mirror_terminal_settings() {
    let cfg = Config::parse("[font]\nemoji_path = \"/x/emoji.ttf\"\n[window]\nopacity = 0.75\n");
    assert_eq!(cfg.bar_emoji_path(), Some("/x/emoji.ttf"));
    assert!((cfg.bar_opacity() - 0.75).abs() < 1e-9);
}

#[test]
fn bar_emoji_and_opacity_overrides_win() {
    let cfg = Config::parse(
        "[font]\nemoji_path = \"/terminal-emoji.ttf\"\n[window]\nopacity = 0.75\n[bar]\nemoji_path = \"/bar-emoji.ttf\"\nopacity = 0.5\n",
    );
    assert_eq!(cfg.bar_emoji_path(), Some("/bar-emoji.ttf"));
    assert!((cfg.bar_opacity() - 0.5).abs() < 1e-9);
}

#[test]
fn bar_emoji_path_none_and_opacity_defaults_when_unconfigured() {
    let cfg = Config::parse("");
    assert_eq!(cfg.bar_emoji_path(), None);
    assert!((cfg.bar_opacity() - 1.0).abs() < f64::EPSILON);
}

#[test]
fn bar_opacity_out_of_range_rejected() {
    let err = Config::fail("[bar]\nopacity = 1.5\n");
    assert!(err.contains("bar.opacity"));
}

#[test]
fn bar_icon_path_none_by_default() {
    let cfg = Config::parse("");
    assert_eq!(cfg.bar_icon_path(), None);
}

#[test]
fn bar_icon_path_set_from_bar_table() {
    let cfg = Config::parse("[bar]\nicon_path = \"/nerd.ttf\"\n");
    assert_eq!(cfg.bar_icon_path(), Some("/nerd.ttf"));
}

#[test]
fn bar_notifications_empty_by_default() {
    let cfg = Config::parse("");
    assert!(cfg.bar_notifications().is_empty());
}

#[test]
fn bar_notifications_parses_array_of_tables_in_order() {
    let cfg = Config::parse(
        "[[bar.notifications]]\napp_id = \"mattermost\"\nicon = \"M\"\n\n\
         [[bar.notifications]]\napp_id = \"Slack\"\nicon = \"S\"\n",
    );
    let rules = cfg.bar_notifications();
    assert_eq!(rules.len(), 2);
    assert_eq!(rules[0].app_id, "mattermost");
    assert_eq!(rules[0].icon, "M");
    assert_eq!(rules[1].app_id, "Slack");
    assert_eq!(rules[1].icon, "S");
}

#[test]
fn bar_notifications_missing_app_id_is_a_validation_error() {
    let err = Config::fail("[[bar.notifications]]\nicon = \"M\"\n");
    assert!(err.contains("bar.notifications.app_id"));
}

#[test]
fn bar_notifications_missing_icon_is_a_validation_error() {
    let err = Config::fail("[[bar.notifications]]\napp_id = \"mattermost\"\n");
    assert!(err.contains("bar.notifications.icon"));
}

// ── [monitor] ─────────────────────────────────────────────────────────────

// ── [launcher] ──────────────────────────────────────────

#[test]
fn launcher_defaults_mirror_terminal_settings() {
    let cfg = Config::parse(
        "[font]\npath = \"/x/y.ttf\"\nsize = 16\n[colors]\nforeground = \"#111111\"\nbackground = \"#222222\"\n",
    );
    assert_eq!(cfg.launcher_font_path(), Some("/x/y.ttf"));
    assert_eq!(cfg.launcher_font_size(), 16);
    assert_eq!(
        cfg.launcher_background(),
        Color::parse_hex("#222222").unwrap()
    );
    assert_eq!(
        cfg.launcher_foreground(),
        Color::parse_hex("#111111").unwrap()
    );
}

#[test]
fn launcher_overrides_win_over_mirrored_terminal_settings() {
    let cfg = Config::parse(
        "[font]\npath = \"/terminal.ttf\"\nsize = 16\n[colors]\nforeground = \"#111111\"\nbackground = \"#222222\"\n[launcher]\nbackground = \"#abcdef\"\nforeground = \"#fedcba\"\nfont_path = \"/launcher.ttf\"\nfont_size = 11\n",
    );
    assert_eq!(cfg.launcher_font_path(), Some("/launcher.ttf"));
    assert_eq!(cfg.launcher_font_size(), 11);
    assert_eq!(
        cfg.launcher_background(),
        Color::parse_hex("#abcdef").unwrap()
    );
    assert_eq!(
        cfg.launcher_foreground(),
        Color::parse_hex("#fedcba").unwrap()
    );
}

#[test]
fn launcher_opacity_defaults_darker_than_bar() {
    // No [window]/[bar]/[launcher] opacity set at all: the bar mirrors
    // [window].opacity's own default of 1.0 (fully opaque), but the
    // launcher does *not* mirror that -- it has its own darker built-in
    // default, per this task's ask.
    let cfg = Config::parse("");
    assert!((cfg.bar_opacity() - 1.0).abs() < f64::EPSILON);
    assert!((cfg.launcher_opacity() - 0.85).abs() < 1e-9);
}

#[test]
fn launcher_opacity_override_wins() {
    let cfg = Config::parse("[launcher]\nopacity = 0.5\n");
    assert!((cfg.launcher_opacity() - 0.5).abs() < 1e-9);
}

#[test]
fn launcher_opacity_out_of_range_rejected() {
    let err = Config::fail("[launcher]\nopacity = 1.5\n");
    assert!(err.contains("launcher.opacity"));
}

#[test]
fn launcher_highlight_defaults_to_built_in_slate() {
    let cfg = Config::parse("");
    assert_eq!(cfg.launcher_highlight(), Color::from_rgb(0x31, 0x32, 0x44));
}

#[test]
fn launcher_highlight_override_wins() {
    let cfg = Config::parse("[launcher]\nhighlight = \"#2a2a37\"\n");
    assert_eq!(
        cfg.launcher_highlight(),
        Color::parse_hex("#2a2a37").unwrap()
    );
}

#[test]
fn launcher_highlight_bad_color_is_a_validation_error() {
    let err = Config::fail("[launcher]\nhighlight = \"nope\"\n");
    assert!(err.contains("launcher.highlight"));
}

#[test]
fn launcher_emoji_and_icon_path_mirror_font_and_bar() {
    let cfg =
        Config::parse("[font]\nemoji_path = \"/x/emoji.ttf\"\n[bar]\nicon_path = \"/nerd.ttf\"\n");
    assert_eq!(cfg.launcher_emoji_path(), Some("/x/emoji.ttf"));
    assert_eq!(cfg.launcher_icon_path(), Some("/nerd.ttf"));
}

#[test]
fn launcher_emoji_and_icon_path_overrides_win() {
    let cfg = Config::parse(
        "[font]\nemoji_path = \"/terminal-emoji.ttf\"\n[bar]\nicon_path = \"/bar-nerd.ttf\"\n[launcher]\nemoji_path = \"/launcher-emoji.ttf\"\nicon_path = \"/launcher-nerd.ttf\"\n",
    );
    assert_eq!(cfg.launcher_emoji_path(), Some("/launcher-emoji.ttf"));
    assert_eq!(cfg.launcher_icon_path(), Some("/launcher-nerd.ttf"));
}

#[test]
fn launcher_bad_color_is_a_validation_error() {
    let err = Config::fail("[launcher]\nbackground = \"not-a-color\"\n");
    assert!(err.contains("launcher.background"));
}

#[test]
fn monitor_defaults_mirror_bar_background_and_opacity() {
    let cfg = Config::parse("[colors]\nbackground = \"#222222\"\n[window]\nopacity = 0.75\n");
    assert_eq!(cfg.monitor_background(), Color::from_rgb(0x22, 0x22, 0x22));
    assert_eq!(cfg.monitor_surface(), Color::from_rgb(0x22, 0x22, 0x22));
    assert!((cfg.monitor_background_opacity() - 0.75).abs() < 1e-9);
}

#[test]
fn monitor_color_overrides_win_over_mirrored_bar_background() {
    let cfg = Config::parse(
        "[colors]\nbackground = \"#222222\"\n[monitor]\nbackground = \"#0a0a0b\"\nsurface = \"#181616\"\n",
    );
    assert_eq!(cfg.monitor_background(), Color::from_rgb(0x0a, 0x0a, 0x0b));
    assert_eq!(cfg.monitor_surface(), Color::from_rgb(0x18, 0x16, 0x16));
}

#[test]
fn monitor_text_and_accent_colors_default_to_built_ins() {
    let cfg = Config::parse("");
    assert_eq!(cfg.monitor_foreground(), Color::from_rgb(0xf2, 0xf2, 0xf3));
    assert_eq!(cfg.monitor_dim(), Color::from_rgb(0x8a, 0x8a, 0x90));
    assert_eq!(cfg.monitor_faint(), Color::from_rgb(0x55, 0x55, 0x5a));
    assert_eq!(cfg.monitor_border(), Color::from_rgb(0x1e, 0x1e, 0x21));
    assert_eq!(cfg.monitor_cpu(), Color::from_rgb(0x8a, 0x9a, 0x7b));
    assert_eq!(cfg.monitor_memory(), Color::from_rgb(0x8e, 0xa4, 0xa2));
    assert_eq!(cfg.monitor_temperature(), Color::from_rgb(0xb6, 0x92, 0x7b));
    assert_eq!(cfg.monitor_network(), Color::from_rgb(0x8e, 0xa4, 0xa2));
}

#[test]
fn monitor_text_and_accent_colors_are_overridable() {
    let cfg = Config::parse(
        "[monitor]\nforeground = \"#ffffff\"\ndim = \"#aaaaaa\"\nfaint = \"#333333\"\nborder = \"#444444\"\ncpu = \"#ff0000\"\nmemory = \"#00ff00\"\ntemperature = \"#0000ff\"\nnetwork = \"#123456\"\n",
    );
    assert_eq!(cfg.monitor_foreground(), Color::from_rgb(0xff, 0xff, 0xff));
    assert_eq!(cfg.monitor_dim(), Color::from_rgb(0xaa, 0xaa, 0xaa));
    assert_eq!(cfg.monitor_faint(), Color::from_rgb(0x33, 0x33, 0x33));
    assert_eq!(cfg.monitor_border(), Color::from_rgb(0x44, 0x44, 0x44));
    assert_eq!(cfg.monitor_cpu(), Color::from_rgb(0xff, 0x00, 0x00));
    assert_eq!(cfg.monitor_memory(), Color::from_rgb(0x00, 0xff, 0x00));
    assert_eq!(cfg.monitor_temperature(), Color::from_rgb(0x00, 0x00, 0xff));
    assert_eq!(cfg.monitor_network(), Color::from_rgb(0x12, 0x34, 0x56));
}

#[test]
fn monitor_bad_color_is_a_validation_error() {
    let err = Config::fail("[monitor]\nforeground = \"not-a-color\"\n");
    assert!(err.contains("monitor.foreground"));
}

#[test]
fn monitor_font_path_falls_back_to_terminal_font() {
    let cfg = Config::parse("[font]\npath = \"/terminal.ttf\"\n");
    assert_eq!(cfg.monitor_font_path(), Some("/terminal.ttf"));
}

#[test]
fn monitor_font_path_override_wins() {
    let cfg = Config::parse(
        "[font]\npath = \"/terminal.ttf\"\n[monitor]\nfont_path = \"/monitor.ttf\"\n",
    );
    assert_eq!(cfg.monitor_font_path(), Some("/monitor.ttf"));
}

// ── ConfigError ───────────────────────────────────────────────────────────

#[test]
fn config_error_display_io() {
    let inner = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
    let e = ConfigError::Io(inner);
    assert_eq!(e.to_string(), "io error: no such file");
}

// ── Color::mix ────────────────────────────────────────────────────────────────

#[test]
fn mixing_zero_and_one_returns_the_endpoints() {
    let a = Color::from_rgb(0x00, 0x10, 0x20);
    let b = Color::from_rgb(0xf0, 0xe0, 0xd0);
    assert_eq!(a.mix(b, 0.0), a);
    assert_eq!(a.mix(b, 1.0), b);
}

#[test]
fn mixing_halfway_lands_between() {
    let a = Color::from_rgb(0x00, 0x00, 0x00);
    let b = Color::from_rgb(0xff, 0xff, 0xff);
    assert_eq!(a.mix(b, 0.5), Color::from_rgb(0x80, 0x80, 0x80));
}

/// The property the file browser's chrome derivation depends on: a negative
/// `t` moves *away* from `other`. On a dark palette that darkens, on a light
/// one it lightens -- which is the whole reason chrome is derived rather
/// than hardcoded.
#[test]
fn a_negative_mix_moves_away_from_the_other_color() {
    let dark_base = Color::from_rgb(0x1e, 0x1e, 0x2e);
    let pale_text = Color::from_rgb(0xcd, 0xd6, 0xf4);
    let receded = dark_base.mix(pale_text, -0.35);
    assert!(
        receded.r < dark_base.r && receded.g < dark_base.g && receded.b < dark_base.b,
        "on a dark theme, receding from the text must darken: {receded}"
    );

    let pale_base = Color::from_rgb(0xff, 0xff, 0xff);
    let dark_text = Color::from_rgb(0x20, 0x20, 0x20);
    let receded = pale_base.mix(dark_text, -0.35);
    assert_eq!(
        receded,
        Color::from_rgb(0xff, 0xff, 0xff),
        "pure white cannot lighten further; it must clamp, not wrap"
    );
}

/// An extreme `t` must saturate rather than wrapping around to a wildly
/// wrong hue -- wraparound here would be a spectacular visual bug.
#[test]
fn extreme_mix_factors_clamp_rather_than_wrapping() {
    let a = Color::from_rgb(0x80, 0x80, 0x80);
    let b = Color::from_rgb(0xff, 0xff, 0xff);
    assert_eq!(a.mix(b, 100.0), Color::from_rgb(0xff, 0xff, 0xff));
    assert_eq!(a.mix(b, -100.0), Color::from_rgb(0x00, 0x00, 0x00));
}

/// The property [`Color::recede_from`] exists to guarantee: a distinct
/// shade for *every* palette, including the two extremes where a plain
/// negative mix has nowhere to go and silently returns the input.
#[test]
fn receding_always_produces_a_distinct_shade() {
    let cases = [
        (
            Color::from_rgb(0xff, 0xff, 0xff),
            Color::from_rgb(0x00, 0x00, 0x00),
        ),
        (
            Color::from_rgb(0x00, 0x00, 0x00),
            Color::from_rgb(0xff, 0xff, 0xff),
        ),
        (
            Color::from_rgb(0x1e, 0x1e, 0x2e),
            Color::from_rgb(0xcd, 0xd6, 0xf4),
        ),
        (
            Color::from_rgb(0xfd, 0xf6, 0xe3),
            Color::from_rgb(0x65, 0x7b, 0x83),
        ),
        (
            Color::from_rgb(0x80, 0x80, 0x80),
            Color::from_rgb(0x80, 0x80, 0x80),
        ),
    ];
    for (base, contrast) in cases {
        let receded = base.recede_from(contrast, 0.35);
        if base == contrast {
            // Degenerate: nothing to contrast against. Must not panic; an
            // equal result is the only honest answer.
            continue;
        }
        assert_ne!(receded, base, "recede_from({base}, {contrast}) vanished");
    }
}

// ── [files] ───────────────────────────────────────────────────────────────────

#[test]
fn files_colors_mirror_the_terminal_palette_by_default() {
    let cfg = Config::parse("");
    assert_eq!(cfg.files_background(), cfg.palette.background);
    assert_eq!(cfg.files_text(), cfg.palette.foreground);
    assert_eq!(cfg.files_dir(), cfg.palette.blue);
    assert_eq!(cfg.files_symlink(), cfg.palette.cyan);
    assert_eq!(cfg.files_broken(), cfg.palette.red);
    assert_eq!(cfg.files_error(), cfg.palette.red);
    assert_eq!(cfg.files_muted(), cfg.palette.bright_black);
    assert_eq!(cfg.files_caret(), cfg.palette.yellow);
    assert_eq!(cfg.files_cursor(), cfg.palette.blue);
}

/// Re-theming the terminal must carry into the browser without the user
/// touching `[files]` at all -- that is the point of the mirroring rule.
#[test]
fn retheming_the_terminal_moves_the_browser_with_it() {
    let cfg = Config::parse("[colors]\nbackground = \"#fdf6e3\"\nblue = \"#268bd2\"\n");
    assert_eq!(cfg.files_background(), Color::from_rgb(0xfd, 0xf6, 0xe3));
    assert_eq!(cfg.files_dir(), Color::from_rgb(0x26, 0x8b, 0xd2));
}

#[test]
fn files_overrides_win_over_the_mirrored_palette() {
    let cfg = Config::parse("[files]\ndir = \"#ff0000\"\nmuted = \"#00ff00\"\n");
    assert_eq!(cfg.files_dir(), Color::from_rgb(0xff, 0x00, 0x00));
    assert_eq!(cfg.files_muted(), Color::from_rgb(0x00, 0xff, 0x00));
    // Untouched keys still mirror.
    assert_eq!(cfg.files_symlink(), cfg.palette.cyan);
}

/// The derived roles have no `[colors]` equivalent, so the thing that must
/// hold is *contrast against what they sit on*, not a specific value.
#[test]
fn derived_chrome_stays_distinguishable_on_a_light_palette() {
    let cfg = Config::parse("[colors]\nbackground = \"#ffffff\"\nforeground = \"#000000\"\n");
    assert_ne!(cfg.files_header_bg(), cfg.files_background());
    assert_ne!(cfg.files_selection(), cfg.files_background());
    assert_ne!(cfg.files_scrollbar_thumb(), cfg.files_scrollbar_track());
    assert_ne!(cfg.files_filter_bg(), cfg.files_background());
}

#[test]
fn derived_chrome_stays_distinguishable_on_a_dark_palette() {
    let cfg = Config::parse("");
    assert_ne!(cfg.files_header_bg(), cfg.files_background());
    assert_ne!(cfg.files_selection(), cfg.files_background());
    assert_ne!(cfg.files_scrollbar_thumb(), cfg.files_scrollbar_track());
    assert_ne!(cfg.files_filter_bg(), cfg.files_background());
}

#[test]
fn the_status_strip_follows_the_header_unless_overridden() {
    let cfg = Config::parse("");
    assert_eq!(cfg.files_status_bg(), cfg.files_header_bg());

    let cfg = Config::parse("[files]\nstatus_bg = \"#123456\"\n");
    assert_eq!(cfg.files_status_bg(), Color::from_rgb(0x12, 0x34, 0x56));
    assert_ne!(cfg.files_status_bg(), cfg.files_header_bg());
}

#[test]
fn files_metrics_default_and_override() {
    let cfg = Config::parse("");
    assert_eq!(cfg.files_row_height(), 20);
    assert_eq!(cfg.files_header_height(), 24);
    assert_eq!(cfg.files_status_height(), 20);
    assert_eq!(cfg.files_padding(), 8);

    let cfg = Config::parse("[files]\nrow_height = 28\npadding = 12\n");
    assert_eq!(cfg.files_row_height(), 28);
    assert_eq!(cfg.files_padding(), 12);
    assert_eq!(cfg.files_header_height(), 24);
}

#[test]
fn files_bad_color_is_a_validation_error() {
    let err = Config::fail("[files]\ndir = \"nope\"\n");
    assert!(err.contains("files.dir"), "{err}");
}

/// A non-string colour is a plausible typo (`dir = 0x89b4fa`) and must be
/// reported rather than silently leaving the default in place.
#[test]
fn files_non_string_color_is_reported_not_ignored() {
    let err = Config::fail("[files]\ndir = 123\n");
    assert!(err.contains("files.dir"), "{err}");
}

/// A zero row height would divide by zero when computing how many rows fit.
#[test]
fn a_zero_row_height_is_rejected() {
    let err = Config::fail("[files]\nrow_height = 0\n");
    assert!(err.contains("files.row_height"), "{err}");
    assert!(
        err.contains('4'),
        "the message should state the real bound: {err}"
    );
}

/// Chrome strips, unlike rows, may legitimately be collapsed to nothing.
#[test]
fn chrome_strips_may_be_zero_height() {
    let cfg = Config::parse("[files]\nheader_height = 0\nstatus_height = 0\n");
    assert_eq!(cfg.files_header_height(), 0);
    assert_eq!(cfg.files_status_height(), 0);
}

#[test]
fn a_negative_metric_is_rejected() {
    let err = Config::fail("[files]\npadding = -4\n");
    assert!(err.contains("files.padding"), "{err}");
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
    let io_err = ConfigError::Io(std::io::Error::other("x"));
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

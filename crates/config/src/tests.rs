#![cfg(test)]

use crate::{Action, BarPosition, Color, Config, ConfigError, CursorStyle, PanelSide};

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
fn bar_position_defaults_to_top() {
    assert_eq!(Config::parse("").bar_position(), BarPosition::Top);
}

#[test]
fn bar_position_parses_both_edges() {
    let bottom = Config::parse("[bar]\nposition = \"bottom\"\n");
    assert_eq!(bottom.bar_position(), BarPosition::Bottom);
    let top = Config::parse("[bar]\nposition = \"top\"\n");
    assert_eq!(top.bar_position(), BarPosition::Top);
}

#[test]
fn bar_position_unknown_value_rejected() {
    let err = Config::fail("[bar]\nposition = \"left\"\n");
    assert!(err.contains("bar.position"));
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
fn bar_click_commands_parse() {
    let cfg = Config::from_bytes(
        br#"
[bar.click]
wifi = "panel-wifi --at {x}"
bluetooth = "panel-bluetooth --at {x}"
volume_output = "panel-volume --at {x}"
"#,
    )
    .unwrap();
    assert_eq!(
        cfg.bar_clicks().wifi.as_deref(),
        Some("panel-wifi --at {x}")
    );
    assert_eq!(
        cfg.bar_clicks().bluetooth.as_deref(),
        Some("panel-bluetooth --at {x}")
    );
    assert_eq!(
        cfg.bar_clicks().volume_output.as_deref(),
        Some("panel-volume --at {x}")
    );
    assert!(cfg.bar_clicks().battery.is_none());
}

#[test]
fn bar_click_commands_are_empty_by_default() {
    assert_eq!(
        Config::default().bar_clicks(),
        &crate::BarClickConfig::default()
    );
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
fn panel_defaults_mirror_the_shared_surface() {
    let cfg = Config::parse(
        "[colors]\nbackground = \"#181616\"\nforeground = \"#c5c9c5\"\n[window]\nopacity = 0.80\n",
    );
    assert_eq!(cfg.panel_background(), Color::from_rgb(0x18, 0x16, 0x16));
    assert_eq!(cfg.panel_foreground(), Color::from_rgb(0xc5, 0xc9, 0xc5));
    assert_eq!(cfg.panel_dim(), cfg.palette.bright_black);
    assert_eq!(cfg.panel_accent(), cfg.palette.blue);
    assert!((cfg.panel_opacity() - 0.80).abs() < 1e-9);
    assert_eq!(cfg.panel_gap(), 8);
    assert_eq!(cfg.panel_side(), PanelSide::Auto);
}

#[test]
fn panel_bar_overrides_flow_into_the_panels() {
    // The bar is the panels' immediate mirror target -- a `[bar]` override
    // must reach them without any `[panel]` table present.
    let cfg =
        Config::parse("[colors]\nbackground = \"#101010\"\n[bar]\nbackground = \"#1a1a2a\"\n");
    assert_eq!(cfg.panel_background(), Color::from_rgb(0x1a, 0x1a, 0x2a));
}

#[test]
fn panel_overrides_win_over_mirrored_bar() {
    let cfg = Config::parse(
        "[colors]\nbackground = \"#101010\"\n[panel]\nbackground = \"#0a0a0b\"\nforeground = \"#ffffff\"\ndim = \"#8a8a90\"\naccent = \"#8ba4b0\"\nhighlight = \"#252525\"\nborder = \"#444444\"\n",
    );
    assert_eq!(cfg.panel_background(), Color::from_rgb(0x0a, 0x0a, 0x0b));
    assert_eq!(cfg.panel_foreground(), Color::from_rgb(0xff, 0xff, 0xff));
    assert_eq!(cfg.panel_dim(), Color::from_rgb(0x8a, 0x8a, 0x90));
    assert_eq!(cfg.panel_accent(), Color::from_rgb(0x8b, 0xa4, 0xb0));
    assert_eq!(cfg.panel_highlight(), Color::from_rgb(0x25, 0x25, 0x25));
    assert_eq!(cfg.panel_border(), Color::from_rgb(0x44, 0x44, 0x44));
}

#[test]
fn derived_panel_shades_stay_between_their_endpoints() {
    // The two derived roles must read as fills on the real dark palette
    // (Kanagawa Dragon), not vanish into it or overshoot their sources.
    let cfg = Config::parse(
        "[colors]\nbackground = \"#181616\"\nforeground = \"#c5c9c5\"\nbright_black = \"#a6a69c\"\n",
    );
    let highlight = cfg.panel_highlight();
    let border = cfg.panel_border();
    let bg = cfg.panel_background();
    let fg = cfg.panel_foreground();
    assert!(highlight != bg, "the selected row must be visible");
    assert!(border != bg, "the groove must be visible");
    // Both derived fills must sit between background and foreground in
    // brightness -- washed *in*, not stamped over.
    let luma = |c: Color| i32::from(c.r) + i32::from(c.g) + i32::from(c.b);
    assert!(luma(highlight) > luma(bg) && luma(highlight) < luma(fg));
    assert!(luma(border) > luma(bg) && luma(border) < luma(fg));
}

#[test]
fn panel_gap_and_side_are_parsed_and_validated() {
    let cfg = Config::parse("[panel]\ngap = 12\nside = \"right\"\n");
    assert_eq!(cfg.panel_gap(), 12);
    assert_eq!(cfg.panel_side(), PanelSide::Right);
    let cfg = Config::parse("[panel]\nside = \"left\"");
    assert_eq!(cfg.panel_side(), PanelSide::Left);
    let err = Config::fail("[panel]\nside = \"middle\"\n");
    assert!(err.contains("panel.side"));
    let err = Config::fail("[panel]\ngap = 500\n");
    assert!(err.contains("panel.gap"));
}

#[test]
fn panel_bad_color_is_a_validation_error() {
    let err = Config::fail("[panel]\naccent = \"nope\"\n");
    assert!(err.contains("panel.accent"));
}

#[test]
fn panel_opacity_out_of_range_is_rejected() {
    let err = Config::fail("[panel]\nopacity = 1.5\n");
    assert!(err.contains("panel.opacity"));
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

/// Luma must be weighted, not a channel average: the eye reads green as far
/// brighter than blue, and an average would call these two equal.
#[test]
fn green_reads_brighter_than_blue_at_the_same_channel_value() {
    let green = Color::from_rgb(0x00, 0xff, 0x00);
    let blue = Color::from_rgb(0x00, 0x00, 0xff);
    assert!(
        green.luma() > blue.luma() * 4.0,
        "green {} vs blue {}",
        green.luma(),
        blue.luma()
    );
}

/// The endpoints, so the scale is anchored.
#[test]
fn luma_spans_black_to_white() {
    assert!(Color::from_rgb(0x00, 0x00, 0x00).luma() < 0.01);
    assert!((Color::from_rgb(0xff, 0xff, 0xff).luma() - 255.0).abs() < 0.01);
}

/// The point of `at_luma_of`: two inks end up equally bright, so any
/// remaining visible difference between them is hue rather than lightness.
#[test]
fn equalising_luma_leaves_only_hue_between_two_inks() {
    let cool = Color::from_rgb(0x8b, 0xa4, 0xb0);
    let pale = Color::from_rgb(0xc5, 0xc9, 0xc5);
    let matched = cool.at_luma_of(pale);
    assert!(
        (matched.luma() - pale.luma()).abs() < 2.0,
        "luma must match within rounding: {} vs {}",
        matched.luma(),
        pale.luma()
    );
    assert_ne!(matched, pale, "matching brightness must not erase the hue");
}

/// Hue is the thing being preserved, so the channel *ordering* must survive:
/// a blue-dominant ink must stay blue-dominant after rescaling.
#[test]
fn equalising_luma_preserves_which_channel_dominates() {
    let cool = Color::from_rgb(0x8b, 0xa4, 0xb0);
    assert!(cool.b > cool.r, "precondition: this ink is blue-leaning");
    let brightened = cool.at_luma_of(Color::from_rgb(0xff, 0xff, 0xff));
    assert!(
        brightened.b >= brightened.r,
        "the blue lean must survive rescaling: {brightened}"
    );
}

/// Black has no hue to scale and no ratio to preserve; it must be returned
/// as-is rather than dividing by zero and producing a NaN.
#[test]
fn black_has_no_hue_to_rescale() {
    let black = Color::from_rgb(0x00, 0x00, 0x00);
    assert_eq!(black.at_luma_of(Color::from_rgb(0xff, 0xff, 0xff)), black);
}

/// Rescaling to a brightness the hue cannot reach must clamp per channel
/// rather than wrapping into a wildly wrong colour.
#[test]
fn an_unreachable_target_clamps_rather_than_wrapping() {
    let dim_blue = Color::from_rgb(0x00, 0x00, 0x40);
    let result = dim_blue.at_luma_of(Color::from_rgb(0xff, 0xff, 0xff));
    assert_eq!(result.b, 0xff, "the dominant channel saturates");
    // The zero channels must stay zero: 0 * anything is 0, so a non-zero red
    // or green here would mean the scaling had wrapped rather than clamped.
    assert_eq!(
        (result.r, result.g),
        (0x00, 0x00),
        "clamping must not invent colour in empty channels: {result}"
    );
}

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
fn the_card_is_unplaced_unless_configured() {
    assert_eq!(Config::parse("").files.card, None);
}

#[test]
fn the_card_bounds_are_read_from_files() {
    let cfg =
        Config::parse("[files]\ncard_x = 10\ncard_y = 20\ncard_width = 300\ncard_height = 200\n");
    assert_eq!(
        cfg.files.card,
        Some(crate::CardBounds {
            x: 10,
            y: 20,
            width: 300,
            height: 200,
        })
    );
}

#[test]
fn part_of_the_card_bounds_is_an_error() {
    let err = Config::fail("[files]\ncard_x = 10\ncard_y = 20\n");
    assert!(err.contains("set together"), "{err}");
}

#[test]
fn a_zero_card_size_is_an_error() {
    let err = Config::fail("[files]\ncard_x = 0\ncard_y = 0\ncard_width = 0\ncard_height = 200\n");
    assert!(err.contains("files.card_width"), "{err}");
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
fn the_four_tile_colours_default_to_four_distinct_shades() {
    for doc in [
        "",
        "[colors]\nbackground = \"#ffffff\"\nforeground = \"#000000\"\n",
    ] {
        let cfg = Config::parse(doc);
        let shades: Vec<Color> = (0..4).map(|slot| cfg.files_tile_color(slot)).collect();
        for (i, a) in shades.iter().enumerate() {
            for b in &shades[i + 1..] {
                assert_ne!(a, b, "{doc:?}");
            }
            assert_ne!(*a, cfg.files_background(), "{doc:?}");
        }
    }
}

#[test]
fn each_tile_colour_key_overrides_only_its_own_slot() {
    let cfg = Config::parse("[files]\ntile_color_2 = \"#102030\"\ntile_color_4 = \"#405060\"\n");
    let plain = Config::parse("");
    assert_eq!(cfg.files_tile_color(1), Color::from_rgb(0x10, 0x20, 0x30));
    assert_eq!(cfg.files_tile_color(3), Color::from_rgb(0x40, 0x50, 0x60));
    assert_eq!(cfg.files_tile_color(0), plain.files_tile_color(0));
    assert_eq!(cfg.files_tile_color(2), plain.files_tile_color(2));
}

#[test]
fn tile_colour_slots_wrap_around() {
    let cfg = Config::parse("");
    assert_eq!(cfg.files_tile_color(5), cfg.files_tile_color(1));
}

#[test]
fn a_malformed_tile_colour_is_reported() {
    assert!(Config::from_bytes(b"[files]\ntile_color_3 = \"nope\"\n").is_err());
}

#[test]
fn derived_chrome_stays_distinguishable_on_a_dark_palette() {
    let cfg = Config::parse("");
    assert_ne!(cfg.files_header_bg(), cfg.files_background());
    assert_ne!(cfg.files_selection(), cfg.files_background());
    assert_ne!(cfg.files_scrollbar_thumb(), cfg.files_scrollbar_track());
    assert_ne!(cfg.files_filter_bg(), cfg.files_background());
}

/// Pins the *measured* chrome derivation for the palette actually in use
/// (Kanagawa Dragon). Asserting the value rather than just `assert_ne!`
/// against the background is the point: the contrast tests below pass
/// happily on a difference far larger than intended, which is how the
/// chrome came to derive to pure `#000000` -- a ~22-point drop sold as a
/// nudge, and visibly a *different surface* from the file list once the
/// window went translucent.
///
/// The step is now scaled against bounded headroom, so the bands stay in
/// the same near-black family as the list, per the design guide's rule to
/// keep large surfaces near-black rather than pure black.
#[test]
fn dragon_chrome_stays_in_the_same_near_black_family_as_the_list() {
    let cfg = Config::parse("[colors]\nforeground = \"#c5c9c5\"\nbackground = \"#181616\"\n");
    let bg = cfg.files_background();
    let chrome = cfg.files_header_bg();
    assert_eq!(bg, Color::from_rgb(0x18, 0x16, 0x16));
    // 0.35 of min(channel, 64) headroom: 0x18 - 8 and 0x16 - 8.
    assert_eq!(chrome, Color::from_rgb(0x10, 0x0e, 0x0e));
    assert_eq!(cfg.files_status_bg(), chrome);

    // The property that actually matters: distinct, but by a surface-tint
    // margin rather than a full-contrast leap, and never pure black.
    assert_ne!(chrome, bg);
    assert_ne!(chrome, Color::from_rgb(0x00, 0x00, 0x00));
    for (a, b) in [(bg.r, chrome.r), (bg.g, chrome.g), (bg.b, chrome.b)] {
        assert!(a.abs_diff(b) <= 12, "chrome step {a}->{b} is not subtle");
    }
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

/// The browser must be as translucent as its siblings by default.
///
/// It was the one opaque window on the desktop: `[window].opacity = 0.80`
/// moved the terminal, bar, launcher and monitor together and left the file
/// browser a solid rectangle among them. Mirroring is the fix, so this pins
/// the mirror rather than a literal.
#[test]
fn files_opacity_mirrors_the_terminal_window() {
    let cfg = Config::parse("[window]\nopacity = 0.8\n");
    assert!(
        (cfg.files_opacity() - 0.8).abs() < f64::EPSILON,
        "got {}",
        cfg.files_opacity()
    );
    // And with nothing configured at all it matches the window default,
    // whatever that is -- not a second hardcoded number that could drift.
    let cfg = Config::parse("");
    assert!((cfg.files_opacity() - cfg.window.opacity).abs() < f64::EPSILON);
}

/// …but an explicit `[files].opacity` still wins, for a user who wants the
/// browser more solid than the terminal behind it.
#[test]
fn files_opacity_override_wins() {
    let cfg = Config::parse("[window]\nopacity = 0.8\n[files]\nopacity = 1.0\n");
    assert!((cfg.files_opacity() - 1.0).abs() < f64::EPSILON);
}

/// An out-of-range opacity is a typo, not a clamp request: silently accepting
/// `opacity = 80` (meaning 80%) would render an invisible window.
#[test]
fn files_opacity_out_of_range_rejected() {
    let err = Config::fail("[files]\nopacity = 80\n");
    assert!(err.contains("files.opacity"), "{err}");
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

// ── [canvas] ───────────────────────────────────────────────────────────────────

#[test]
fn canvas_colors_mirror_the_terminal_palette_by_default() {
    let cfg = Config::parse("");
    assert_eq!(cfg.canvas_background(), cfg.palette.background);
    assert_eq!(cfg.canvas_text(), cfg.palette.foreground);
    assert_eq!(cfg.canvas_selection(), cfg.palette.blue);
    // The derived roles must differ from what they sit on, on both a dark
    // and a light palette, or the cards and grid would be invisible.
    assert_ne!(cfg.canvas_grid(), cfg.palette.background);
    assert_ne!(cfg.canvas_surface(), cfg.palette.background);
    assert_ne!(cfg.canvas_border(), cfg.canvas_surface());
    let light = Config::parse("[colors]\nbackground = \"#ffffff\"\nforeground = \"#000000\"\n");
    assert_ne!(light.canvas_grid(), light.palette.background);
    assert_ne!(light.canvas_surface(), light.palette.background);
    assert_ne!(light.canvas_border(), light.canvas_surface());
}

#[test]
fn canvas_overrides_win_over_the_mirrored_palette() {
    let cfg = Config::parse("[canvas]\nbackground = \"#101014\"\nselection = \"#ff8800\"\n");
    assert_eq!(cfg.canvas_background(), Color::from_rgb(0x10, 0x10, 0x14));
    assert_eq!(cfg.canvas_selection(), Color::from_rgb(0xff, 0x88, 0x00));
    // Untouched keys still mirror.
    assert_eq!(cfg.canvas_text(), cfg.palette.foreground);
}

#[test]
fn the_canvas_caret_mirrors_yellow_and_is_overridable() {
    let cfg = Config::parse("");
    assert_eq!(cfg.canvas_caret(), cfg.palette.yellow);
    // It must not be the selection colour: the caret sits on top of the
    // note's own glyphs and needs to stand apart from the selection accent.
    assert_ne!(cfg.canvas_caret(), cfg.canvas_selection());
    assert_ne!(cfg.canvas_caret(), cfg.canvas_background());

    let cfg = Config::parse("[canvas]\ncaret = \"#ff0000\"\n");
    assert_eq!(cfg.canvas_caret(), Color::from_rgb(0xff, 0x00, 0x00));
}

#[test]
fn canvas_metrics_default_and_override() {
    let cfg = Config::parse("");
    assert_eq!(cfg.canvas_stroke_width(), 2);
    assert_eq!(cfg.canvas_grid_size(), 8);

    let cfg = Config::parse("[canvas]\nstroke_width = 3\ngrid_size = 16\n");
    assert_eq!(cfg.canvas_stroke_width(), 3);
    assert_eq!(cfg.canvas_grid_size(), 16);
}

#[test]
fn canvas_opacity_mirrors_and_overrides() {
    let cfg = Config::parse("[window]\nopacity = 0.8\n");
    assert!((cfg.canvas_opacity() - 0.8).abs() < f64::EPSILON);
    let cfg = Config::parse("[window]\nopacity = 0.8\n[canvas]\nopacity = 1.0\n");
    assert!((cfg.canvas_opacity() - 1.0).abs() < f64::EPSILON);
}

#[test]
fn canvas_bad_color_is_a_validation_error() {
    let err = Config::fail("[canvas]\ngrid = \"not-a-color\"\n");
    assert!(err.contains("canvas.grid"), "{err}");
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

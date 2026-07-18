//! Terminal emulator configuration.
//!
//! Reads a TOML file via [`shelby`] and produces a validated [`Config`].
//! Zero external dependencies beyond shelby itself.
//!
//! # File layout
//!
//! ```toml
//! [general]
//! shell        = "/bin/bash"       # default: $SHELL or /bin/sh
//! term         = "xterm-256color"  # $TERM value advertised to the pty
//! scrollback   = 10000             # lines of scrollback; 0 = unlimited
//!
//! [font]
//! path = "/usr/share/fonts/TTF/JetBrainsMono-Regular.ttf"
//! size = 14                        # point size, like Alacritty/most terminals
//!                                   # (rendered at 96 DPI: 1pt = 96/72 px)
//! emoji_path  = "/path/to/emoji-font.ttf"  # optional: fallback face consulted
//!                                           # for codepoints `path` lacks (in
//!                                           # practice, almost always emoji).
//!                                           # unset = no fallback; those
//!                                           # codepoints render as .notdef.
//!
//! [window]
//! opacity     = 1.0                # 0.0 (fully transparent) .. 1.0 (opaque)
//! target_fps  = 60                 # animation poll target; raise for high-Hz displays
//!
//! [cursor]
//! style       = "block"            # block | beam | underline
//! blink       = true
//! blink_rate  = 500                # milliseconds per half-period
//!
//! [colors]
//! foreground  = "#d8d8d8"
//! background  = "#181818"
//! # 16 ANSI palette entries
//! black       = "#181818"
//! red         = "#ab4642"
//! green       = "#a1b56c"
//! yellow      = "#f7ca88"
//! blue        = "#7cafc2"
//! magenta     = "#ba8baf"
//! cyan        = "#86c1b9"
//! white       = "#d8d8d8"
//! bright_black   = "#585858"
//! bright_red     = "#ab4642"
//! bright_green   = "#a1b56c"
//! bright_yellow  = "#f7ca88"
//! bright_blue    = "#7cafc2"
//! bright_magenta = "#ba8baf"
//! bright_cyan    = "#86c1b9"
//! bright_white   = "#f8f8f8"
//!
//! [keybinds]
//! # "Ctrl+Shift+C" = "copy"
//! # "Ctrl+Shift+V" = "paste"
//!
//! [bar]
//! # Everything here is optional and defaults to *mirroring* the terminal's
//! # own [font]/[colors] above -- set any of these only to make the bar
//! # diverge from the terminal's look.
//! background  = "#181818"          # defaults to [colors].background
//! foreground  = "#d8d8d8"          # defaults to [colors].foreground
//! height      = 28                 # bar thickness in logical pixels
//! font_path   = "/usr/share/fonts/TTF/JetBrainsMono-Regular.ttf" # defaults to [font].path
//! font_size   = 12                 # points; defaults to [font].size
//! emoji_path  = "/path/to/emoji-font.ttf" # defaults to [font].emoji_path
//! icon_path   = "/path/to/NerdFont.ttf"    # optional: fallback face for
//!                                           # icon glyphs (network/battery/
//!                                           # volume symbols etc, matching
//!                                           # waybar's icon set). unset = no
//!                                           # icon fallback, no icons drawn.
//! opacity     = 1.0                # defaults to [window].opacity
//! ```

#![allow(clippy::doc_markdown)]

mod color;
mod extract;
mod tests;

pub use color::Color;

use extract::TableExt;
use shelby::Table as TomlTable;

// ── Error ─────────────────────────────────────────────────────────────────────

/// Everything that can go wrong while loading a config file.
#[derive(Debug)]
pub enum ConfigError {
    /// The file could not be read from disk.
    Io(std::io::Error),
    /// The TOML was syntactically or semantically invalid.
    Toml(shelby::Error),
    /// A value was present but had the wrong type or an out-of-range value.
    InvalidValue {
        field: &'static str,
        reason: &'static str,
    },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io error: {e}"),
            Self::Toml(e) => write!(f, "toml error: {e}"),
            Self::InvalidValue { field, reason } => {
                write!(f, "invalid value for `{field}`: {reason}")
            }
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Toml(e) => Some(e),
            Self::InvalidValue { .. } => None,
        }
    }
}

impl From<std::io::Error> for ConfigError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<shelby::Error> for ConfigError {
    fn from(e: shelby::Error) -> Self {
        Self::Toml(e)
    }
}

// ── Sub-types ─────────────────────────────────────────────────────────────────

/// Cursor rendering style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorStyle {
    #[default]
    Block,
    Beam,
    Underline,
}

/// The 16-color ANSI palette plus foreground/background.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    pub foreground: Color,
    pub background: Color,
    pub black: Color,
    pub red: Color,
    pub green: Color,
    pub yellow: Color,
    pub blue: Color,
    pub magenta: Color,
    pub cyan: Color,
    pub white: Color,
    pub bright_black: Color,
    pub bright_red: Color,
    pub bright_green: Color,
    pub bright_yellow: Color,
    pub bright_blue: Color,
    pub bright_magenta: Color,
    pub bright_cyan: Color,
    pub bright_white: Color,
}

impl Default for Palette {
    /// Base16 default-dark palette.
    fn default() -> Self {
        Self {
            foreground: Color::from_rgb(0xd8, 0xd8, 0xd8),
            background: Color::from_rgb(0x18, 0x18, 0x18),
            black: Color::from_rgb(0x18, 0x18, 0x18),
            red: Color::from_rgb(0xab, 0x46, 0x42),
            green: Color::from_rgb(0xa1, 0xb5, 0x6c),
            yellow: Color::from_rgb(0xf7, 0xca, 0x88),
            blue: Color::from_rgb(0x7c, 0xaf, 0xc2),
            magenta: Color::from_rgb(0xba, 0x8b, 0xaf),
            cyan: Color::from_rgb(0x86, 0xc1, 0xb9),
            white: Color::from_rgb(0xd8, 0xd8, 0xd8),
            bright_black: Color::from_rgb(0x58, 0x58, 0x58),
            bright_red: Color::from_rgb(0xab, 0x46, 0x42),
            bright_green: Color::from_rgb(0xa1, 0xb5, 0x6c),
            bright_yellow: Color::from_rgb(0xf7, 0xca, 0x88),
            bright_blue: Color::from_rgb(0x7c, 0xaf, 0xc2),
            bright_magenta: Color::from_rgb(0xba, 0x8b, 0xaf),
            bright_cyan: Color::from_rgb(0x86, 0xc1, 0xb9),
            bright_white: Color::from_rgb(0xf8, 0xf8, 0xf8),
        }
    }
}

impl Palette {
    /// Return the color for ANSI index 0–15.
    #[must_use]
    pub const fn ansi(&self, idx: u8) -> Color {
        match idx {
            0 => self.black,
            1 => self.red,
            2 => self.green,
            3 => self.yellow,
            4 => self.blue,
            5 => self.magenta,
            6 => self.cyan,
            7 => self.white,
            8 => self.bright_black,
            9 => self.bright_red,
            10 => self.bright_green,
            11 => self.bright_yellow,
            12 => self.bright_blue,
            13 => self.bright_magenta,
            14 => self.bright_cyan,
            15 => self.bright_white,
            _ => self.foreground,
        }
    }
}

/// Font configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontConfig {
    /// Path to the TTF/OTF file.
    pub path: Option<String>,
    /// Point size (typographic points, matching Alacritty/most terminals --
    /// *not* pixels). Converted to a pixel size for rasterization assuming
    /// the standard 96 DPI (`px = pt * 96 / 72`); see [`FontConfig::px_size`].
    pub size: u32,
    /// Path to a fallback TTF/OTF file consulted for codepoints the primary
    /// `path` face doesn't cover -- in practice, almost always emoji
    /// (monospace programming fonts essentially never ship them). `None`
    /// (the default) means no fallback is loaded: codepoints the primary
    /// face lacks render as `.notdef` (tofu), same as before emoji fallback
    /// support existed. There is no bundled/embedded emoji font -- point
    /// this at a real one (e.g. Noto Emoji, Noto Color Emoji if/when color
    /// glyphs are supported, OpenMoji, …) to get emoji rendering.
    pub emoji_path: Option<String>,
}

impl FontConfig {
    /// Standard screen DPI used to convert `size` (points) to a pixel size
    /// for the rasterizer. 96 DPI is the universal web/desktop-Linux default
    /// (and what Alacritty assumes absent real monitor DPI info), so a given
    /// point size renders at the same pixel size other terminals show it at.
    pub(crate) const DPI: f32 = 96.0;

    /// This config's font size in pixels, as consumed by [`FontFace::load`](../font/struct.FontFace.html#method.load).
    ///
    /// `size` is validated to `1..=256` points; the result comfortably fits
    /// `f32` with no precision loss.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn px_size(&self) -> f32 {
        self.size as f32 * Self::DPI / 72.0
    }
}

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            path: None,
            size: 14, // points, matching Alacritty's default-ish size
            emoji_path: None,
        }
    }
}

/// Window configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowConfig {
    /// Background opacity, `0.0` (fully transparent) to `1.0` (opaque).
    ///
    /// Only the window background is affected -- cell foreground (glyph)
    /// pixels are always drawn fully opaque, matching Alacritty's default
    /// `transparent_background_colors = false` behavior. Requires a
    /// compositor that composites transparent surfaces (most Wayland ones
    /// do); on one that doesn't, this has no visible effect.
    pub opacity: f64,
    /// Target frame rate, in frames per second, for the animation poll loop
    /// (cursor slide/blink). This is only a *target* poll cadence, not a
    /// guaranteed present rate: the compositor's own vsync (`wl_callback`)
    /// is the real ceiling, so setting this above your display's refresh
    /// rate just spends a bit more CPU polling without visible benefit.
    /// Default `60` matches most displays; bump to `120`/`144`/etc. on a
    /// high refresh-rate output for smoother cursor animation.
    pub target_fps: u32,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            opacity: 1.0,
            target_fps: 60,
        }
    }
}

/// Cursor configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorConfig {
    pub style: CursorStyle,
    pub blink: bool,
    /// Milliseconds per half-blink-period.
    pub blink_rate: u32,
}

impl Default for CursorConfig {
    fn default() -> Self {
        Self {
            style: CursorStyle::Block,
            blink: true,
            blink_rate: 500,
        }
    }
}

/// A single keybind action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Copy,
    Paste,
    ScrollUp,
    ScrollDown,
    NewTab,
    CloseTab,
    /// A raw string to send to the PTY.
    Send(String),
}

/// One keybind entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keybind {
    pub chord: String,
    pub action: Action,
}

/// Status-bar configuration.
///
/// Every field is optional in the TOML `[bar]` table and, when unset,
/// *mirrors* the corresponding terminal setting ([`Config::font`] /
/// [`Config::palette`]) -- see [`Config::bar_background`] etc., which
/// resolve the mirroring. This struct only holds the raw overrides actually
/// present in the file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BarConfig {
    /// Background color override. `None` mirrors `[colors].background`.
    pub background: Option<Color>,
    /// Foreground (text) color override. `None` mirrors `[colors].foreground`.
    pub foreground: Option<Color>,
    /// Bar thickness in logical pixels. `None` defaults to 28.
    pub height: Option<u32>,
    /// Font file override. `None` mirrors `[font].path` (and, if that's also
    /// unset, the bar falls back to the embedded bitmap font same as the
    /// terminal does).
    pub font_path: Option<String>,
    /// Font point size override. `None` mirrors `[font].size`.
    pub font_size: Option<u32>,
    /// Emoji fallback face override. `None` mirrors `[font].emoji_path`
    /// (which itself may be unset -- see `FontConfig::emoji_path`).
    pub emoji_path: Option<String>,
    /// Icon fallback face -- typically a Nerd Font, consulted for the
    /// private-use-area glyphs modules render (network/battery/volume
    /// icons etc, matching waybar's icon set -- see `statusbar::modules`).
    /// `None` (the default) means no icon fallback; those codepoints render
    /// as `.notdef` on `font_path`'s face. No terminal-side setting to
    /// mirror -- the main terminal has no use for a PUA icon font.
    pub icon_path: Option<String>,
    /// Background opacity override, `0.0`-`1.0`. `None` mirrors
    /// `[window].opacity`.
    pub opacity: Option<f64>,
}

// ── Root config ───────────────────────────────────────────────────────────────

/// The validated, fully-resolved terminal configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Shell binary to spawn (e.g. `/bin/bash`).
    pub shell: String,
    /// `$TERM` value advertised to the PTY.
    pub term: String,
    /// Lines of scrollback (`0` = unlimited).
    pub scrollback: usize,
    pub font: FontConfig,
    pub window: WindowConfig,
    pub cursor: CursorConfig,
    pub palette: Palette,
    pub keybinds: Vec<Keybind>,
    pub bar: BarConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            shell: Self::default_shell(),
            term: "xterm-256color".to_owned(),
            scrollback: 10_000,
            font: FontConfig::default(),
            window: WindowConfig::default(),
            cursor: CursorConfig::default(),
            palette: Palette::default(),
            keybinds: Vec::new(),
            bar: BarConfig::default(),
        }
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

impl Config {
    /// The shell to spawn when no config file (or no `general.shell` key)
    /// overrides it: `$SHELL`, falling back to `/bin/sh`.
    ///
    /// Was a free `default_shell()` function -- moved onto `Config` since
    /// it only ever feeds `Config::default()`.
    fn default_shell() -> String {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned())
    }

    /// The standard XDG config file path:
    /// `$XDG_CONFIG_HOME/terminal/config.toml` or
    /// `~/.config/terminal/config.toml`.
    ///
    /// Was a free `xdg_config_path()` function -- moved onto `Config` since
    /// it only ever feeds `Config::load()`.
    fn xdg_config_path() -> std::path::PathBuf {
        let base = std::env::var("XDG_CONFIG_HOME").map_or_else(
            |_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_owned());
                std::path::PathBuf::from(home).join(".config")
            },
            std::path::PathBuf::from,
        );
        base.join("terminal").join("config.toml")
    }

    /// Load config from the standard XDG location:
    /// `$XDG_CONFIG_HOME/terminal/config.toml` or
    /// `~/.config/terminal/config.toml`.
    ///
    /// Returns `Ok(Config::default())` if no file exists yet.
    pub fn load() -> Result<Self, ConfigError> {
        let path = Self::xdg_config_path();
        match std::fs::read(&path) {
            Ok(bytes) => Self::from_bytes(&bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(ConfigError::Io(e)),
        }
    }

    /// Load config from a specific file path.
    pub fn load_from(path: impl AsRef<std::path::Path>) -> Result<Self, ConfigError> {
        let bytes = std::fs::read(path)?;
        Self::from_bytes(&bytes)
    }

    /// Parse config from raw TOML bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ConfigError> {
        let table = TomlTable::parse(bytes)?;
        let mut cfg = Self::default();
        cfg.apply_general(&table)?;
        cfg.apply_font(&table)?;
        cfg.apply_window(&table)?;
        cfg.apply_cursor(&table)?;
        cfg.apply_colors(&table)?;
        cfg.apply_keybinds(&table);
        cfg.apply_bar(&table)?;
        Ok(cfg)
    }

    /// Apply the `[general]` table, if present.
    ///
    /// Split out of [`Self::from_bytes`] (along with its sibling `apply_*`
    /// methods below) purely to keep that function under clippy's
    /// line-count lint -- each section is independent and self-contained.
    fn apply_general(&mut self, table: &TomlTable<'_>) -> Result<(), ConfigError> {
        let Some(general) = table.get_table("general") else {
            return Ok(());
        };
        if let Some(s) = general.get_str("shell") {
            s.clone_into(&mut self.shell);
        }
        if let Some(s) = general.get_str("term") {
            s.clone_into(&mut self.term);
        }
        // Scrollback fits comfortably in u32; clamp the validated range to usize.
        if let Some(n) = general.get_int_range(
            "scrollback",
            "general.scrollback",
            0,
            1_000_000,
            "must be between 0 and 1000000",
        )? {
            self.scrollback = usize::try_from(n).unwrap_or(0);
        }
        Ok(())
    }

    /// Apply the `[font]` table, if present.
    fn apply_font(&mut self, table: &TomlTable<'_>) -> Result<(), ConfigError> {
        let Some(font) = table.get_table("font") else {
            return Ok(());
        };
        if let Some(s) = font.get_str("path") {
            self.font.path = Some(s.to_owned());
        }
        if let Some(n) =
            font.get_int_range("size", "font.size", 1, 256, "must be between 1 and 256")?
        {
            self.font.size = u32::try_from(n).unwrap_or(self.font.size);
        }
        if let Some(s) = font.get_str("emoji_path") {
            self.font.emoji_path = Some(s.to_owned());
        }
        Ok(())
    }

    /// Apply the `[window]` table, if present.
    fn apply_window(&mut self, table: &TomlTable<'_>) -> Result<(), ConfigError> {
        let Some(window) = table.get_table("window") else {
            return Ok(());
        };
        if let Some(n) = window.get_float_range(
            "opacity",
            "window.opacity",
            0.0,
            1.0,
            "must be between 0.0 and 1.0",
        )? {
            self.window.opacity = n;
        }
        if let Some(n) = window.get_int_range(
            "target_fps",
            "window.target_fps",
            1,
            1000,
            "must be between 1 and 1000",
        )? {
            self.window.target_fps = u32::try_from(n).unwrap_or(self.window.target_fps);
        }
        Ok(())
    }

    /// Apply the `[cursor]` table, if present.
    fn apply_cursor(&mut self, table: &TomlTable<'_>) -> Result<(), ConfigError> {
        let Some(cursor) = table.get_table("cursor") else {
            return Ok(());
        };
        if let Some(s) = cursor.get_str("style") {
            self.cursor.style = match s {
                "block" => CursorStyle::Block,
                "beam" => CursorStyle::Beam,
                "underline" => CursorStyle::Underline,
                _ => {
                    return Err(ConfigError::InvalidValue {
                        field: "cursor.style",
                        reason: "expected block | beam | underline",
                    });
                }
            };
        }
        if let Some(b) = cursor.get_bool("blink") {
            self.cursor.blink = b;
        }
        if let Some(n) = cursor.get_int_range(
            "blink_rate",
            "cursor.blink_rate",
            0,
            60_000,
            "must be between 0 and 60000",
        )? {
            self.cursor.blink_rate = u32::try_from(n).unwrap_or(self.cursor.blink_rate);
        }
        Ok(())
    }

    /// Apply the `[colors]` table, if present.
    fn apply_colors(&mut self, table: &TomlTable<'_>) -> Result<(), ConfigError> {
        let Some(colors) = table.get_table("colors") else {
            return Ok(());
        };
        macro_rules! color_field {
            ($field:ident, $key:literal) => {
                if let Some(s) = colors.get_str($key) {
                    self.palette.$field = Color::parse_hex(s).ok_or(ConfigError::InvalidValue {
                        field: $key,
                        reason: "invalid #rrggbb hex color",
                    })?;
                }
            };
        }
        color_field!(foreground, "foreground");
        color_field!(background, "background");
        color_field!(black, "black");
        color_field!(red, "red");
        color_field!(green, "green");
        color_field!(yellow, "yellow");
        color_field!(blue, "blue");
        color_field!(magenta, "magenta");
        color_field!(cyan, "cyan");
        color_field!(white, "white");
        color_field!(bright_black, "bright_black");
        color_field!(bright_red, "bright_red");
        color_field!(bright_green, "bright_green");
        color_field!(bright_yellow, "bright_yellow");
        color_field!(bright_blue, "bright_blue");
        color_field!(bright_magenta, "bright_magenta");
        color_field!(bright_cyan, "bright_cyan");
        color_field!(bright_white, "bright_white");
        Ok(())
    }

    /// Apply the `[keybinds]` table, if present.
    fn apply_keybinds(&mut self, table: &TomlTable<'_>) {
        let Some(keybinds) = table.get_table("keybinds") else {
            return;
        };
        for (chord, val) in keybinds.iter() {
            if let shelby::Value::String(action_str) = val {
                let action = match action_str.as_ref() {
                    "copy" => Action::Copy,
                    "paste" => Action::Paste,
                    "scroll_up" => Action::ScrollUp,
                    "scroll_down" => Action::ScrollDown,
                    "new_tab" => Action::NewTab,
                    "close_tab" => Action::CloseTab,
                    other => Action::Send(other.to_owned()),
                };
                self.keybinds.push(Keybind {
                    chord: chord.to_owned(),
                    action,
                });
            }
        }
    }

    /// Apply the `[bar]` table, if present. Every key is an *override* --
    /// leaving one unset means the status bar mirrors the corresponding
    /// terminal setting; see [`Config::bar_background`] /
    /// [`Config::bar_foreground`] / [`Config::bar_font_path`] /
    /// [`Config::bar_font_size`] for the resolved (mirrored-or-overridden)
    /// values consumers should actually read.
    fn apply_bar(&mut self, table: &TomlTable<'_>) -> Result<(), ConfigError> {
        let Some(bar) = table.get_table("bar") else {
            return Ok(());
        };
        if let Some(s) = bar.get_str("background") {
            self.bar.background = Some(Color::parse_hex(s).ok_or(ConfigError::InvalidValue {
                field: "bar.background",
                reason: "invalid #rrggbb hex color",
            })?);
        }
        if let Some(s) = bar.get_str("foreground") {
            self.bar.foreground = Some(Color::parse_hex(s).ok_or(ConfigError::InvalidValue {
                field: "bar.foreground",
                reason: "invalid #rrggbb hex color",
            })?);
        }
        if let Some(n) = bar.get_int_range(
            "height",
            "bar.height",
            1,
            512,
            "must be between 1 and 512",
        )? {
            self.bar.height = Some(u32::try_from(n).unwrap_or(28));
        }
        if let Some(s) = bar.get_str("font_path") {
            self.bar.font_path = Some(s.to_owned());
        }
        if let Some(n) = bar.get_int_range(
            "font_size",
            "bar.font_size",
            1,
            256,
            "must be between 1 and 256",
        )? {
            self.bar.font_size = Some(u32::try_from(n).unwrap_or(12));
        }
        if let Some(s) = bar.get_str("emoji_path") {
            self.bar.emoji_path = Some(s.to_owned());
        }
        if let Some(s) = bar.get_str("icon_path") {
            self.bar.icon_path = Some(s.to_owned());
        }
        if let Some(n) = bar.get_float_range(
            "opacity",
            "bar.opacity",
            0.0,
            1.0,
            "must be between 0.0 and 1.0",
        )? {
            self.bar.opacity = Some(n);
        }
        Ok(())
    }

    /// The status bar's background color: `[bar].background` if set,
    /// otherwise mirrors `[colors].background` (the terminal's own
    /// background) -- so a bare bar `[bar]` table with no keys, or no
    /// `[bar]` table at all, gives a bar that matches the terminal by
    /// default.
    #[must_use]
    pub fn bar_background(&self) -> Color {
        self.bar.background.unwrap_or(self.palette.background)
    }

    /// The status bar's text color: `[bar].foreground` if set, otherwise
    /// mirrors `[colors].foreground`.
    #[must_use]
    pub fn bar_foreground(&self) -> Color {
        self.bar.foreground.unwrap_or(self.palette.foreground)
    }

    /// The status bar's thickness in logical pixels: `[bar].height` if set,
    /// otherwise `28`.
    #[must_use]
    pub fn bar_height(&self) -> u32 {
        self.bar.height.unwrap_or(28)
    }

    /// The status bar's font file: `[bar].font_path` if set, otherwise
    /// mirrors `[font].path` (which itself may be `None`, meaning the
    /// embedded bitmap font -- same fallback the terminal uses).
    #[must_use]
    pub fn bar_font_path(&self) -> Option<&str> {
        self.bar
            .font_path
            .as_deref()
            .or(self.font.path.as_deref())
    }

    /// The status bar's font point size: `[bar].font_size` if set, otherwise
    /// mirrors `[font].size`.
    #[must_use]
    pub fn bar_font_size(&self) -> u32 {
        self.bar.font_size.unwrap_or(self.font.size)
    }

    /// [`Config::bar_font_size`] converted to a pixel size the same way
    /// [`FontConfig::px_size`] does (96 DPI), for the bar's own font loader.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn bar_font_px_size(&self) -> f32 {
        self.bar_font_size() as f32 * FontConfig::DPI / 72.0
    }

    /// The status bar's emoji fallback face: `[bar].emoji_path` if set,
    /// otherwise mirrors `[font].emoji_path` (which itself may be `None` --
    /// see [`FontConfig::emoji_path`]).
    #[must_use]
    pub fn bar_emoji_path(&self) -> Option<&str> {
        self.bar
            .emoji_path
            .as_deref()
            .or(self.font.emoji_path.as_deref())
    }

    /// The status bar's icon fallback face -- typically a Nerd Font,
    /// consulted for the private-use-area glyphs modules render (see
    /// `BarConfig::icon_path`). `None` if `[bar].icon_path` isn't set; no
    /// terminal-side setting to mirror.
    #[must_use]
    pub fn bar_icon_path(&self) -> Option<&str> {
        self.bar.icon_path.as_deref()
    }

    /// The status bar's background opacity, `0.0`-`1.0`: `[bar].opacity` if
    /// set, otherwise mirrors `[window].opacity`.
    #[must_use]
    pub fn bar_opacity(&self) -> f64 {
        self.bar.opacity.unwrap_or(self.window.opacity)
    }
}

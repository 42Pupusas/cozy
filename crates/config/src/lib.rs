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
//! size = 14                        # cell height in pixels (will be rounded)
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
//! ```

#![allow(clippy::doc_markdown)]

mod color;
mod extract;
mod tests;

pub use color::Color;

use extract as ex;
use shelby::parse_document;

// ── Error ─────────────────────────────────────────────────────────────────────

/// Everything that can go wrong while loading a config file.
#[derive(Debug)]
pub enum ConfigError {
    /// The file could not be read from disk.
    Io(std::io::Error),
    /// The TOML was syntactically or semantically invalid.
    Toml(shelby::Error),
    /// A value was present but had the wrong type or an out-of-range value.
    InvalidValue { field: &'static str, reason: &'static str },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e)  => write!(f, "io error: {e}"),
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
            Self::Io(e)   => Some(e),
            Self::Toml(e) => Some(e),
            Self::InvalidValue { .. } => None,
        }
    }
}

impl From<std::io::Error>  for ConfigError { fn from(e: std::io::Error)  -> Self { Self::Io(e) } }
impl From<shelby::Error>   for ConfigError { fn from(e: shelby::Error)   -> Self { Self::Toml(e) } }

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
    pub foreground:     Color,
    pub background:     Color,
    pub black:          Color,
    pub red:            Color,
    pub green:          Color,
    pub yellow:         Color,
    pub blue:           Color,
    pub magenta:        Color,
    pub cyan:           Color,
    pub white:          Color,
    pub bright_black:   Color,
    pub bright_red:     Color,
    pub bright_green:   Color,
    pub bright_yellow:  Color,
    pub bright_blue:    Color,
    pub bright_magenta: Color,
    pub bright_cyan:    Color,
    pub bright_white:   Color,
}

impl Default for Palette {
    /// Base16 default-dark palette.
    fn default() -> Self {
        Self {
            foreground:     Color::from_rgb(0xd8, 0xd8, 0xd8),
            background:     Color::from_rgb(0x18, 0x18, 0x18),
            black:          Color::from_rgb(0x18, 0x18, 0x18),
            red:            Color::from_rgb(0xab, 0x46, 0x42),
            green:          Color::from_rgb(0xa1, 0xb5, 0x6c),
            yellow:         Color::from_rgb(0xf7, 0xca, 0x88),
            blue:           Color::from_rgb(0x7c, 0xaf, 0xc2),
            magenta:        Color::from_rgb(0xba, 0x8b, 0xaf),
            cyan:           Color::from_rgb(0x86, 0xc1, 0xb9),
            white:          Color::from_rgb(0xd8, 0xd8, 0xd8),
            bright_black:   Color::from_rgb(0x58, 0x58, 0x58),
            bright_red:     Color::from_rgb(0xab, 0x46, 0x42),
            bright_green:   Color::from_rgb(0xa1, 0xb5, 0x6c),
            bright_yellow:  Color::from_rgb(0xf7, 0xca, 0x88),
            bright_blue:    Color::from_rgb(0x7c, 0xaf, 0xc2),
            bright_magenta: Color::from_rgb(0xba, 0x8b, 0xaf),
            bright_cyan:    Color::from_rgb(0x86, 0xc1, 0xb9),
            bright_white:   Color::from_rgb(0xf8, 0xf8, 0xf8),
        }
    }
}

impl Palette {
    /// Return the color for ANSI index 0–15.
    #[must_use]
    pub const fn ansi(&self, idx: u8) -> Color {
        match idx {
            0  => self.black,
            1  => self.red,
            2  => self.green,
            3  => self.yellow,
            4  => self.blue,
            5  => self.magenta,
            6  => self.cyan,
            7  => self.white,
            8  => self.bright_black,
            9  => self.bright_red,
            10 => self.bright_green,
            11 => self.bright_yellow,
            12 => self.bright_blue,
            13 => self.bright_magenta,
            14 => self.bright_cyan,
            15 => self.bright_white,
            _  => self.foreground,
        }
    }
}

/// Font configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontConfig {
    /// Path to the TTF/OTF file.
    pub path: Option<String>,
    /// Cell height in pixels.
    pub size: u32,
}

impl Default for FontConfig {
    fn default() -> Self {
        Self { path: None, size: 14 }
    }
}

/// Cursor configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorConfig {
    pub style:      CursorStyle,
    pub blink:      bool,
    /// Milliseconds per half-blink-period.
    pub blink_rate: u32,
}

impl Default for CursorConfig {
    fn default() -> Self {
        Self { style: CursorStyle::Block, blink: true, blink_rate: 500 }
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
    pub chord:  String,
    pub action: Action,
}

// ── Root config ───────────────────────────────────────────────────────────────

/// The validated, fully-resolved terminal configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Shell binary to spawn (e.g. `/bin/bash`).
    pub shell:      String,
    /// `$TERM` value advertised to the PTY.
    pub term:       String,
    /// Lines of scrollback (`0` = unlimited).
    pub scrollback: usize,
    pub font:       FontConfig,
    pub cursor:     CursorConfig,
    pub palette:    Palette,
    pub keybinds:   Vec<Keybind>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            shell:      default_shell(),
            term:       "xterm-256color".to_owned(),
            scrollback: 10_000,
            font:       FontConfig::default(),
            cursor:     CursorConfig::default(),
            palette:    Palette::default(),
            keybinds:   Vec::new(),
        }
    }
}

fn default_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned())
}

// ── Public API ────────────────────────────────────────────────────────────────

impl Config {
    /// Load config from the standard XDG location:
    /// `$XDG_CONFIG_HOME/terminal/config.toml` or
    /// `~/.config/terminal/config.toml`.
    ///
    /// Returns `Ok(Config::default())` if no file exists yet.
    pub fn load() -> Result<Self, ConfigError> {
        let path = xdg_config_path();
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
        let table = parse_document(bytes)?;
        let mut cfg = Self::default();
        // [general]
        if let Some(general) = ex::table(&table, "general") {
            if let Some(s) = ex::str(general, "shell")  { s.clone_into(&mut cfg.shell); }
            if let Some(s) = ex::str(general, "term")   { s.clone_into(&mut cfg.term); }
            // Scrollback fits comfortably in u32; clamp the validated range to usize.
            if let Some(n) = ex::int_range(general, "scrollback", "general.scrollback", 0, 1_000_000, "must be between 0 and 1000000")? {
                cfg.scrollback = usize::try_from(n).unwrap_or(0);
            }
        }

        // [font]
        if let Some(font) = ex::table(&table, "font") {
            if let Some(s) = ex::str(font, "path") { cfg.font.path = Some(s.to_owned()); }
            if let Some(n) = ex::int_range(font, "size", "font.size", 1, 256, "must be between 1 and 256")? {
                cfg.font.size = u32::try_from(n).unwrap_or(cfg.font.size);
            }
        }

        // [cursor]
        if let Some(cursor) = ex::table(&table, "cursor") {
            if let Some(s) = ex::str(cursor, "style") {
                cfg.cursor.style = match s {
                    "block"     => CursorStyle::Block,
                    "beam"      => CursorStyle::Beam,
                    "underline" => CursorStyle::Underline,
                    _ => return Err(ConfigError::InvalidValue {
                        field: "cursor.style",
                        reason: "expected block | beam | underline",
                    }),
                };
            }
            if let Some(b) = ex::bool(cursor, "blink") { cfg.cursor.blink = b; }
            if let Some(n) = ex::int_range(cursor, "blink_rate", "cursor.blink_rate", 0, 60_000, "must be between 0 and 60000")? {
                cfg.cursor.blink_rate = u32::try_from(n).unwrap_or(cfg.cursor.blink_rate);
            }
        }

        // [colors]
        if let Some(colors) = ex::table(&table, "colors") {
            macro_rules! color_field {
                ($field:ident, $key:literal) => {
                    if let Some(s) = ex::str(colors, $key) {
                        cfg.palette.$field = Color::parse_hex(s).ok_or(
                            ConfigError::InvalidValue { field: $key, reason: "invalid #rrggbb hex color" }
                        )?;
                    }
                };
            }
            color_field!(foreground,     "foreground");
            color_field!(background,     "background");
            color_field!(black,          "black");
            color_field!(red,            "red");
            color_field!(green,          "green");
            color_field!(yellow,         "yellow");
            color_field!(blue,           "blue");
            color_field!(magenta,        "magenta");
            color_field!(cyan,           "cyan");
            color_field!(white,          "white");
            color_field!(bright_black,   "bright_black");
            color_field!(bright_red,     "bright_red");
            color_field!(bright_green,   "bright_green");
            color_field!(bright_yellow,  "bright_yellow");
            color_field!(bright_blue,    "bright_blue");
            color_field!(bright_magenta, "bright_magenta");
            color_field!(bright_cyan,    "bright_cyan");
            color_field!(bright_white,   "bright_white");
        }

        // [keybinds]
        if let Some(keybinds) = ex::table(&table, "keybinds") {
            for (chord, val) in keybinds.iter() {
                if let shelby::Value::String(action_str) = val {
                    let action = match action_str.as_ref() {
                        "copy"        => Action::Copy,
                        "paste"       => Action::Paste,
                        "scroll_up"   => Action::ScrollUp,
                        "scroll_down" => Action::ScrollDown,
                        "new_tab"     => Action::NewTab,
                        "close_tab"   => Action::CloseTab,
                        other         => Action::Send(other.to_owned()),
                    };
                    cfg.keybinds.push(Keybind { chord: chord.to_owned(), action });
                }
            }
        }

        Ok(cfg)
    }
}

fn xdg_config_path() -> std::path::PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME").map_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_owned());
            std::path::PathBuf::from(home).join(".config")
        }, std::path::PathBuf::from);
    base.join("terminal").join("config.toml")
}

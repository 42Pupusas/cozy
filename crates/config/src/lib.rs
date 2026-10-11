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
//! position    = "top"             # "top" or "bottom" screen edge
//! font_path   = "/usr/share/fonts/TTF/JetBrainsMono-Regular.ttf" # defaults to [font].path
//! font_size   = 12                 # points; defaults to [font].size
//! emoji_path  = "/path/to/emoji-font.ttf" # defaults to [font].emoji_path
//! icon_path   = "/path/to/NerdFont.ttf"    # optional: fallback face for
//!                                           # icon glyphs (network/battery/
//!                                           # volume symbols etc, matching
//!                                           # waybar's icon set). unset = no
//!                                           # icon fallback, no icons drawn.
//! opacity     = 1.0                # defaults to [window].opacity
//!
//! # Per-module click commands. Wi-Fi, Bluetooth and both volume modules
//! # open a built-in panel when unset; a command set here replaces it.
//! # `{x}` expands to the clicked module's left edge and `{x1}` to its
//! # right edge (bar-relative logical px).
//! [bar.click]
//! battery       = "powerprofiles --at {x}"
//!
//! # Zero or more app windows to watch for niri's "urgent" (attention-
//! # demanding) state -- e.g. an Electron chat app flashing its taskbar
//! # icon on an unread mention. Each entry becomes its own bar segment:
//! # `icon` alone when that app has a window open with nothing new,
//! # `icon` plus a red dot when niri reports one of its windows urgent.
//! # An app with no window open at all renders nothing (segment hidden).
//! [[bar.notifications]]
//! app_id = "Mattermost"            # matched case-insensitively as a substring
//!                                   # of niri's window app_id (so "mattermost"
//!                                   # matches both "Mattermost" and the
//!                                   # Flatpak's "mattermost-desktop")
//! icon   = "nf-md-message-text"    # any single glyph, e.g. from a Nerd Font
//!
//! [[bar.notifications]]
//! app_id = "Slack"
//! icon   = "nf-md-slack"
//!
//! # Everything here is optional and defaults to *mirroring* the terminal's
//! # own [font]/[colors] (and, for icon_path, the bar's) -- same shape and
//! # mirroring rule as [bar] above, with one deliberate difference: opacity
//! # defaults to a darker 0.85 rather than mirroring [window].opacity, since
//! # the launcher is a floating overlay meant to stand out, not blend into
//! # an edge like the bar does.
//! [launcher]
//! background  = "#181818"          # defaults to [colors].background
//! foreground  = "#d8d8d8"          # defaults to [colors].foreground
//! helper      = "#727169"          # secondary launcher hints/placeholder
//! font_path   = "/usr/share/fonts/TTF/JetBrainsMono-Regular.ttf" # defaults to [font].path
//! font_size   = 12                 # points; defaults to [font].size
//! emoji_path  = "/path/to/emoji-font.ttf" # defaults to [font].emoji_path
//! icon_path   = "/path/to/NerdFont.ttf"    # defaults to [bar].icon_path
//! opacity     = 0.85               # defaults to 0.85 (darker than the bar's default)
//! highlight   = "#313244"          # selected-result row background; defaults to
//!                                   # a built-in dark slate (no [colors] equivalent)
//!
//! # The file browser. Everything is optional; unset colour keys mirror the
//! # terminal's own [colors], following `ls` convention where one applies
//! # (directories blue, symlinks cyan, broken links red). The roles a
//! # terminal has no equivalent for -- chrome fills, the scrollbar, the
//! # selected row -- are *derived* from the palette rather than hardcoded,
//! # so a re-themed terminal carries the browser with it instead of
//! # stranding grey chrome on a light background.
//! [files]
//! background      = "#1e1e2e"     # defaults to [colors].background
//! text            = "#cdd6f4"     # defaults to [colors].foreground
//! dir             = "#89b4fa"     # defaults to [colors].blue
//! symlink         = "#94e2d5"     # defaults to [colors].cyan
//! broken          = "#f38ba8"     # broken symlink; defaults to [colors].red
//! error           = "#f38ba8"     # defaults to [colors].red
//! muted           = "#6c7086"     # sizes/counts/status; defaults to [colors].bright_black
//! caret           = "#f9e2af"     # filter-box caret; defaults to [colors].yellow
//! cursor          = "#89b4fa"     # cursor-row outline; defaults to [colors].blue
//! selection       = "#45475a"     # selected-row fill; defaults to a derived shade
//! header_bg       = "#181825"     # defaults to a derived shade
//! status_bg       = "#181825"     # defaults to header_bg
//! scrollbar_track = "#181825"     # defaults to header_bg
//! scrollbar_thumb = "#585b70"     # defaults to a derived shade
//! filter_bg       = "#313244"     # focused filter box; defaults to a derived shade
//! tile_color_1    = "#222a3a"     # the four tile fills; no two bordering tiles share one.
//! tile_color_2    = "#232d29"     # Each defaults to the chrome tinted toward
//! tile_color_3    = "#2d2531"     # [colors] blue, green, magenta and yellow
//! tile_color_4    = "#302d25"     # respectively
//! row_height      = 20            # pixels, 4-512
//! header_height   = 24            # pixels, 0-512 (0 hides the strip)
//! status_height   = 20            # pixels, 0-512 (0 hides the strip)
//! padding         = 8             # left/right padding inside a row, 0-512
//!
//! # Everything here is optional and defaults to *mirroring* the bar's own
//! # resolved background/opacity (which itself mirrors the terminal's), or
//! # a built-in Kanagawa-ish dark palette for the roles ([colors] has no
//! # equivalent of).
//! [monitor]
//! background         = "#0a0a0b"   # window background; defaults to bar_background()
//! background_opacity = 1.0         # defaults to bar_opacity()
//! surface            = "#181616"   # card/chart-panel fill; defaults to bar_background()
//! surface_opacity    = 1.0         # currently unused by the renderer; reserved
//! foreground         = "#f2f2f3"   # hero/metric text
//! dim                = "#8a8a90"   # secondary text (titles, units, process rows)
//! faint              = "#55555a"  # tertiary text (detail labels, table headers)
//! border             = "#1e1e21"  # card outline / bar track
//! cpu                = "#8a9a7b"   # CPU card accent
//! memory             = "#8ea4a2"   # memory card accent
//! temperature        = "#b6927b"   # temperature card accent
//! network            = "#8ea4a2"   # network card accent
//! font_path          = "/path/to/font.ttf"  # defaults to [font].path
//! font_size          = 14          # points; defaults to [font].size
//! hero_size          = 2.5         # hero text size, as a multiple of font_size
//! title_size         = 1.0         # card title size, as a multiple of font_size
//! subtitle_size      = 0.90        # unit/metadata size, as a multiple of font_size
//! normal_size        = 0.80        # detail-row/process-table size, as a multiple of font_size
//!
//! # The statusbar popup panels (wifi / bluetooth / volume). Unset colors
//! # mirror the bar's resolved palette (which itself mirrors [colors]), so
//! # a panel reads as part of the desktop, not a foreign window.
//! [panel]
//! background   = "#181616"  # defaults to bar_background()
//! foreground   = "#c5c9c5"  # defaults to bar_foreground()
//! dim          = "#a6a69c"  # secondary text; defaults to [colors].bright_black
//! accent       = "#8ba4b0"  # toggles/slider fill/icon ink; defaults to [colors].blue
//! highlight    = "#2a2a2a"  # selected-row fill; defaults to a derived shade
//! border       = "#45475a"  # slider groove / idle toggle track; defaults to a derived shade
//! opacity      = 0.80       # defaults to bar_opacity()
//! gap          = 8          # px, floating gap to the bar and screen edges, 0-128
//! side         = "auto"     # "left" | "right" | "auto"; auto flips to whichever
//!                           # edge leaves the panel fully on-screen
//! ```

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
    /// glyphs are supported, `OpenMoji`, …) to get emoji rendering.
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
    /// `size` is validated to `1..=256` points, which fits in `u16`; going
    /// through `f32::from(u16)` (a lossless widening conversion, unlike a
    /// bare `as f32`) lets clippy prove there's no precision loss instead of
    /// needing an allow.
    #[must_use]
    pub fn px_size(&self) -> f32 {
        let size = u16::try_from(self.size).unwrap_or(u16::MAX);
        f32::from(size) * Self::DPI / 72.0
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

/// One `[[bar.notifications]]` entry: an app to watch for niri's
/// window-urgency signal, and the glyph its bar segment should render.
///
/// This is deliberately compositor-agnostic in shape (just "an `app_id`
/// and an icon") even though today's only consumer
/// (`statusbar::notifications`) happens to source urgency from niri
/// specifically -- the config format itself doesn't need to know that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationRule {
    /// Matched case-insensitively as a substring of the window's `app_id`
    /// (not an exact match -- Flatpak/native builds of the same app often
    /// differ in casing/suffix, e.g. "Mattermost" vs "mattermost-desktop").
    pub app_id: String,
    /// The glyph to render for this app's segment.
    pub icon: String,
}

/// Status-bar configuration.
///
/// Every field is optional in the TOML `[bar]` table and, when unset,
/// *mirrors* the corresponding terminal setting ([`Config::font`] /
/// [`Config::palette`]) -- see [`Config::bar_background`] etc., which
/// resolve the mirroring. This struct only holds the raw overrides actually
/// present in the file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MonitorConfig {
    pub background: Option<Color>,
    pub background_opacity: Option<f64>,
    pub surface: Option<Color>,
    pub surface_opacity: Option<f64>,
    pub font_path: Option<String>,
    pub font_size: Option<u32>,
    pub hero_size: Option<f32>,
    pub title_size: Option<f32>,
    pub subtitle_size: Option<f32>,
    pub normal_size: Option<f32>,
    /// Hero/metric text color override. `None` uses a built-in near-white
    /// -- there's no single terminal-side setting to mirror (unlike
    /// `background`/`surface`, which mirror the bar's own resolved
    /// colors), since the dashboard's typography tiers (foreground/dim/
    /// faint) have no equivalent concept in `[colors]`.
    pub foreground: Option<Color>,
    /// Secondary label/value text color override (card titles, hero units,
    /// process names/PIDs). `None` uses a built-in mid-grey.
    pub dim: Option<Color>,
    /// Tertiary label color override (detail-row labels, table headers).
    /// `None` uses a built-in dark grey.
    pub faint: Option<Color>,
    /// Card outline / bar-track color override. `None` uses a built-in
    /// near-black grey.
    pub border: Option<Color>,
    pub cpu: Option<Color>,
    pub memory: Option<Color>,
    pub temperature: Option<Color>,
    pub network: Option<Color>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BarClickConfig {
    pub wifi: Option<String>,
    pub bluetooth: Option<String>,
    pub battery: Option<String>,
    pub brightness: Option<String>,
    pub volume_output: Option<String>,
    pub volume_input: Option<String>,
}

/// Which screen edge a panel floats toward, from `[panel].side`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PanelSide {
    Left,
    Right,
    /// Open on whichever side leaves the panel fully on-screen.
    #[default]
    Auto,
}

/// Which screen edge the status bar docks to, from `[bar].position`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BarPosition {
    #[default]
    Top,
    Bottom,
}

/// `[panel]` -- the statusbar popup panels' theme and placement.
///
/// Every field is `Option`; unset keys mirror the bar's resolved palette
/// (same rule as `[monitor]`), so the panels track a re-themed desktop
/// instead of carrying a stale palette of their own.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PanelConfig {
    pub background: Option<Color>,
    pub foreground: Option<Color>,
    pub dim: Option<Color>,
    pub accent: Option<Color>,
    pub highlight: Option<Color>,
    pub border: Option<Color>,
    /// `0.0`-`1.0`; `None` mirrors [`Config::bar_opacity`].
    pub opacity: Option<f64>,
    /// Gap in logical px between the panel and the bar / screen edges.
    /// `None` defaults to 8.
    pub gap: Option<u32>,
    pub side: Option<PanelSide>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct BarConfig {
    /// Background color override. `None` mirrors `[colors].background`.
    pub background: Option<Color>,
    /// Foreground (text) color override. `None` mirrors `[colors].foreground`.
    pub foreground: Option<Color>,
    /// Bar thickness in logical pixels. `None` defaults to 28.
    pub height: Option<u32>,
    /// Screen edge the bar docks to. `None` defaults to the top.
    pub position: Option<BarPosition>,
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
    /// `[[bar.notifications]]` entries -- apps to watch for a
    /// compositor-reported urgent/attention-demanding window. Empty by
    /// default: no notification segments render unless the user opts an
    /// app in. See [`NotificationRule`].
    pub notifications: Vec<NotificationRule>,
    /// `[bar.click]` command overrides. Each command can contain `{x}`
    /// (the clicked module's left edge) and `{x1}` (its right edge).
    pub click: BarClickConfig,
}

/// App-launcher configuration.
///
/// Same shape as [`BarConfig`] (and same mirroring rule: unset keys mirror
/// the corresponding terminal setting) with one deliberate difference --
/// [`Config::launcher_opacity`]'s *unset* default is darker than the bar's,
/// since a launcher is a floating overlay meant to stand out over whatever
/// was on screen when it was summoned, not blend into an edge like the bar
/// does.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LauncherConfig {
    /// Background color override. `None` mirrors `[colors].background`.
    pub background: Option<Color>,
    /// Foreground (text) color override. `None` mirrors `[colors].foreground`.
    pub foreground: Option<Color>,
    /// Secondary helper text color (shortcut hints and search placeholder).
    /// `None` uses a built-in muted color.
    pub helper: Option<Color>,
    /// Font file override. `None` mirrors `[font].path`.
    pub font_path: Option<String>,
    /// Font point size override. `None` mirrors `[font].size`.
    pub font_size: Option<u32>,
    /// Emoji fallback face override. `None` mirrors `[font].emoji_path`.
    pub emoji_path: Option<String>,
    /// Icon fallback face override. `None` mirrors `[bar].icon_path` (which
    /// itself may be unset).
    pub icon_path: Option<String>,
    /// Background opacity override, `0.0`-`1.0`. `None` uses a built-in
    /// `0.85` -- darker (more transparent) than the bar's mirrored-opaque
    /// default, per the launcher's own overlay styling; see the struct doc.
    pub opacity: Option<f64>,
    /// Selected-result highlight background color override. `None` uses a
    /// built-in dark slate (`#313244`) -- unlike every other field here,
    /// this one has no `[colors]`/`[font]` equivalent to mirror, since the
    /// terminal itself has no concept of a "selected row".
    pub highlight: Option<Color>,
}

/// File-browser configuration.
///
/// Same mirroring rule as [`BarConfig`]/[`LauncherConfig`]: unset keys take
/// their value from the terminal's own `[colors]`, so a user who themes the
/// terminal gets a matching browser for free.
///
/// The browser needs more *roles* than the terminal has settings, though. A
/// terminal has a foreground and a background; a file list additionally has
/// a selected row, a focused row, a header strip, a status strip, and a
/// scrollbar. Where a role has an honest ANSI equivalent it mirrors it --
/// directories are `blue`, symlinks `cyan`, broken links and errors `red` --
/// because those are the colours `ls` has used for the same things for
/// decades and the association is worth inheriting. Where a role has no
/// terminal equivalent at all (chrome fills, the scrollbar) it falls back to
/// a built-in derived from the palette rather than a literal, so the browser
/// still tracks a re-themed terminal instead of stranding grey chrome on a
/// light background.
// `Eq` is deliberately absent: `opacity` is an `f64`, and the other config
// structs carrying a float (`BarConfig`, `LauncherConfig`) drop it for the
// same reason. `PartialEq` is all any caller needs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FilesConfig {
    /// Background color override. `None` mirrors `[colors].background`.
    pub background: Option<Color>,
    /// Row text color override. `None` mirrors `[colors].foreground`.
    pub text: Option<Color>,
    /// Directory name color. `None` mirrors `[colors].blue`.
    pub dir: Option<Color>,
    /// Symlink name color. `None` mirrors `[colors].cyan`.
    pub symlink: Option<Color>,
    /// Broken-symlink color. `None` mirrors `[colors].red`.
    pub broken: Option<Color>,
    /// Error text color. `None` mirrors `[colors].red`.
    pub error: Option<Color>,
    /// Dimmed text (sizes, counts, status line). `None` mirrors
    /// `[colors].bright_black`.
    pub muted: Option<Color>,
    /// Filter-box caret color. `None` mirrors `[colors].yellow`.
    pub caret: Option<Color>,
    /// Selected-row fill. `None` uses a built-in derived from the palette.
    pub selection: Option<Color>,
    /// Cursor-row outline, drawn even when the row is not selected so
    /// keyboard focus stays locatable. `None` mirrors `[colors].blue`.
    pub cursor: Option<Color>,
    /// Header strip fill. `None` uses a built-in derived from the palette.
    pub header_bg: Option<Color>,
    /// Status strip fill. `None` uses a built-in derived from the palette.
    pub status_bg: Option<Color>,
    /// Scrollbar trough. `None` uses a built-in derived from the palette.
    pub scrollbar_track: Option<Color>,
    /// Scrollbar thumb. `None` uses a built-in derived from the palette.
    pub scrollbar_thumb: Option<Color>,
    /// Focused filter-box fill. `None` uses a built-in derived from the
    /// palette.
    pub filter_bg: Option<Color>,
    /// The four tile fills. `None` tints the chrome toward
    /// `[colors].blue`, `green`, `magenta` and `yellow`.
    pub tile_colors: [Option<Color>; Self::TILE_COLORS],
    /// Row height in pixels. `None` uses a built-in `20`.
    pub row_height: Option<u32>,
    /// Header strip height in pixels. `None` uses a built-in `24`.
    pub header_height: Option<u32>,
    /// Status strip height in pixels. `None` uses a built-in `20`.
    pub status_height: Option<u32>,
    /// Left/right padding inside a row, in pixels. `None` uses a built-in `8`.
    pub padding: Option<u32>,
    /// Background opacity, `0.0`-`1.0`. `None` mirrors `[window].opacity`.
    ///
    /// Mirrored rather than given its own default because a desktop where one
    /// window is opaque among translucent siblings reads as a rendering fault,
    /// not as a design choice -- the same reasoning as [`BarConfig::opacity`].
    pub opacity: Option<f64>,
}

impl FilesConfig {
    /// How many fills the tiles cycle through: the four-colour theorem's
    /// bound for a planar tiling.
    pub const TILE_COLORS: usize = 4;

    /// How far a default tile fill leans from the chrome toward its hue.
    const TILE_TINT: f32 = 0.18;
}

/// The infinite-canvas diagram tool's overrides.
///
/// Same mirror-the-palette shape as [`FilesConfig`]: every colour key is
/// `Option` and resolves against `[colors]` when unset, so a re-themed
/// terminal re-themes the canvas with it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CanvasConfig {
    /// Background color override. `None` mirrors `[colors].background`.
    pub background: Option<Color>,
    /// Grid-dot color. `None` derives from the palette.
    pub grid: Option<Color>,
    /// Text and shape-stroke color. `None` mirrors `[colors].foreground`.
    pub text: Option<Color>,
    /// Note-card fill. `None` derives from the palette.
    pub surface: Option<Color>,
    /// Note-card border. `None` derives from the palette.
    pub border: Option<Color>,
    /// Selected-shape accent. `None` mirrors `[colors].blue`.
    pub selection: Option<Color>,
    /// Text caret. `None` mirrors `[colors].yellow`.
    pub caret: Option<Color>,
    /// Shape stroke width in pixels. `None` uses a built-in `2`.
    pub stroke_width: Option<u32>,
    /// Snap-to-grid size in pixels (0 disables snapping). `None` uses a
    /// built-in `8`.
    pub grid_size: Option<u32>,
    /// Background opacity, `0.0`-`1.0`. `None` mirrors `[window].opacity`.
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
    pub monitor: MonitorConfig,
    pub panel: PanelConfig,
    pub launcher: LauncherConfig,
    pub files: FilesConfig,
    pub canvas: CanvasConfig,
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
            launcher: LauncherConfig::default(),
            monitor: MonitorConfig::default(),
            panel: PanelConfig::default(),
            files: FilesConfig::default(),
            canvas: CanvasConfig::default(),
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
    #[must_use]
    pub fn xdg_config_path() -> std::path::PathBuf {
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
        cfg.apply_monitor(&table)?;
        cfg.apply_panel(&table)?;
        cfg.apply_launcher(&table)?;
        cfg.apply_files(&table)?;
        cfg.apply_canvas(&table)?;
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

    /// Apply the `[monitor]` table, if present. It uses the same Shelby TOML
    /// loader as the terminal and keeps dashboard styling independent.
    fn apply_monitor(&mut self, table: &TomlTable<'_>) -> Result<(), ConfigError> {
        let Some(monitor) = table.get_table("monitor") else {
            return Ok(());
        };
        macro_rules! color {
            ($field:ident, $key:literal) => {
                if let Some(value) = monitor.get_str($key) {
                    self.monitor.$field =
                        Some(Color::parse_hex(value).ok_or(ConfigError::InvalidValue {
                            field: concat!("monitor.", $key),
                            reason: "invalid #rrggbb hex color",
                        })?);
                }
            };
        }
        color!(background, "background");
        color!(surface, "surface");
        color!(cpu, "cpu");
        color!(foreground, "foreground");
        color!(dim, "dim");
        color!(faint, "faint");
        color!(border, "border");
        if let Some(value) = monitor.get_float_range(
            "background_opacity",
            "monitor.background_opacity",
            0.0,
            1.0,
            "must be between 0.0 and 1.0",
        )? {
            self.monitor.background_opacity = Some(value);
        }
        if let Some(value) = monitor.get_float_range(
            "surface_opacity",
            "monitor.surface_opacity",
            0.0,
            1.0,
            "must be between 0.0 and 1.0",
        )? {
            self.monitor.surface_opacity = Some(value);
        }
        color!(memory, "memory");
        color!(temperature, "temperature");
        color!(network, "network");
        if let Some(path) = monitor.get_str("font_path") {
            self.monitor.font_path = Some(path.to_owned());
        }
        if let Some(size) = monitor.get_int_range(
            "font_size",
            "monitor.font_size",
            1,
            256,
            "must be between 1 and 256",
        )? {
            self.monitor.font_size = Some(u32::try_from(size).unwrap_or(14));
        }
        for (key, slot) in [
            ("hero_size", &mut self.monitor.hero_size),
            ("title_size", &mut self.monitor.title_size),
            ("subtitle_size", &mut self.monitor.subtitle_size),
            ("normal_size", &mut self.monitor.normal_size),
        ] {
            if let Some(size) = monitor.get_float_range(
                key,
                "monitor.typography",
                0.1,
                10.0,
                "must be between 0.1 and 10.0",
            )? {
                // The validated 0.1..=10.0 range is exactly representable
                // enough for the renderer's f32 font-scale API; precision
                // beyond that API is intentionally discarded here.
                #[allow(clippy::cast_possible_truncation)]
                {
                    *slot = Some(size as f32);
                }
            }
        }
        Ok(())
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
        if let Some(n) =
            bar.get_int_range("height", "bar.height", 1, 512, "must be between 1 and 512")?
        {
            self.bar.height = Some(u32::try_from(n).unwrap_or(28));
        }
        if let Some(position) = bar.get_str("position") {
            self.bar.position = Some(match position {
                "top" => BarPosition::Top,
                "bottom" => BarPosition::Bottom,
                _ => {
                    return Err(ConfigError::InvalidValue {
                        field: "bar.position",
                        reason: "must be one of \"top\", \"bottom\"",
                    });
                }
            });
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
        self.apply_bar_notifications(bar)?;
        self.apply_bar_clicks(bar);
        Ok(())
    }

    fn apply_bar_clicks(&mut self, bar: &TomlTable<'_>) {
        let Some(click) = bar.get_table("click") else {
            return;
        };
        self.bar.click.wifi = click.get_str("wifi").map(str::to_owned);
        self.bar.click.bluetooth = click.get_str("bluetooth").map(str::to_owned);
        self.bar.click.battery = click.get_str("battery").map(str::to_owned);
        self.bar.click.brightness = click.get_str("brightness").map(str::to_owned);
        self.bar.click.volume_output = click.get_str("volume_output").map(str::to_owned);
        self.bar.click.volume_input = click.get_str("volume_input").map(str::to_owned);
    }

    /// Apply `[[bar.notifications]]`, if present. Split out of
    /// [`Self::apply_bar`] for the same line-count reason every other
    /// `apply_*` helper is split out.
    fn apply_bar_notifications(&mut self, bar: &TomlTable<'_>) -> Result<(), ConfigError> {
        let Some(entries) = bar.get_array_of_tables("notifications") else {
            return Ok(());
        };
        for entry in entries {
            let app_id = entry
                .get_str("app_id")
                .ok_or(ConfigError::InvalidValue {
                    field: "bar.notifications.app_id",
                    reason: "required, must be a string",
                })?
                .to_owned();
            let icon = entry
                .get_str("icon")
                .ok_or(ConfigError::InvalidValue {
                    field: "bar.notifications.icon",
                    reason: "required, must be a string",
                })?
                .to_owned();
            self.bar
                .notifications
                .push(NotificationRule { app_id, icon });
        }
        Ok(())
    }

    /// Apply the `[panel]` table, if present -- the popup panels' theme
    /// and placement. Same Shelby extraction shape as `apply_monitor`.
    fn apply_panel(&mut self, table: &TomlTable<'_>) -> Result<(), ConfigError> {
        let Some(panel) = table.get_table("panel") else {
            return Ok(());
        };
        if let Some(color) = panel.get_color("background", "panel.background")? {
            self.panel.background = Some(color);
        }
        if let Some(color) = panel.get_color("foreground", "panel.foreground")? {
            self.panel.foreground = Some(color);
        }
        if let Some(color) = panel.get_color("dim", "panel.dim")? {
            self.panel.dim = Some(color);
        }
        if let Some(color) = panel.get_color("accent", "panel.accent")? {
            self.panel.accent = Some(color);
        }
        if let Some(color) = panel.get_color("highlight", "panel.highlight")? {
            self.panel.highlight = Some(color);
        }
        if let Some(color) = panel.get_color("border", "panel.border")? {
            self.panel.border = Some(color);
        }
        if let Some(value) = panel.get_float_range(
            "opacity",
            "panel.opacity",
            0.0,
            1.0,
            "must be between 0.0 and 1.0",
        )? {
            self.panel.opacity = Some(value);
        }
        if let Some(value) =
            panel.get_int_range("gap", "panel.gap", 0, 128, "must be between 0 and 128")?
        {
            self.panel.gap = Some(u32::try_from(value).unwrap_or(8));
        }
        if let Some(side) = panel.get_str("side") {
            self.panel.side = Some(match side {
                "left" => PanelSide::Left,
                "right" => PanelSide::Right,
                "auto" => PanelSide::Auto,
                _ => {
                    return Err(ConfigError::InvalidValue {
                        field: "panel.side",
                        reason: "must be one of \"left\", \"right\", \"auto\"",
                    });
                }
            });
        }
        Ok(())
    }

    /// Apply the `[launcher]` table, if present. Same override/mirror shape
    /// as [`Self::apply_bar`]; see [`Config::launcher_background`] /
    /// [`Config::launcher_foreground`] / [`Config::launcher_font_path`] /
    /// [`Config::launcher_font_size`] / [`Config::launcher_opacity`] for the
    /// resolved values consumers should actually read.
    fn apply_launcher(&mut self, table: &TomlTable<'_>) -> Result<(), ConfigError> {
        let Some(launcher) = table.get_table("launcher") else {
            return Ok(());
        };
        if let Some(s) = launcher.get_str("background") {
            self.launcher.background =
                Some(Color::parse_hex(s).ok_or(ConfigError::InvalidValue {
                    field: "launcher.background",
                    reason: "invalid #rrggbb hex color",
                })?);
        }
        if let Some(s) = launcher.get_str("foreground") {
            self.launcher.foreground =
                Some(Color::parse_hex(s).ok_or(ConfigError::InvalidValue {
                    field: "launcher.foreground",
                    reason: "invalid #rrggbb hex color",
                })?);
        }
        if let Some(s) = launcher.get_str("helper") {
            self.launcher.helper = Some(Color::parse_hex(s).ok_or(ConfigError::InvalidValue {
                field: "launcher.helper",
                reason: "invalid #rrggbb hex color",
            })?);
        }
        if let Some(s) = launcher.get_str("font_path") {
            self.launcher.font_path = Some(s.to_owned());
        }
        if let Some(n) = launcher.get_int_range(
            "font_size",
            "launcher.font_size",
            1,
            256,
            "must be between 1 and 256",
        )? {
            self.launcher.font_size = Some(u32::try_from(n).unwrap_or(12));
        }
        if let Some(s) = launcher.get_str("emoji_path") {
            self.launcher.emoji_path = Some(s.to_owned());
        }
        if let Some(s) = launcher.get_str("icon_path") {
            self.launcher.icon_path = Some(s.to_owned());
        }
        if let Some(n) = launcher.get_float_range(
            "opacity",
            "launcher.opacity",
            0.0,
            1.0,
            "must be between 0.0 and 1.0",
        )? {
            self.launcher.opacity = Some(n);
        }
        if let Some(s) = launcher.get_str("highlight") {
            self.launcher.highlight =
                Some(Color::parse_hex(s).ok_or(ConfigError::InvalidValue {
                    field: "launcher.highlight",
                    reason: "invalid #rrggbb hex color",
                })?);
        }
        Ok(())
    }

    /// Apply the `[files]` table, if present.
    ///
    /// Flatter than its siblings because every key is either a colour or a
    /// pixel measurement -- see [`FilesConfig`] for what each one means and
    /// [`Config::files_theme_colors`] for how unset keys resolve.
    fn apply_files(&mut self, table: &TomlTable<'_>) -> Result<(), ConfigError> {
        let Some(files) = table.get_table("files") else {
            return Ok(());
        };

        // Paired with the field it fills so a mistyped error label is
        // visible on one line rather than six lines apart.
        let [tile_1, tile_2, tile_3, tile_4] = &mut self.files.tile_colors;
        let colors: [(&str, &'static str, &mut Option<Color>); 19] = [
            ("background", "files.background", &mut self.files.background),
            ("text", "files.text", &mut self.files.text),
            ("dir", "files.dir", &mut self.files.dir),
            ("symlink", "files.symlink", &mut self.files.symlink),
            ("broken", "files.broken", &mut self.files.broken),
            ("error", "files.error", &mut self.files.error),
            ("muted", "files.muted", &mut self.files.muted),
            ("caret", "files.caret", &mut self.files.caret),
            ("selection", "files.selection", &mut self.files.selection),
            ("cursor", "files.cursor", &mut self.files.cursor),
            ("header_bg", "files.header_bg", &mut self.files.header_bg),
            ("status_bg", "files.status_bg", &mut self.files.status_bg),
            (
                "scrollbar_track",
                "files.scrollbar_track",
                &mut self.files.scrollbar_track,
            ),
            (
                "scrollbar_thumb",
                "files.scrollbar_thumb",
                &mut self.files.scrollbar_thumb,
            ),
            ("filter_bg", "files.filter_bg", &mut self.files.filter_bg),
            ("tile_color_1", "files.tile_color_1", tile_1),
            ("tile_color_2", "files.tile_color_2", tile_2),
            ("tile_color_3", "files.tile_color_3", tile_3),
            ("tile_color_4", "files.tile_color_4", tile_4),
        ];
        for (key, field, slot) in colors {
            if let Some(c) = files.get_color(key, field)? {
                *slot = Some(c);
            }
        }

        // Upper bounds are deliberately loose rather than tasteful: a 400px
        // row is silly but harmless and someone on a HiDPI panel may want
        // something this side of silly. What they stop is the degenerate
        // end -- a zero row height would divide by zero when computing how
        // many rows fit, and a negative one cannot be represented at all.
        let metrics: [(
            &'static str,
            &'static str,
            i64,
            &'static str,
            &mut Option<u32>,
        ); 4] = [
            (
                "row_height",
                "files.row_height",
                4,
                "must be between 4 and 512",
                &mut self.files.row_height,
            ),
            (
                "header_height",
                "files.header_height",
                0,
                "must be between 0 and 512",
                &mut self.files.header_height,
            ),
            (
                "status_height",
                "files.status_height",
                0,
                "must be between 0 and 512",
                &mut self.files.status_height,
            ),
            (
                "padding",
                "files.padding",
                0,
                "must be between 0 and 512",
                &mut self.files.padding,
            ),
        ];
        for (key, field, lo, reason, slot) in metrics {
            if let Some(n) = files.get_int_range(key, field, lo, 512, reason)? {
                *slot = Some(u32::try_from(n).unwrap_or(0));
            }
        }

        if let Some(n) = files.get_float_range(
            "opacity",
            "files.opacity",
            0.0,
            1.0,
            "must be between 0.0 and 1.0",
        )? {
            self.files.opacity = Some(n);
        }

        Ok(())
    }

    /// Apply the `[canvas]` table, if present. Same shape as
    /// [`Self::apply_files`]: colour keys mirror `[colors]` when unset,
    /// metric keys carry built-in defaults.
    fn apply_canvas(&mut self, table: &TomlTable<'_>) -> Result<(), ConfigError> {
        let Some(canvas) = table.get_table("canvas") else {
            return Ok(());
        };
        let colors: [(&str, &'static str, &mut Option<Color>); 7] = [
            (
                "background",
                "canvas.background",
                &mut self.canvas.background,
            ),
            ("grid", "canvas.grid", &mut self.canvas.grid),
            ("text", "canvas.text", &mut self.canvas.text),
            ("surface", "canvas.surface", &mut self.canvas.surface),
            ("border", "canvas.border", &mut self.canvas.border),
            ("selection", "canvas.selection", &mut self.canvas.selection),
            ("caret", "canvas.caret", &mut self.canvas.caret),
        ];
        for (key, field, slot) in colors {
            if let Some(c) = canvas.get_color(key, field)? {
                *slot = Some(c);
            }
        }
        if let Some(n) = canvas.get_int_range(
            "stroke_width",
            "canvas.stroke_width",
            1,
            16,
            "must be between 1 and 16",
        )? {
            self.canvas.stroke_width = Some(u32::try_from(n).unwrap_or(2));
        }
        if let Some(n) = canvas.get_int_range(
            "grid_size",
            "canvas.grid_size",
            0,
            256,
            "must be between 0 and 256",
        )? {
            self.canvas.grid_size = Some(u32::try_from(n).unwrap_or(8));
        }
        if let Some(n) = canvas.get_float_range(
            "opacity",
            "canvas.opacity",
            0.0,
            1.0,
            "must be between 0.0 and 1.0",
        )? {
            self.canvas.opacity = Some(n);
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

    /// The screen edge the status bar docks to: `[bar].position` if set,
    /// otherwise the top.
    #[must_use]
    pub const fn bar_position(&self) -> BarPosition {
        match self.bar.position {
            Some(position) => position,
            None => BarPosition::Top,
        }
    }

    /// The status bar's font file: `[bar].font_path` if set, otherwise
    /// mirrors `[font].path` (which itself may be `None`, meaning the
    /// embedded bitmap font -- same fallback the terminal uses).
    #[must_use]
    pub fn bar_font_path(&self) -> Option<&str> {
        self.bar.font_path.as_deref().or(self.font.path.as_deref())
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
    pub fn bar_font_px_size(&self) -> f32 {
        // Same validated-range argument as FontConfig::px_size: this value
        // is always a validated font size (1..=256), which fits in u16, so
        // f32::from(u16) proves losslessness instead of needing an allow.
        let size = u16::try_from(self.bar_font_size()).unwrap_or(u16::MAX);
        f32::from(size) * FontConfig::DPI / 72.0
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

    /// Resolved monitor font path, falling back to the terminal font.
    #[must_use]
    pub fn monitor_font_path(&self) -> Option<&str> {
        self.monitor
            .font_path
            .as_deref()
            .or(self.font.path.as_deref())
    }

    /// The dashboard's window background: `[monitor].background` if set,
    /// otherwise mirrors [`Config::bar_background`] (which itself mirrors
    /// `[colors].background` unless the bar overrides it) -- so a monitor
    /// with no `[monitor]` overrides matches the bar, which matches the
    /// terminal, by default.
    #[must_use]
    pub fn monitor_background(&self) -> Color {
        self.monitor
            .background
            .unwrap_or_else(|| self.bar_background())
    }

    /// The dashboard's card/chart-panel surface color: `[monitor].surface`
    /// if set, otherwise mirrors [`Config::bar_background`] (same mirroring
    /// rationale as [`Config::monitor_background`] -- there is no separate
    /// `[colors]` concept of a "panel" color to mirror instead).
    #[must_use]
    pub fn monitor_surface(&self) -> Color {
        self.monitor
            .surface
            .unwrap_or_else(|| self.bar_background())
    }

    /// The dashboard's background opacity, `0.0`-`1.0`: `[monitor].background_opacity`
    /// if set, otherwise mirrors [`Config::bar_opacity`].
    #[must_use]
    pub fn monitor_background_opacity(&self) -> f64 {
        self.monitor
            .background_opacity
            .unwrap_or_else(|| self.bar_opacity())
    }

    /// The dashboard's hero/metric text color: `[monitor].foreground` if
    /// set, otherwise a built-in near-white (`#f2f2f3`) chosen for contrast
    /// against the default dark surface -- unlike `background`/`surface`,
    /// there's no terminal-side `[colors]` field this specific role
    /// ("brightest dashboard text") naturally mirrors.
    #[must_use]
    pub fn monitor_foreground(&self) -> Color {
        self.monitor
            .foreground
            .unwrap_or_else(|| Color::from_rgb(0xf2, 0xf2, 0xf3))
    }

    /// The dashboard's secondary text color (card titles, hero units,
    /// process names/PIDs): `[monitor].dim` if set, otherwise a built-in
    /// mid-grey (`#8a8a90`).
    #[must_use]
    pub fn monitor_dim(&self) -> Color {
        self.monitor
            .dim
            .unwrap_or_else(|| Color::from_rgb(0x8a, 0x8a, 0x90))
    }

    /// The dashboard's tertiary text color (detail-row labels, table
    /// headers): `[monitor].faint` if set, otherwise a built-in dark grey
    /// (`#55555a`).
    #[must_use]
    pub fn monitor_faint(&self) -> Color {
        self.monitor
            .faint
            .unwrap_or_else(|| Color::from_rgb(0x55, 0x55, 0x5a))
    }

    /// The dashboard's card outline / bar-track color: `[monitor].border`
    /// if set, otherwise a built-in near-black grey (`#1e1e21`).
    #[must_use]
    pub fn monitor_border(&self) -> Color {
        self.monitor
            .border
            .unwrap_or_else(|| Color::from_rgb(0x1e, 0x1e, 0x21))
    }

    /// The CPU card's accent color (hero line, per-core bar 0): `[monitor].cpu`
    /// if set, otherwise a built-in muted green (`#8a9a7b`).
    #[must_use]
    pub fn monitor_cpu(&self) -> Color {
        self.monitor
            .cpu
            .unwrap_or_else(|| Color::from_rgb(0x8a, 0x9a, 0x7b))
    }

    /// The memory card's accent color: `[monitor].memory` if set, otherwise
    /// a built-in muted cyan (`#8ea4a2`).
    #[must_use]
    pub fn monitor_memory(&self) -> Color {
        self.monitor
            .memory
            .unwrap_or_else(|| Color::from_rgb(0x8e, 0xa4, 0xa2))
    }

    /// The temperature card's accent color: `[monitor].temperature` if set,
    /// otherwise a built-in muted orange (`#b6927b`).
    #[must_use]
    pub fn monitor_temperature(&self) -> Color {
        self.monitor
            .temperature
            .unwrap_or_else(|| Color::from_rgb(0xb6, 0x92, 0x7b))
    }

    /// The network card's accent color (download line/bars): `[monitor].network`
    /// if set, otherwise a built-in muted cyan (`#8ea4a2`, same as
    /// [`Config::monitor_memory`]'s default -- the two cards don't collide
    /// on-screen so sharing a hue is fine, matching the dashboard's
    /// original hardcoded palette).
    #[must_use]
    pub fn monitor_network(&self) -> Color {
        self.monitor
            .network
            .unwrap_or_else(|| Color::from_rgb(0x8e, 0xa4, 0xa2))
    }

    /// The apps to watch for a compositor-reported urgent window -- see
    /// [`NotificationRule`]. Empty (no notification segments) unless the
    /// user configured `[[bar.notifications]]` entries.
    #[must_use]
    pub fn bar_notifications(&self) -> &[NotificationRule] {
        &self.bar.notifications
    }

    #[must_use]
    pub const fn bar_clicks(&self) -> &BarClickConfig {
        &self.bar.click
    }

    /// The panels' surface color: `[panel].background` if set, otherwise
    /// mirrors [`Config::bar_background`] -- one shared surface across the
    /// bar, the panels, and the terminal by default.
    #[must_use]
    pub fn panel_background(&self) -> Color {
        self.panel
            .background
            .unwrap_or_else(|| self.bar_background())
    }

    /// The panels' text color: `[panel].foreground` if set, otherwise
    /// mirrors [`Config::bar_foreground`].
    #[must_use]
    pub fn panel_foreground(&self) -> Color {
        self.panel
            .foreground
            .unwrap_or_else(|| self.bar_foreground())
    }

    /// The panels' secondary text color (badges, detail text): `[panel].dim`
    /// if set, otherwise mirrors `[colors].bright_black` -- the same role
    /// `[files].muted` mirrors.
    #[must_use]
    pub fn panel_dim(&self) -> Color {
        self.panel.dim.unwrap_or(self.palette.bright_black)
    }

    /// The panels' accent (slider fill, toggle knob track, icon ink):
    /// `[panel].accent` if set, otherwise mirrors `[colors].blue`.
    #[must_use]
    pub fn panel_accent(&self) -> Color {
        self.panel.accent.unwrap_or(self.palette.blue)
    }

    /// The panels' selected-row fill: `[panel].highlight` if set, otherwise
    /// a shade derived by washing the foreground into the background -- the
    /// same derivation `[files].selection` uses.
    #[must_use]
    pub fn panel_highlight(&self) -> Color {
        self.panel
            .highlight
            .unwrap_or_else(|| self.bar_background().mix(self.palette.foreground, 0.14))
    }

    /// The panels' quiet-edge color (slider groove, idle toggle track):
    /// `[panel].border` if set, otherwise a shade derived between the
    /// background and `dim`, so it reads as a groove rather than a second
    /// background.
    #[must_use]
    pub fn panel_border(&self) -> Color {
        self.panel
            .border
            .unwrap_or_else(|| self.panel_dim().mix(self.bar_background(), 0.5))
    }

    /// The panels' surface opacity: `[panel].opacity` if set, otherwise
    /// mirrors [`Config::bar_opacity`].
    #[must_use]
    pub fn panel_opacity(&self) -> f64 {
        self.panel.opacity.unwrap_or_else(|| self.bar_opacity())
    }

    /// The gap in logical px between a panel and the bar / screen edges:
    /// `[panel].gap` if set, otherwise 8.
    #[must_use]
    pub fn panel_gap(&self) -> u32 {
        self.panel.gap.unwrap_or(8)
    }

    /// Which screen edge a panel floats toward: `[panel].side` if set,
    /// otherwise `auto`.
    #[must_use]
    pub const fn panel_side(&self) -> PanelSide {
        match self.panel.side {
            Some(side) => side,
            None => PanelSide::Auto,
        }
    }

    /// The launcher's background color: `[launcher].background` if set,
    /// otherwise mirrors `[colors].background` -- same default-matches-
    /// terminal rule as [`Config::bar_background`].
    #[must_use]
    pub fn launcher_background(&self) -> Color {
        self.launcher.background.unwrap_or(self.palette.background)
    }

    /// The launcher's text color: `[launcher].foreground` if set, otherwise
    /// mirrors `[colors].foreground`.
    #[must_use]
    pub fn launcher_foreground(&self) -> Color {
        self.launcher.foreground.unwrap_or(self.palette.foreground)
    }

    /// The launcher's helper text color: `[launcher].helper` if set,
    /// otherwise a muted Kanagawa Dragon-style gray.
    #[must_use]
    pub fn launcher_helper(&self) -> Color {
        self.launcher
            .helper
            .unwrap_or_else(|| Color::from_rgb(0x72, 0x71, 0x69))
    }

    /// The launcher's font file: `[launcher].font_path` if set, otherwise
    /// mirrors `[font].path` (which itself may be `None`, meaning the
    /// embedded bitmap font).
    #[must_use]
    pub fn launcher_font_path(&self) -> Option<&str> {
        self.launcher
            .font_path
            .as_deref()
            .or(self.font.path.as_deref())
    }

    /// The launcher's font point size: `[launcher].font_size` if set,
    /// otherwise mirrors `[font].size`.
    #[must_use]
    pub fn launcher_font_size(&self) -> u32 {
        self.launcher.font_size.unwrap_or(self.font.size)
    }

    /// [`Config::launcher_font_size`] converted to a pixel size the same way
    /// [`FontConfig::px_size`]/[`Config::bar_font_px_size`] do (96 DPI).
    #[must_use]
    pub fn launcher_font_px_size(&self) -> f32 {
        let size = u16::try_from(self.launcher_font_size()).unwrap_or(u16::MAX);
        f32::from(size) * FontConfig::DPI / 72.0
    }

    /// The launcher's emoji fallback face: `[launcher].emoji_path` if set,
    /// otherwise mirrors `[font].emoji_path`.
    #[must_use]
    pub fn launcher_emoji_path(&self) -> Option<&str> {
        self.launcher
            .emoji_path
            .as_deref()
            .or(self.font.emoji_path.as_deref())
    }

    /// The launcher's icon fallback face: `[launcher].icon_path` if set,
    /// otherwise mirrors `[bar].icon_path` (which itself may be `None`).
    #[must_use]
    pub fn launcher_icon_path(&self) -> Option<&str> {
        self.launcher
            .icon_path
            .as_deref()
            .or(self.bar.icon_path.as_deref())
    }

    /// The launcher's background opacity, `0.0`-`1.0`: `[launcher].opacity`
    /// if set, otherwise a built-in `0.85` -- deliberately darker/more
    /// transparent than the bar's mirrored-from-terminal default, since a
    /// floating launcher overlay reads better with a bit of see-through
    /// than an edge-docked bar does. See [`LauncherConfig::opacity`].
    #[must_use]
    pub fn launcher_opacity(&self) -> f64 {
        self.launcher.opacity.unwrap_or(0.85)
    }

    /// The selected-result row's highlight background: `[launcher].highlight`
    /// if set, otherwise a built-in dark slate (`#313244`) -- see
    /// [`LauncherConfig::highlight`] for why this has no mirror source.
    #[must_use]
    pub fn launcher_highlight(&self) -> Color {
        self.launcher
            .highlight
            .unwrap_or_else(|| Color::from_rgb(0x31, 0x32, 0x44))
    }

    /// The file browser's window background: `[files].background` if set,
    /// otherwise mirrors `[colors].background`.
    #[must_use]
    pub fn files_background(&self) -> Color {
        self.files.background.unwrap_or(self.palette.background)
    }

    /// Row text: `[files].text` if set, otherwise mirrors
    /// `[colors].foreground`.
    #[must_use]
    pub fn files_text(&self) -> Color {
        self.files.text.unwrap_or(self.palette.foreground)
    }

    /// Directory names: `[files].dir` if set, otherwise mirrors
    /// `[colors].blue` -- the colour `ls` has used for directories long
    /// enough that it reads as "directory" without a legend.
    #[must_use]
    pub fn files_dir(&self) -> Color {
        self.files.dir.unwrap_or(self.palette.blue)
    }

    /// Symlinks: `[files].symlink` if set, otherwise mirrors
    /// `[colors].cyan`, again following `ls`.
    #[must_use]
    pub fn files_symlink(&self) -> Color {
        self.files.symlink.unwrap_or(self.palette.cyan)
    }

    /// Broken symlinks: `[files].broken` if set, otherwise mirrors
    /// `[colors].red`. Worth shouting about -- every operation on one
    /// behaves surprisingly.
    #[must_use]
    pub fn files_broken(&self) -> Color {
        self.files.broken.unwrap_or(self.palette.red)
    }

    /// Error text: `[files].error` if set, otherwise mirrors
    /// `[colors].red`.
    #[must_use]
    pub fn files_error(&self) -> Color {
        self.files.error.unwrap_or(self.palette.red)
    }

    /// Dimmed text -- sizes, counts, the status line: `[files].muted` if
    /// set, otherwise mirrors `[colors].bright_black`, which is what that
    /// palette slot is for.
    #[must_use]
    pub fn files_muted(&self) -> Color {
        self.files.muted.unwrap_or(self.palette.bright_black)
    }

    /// The filter-box caret: `[files].caret` if set, otherwise mirrors
    /// `[colors].yellow` so it stays visible against a selected row.
    #[must_use]
    pub fn files_caret(&self) -> Color {
        self.files.caret.unwrap_or(self.palette.yellow)
    }

    /// The cursor row's outline: `[files].cursor` if set, otherwise mirrors
    /// `[colors].blue`.
    #[must_use]
    pub fn files_cursor(&self) -> Color {
        self.files.cursor.unwrap_or(self.palette.blue)
    }

    /// Selected-row fill: `[files].selection` if set, otherwise a lightened
    /// background.
    ///
    /// Derived rather than a literal because this role has no `[colors]`
    /// equivalent -- a terminal has no "selected row" -- and a hardcoded
    /// dark slate would sit invisibly on a light-themed palette. See
    /// [`Color::mix`] for the derivation.
    #[must_use]
    pub fn files_selection(&self) -> Color {
        self.files
            .selection
            .unwrap_or_else(|| self.palette.background.mix(self.palette.foreground, 0.22))
    }

    /// The header strip: `[files].header_bg` if set, otherwise a background
    /// nudged *away* from the foreground, so the chrome recedes behind the
    /// list on dark and light palettes alike.
    ///
    /// Uses [`Color::recede_from`] rather than a negative [`Color::mix`]
    /// because a pure-white background has nothing darker to recede to and
    /// would leave the strip invisible.
    #[must_use]
    pub fn files_header_bg(&self) -> Color {
        self.files.header_bg.unwrap_or_else(|| {
            self.palette
                .background
                .recede_from(self.palette.foreground, 0.35)
        })
    }

    /// The status strip: `[files].status_bg` if set, otherwise the same
    /// derivation as [`Config::files_header_bg`], so the two strips match
    /// by default while staying independently overridable.
    #[must_use]
    pub fn files_status_bg(&self) -> Color {
        self.files
            .status_bg
            .unwrap_or_else(|| self.files_header_bg())
    }

    /// The scrollbar trough: `[files].scrollbar_track` if set, otherwise
    /// the chrome fill, so the bar reads as part of the frame.
    #[must_use]
    pub fn files_scrollbar_track(&self) -> Color {
        self.files
            .scrollbar_track
            .unwrap_or_else(|| self.files_header_bg())
    }

    /// The scrollbar thumb: `[files].scrollbar_thumb` if set, otherwise a
    /// mid-tone that stays visible against its own track.
    #[must_use]
    pub fn files_scrollbar_thumb(&self) -> Color {
        self.files
            .scrollbar_thumb
            .unwrap_or_else(|| self.palette.background.mix(self.palette.foreground, 0.38))
    }

    /// Tile fill number `slot` (taken modulo four): `[files].tile_color_N`
    /// if set, otherwise the chrome tinted toward one palette hue per slot.
    ///
    /// Derived from the chrome rather than the hue itself so the default
    /// fills stay dark enough to carry the palette's text colours.
    #[must_use]
    pub fn files_tile_color(&self, slot: usize) -> Color {
        let slot = slot % FilesConfig::TILE_COLORS;
        let hue = [
            self.palette.blue,
            self.palette.green,
            self.palette.magenta,
            self.palette.yellow,
        ][slot];
        self.files.tile_colors[slot]
            .unwrap_or_else(|| self.files_header_bg().mix(hue, FilesConfig::TILE_TINT))
    }

    /// The focused filter box: `[files].filter_bg` if set, otherwise a
    /// slightly lifted background -- enough to show focus, not enough to
    /// compete with the selection.
    #[must_use]
    pub fn files_filter_bg(&self) -> Color {
        self.files
            .filter_bg
            .unwrap_or_else(|| self.palette.background.mix(self.palette.foreground, 0.13))
    }

    /// The browser's background opacity, `0.0`-`1.0`: `[files].opacity` if
    /// set, otherwise mirrors `[window].opacity`.
    ///
    /// Mirrors rather than defaulting to opaque so the browser is as
    /// translucent as the terminal, bar and monitor by default. Only the
    /// background is affected; text and chrome strokes stay fully opaque, or
    /// a translucent window would render unreadable filenames.
    #[must_use]
    pub fn files_opacity(&self) -> f64 {
        self.files.opacity.unwrap_or(self.window.opacity)
    }

    /// The canvas background: `[canvas].background` if set, otherwise
    /// mirrors `[colors].background`.
    #[must_use]
    pub fn canvas_background(&self) -> Color {
        self.canvas.background.unwrap_or(self.palette.background)
    }

    /// The canvas grid-dot colour: `[canvas].grid` if set, otherwise a
    /// quiet derivation from the palette that stays visible without
    /// competing with content.
    #[must_use]
    pub fn canvas_grid(&self) -> Color {
        self.canvas
            .grid
            .unwrap_or_else(|| self.palette.background.mix(self.palette.foreground, 0.14))
    }

    /// The canvas text/stroke colour: `[canvas].text` if set, otherwise
    /// mirrors `[colors].foreground`.
    #[must_use]
    pub fn canvas_text(&self) -> Color {
        self.canvas.text.unwrap_or(self.palette.foreground)
    }

    /// The note-card fill: `[canvas].surface` if set, otherwise a surface
    /// stepped slightly away from the background.
    #[must_use]
    pub fn canvas_surface(&self) -> Color {
        self.canvas
            .surface
            .unwrap_or_else(|| self.palette.background.mix(self.palette.foreground, 0.07))
    }

    /// The note-card border: `[canvas].border` if set, otherwise a
    /// mid-tone derivation.
    #[must_use]
    pub fn canvas_border(&self) -> Color {
        self.canvas
            .border
            .unwrap_or_else(|| self.palette.background.mix(self.palette.foreground, 0.22))
    }

    /// The selection accent: `[canvas].selection` if set, otherwise mirrors
    /// `[colors].blue`.
    #[must_use]
    pub fn canvas_selection(&self) -> Color {
        self.canvas.selection.unwrap_or(self.palette.blue)
    }

    /// The text caret: `[canvas].caret` if set, otherwise mirrors
    /// `[colors].yellow`, as `[files]` does -- a warm bar stays legible
    /// against both the canvas background and the note's own glyphs, which
    /// the selection blue does not.
    #[must_use]
    pub fn canvas_caret(&self) -> Color {
        self.canvas.caret.unwrap_or(self.palette.yellow)
    }

    /// The shape stroke width in pixels: `[canvas].stroke_width` if set,
    /// otherwise `2`.
    #[must_use]
    pub fn canvas_stroke_width(&self) -> u32 {
        self.canvas.stroke_width.unwrap_or(2)
    }

    /// The snap-grid pitch in pixels: `[canvas].grid_size` if set,
    /// otherwise `8`. `0` disables snapping.
    #[must_use]
    pub fn canvas_grid_size(&self) -> u32 {
        self.canvas.grid_size.unwrap_or(8)
    }

    /// The canvas window's background opacity, `0.0`-`1.0`:
    /// `[canvas].opacity` if set, otherwise mirrors `[window].opacity`.
    #[must_use]
    pub fn canvas_opacity(&self) -> f64 {
        self.canvas.opacity.unwrap_or(self.window.opacity)
    }

    /// Row height in pixels: `[files].row_height` if set, otherwise `20`.
    ///
    /// The bitmap font is 16px tall; 20 leaves 2px above and below without
    /// making the list feel sparse.
    #[must_use]
    pub fn files_row_height(&self) -> u32 {
        self.files.row_height.unwrap_or(20)
    }

    /// Header strip height in pixels: `[files].header_height` if set,
    /// otherwise `24`.
    #[must_use]
    pub fn files_header_height(&self) -> u32 {
        self.files.header_height.unwrap_or(24)
    }

    /// Status strip height in pixels: `[files].status_height` if set,
    /// otherwise `20`.
    #[must_use]
    pub fn files_status_height(&self) -> u32 {
        self.files.status_height.unwrap_or(20)
    }

    /// Left/right padding inside a row, in pixels: `[files].padding` if
    /// set, otherwise `8`.
    #[must_use]
    pub fn files_padding(&self) -> u32 {
        self.files.padding.unwrap_or(8)
    }
}

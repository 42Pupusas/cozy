//! Terminal cell and attribute types.

/// A terminal color value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Color {
    /// Terminal default (inherits from theme).
    #[default]
    Default,
    /// One of the 256 palette indices (0–255).
    Indexed(u8),
    /// True-color RGB.
    Rgb(u8, u8, u8),
}

/// Cell graphic attributes (SGR state).
///
/// The eight boolean SGR flags (bold/dim/italic/…) are packed into a single
/// `u8` bitfield rather than eight separate `bool` fields. That shrinks
/// `Attrs` from 16 to 9 bytes and `Cell` from 24 to 16 -- a third less memory
/// traffic on every full-screen `fill`/`copy_within`/snapshot/blit, which are
/// pure `size_of::<Cell>()`-bound memsets. The flags are read/written through
/// the generated accessor pairs (`bold()` / `set_bold()`); `fg`/`bg` stay
/// public fields so `Attrs { fg, bg, ..default() }` literals keep working.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Attrs {
    pub fg: Color,
    pub bg: Color,
    flags: u8,
    /// Underline color override (`SGR 58`/`59`). `Color::Default` means
    /// "use the glyph's own foreground" -- the universal default and the
    /// common case, so this costs nothing extra to check.
    pub underline_color: Color,
    /// Underline style (`SGR 4:n`). Only meaningful when `underline()` is
    /// set; a plain `SGR 4` (no colon sub-parameter) always means
    /// [`UnderlineStyle::Single`].
    pub underline_style: UnderlineStyle,
}

/// Underline rendering style, from the `CSI 4:n m` colon sub-parameter.
/// `Default` (`Single`) matches a bare `CSI 4 m` with no style selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnderlineStyle {
    #[default]
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

impl UnderlineStyle {
    /// Map an `SGR 4:n` style number to its style, defaulting unrecognised
    /// values to [`Self::Single`] rather than being dropped -- matching how
    /// unknown SGR codes elsewhere in the parser degrade gracefully.
    #[must_use]
    pub const fn from_sgr(n: u8) -> Self {
        match n {
            2 => Self::Double,
            3 => Self::Curly,
            4 => Self::Dotted,
            5 => Self::Dashed,
            _ => Self::Single,
        }
    }
}

/// Bit positions for the packed SGR flags. Private -- callers go through the
/// `bold()`/`set_bold()` accessors, keeping the layout an implementation
/// detail.
mod flag {
    pub const BOLD: u8 = 1 << 0;
    pub const DIM: u8 = 1 << 1;
    pub const ITALIC: u8 = 1 << 2;
    pub const UNDERLINE: u8 = 1 << 3;
    pub const BLINK: u8 = 1 << 4;
    pub const INVERSE: u8 = 1 << 5;
    pub const INVISIBLE: u8 = 1 << 6;
    pub const STRIKETHROUGH: u8 = 1 << 7;
}

/// Generate a `name() -> bool` getter and `set_name(bool)` setter pair for
/// each packed flag, so call sites read almost like plain field access
/// (`attrs.bold()` / `attrs.set_bold(true)`) while the storage stays a single
/// byte.
macro_rules! flag_accessors {
    ($($get:ident, $set:ident => $bit:ident;)*) => {
        $(
            #[doc = concat!("Whether the ", stringify!($get), " SGR flag is set.")]
            #[inline]
            #[must_use]
            pub const fn $get(&self) -> bool {
                self.flags & flag::$bit != 0
            }
            #[doc = concat!("Set or clear the ", stringify!($get), " SGR flag.")]
            #[inline]
            pub const fn $set(&mut self, on: bool) {
                if on {
                    self.flags |= flag::$bit;
                } else {
                    self.flags &= !flag::$bit;
                }
            }
        )*
    };
}

impl Attrs {
    /// Construct plain attributes with the given fore/background colors and no
    /// SGR flags set. Handy across crate boundaries, where the private `flags`
    /// field rules out `Attrs { fg, bg, ..Default::default() }` struct-update
    /// syntax.
    #[must_use]
    pub const fn with_colors(fg: Color, bg: Color) -> Self {
        Self {
            fg,
            bg,
            flags: 0,
            underline_color: Color::Default,
            underline_style: UnderlineStyle::Single,
        }
    }

    flag_accessors! {
        bold, set_bold => BOLD;
        dim, set_dim => DIM;
        italic, set_italic => ITALIC;
        underline, set_underline => UNDERLINE;
        blink, set_blink => BLINK;
        inverse, set_inverse => INVERSE;
        invisible, set_invisible => INVISIBLE;
        strikethrough, set_strikethrough => STRIKETHROUGH;
    }
}

/// A single terminal cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    /// The Unicode character occupying this cell (space = empty).
    pub ch: char,
    /// Graphic attributes.
    pub attrs: Attrs,
    /// True if this cell holds the leading (visible) half of a 2-cell-wide
    /// glyph (CJK, fullwidth forms, some emoji). The cell immediately to its
    /// right is a paired [`Cell::spacer`] and must never be drawn or split
    /// from its lead independently.
    pub wide: bool,
    /// True if this cell is the trailing (invisible) half of a wide glyph
    /// occupying the cell to its left. Renderers must skip spacer cells;
    /// grid operations that would erase/overwrite only one half of a pair
    /// must clear the other half too, to avoid an orphaned half-glyph.
    pub spacer: bool,
    /// OSC 8 hyperlink id, or `0` for "no link". Indexes into
    /// `Grid`'s link table (never exposed here -- `Cell` stays a dumb value
    /// type with no back-reference to the grid that produced it) rather than
    /// storing the URI inline, which would blow `Cell` up by the length of
    /// every distinct URI printed instead of one `u32` per cell regardless of
    /// how long or how many links are on screen.
    pub link_id: u32,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            attrs: Attrs::default(),
            wide: false,
            spacer: false,
            link_id: 0,
        }
    }
}

#[cfg(test)]
mod size_tests {
    #[test]
    fn cell_size_is_tracked() {
        // Grew 16 -> 20 bytes when `link_id: u32` was added for OSC 8
        // hyperlink support, then 20 -> 24 when `Attrs` grew an
        // `underline_color: Color` (4 bytes) + `underline_style` (1 byte,
        // padded) for SGR 4:n/58/59 underline styling. Still a plain
        // memcpy-able POD, just wider, so `fill`/`copy_within`/snapshot/blit
        // stay cheap memsets -- this assertion exists so a future field
        // addition is a deliberate, visible size bump rather than a silent
        // one.
        assert_eq!(std::mem::size_of::<super::Cell>(), 24);
        assert_eq!(std::mem::size_of::<super::Attrs>(), 14);
    }
}

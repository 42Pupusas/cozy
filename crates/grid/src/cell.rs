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
/// Everything boolean-ish is packed into a single private `u16` bitfield:
/// the eight SGR flags (bold/dim/italic/…), the 3-bit underline style, and
/// the two cell-placement bits (`wide`/`spacer`) that [`Cell`] exposes.
/// Packing is not cosmetic: the scroll/blank/compact paths in `Screen` are
/// pure `size_of::<Cell>()`-bound memsets/memmoves that move gigabytes per
/// flood, so every byte shaved off `Cell` is a directly measurable
/// throughput win. This layout puts `Attrs` at 14 bytes and `Cell` at 20
/// (down from 24), with no padding anywhere.
///
/// The flags are read/written through the generated accessor pairs
/// (`bold()` / `set_bold()`); `fg`/`bg`/`underline_color` stay public fields
/// so color-only struct updates keep working. The underline *style* moved
/// behind `underline_style()` / `set_underline_style()` accessors when it
/// was folded into the bitfield.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Attrs {
    pub fg: Color,
    pub bg: Color,
    /// Underline color override (`SGR 58`/`59`). `Color::Default` means
    /// "use the glyph's own foreground" -- the universal default and the
    /// common case, so this costs nothing extra to check.
    pub underline_color: Color,
    /// Packed bits: SGR flags (bits 0-7), underline style (bits 8-10), and
    /// the `Cell` placement bits wide/spacer (bits 11-12). Placement bits
    /// participate in derived equality on purpose: a spacer cell must never
    /// compare equal to a blank one (resize's trailing-blank trim relies on
    /// it). `Grid`'s live SGR state never sets placement bits, so `Attrs`
    /// comparisons outside `Cell` are unaffected.
    flags: u16,
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

    /// The 3-bit encoding used inside [`Attrs`]' packed flags word.
    const fn to_bits(self) -> u16 {
        match self {
            Self::Single => 0,
            Self::Double => 1,
            Self::Curly => 2,
            Self::Dotted => 3,
            Self::Dashed => 4,
        }
    }

    /// Inverse of [`Self::to_bits`]; unknown encodings decode to `Single`,
    /// mirroring `from_sgr`'s graceful degradation.
    const fn from_bits(bits: u16) -> Self {
        match bits {
            1 => Self::Double,
            2 => Self::Curly,
            3 => Self::Dotted,
            4 => Self::Dashed,
            _ => Self::Single,
        }
    }
}

/// Bit positions for the packed flags word. Private -- callers go through
/// the `bold()`/`set_bold()` (and `Cell::wide()`/`spacer()`) accessors,
/// keeping the layout an implementation detail.
mod flag {
    pub const BOLD: u16 = 1 << 0;
    pub const DIM: u16 = 1 << 1;
    pub const ITALIC: u16 = 1 << 2;
    pub const UNDERLINE: u16 = 1 << 3;
    pub const BLINK: u16 = 1 << 4;
    pub const INVERSE: u16 = 1 << 5;
    pub const INVISIBLE: u16 = 1 << 6;
    pub const STRIKETHROUGH: u16 = 1 << 7;
    /// Underline style occupies bits 8-10 (values 0-4).
    pub const STYLE_SHIFT: u16 = 8;
    pub const STYLE_MASK: u16 = 0b111 << STYLE_SHIFT;
    /// Cell-placement bits (owned by [`super::Cell`]'s accessors, stored
    /// here so `Cell` stays padding-free at 20 bytes).
    pub const WIDE: u16 = 1 << 11;
    pub const SPACER: u16 = 1 << 12;
}

/// Generate a `name() -> bool` getter and `set_name(bool)` setter pair for
/// each packed flag, so call sites read almost like plain field access
/// (`attrs.bold()` / `attrs.set_bold(true)`) while the storage stays a
/// couple of bits.
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
            underline_color: Color::Default,
            flags: 0,
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

    /// Underline style (`SGR 4:n`). Only meaningful when `underline()` is
    /// set; a plain `SGR 4` (no colon sub-parameter) always means
    /// [`UnderlineStyle::Single`].
    #[inline]
    #[must_use]
    pub const fn underline_style(&self) -> UnderlineStyle {
        UnderlineStyle::from_bits((self.flags & flag::STYLE_MASK) >> flag::STYLE_SHIFT)
    }

    /// Set the underline style bits.
    #[inline]
    pub const fn set_underline_style(&mut self, style: UnderlineStyle) {
        self.flags = (self.flags & !flag::STYLE_MASK) | (style.to_bits() << flag::STYLE_SHIFT);
    }
}

/// A single terminal cell.
///
/// A plain 20-byte POD (`ch` 4 + [`Attrs`] 14 + `link_id` 2, zero padding):
/// `fill`/`copy_within`/snapshot/blit over cell slices stay raw
/// memset/memmove, and the flood/scroll paths that move gigabytes of cells
/// pay 17% less memory traffic than the previous 24-byte layout.
///
/// The former `wide`/`spacer` `bool` fields live in `attrs`' packed flags
/// word now (each was one bit of information in an 8-bit field), and
/// `link_id` narrowed from `u32` to `u16` in storage -- 65 535 distinct
/// hyperlinks per grid is far beyond any real session, and the public
/// accessor still hands out the `u32` the rest of the workspace speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    /// The Unicode character occupying this cell (space = empty).
    pub ch: char,
    /// Graphic attributes (also carries this cell's placement bits).
    pub attrs: Attrs,
    /// OSC 8 hyperlink id, or `0` for "no link". Indexes into `Grid`'s link
    /// table (never exposed here -- `Cell` stays a dumb value type with no
    /// back-reference to the grid that produced it) rather than storing the
    /// URI inline. Stored narrow; see [`Self::link_id`].
    link_id: u16,
}

impl Cell {
    /// An ordinary single-width cell.
    #[must_use]
    pub const fn new(ch: char, attrs: Attrs, link_id: u32) -> Self {
        Self {
            ch,
            attrs,
            link_id: Self::narrow_link(link_id),
        }
    }

    /// The leading (visible) half of a 2-cell-wide glyph. The cell
    /// immediately to its right must be its paired [`Self::wide_spacer`].
    #[must_use]
    pub const fn wide_lead(ch: char, attrs: Attrs, link_id: u32) -> Self {
        let mut cell = Self::new(ch, attrs, link_id);
        cell.attrs.flags |= flag::WIDE;
        cell
    }

    /// The trailing (invisible) half of a wide glyph occupying the cell to
    /// its left. Renderers must skip spacer cells; grid operations that
    /// would erase/overwrite only one half of a pair must clear the other
    /// half too, to avoid an orphaned half-glyph.
    #[must_use]
    pub const fn wide_spacer(attrs: Attrs, link_id: u32) -> Self {
        let mut cell = Self::new(' ', attrs, link_id);
        cell.attrs.flags |= flag::SPACER;
        cell
    }

    /// Clamp a public `u32` link id into the narrow stored form. Ids beyond
    /// `u16::MAX` degrade to "no link" rather than aliasing an unrelated
    /// URI -- `Grid`'s interner stops issuing ids at the same boundary, so
    /// this is a defensive backstop, not a live code path.
    const fn narrow_link(link_id: u32) -> u16 {
        if link_id > u16::MAX as u32 {
            0
        } else {
            #[allow(clippy::cast_possible_truncation)] // guarded above
            {
                link_id as u16
            }
        }
    }

    /// True if this cell holds the leading (visible) half of a 2-cell-wide
    /// glyph (CJK, fullwidth forms, some emoji).
    #[inline]
    #[must_use]
    pub const fn wide(&self) -> bool {
        self.attrs.flags & flag::WIDE != 0
    }

    /// True if this cell is the trailing (invisible) half of a wide glyph.
    #[inline]
    #[must_use]
    pub const fn spacer(&self) -> bool {
        self.attrs.flags & flag::SPACER != 0
    }

    /// This cell's OSC 8 hyperlink id (`0` for "no link"), widened back to
    /// the `u32` the rest of the workspace uses.
    #[inline]
    #[must_use]
    pub const fn link_id(&self) -> u32 {
        self.link_id as u32
    }
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            attrs: Attrs::default(),
            link_id: 0,
        }
    }
}

#[cfg(test)]
mod size_tests {
    use super::{Attrs, Cell, UnderlineStyle};

    #[test]
    fn cell_size_is_tracked() {
        // History: 16 -> 20 when `link_id: u32` was added (OSC 8), 20 -> 24
        // when `Attrs` grew underline color + style (SGR 4:n/58/59), then
        // 24 -> 20 when the two placement bools and the style byte were
        // folded into a packed u16 flags word and link_id narrowed to u16.
        // Still a plain memcpy-able POD -- fill/copy_within/snapshot/blit
        // stay cheap memsets, just 17% narrower. This assertion exists so a
        // future field addition is a deliberate, visible size bump rather
        // than a silent one.
        assert_eq!(std::mem::size_of::<super::Cell>(), 20);
        assert_eq!(std::mem::size_of::<super::Attrs>(), 14);
    }

    #[test]
    fn underline_style_round_trips_through_packed_bits() {
        let mut a = Attrs::default();
        assert_eq!(a.underline_style(), UnderlineStyle::Single);
        for style in [
            UnderlineStyle::Single,
            UnderlineStyle::Double,
            UnderlineStyle::Curly,
            UnderlineStyle::Dotted,
            UnderlineStyle::Dashed,
        ] {
            a.set_underline_style(style);
            assert_eq!(a.underline_style(), style);
        }
        // Style bits must not disturb neighbouring SGR flags.
        a.set_strikethrough(true);
        a.set_underline_style(UnderlineStyle::Curly);
        assert!(a.strikethrough());
        assert_eq!(a.underline_style(), UnderlineStyle::Curly);
    }

    #[test]
    fn placement_bits_distinguish_cells_in_equality() {
        // A spacer must never compare equal to a blank cell: resize's
        // trailing-blank trim and wide-pair sanitizing depend on it.
        let blank = Cell::default();
        let spacer = Cell::wide_spacer(Attrs::default(), 0);
        assert_ne!(blank, spacer);
        assert!(spacer.spacer());
        assert!(!spacer.wide());
        let lead = Cell::wide_lead('全', Attrs::default(), 0);
        assert!(lead.wide());
        assert!(!lead.spacer());
    }

    #[test]
    fn oversized_link_ids_degrade_to_no_link() {
        let cell = Cell::new('a', Attrs::default(), u32::from(u16::MAX) + 1);
        assert_eq!(cell.link_id(), 0);
        let max = Cell::new('a', Attrs::default(), u32::from(u16::MAX));
        assert_eq!(max.link_id(), u32::from(u16::MAX));
    }
}

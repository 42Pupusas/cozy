//! The three colors a monochrome clip needs on a graphical surface.

/// Packed XRGB colors for lit pixels, unlit pixels, and the letterbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoPalette {
    lit: u32,
    unlit: u32,
    pad: u32,
}

impl Default for VideoPalette {
    fn default() -> Self {
        Self {
            lit: 0x00FF_FFFF,
            unlit: 0x0000_0000,
            pad: 0x0000_0000,
        }
    }
}

impl VideoPalette {
    /// Build a palette from three packed XRGB values.
    #[must_use]
    pub const fn new(lit: u32, unlit: u32, pad: u32) -> Self {
        Self { lit, unlit, pad }
    }

    /// The color of a lit source pixel.
    #[must_use]
    pub const fn lit(self) -> u32 {
        self.lit
    }

    /// The color of an unlit source pixel.
    #[must_use]
    pub const fn unlit(self) -> u32 {
        self.unlit
    }

    /// The color of a surface pixel the clip does not cover.
    #[must_use]
    pub const fn pad(self) -> u32 {
        self.pad
    }

    /// Replace the letterbox color.
    #[must_use]
    pub const fn with_pad(self, pad: u32) -> Self {
        Self { pad, ..self }
    }

    /// Parse a `#rrggbb` or `rrggbb` color into packed XRGB.
    pub fn parse_color(raw: &str) -> Result<u32, String> {
        let digits = raw.strip_prefix('#').unwrap_or(raw);
        if digits.len() != 6 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("color must be six hex digits, got {raw}"));
        }
        u32::from_str_radix(digits, 16)
            .map(|value| value & 0x00FF_FFFF)
            .map_err(|_| format!("color must be six hex digits, got {raw}"))
    }
}

#[cfg(test)]
mod tests {
    use super::VideoPalette;

    #[test]
    fn the_default_is_white_on_black() {
        let palette = VideoPalette::default();
        assert_eq!(palette.lit(), 0x00FF_FFFF);
        assert_eq!(palette.unlit(), 0);
        assert_eq!(palette.pad(), 0);
    }

    #[test]
    fn a_hex_color_parses_with_or_without_a_hash() {
        assert_eq!(VideoPalette::parse_color("#ff8800"), Ok(0x00FF_8800));
        assert_eq!(VideoPalette::parse_color("ff8800"), Ok(0x00FF_8800));
    }

    #[test]
    fn a_malformed_color_is_rejected() {
        assert!(VideoPalette::parse_color("#fff").is_err());
        assert!(VideoPalette::parse_color("gggggg").is_err());
        assert!(VideoPalette::parse_color("").is_err());
    }

    #[test]
    fn the_alpha_byte_never_survives_parsing() {
        assert_eq!(VideoPalette::parse_color("ffffff"), Ok(0x00FF_FFFF));
    }

    #[test]
    fn the_pad_color_can_be_replaced() {
        let palette = VideoPalette::default().with_pad(0x0012_3456);
        assert_eq!(palette.pad(), 0x0012_3456);
        assert_eq!(palette.lit(), 0x00FF_FFFF);
    }
}

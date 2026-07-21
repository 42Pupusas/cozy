//! The xterm 256-color palette: indices 0-255 -> (r, g, b).
//!
//! Single source of truth for the "what does color index N mean by default"
//! table, shared by [`crate::Grid`] (answering OSC 4 `?` queries and OSC 104
//! resets with the *actual* color a query would currently resolve to) and
//! `platform::render::Theme` (actually painting it). Used to live duplicated
//! in `platform::render` alone; OSC 4 support needs the grid thread to know
//! it too; a single `const` table is one obviously-correct place instead of
//! two copies that could silently drift apart.

use crate::cell::Color;

pub static XTERM_256: [(u8, u8, u8); 256] = {
    let mut t = [(0u8, 0u8, 0u8); 256];
    // 0-7: standard colors
    t[0] = (0, 0, 0);
    t[1] = (128, 0, 0);
    t[2] = (0, 128, 0);
    t[3] = (128, 128, 0);
    t[4] = (0, 0, 128);
    t[5] = (128, 0, 128);
    t[6] = (0, 128, 128);
    t[7] = (192, 192, 192);
    // 8-15: bright colors
    t[8] = (128, 128, 128);
    t[9] = (255, 0, 0);
    t[10] = (0, 255, 0);
    t[11] = (255, 255, 0);
    t[12] = (0, 0, 255);
    t[13] = (255, 0, 255);
    t[14] = (0, 255, 255);
    t[15] = (255, 255, 255);
    // 16-231: 6x6x6 color cube
    let mut i = 16usize;
    let mut r = 0u8;
    while r < 6 {
        let mut g = 0u8;
        while g < 6 {
            let mut b = 0u8;
            while b < 6 {
                let rv = if r == 0 { 0 } else { r * 40 + 55 };
                let gv = if g == 0 { 0 } else { g * 40 + 55 };
                let bv = if b == 0 { 0 } else { b * 40 + 55 };
                t[i] = (rv, gv, bv);
                i += 1;
                b += 1;
            }
            g += 1;
        }
        r += 1;
    }
    // 232-255: grayscale ramp
    let mut j = 0u8;
    while j < 24 {
        let v = j * 10 + 8;
        t[232 + j as usize] = (v, v, v);
        j += 1;
    }
    t
};

/// Extends [`Color`] with X11/xterm color-spec parsing, as an associated
/// function instead of a free `parse_color_spec(spec)` function -- `Color`
/// is what the parsed result conceptually is, so it's the natural
/// namespace even though the output here is the raw `(u8, u8, u8)` tuple
/// callers actually need (both `Grid`'s palette-override table and
/// `platform::render::Theme` want the tuple, not a `Color::Rgb` wrapper).
impl Color {
    /// Parse an X11/xterm color spec as used by OSC 4/10/11's *set* form.
    ///
    /// Accepts `XParseColor`-style specs: `#RGB`, `#RRGGBB`, `#RRRGGGBBB`,
    /// `#RRRRGGGGBBBB`, or `rgb:R/G/B` with 1-4 hex digits per channel
    /// (short forms are the high bits, e.g. `rgb:f/0/0` == `rgb:ff/00/00`
    /// == pure red). Returns `None` for anything else (including `?`,
    /// which is a query, not a set, and is the caller's job to detect
    /// first).
    #[must_use]
    pub fn parse_spec(spec: &str) -> Option<(u8, u8, u8)> {
        if let Some(hex) = spec.strip_prefix('#') {
            // #RGB / #RRGGBB / #RRRGGGBBB / #RRRRGGGGBBBB -- N hex digits per
            // channel, N in {1,2,3,4}; each channel's *most significant* byte is
            // what we keep (X11 convention: values are the high bits of a wider
            // fixed-point fraction).
            let n = hex.len();
            if n == 0 || n % 3 != 0 || n > 12 {
                return None;
            }
            let per = n / 3;
            let chan = |s: &str| -> Option<u8> {
                let v = u32::from_str_radix(s, 16).ok()?;
                let max = (1u32 << (per * 4)) - 1;
                // `v <= max` (it's `per` hex digits, same as `max`), so
                // `(v * 255 + max/2) / max` is always `<= 255`: fits in u8.
                let scaled = (v * 255 + max / 2) / max;
                Some(u8::try_from(scaled).expect("scaled channel fits in u8"))
            };
            let r = chan(&hex[0..per])?;
            let g = chan(&hex[per..2 * per])?;
            let b = chan(&hex[2 * per..3 * per])?;
            return Some((r, g, b));
        }
        if let Some(rest) = spec.strip_prefix("rgb:") {
            let mut parts = rest.split('/');
            let chan = |s: &str| -> Option<u8> {
                if s.is_empty() || s.len() > 4 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return None;
                }
                let v = u32::from_str_radix(s, 16).ok()?;
                let max = (1u32 << (s.len() * 4)) - 1;
                // `v <= max` (it's `s.len()` hex digits, same as `max`), so
                // `(v * 255 + max/2) / max` is always `<= 255`: fits in u8.
                let scaled = (v * 255 + max / 2) / max;
                Some(u8::try_from(scaled).expect("scaled channel fits in u8"))
            };
            let r = chan(parts.next()?)?;
            let g = chan(parts.next()?)?;
            let b = chan(parts.next()?)?;
            if parts.next().is_some() {
                return None;
            }
            return Some((r, g, b));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xterm_256_low_16_match_standard_palette() {
        assert_eq!(XTERM_256[0], (0, 0, 0));
        assert_eq!(XTERM_256[1], (128, 0, 0));
        assert_eq!(XTERM_256[15], (255, 255, 255));
    }

    #[test]
    fn xterm_256_grayscale_ramp_last_entry() {
        assert_eq!(XTERM_256[255], (238, 238, 238));
    }

    #[test]
    fn parse_hash_rgb_short_form() {
        assert_eq!(Color::parse_spec("#f00"), Some((255, 0, 0)));
        assert_eq!(Color::parse_spec("#0f0"), Some((0, 255, 0)));
    }

    #[test]
    fn parse_hash_rrggbb() {
        assert_eq!(Color::parse_spec("#ff0000"), Some((255, 0, 0)));
        assert_eq!(Color::parse_spec("#1e1e2e"), Some((0x1e, 0x1e, 0x2e)));
    }

    #[test]
    fn parse_hash_wide_forms_scale_down() {
        assert_eq!(Color::parse_spec("#ffff00000000"), Some((255, 0, 0)));
        assert_eq!(Color::parse_spec("#fff000000000"), Some((255, 0, 0)));
    }

    #[test]
    fn parse_rgb_colon_form() {
        assert_eq!(Color::parse_spec("rgb:ff/00/00"), Some((255, 0, 0)));
        assert_eq!(Color::parse_spec("rgb:f/0/0"), Some((255, 0, 0)));
        assert_eq!(Color::parse_spec("rgb:ffff/0000/0000"), Some((255, 0, 0)));
    }

    #[test]
    fn parse_rejects_malformed_specs() {
        assert_eq!(Color::parse_spec("?"), None);
        assert_eq!(Color::parse_spec(""), None);
        assert_eq!(Color::parse_spec("#12345"), None);
        assert_eq!(Color::parse_spec("rgb:zz/00/00"), None);
        assert_eq!(Color::parse_spec("rgb:ff/00"), None);
        assert_eq!(Color::parse_spec("notacolor"), None);
    }
}

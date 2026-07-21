//! Minimal standard-alphabet base64 decoder, just enough for OSC 52
//! (`ESC ] 52 ; <selection> ; <base64> ST`) -- the only thing in this crate
//! that ever needs it. Not pulled in as a dependency because it's a dozen
//! lines and one more crate in the dependency tree isn't worth it for that.

/// Standard base64 alphabet, index = 6-bit value.
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Zero-sized marker struct owning the OSC 52 base64 decode logic, instead
/// of a cluster of free functions (`decode_to_string`/`decode`/`sextet`)
/// hanging off the module -- there's no natural instance data here (the
/// alphabet is a `const`), but the operations are cohesive enough to want a
/// single named owner rather than bare module-level `fn`s.
pub struct Base64;

impl Base64 {
    /// Decode a standard-alphabet base64 string (RFC 4648, with or without
    /// `=` padding) into a UTF-8 `String`. Returns `None` if the input
    /// contains characters outside the base64 alphabet (other than trailing
    /// `=` padding and, tolerantly, any whitespace) or if the decoded bytes
    /// aren't valid UTF-8 -- a clipboard write is meaningless (and unsafe to
    /// hand to Wayland as `text/plain`) with either.
    pub fn decode_to_string(input: &str) -> Option<String> {
        let bytes = Self::decode(input.as_bytes())?;
        String::from_utf8(bytes).ok()
    }

    /// Reverse lookup: byte -> 6-bit value, or `None` for "not a base64
    /// char".
    fn sextet(b: u8) -> Option<u8> {
        ALPHABET.iter().position(|&c| c == b).map(|i| i as u8)
    }

    fn decode(input: &[u8]) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(input.len() / 4 * 3);
        let mut acc: u32 = 0;
        let mut bits = 0u32;
        for &b in input {
            if b == b'=' || b.is_ascii_whitespace() {
                continue;
            }
            let v = Self::sextet(b)?;
            acc = (acc << 6) | u32::from(v);
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((acc >> bits) as u8);
            }
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_plain_ascii() {
        assert_eq!(
            Base64::decode_to_string("aGVsbG8="),
            Some("hello".to_string())
        );
    }

    #[test]
    fn decodes_without_padding() {
        // "hello" without the trailing '=' -- some emitters omit it.
        assert_eq!(
            Base64::decode_to_string("aGVsbG8"),
            Some("hello".to_string())
        );
    }

    #[test]
    fn decodes_empty_string() {
        assert_eq!(Base64::decode_to_string(""), Some(String::new()));
    }

    #[test]
    fn decodes_multibyte_utf8() {
        // "héllo" (é = U+00E9, UTF-8: 0xC3 0xA9).
        let b64 = "aMOpbGxv";
        assert_eq!(Base64::decode_to_string(b64), Some("héllo".to_string()));
    }

    #[test]
    fn rejects_invalid_alphabet_char() {
        assert_eq!(Base64::decode_to_string("!!!!"), None);
    }

    #[test]
    fn rejects_bytes_that_are_not_valid_utf8() {
        // 0xFF 0xFF encoded as base64 -- not valid UTF-8.
        assert_eq!(Base64::decode_to_string("//8="), None);
    }

    #[test]
    fn tolerates_embedded_whitespace() {
        assert_eq!(
            Base64::decode_to_string("aGVs bG8="),
            Some("hello".to_string())
        );
    }
}

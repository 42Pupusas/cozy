//! Percent-encoding for one piece of a URI.

/// Encodes text for use inside a URI component.
pub struct UriComponent;

impl UriComponent {
    const UNRESERVED_PUNCTUATION: &'static [u8] = b"-._~";
    const HEX_DIGITS: &'static [u8; 16] = b"0123456789ABCDEF";

    /// Percent-encode every byte of `text` that is not an ASCII letter,
    /// digit, one of `-._~`, or listed in `also_keep`.
    #[must_use]
    pub fn encode(text: &str, also_keep: &[u8]) -> String {
        Self::encode_bytes(text.as_bytes(), also_keep)
    }

    /// Same as [`Self::encode`] for bytes that need not be UTF-8, such as a
    /// Unix path.
    #[must_use]
    pub fn encode_bytes(bytes: &[u8], also_keep: &[u8]) -> String {
        let mut encoded = String::with_capacity(bytes.len());
        for &byte in bytes {
            if byte.is_ascii_alphanumeric()
                || Self::UNRESERVED_PUNCTUATION.contains(&byte)
                || also_keep.contains(&byte)
            {
                encoded.push(char::from(byte));
            } else {
                encoded.push('%');
                encoded.push(char::from(Self::HEX_DIGITS[usize::from(byte >> 4)]));
                encoded.push(char::from(Self::HEX_DIGITS[usize::from(byte & 0x0F)]));
            }
        }
        encoded
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unreserved_characters_pass_through() {
        assert_eq!(UriComponent::encode("Az09-._~", b""), "Az09-._~");
    }

    #[test]
    fn a_space_becomes_percent_twenty() {
        assert_eq!(UriComponent::encode("a b", b""), "a%20b");
    }

    #[test]
    fn query_delimiters_are_encoded() {
        assert_eq!(UriComponent::encode("a&b=c#d?e", b""), "a%26b%3Dc%23d%3Fe");
    }

    #[test]
    fn a_multibyte_character_encodes_each_utf8_byte() {
        assert_eq!(UriComponent::encode("é", b""), "%C3%A9");
    }

    #[test]
    fn a_newline_encodes_as_percent_zero_a() {
        assert_eq!(UriComponent::encode("a\nb", b""), "a%0Ab");
    }

    #[test]
    fn listed_bytes_are_kept() {
        assert_eq!(UriComponent::encode("a@b+c", b"@+"), "a@b+c");
    }

    #[test]
    fn a_percent_sign_is_encoded_unless_kept() {
        assert_eq!(UriComponent::encode("100%", b""), "100%25");
        assert_eq!(UriComponent::encode("a%20b", b"%"), "a%20b");
    }

    #[test]
    fn bytes_that_are_not_utf8_are_encoded_individually() {
        assert_eq!(UriComponent::encode_bytes(b"a\xff/b", b"/"), "a%FF/b");
    }

    #[test]
    fn empty_text_encodes_to_nothing() {
        assert_eq!(UriComponent::encode("", b""), "");
    }
}

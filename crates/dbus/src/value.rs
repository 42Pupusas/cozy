//! D-Bus type system: signatures, marshalled values, and the (un)marshaller.
//!
//! We only ever *encode* a handful of simple signatures ourselves (`""`,
//! `"s"`, `"ss"` for method-call bodies) — those are built by hand in
//! [`crate::message`]. What genuinely needs to be generic is *decoding*:
//! `NetworkManager` and bluez replies are arbitrarily nested (`a{sv}`,
//! `a{oa{sa{sv}}}`, arrays of struct, …), so [`Unmarshaller::unmarshal`]
//! recurses on the signature grammar rather than hard-coding each shape we
//! care about.

/// One marshalled D-Bus value, tagged by its own type.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Byte(u8),
    Bool(bool),
    I16(i16),
    U16(u16),
    I32(i32),
    U32(u32),
    I64(i64),
    U64(u64),
    Double(f64),
    Str(String),
    ObjectPath(String),
    Signature(String),
    Array(Vec<Self>),
    Struct(Vec<Self>),
    Variant(Box<Self>),
    DictEntry(Box<Self>, Box<Self>),
}

impl Value {
    /// Unwrap a `Variant`, following through to its inner value. Returns
    /// `self` unchanged if it isn't a variant — callers that already know
    /// what they got (e.g. straight off an array element) can call this
    /// unconditionally.
    #[must_use]
    pub fn into_inner(self) -> Self {
        match self {
            Self::Variant(inner) => inner.into_inner(),
            other => other,
        }
    }

    /// Best-effort string view: `Str`/`ObjectPath` directly, or an
    /// array-of-byte SSID (`NetworkManager` hands SSIDs back as `ay`, not
    /// `s` — they're arbitrary bytes, not guaranteed UTF-8) decoded lossily.
    #[must_use]
    pub fn as_display_string(&self) -> Option<String> {
        match self {
            Self::Str(s) | Self::ObjectPath(s) => Some(s.clone()),
            Self::Array(items) => {
                let bytes: Option<Vec<u8>> = items
                    .iter()
                    .map(|v| match v {
                        Self::Byte(b) => Some(*b),
                        _ => None,
                    })
                    .collect();
                bytes.map(|b| String::from_utf8_lossy(&b).into_owned())
            }
            _ => None,
        }
    }

    /// Best-effort integer view, widening any of the fixed-width int types.
    #[must_use]
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Self::Byte(v) => Some(u64::from(*v)),
            Self::U16(v) => Some(u64::from(*v)),
            Self::U32(v) => Some(u64::from(*v)),
            Self::U64(v) => Some(*v),
            Self::I16(v) if *v >= 0 => Some(v.cast_unsigned().into()),
            Self::I32(v) if *v >= 0 => Some(v.cast_unsigned().into()),
            Self::I64(v) if *v >= 0 => Some(v.cast_unsigned()),
            _ => None,
        }
    }

    /// Best-effort bool view.
    #[must_use]
    pub const fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Best-effort array view (also treats a `Struct` as a slice of its
    /// fields, since some callers don't care which of the two produced the
    /// list -- e.g. walking `ManagedObjects` where dict values are arrays
    /// but we only ever want to iterate).
    #[must_use]
    pub fn as_items(&self) -> Option<&[Self]> {
        match self {
            Self::Array(items) | Self::Struct(items) => Some(items),
            _ => None,
        }
    }

    /// Look up a value by string key in an array of `DictEntry` — the shape
    /// every `a{s?}` (property maps, interface maps) unmarshals to. Skips
    /// entries whose key isn't a `Str`/`ObjectPath` (never happens for a
    /// well-formed `a{s?}`, but keeps this total instead of panicking).
    #[must_use]
    pub fn dict_get(&self, key: &str) -> Option<&Self> {
        let items = self.as_items()?;
        items.iter().find_map(|entry| {
            let Self::DictEntry(k, v) = entry else {
                return None;
            };
            (k.as_display_string().as_deref() == Some(key)).then_some(v.as_ref())
        })
    }
}

/// The D-Bus signature grammar and wire (un)marshaller.
///
/// A zero-sized marker type, not a value with fields, since decoding is
/// purely a function of `(buf, offset, sig)` with no state carried between
/// calls (unlike [`crate::conn::DbusConn`], which does own a live
/// connection). Grouping the whole recursive-descent decoder under one
/// type gives it one obvious home instead of a dozen same-purpose
/// functions floating at module scope -- see [`crate::message`] for the
/// sibling encode-side type.
pub struct Unmarshaller;

macro_rules! read_le {
    ($buf:expr, $off:expr, $ty:ty) => {{
        let size = std::mem::size_of::<$ty>();
        let bytes = $buf.get($off..$off + size)?;
        Some(<$ty>::from_le_bytes(bytes.try_into().ok()?))
    }};
}

impl Unmarshaller {
    /// Split a signature into its first complete type and the remainder —
    /// e.g. `"sa{sv}u"` -> `("s", "a{sv}u")`, or `"a{sv}"` -> `("a{sv}",
    /// "")`.
    ///
    /// Recurses through `a` (array of the next complete type) and matches
    /// balanced `(...)`/`{...}` so struct/dict-entry bodies come back
    /// whole.
    #[must_use]
    pub fn split_first_type(sig: &str) -> (&str, &str) {
        let bytes = sig.as_bytes();
        if bytes.is_empty() {
            return ("", "");
        }
        match bytes[0] {
            b'a' => {
                let (inner, _) = Self::split_first_type(&sig[1..]);
                let len = 1 + inner.len();
                (&sig[..len], &sig[len..])
            }
            b'(' | b'{' => {
                let open = bytes[0];
                let close = if open == b'(' { b')' } else { b'}' };
                let mut depth = 1i32;
                let mut i = 1;
                while i < bytes.len() && depth > 0 {
                    if bytes[i] == open {
                        depth += 1;
                    } else if bytes[i] == close {
                        depth -= 1;
                    }
                    i += 1;
                }
                (&sig[..i], &sig[i..])
            }
            _ => (&sig[..1], &sig[1..]),
        }
    }

    /// The alignment (in bytes) a value of this signature's first type is
    /// padded to before it's written/read.
    #[must_use]
    pub const fn alignment_of(sig: &str) -> usize {
        match sig.as_bytes().first() {
            Some(b'b' | b'i' | b'u' | b's' | b'o' | b'a') => 4,
            Some(b'n' | b'q') => 2,
            Some(b'x' | b't' | b'd' | b'(' | b'{') => 8,
            // b'y' (byte), b'g'/b'v' (signature/variant), and everything
            // unrecognized are all 1-byte aligned.
            _ => 1,
        }
    }

    const fn align_up(offset: usize, n: usize) -> usize {
        if n <= 1 {
            offset
        } else {
            offset.div_ceil(n) * n
        }
    }

    /// Decode a UTF-8 string body (`s`/`o`: 4-byte length prefix, `g`:
    /// 1-byte) plus its NUL terminator, returning the string and the offset
    /// just past the terminator.
    fn unmarshal_str_body(buf: &[u8], start: usize, len: usize) -> Option<(String, usize)> {
        let end = start + len;
        let text = std::str::from_utf8(buf.get(start..end)?).ok()?.to_string();
        Some((text, end + 1))
    }

    /// Decode an array (`a<elem_sig>`) starting at `offset`.
    fn unmarshal_array(buf: &[u8], offset: usize, elem_sig: &str) -> Option<(Value, usize)> {
        let aligned = Self::align_up(offset, 4);
        let len: u32 = read_le!(buf, aligned, u32)?;
        let elem_align = Self::alignment_of(elem_sig);
        let start = Self::align_up(aligned + 4, elem_align);
        let end = start + len as usize;
        let mut items = Vec::new();
        let mut pos = start;
        while pos < end {
            let (item, next) = Self::unmarshal(buf, pos, elem_sig)?;
            items.push(item);
            pos = next;
        }
        Some((Value::Array(items), end.max(start)))
    }

    /// Decode a struct body (the part of `sig` between `(` and `)`) starting
    /// at `offset`, which is 8-byte aligned per the D-Bus struct rule.
    fn unmarshal_struct(buf: &[u8], offset: usize, inner_sig: &str) -> Option<(Value, usize)> {
        let mut items = Vec::new();
        let mut pos = Self::align_up(offset, 8);
        let mut rest = inner_sig;
        while !rest.is_empty() {
            let (field_sig, remainder) = Self::split_first_type(rest);
            let (field, next) = Self::unmarshal(buf, pos, field_sig)?;
            items.push(field);
            pos = next;
            rest = remainder;
        }
        Some((Value::Struct(items), pos))
    }

    /// Decode a dict-entry body (the part of `sig` between `{` and `}`)
    /// starting at `offset` -- always exactly one key then one value.
    fn unmarshal_dict_entry(buf: &[u8], offset: usize, inner_sig: &str) -> Option<(Value, usize)> {
        let aligned = Self::align_up(offset, 8);
        let (key_sig, rest) = Self::split_first_type(inner_sig);
        let (key, after_key) = Self::unmarshal(buf, aligned, key_sig)?;
        let (val_sig, _) = Self::split_first_type(rest);
        let (val, after_val) = Self::unmarshal(buf, after_key, val_sig)?;
        Some((Value::DictEntry(Box::new(key), Box::new(val)), after_val))
    }

    /// Decode one complete value of `sig` (its *first* type — trailing
    /// bytes in `sig` are ignored, matching [`Self::split_first_type`]'s
    /// contract).
    ///
    /// Starts at `offset` in `buf`; returns the value and the offset just
    /// past it.
    #[must_use]
    pub fn unmarshal(buf: &[u8], offset: usize, sig: &str) -> Option<(Value, usize)> {
        let type_byte = *sig.as_bytes().first()?;
        match type_byte {
            b'y' | b'b' | b'n' | b'q' | b'i' | b'u' | b'x' | b't' | b'd' => {
                Self::unmarshal_fixed_width(type_byte, buf, offset)
            }
            b's' | b'o' | b'g' | b'v' => Self::unmarshal_string_like(type_byte, buf, offset),
            b'a' => Self::unmarshal_array(buf, offset, &sig[1..]),
            b'(' => Self::unmarshal_struct(buf, offset, &sig[1..sig.len().saturating_sub(1)]),
            b'{' => Self::unmarshal_dict_entry(buf, offset, &sig[1..sig.len().saturating_sub(1)]),
            _ => None,
        }
    }

    /// Decode one of the fixed-width scalar types (`y`/`b`/`n`/`q`/`i`/`u`/
    /// `x`/`t`/`d`) at `offset`. Split out of [`Self::unmarshal`] -- these
    /// nine arms are all "align, read a fixed-size little-endian value,
    /// wrap it, advance by its width" with nothing in common with the
    /// string-like or container arms beyond being part of the same grammar,
    /// so folding them into `unmarshal`'s own complexity budget bought
    /// nothing.
    fn unmarshal_fixed_width(type_byte: u8, buf: &[u8], offset: usize) -> Option<(Value, usize)> {
        match type_byte {
            b'y' => Some((Value::Byte(*buf.get(offset)?), offset + 1)),
            b'b' => {
                let aligned = Self::align_up(offset, 4);
                let raw: u32 = read_le!(buf, aligned, u32)?;
                Some((Value::Bool(raw != 0), aligned + 4))
            }
            b'n' => {
                let aligned = Self::align_up(offset, 2);
                Some((Value::I16(read_le!(buf, aligned, i16)?), aligned + 2))
            }
            b'q' => {
                let aligned = Self::align_up(offset, 2);
                Some((Value::U16(read_le!(buf, aligned, u16)?), aligned + 2))
            }
            b'i' => {
                let aligned = Self::align_up(offset, 4);
                Some((Value::I32(read_le!(buf, aligned, i32)?), aligned + 4))
            }
            b'u' => {
                let aligned = Self::align_up(offset, 4);
                Some((Value::U32(read_le!(buf, aligned, u32)?), aligned + 4))
            }
            b'x' => {
                let aligned = Self::align_up(offset, 8);
                Some((Value::I64(read_le!(buf, aligned, i64)?), aligned + 8))
            }
            b't' => {
                let aligned = Self::align_up(offset, 8);
                Some((Value::U64(read_le!(buf, aligned, u64)?), aligned + 8))
            }
            b'd' => {
                let aligned = Self::align_up(offset, 8);
                Some((Value::Double(read_le!(buf, aligned, f64)?), aligned + 8))
            }
            // Unreachable: unmarshal only calls this for one of the 9 bytes
            // matched above.
            _ => None,
        }
    }

    /// Decode one of the string-like types (`s`/`o`/`g`/`v`) at `offset`.
    /// Split out of [`Self::unmarshal`] for the same reason as
    /// [`Self::unmarshal_fixed_width`] -- `s`/`o` share a 4-byte length
    /// prefix, `g` a 1-byte one, and `v` recurses back into `unmarshal`
    /// with the inner signature it just decoded as a string; none of that
    /// has anything to do with the fixed-width or container arms.
    fn unmarshal_string_like(type_byte: u8, buf: &[u8], offset: usize) -> Option<(Value, usize)> {
        match type_byte {
            b's' | b'o' => {
                let aligned = Self::align_up(offset, 4);
                let len: u32 = read_le!(buf, aligned, u32)?;
                let (text, end) = Self::unmarshal_str_body(buf, aligned + 4, len as usize)?;
                let val = if type_byte == b's' {
                    Value::Str(text)
                } else {
                    Value::ObjectPath(text)
                };
                Some((val, end))
            }
            b'g' => {
                let len = *buf.get(offset)? as usize;
                let (text, end) = Self::unmarshal_str_body(buf, offset + 1, len)?;
                Some((Value::Signature(text), end))
            }
            b'v' => {
                let len = *buf.get(offset)? as usize;
                let (inner_sig, val_start) = Self::unmarshal_str_body(buf, offset + 1, len)?;
                let (inner, end) = Self::unmarshal(buf, val_start, &inner_sig)?;
                Some((Value::Variant(Box::new(inner)), end))
            }
            // Unreachable: unmarshal only calls this for one of the 4 bytes
            // matched above.
            _ => None,
        }
    }

    /// Decode every complete value in `body` per the (possibly multi-type)
    /// top-level signature `sig`, e.g. a method-return body of signature
    /// `"sa{sv}"`.
    #[must_use]
    pub fn unmarshal_body(buf: &[u8], sig: &str) -> Option<Vec<Value>> {
        let mut out = Vec::new();
        let mut offset = 0;
        let mut rest = sig;
        while !rest.is_empty() {
            let (t, r) = Self::split_first_type(rest);
            let (v, next) = Self::unmarshal(buf, offset, t)?;
            out.push(v);
            offset = next;
            rest = r;
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_first_type_basic() {
        assert_eq!(Unmarshaller::split_first_type("s"), ("s", ""));
        assert_eq!(Unmarshaller::split_first_type("su"), ("s", "u"));
        assert_eq!(Unmarshaller::split_first_type("a{sv}"), ("a{sv}", ""));
        assert_eq!(Unmarshaller::split_first_type("a{sv}u"), ("a{sv}", "u"));
        assert_eq!(Unmarshaller::split_first_type("(sy)u"), ("(sy)", "u"));
        assert_eq!(Unmarshaller::split_first_type("aau"), ("aau", ""));
        assert_eq!(Unmarshaller::split_first_type("aa{sv}s"), ("aa{sv}", "s"));
        assert_eq!(Unmarshaller::split_first_type(""), ("", ""));
    }

    fn pad_to(buf: &mut Vec<u8>, n: usize) {
        while !buf.len().is_multiple_of(n) {
            buf.push(0);
        }
    }

    fn push_string(buf: &mut Vec<u8>, s: &str) {
        pad_to(buf, 4);
        let len = u32::try_from(s.len()).expect("test string fits in u32");
        buf.extend_from_slice(&len.to_le_bytes());
        buf.extend_from_slice(s.as_bytes());
        buf.push(0);
    }

    #[test]
    fn unmarshal_string_roundtrip() {
        let mut buf = Vec::new();
        push_string(&mut buf, "hello");
        let (v, end) = Unmarshaller::unmarshal(&buf, 0, "s").unwrap();
        assert_eq!(v, Value::Str("hello".into()));
        assert_eq!(end, buf.len());
    }

    #[test]
    fn unmarshal_array_of_string() {
        let mut buf = Vec::new();
        // array length placeholder, then elements aligned to 4 (string align).
        pad_to(&mut buf, 4);
        let len_pos = buf.len();
        buf.extend_from_slice(&0u32.to_le_bytes());
        let start = buf.len();
        push_string(&mut buf, "a");
        push_string(&mut buf, "bb");
        let elems_len = u32::try_from(buf.len() - start).expect("test array fits in u32");
        buf[len_pos..len_pos + 4].copy_from_slice(&elems_len.to_le_bytes());

        let (v, end) = Unmarshaller::unmarshal(&buf, 0, "as").unwrap();
        assert_eq!(
            v,
            Value::Array(vec![Value::Str("a".into()), Value::Str("bb".into())])
        );
        assert_eq!(end, buf.len());
    }

    #[test]
    fn unmarshal_variant_wrapping_u32() {
        let mut buf = Vec::new();
        buf.push(1u8); // signature length
        buf.push(b'u');
        buf.push(0); // signature NUL
        pad_to(&mut buf, 4);
        buf.extend_from_slice(&42u32.to_le_bytes());
        let (v, end) = Unmarshaller::unmarshal(&buf, 0, "v").unwrap();
        assert_eq!(v, Value::Variant(Box::new(Value::U32(42))));
        assert_eq!(end, buf.len());
        assert_eq!(v.into_inner(), Value::U32(42));
    }

    #[test]
    fn as_display_string_decodes_byte_array_ssid() {
        let v = Value::Array(vec![Value::Byte(b'h'), Value::Byte(b'i')]);
        assert_eq!(v.as_display_string().as_deref(), Some("hi"));
    }
}

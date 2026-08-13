//! Generic D-Bus value **encoding** — the mirror of
//! [`crate::value::Unmarshaller`], which only decodes.
//!
//! Every arm here is the write-side twin of the matching
//! [`Unmarshaller`](crate::value::Unmarshaller) arm: same signature grammar,
//! same alignment rules, same recursion shape, just writing bytes into a
//! `Vec<u8>` instead of reading them out of a `&[u8]`. Keeping the two next
//! to each other in the same crate (rather than, say, folding marshal logic
//! into [`crate::message::MessageBuilder`], which only ever emits a handful
//! of hand-picked shapes for our own outbound calls) is what makes it
//! possible to answer an arbitrary portal method: portal replies are
//! `a{sv}`, nested structs, arrays of struct, and worse, and hard-coding each
//! shape as `message.rs` does today does not scale to that.
//!
//! [`Marshaller::marshal`] takes an explicit target signature, just like
//! [`Unmarshaller::unmarshal`] does — not just because that keeps the two
//! sides symmetric, but because a [`Value::Array`] carrying zero elements
//! has no element anywhere to infer a signature from, and the D-Bus wire
//! format still requires the (possibly empty) array's contents to be padded
//! to its element type's alignment. Only an explicit signature can supply
//! that for an empty array. A [`Value::Variant`], on the other hand, *is*
//! self-describing on the wire (it carries its own inline signature), so
//! encoding one derives that inline signature from the boxed value itself
//! via [`Value::signature`].

use crate::value::{Unmarshaller, Value};

/// Generic `Value` -> wire encoder, mirroring [`Unmarshaller`](crate::value::Unmarshaller).
///
/// A zero-sized marker type for the same reason as its decode-side sibling:
/// encoding is purely a function of `(buf, value, sig)` with nothing to
/// carry between calls.
pub struct Marshaller;

impl Marshaller {
    const fn align_up(n: usize, to: usize) -> usize {
        if to <= 1 { n } else { n.div_ceil(to) * to }
    }

    fn pad_to(buf: &mut Vec<u8>, to: usize) {
        let target = Self::align_up(buf.len(), to);
        buf.resize(target, 0);
    }

    /// Encode one complete value of `sig` (its *first* type — trailing
    /// bytes in `sig` are ignored, matching
    /// [`Unmarshaller::split_first_type`]'s contract) onto the end of
    /// `buf`.
    ///
    /// Returns `None` if `value`'s shape doesn't match `sig` — a mismatch
    /// here means a caller built the wrong `Value` tree for the method it's
    /// replying to, and leaving `buf` possibly half-written is fine because
    /// callers are expected to discard the whole buffer on `None` rather
    /// than send a partial message.
    #[must_use]
    pub fn marshal(buf: &mut Vec<u8>, value: &Value, sig: &str) -> Option<()> {
        let type_byte = *sig.as_bytes().first()?;
        match type_byte {
            b'y' | b'b' | b'n' | b'q' | b'i' | b'u' | b'h' | b'x' | b't' | b'd' => {
                Self::marshal_fixed_width(type_byte, buf, value)
            }
            b's' | b'o' | b'g' | b'v' => Self::marshal_string_like(type_byte, buf, value),
            b'a' => Self::marshal_array(buf, value, &sig[1..]),
            b'(' => Self::marshal_struct(buf, value, &sig[1..sig.len().saturating_sub(1)]),
            b'{' => Self::marshal_dict_entry(buf, value, &sig[1..sig.len().saturating_sub(1)]),
            _ => None,
        }
    }

    /// Encode one of the fixed-width scalars (`y`/`b`/`n`/`q`/`i`/`u`/`h`/
    /// `x`/`t`/`d`). Split out for the same reason
    /// [`Unmarshaller::unmarshal_fixed_width`] is: these nine arms share
    /// nothing with the string-like or container arms beyond the grammar
    /// they're part of.
    fn marshal_fixed_width(type_byte: u8, buf: &mut Vec<u8>, value: &Value) -> Option<()> {
        match type_byte {
            b'y' => {
                let &Value::Byte(b) = value else { return None };
                buf.push(b);
            }
            b'b' => {
                Self::pad_to(buf, 4);
                let &Value::Bool(b) = value else { return None };
                buf.extend_from_slice(&u32::from(b).to_le_bytes());
            }
            b'n' => {
                Self::pad_to(buf, 2);
                let &Value::I16(v) = value else { return None };
                buf.extend_from_slice(&v.to_le_bytes());
            }
            b'q' => {
                Self::pad_to(buf, 2);
                let &Value::U16(v) = value else { return None };
                buf.extend_from_slice(&v.to_le_bytes());
            }
            b'i' => {
                Self::pad_to(buf, 4);
                let &Value::I32(v) = value else { return None };
                buf.extend_from_slice(&v.to_le_bytes());
            }
            b'u' => {
                Self::pad_to(buf, 4);
                let &Value::U32(v) = value else { return None };
                buf.extend_from_slice(&v.to_le_bytes());
            }
            b'h' => {
                Self::pad_to(buf, 4);
                let &Value::UnixFd(index) = value else {
                    return None;
                };
                buf.extend_from_slice(&index.to_le_bytes());
            }
            b'x' => {
                Self::pad_to(buf, 8);
                let &Value::I64(v) = value else { return None };
                buf.extend_from_slice(&v.to_le_bytes());
            }
            b't' => {
                Self::pad_to(buf, 8);
                let &Value::U64(v) = value else { return None };
                buf.extend_from_slice(&v.to_le_bytes());
            }
            b'd' => {
                Self::pad_to(buf, 8);
                let &Value::Double(v) = value else {
                    return None;
                };
                buf.extend_from_slice(&v.to_le_bytes());
            }
            // Unreachable: marshal only calls this for one of the 10 bytes
            // matched above.
            _ => return None,
        }
        Some(())
    }

    /// Encode one of the string-like types (`s`/`o`/`g`/`v`). Split out for
    /// the same reason as [`Unmarshaller::unmarshal_string_like`].
    fn marshal_string_like(type_byte: u8, buf: &mut Vec<u8>, value: &Value) -> Option<()> {
        match type_byte {
            b's' | b'o' => {
                let ((b's', Value::Str(text)) | (b'o', Value::ObjectPath(text))) =
                    (type_byte, value)
                else {
                    return None;
                };
                Self::pad_to(buf, 4);
                buf.extend_from_slice(&u32::try_from(text.len()).ok()?.to_le_bytes());
                buf.extend_from_slice(text.as_bytes());
                buf.push(0);
            }
            b'g' => {
                let Value::Signature(s) = value else {
                    return None;
                };
                buf.push(u8::try_from(s.len()).ok()?);
                buf.extend_from_slice(s.as_bytes());
                buf.push(0);
            }
            b'v' => return Self::marshal_variant(buf, value),
            // Unreachable: marshal only calls this for one of the 4 bytes
            // matched above.
            _ => return None,
        }
        Some(())
    }

    /// Encode a `Variant`: its own inline signature (derived from the boxed
    /// value via [`Value::signature`], since a variant is self-describing
    /// on the wire rather than getting its type from the enclosing
    /// signature), then the value itself per that signature.
    fn marshal_variant(buf: &mut Vec<u8>, value: &Value) -> Option<()> {
        let Value::Variant(inner) = value else {
            return None;
        };
        let inner_sig = inner.signature();
        buf.push(u8::try_from(inner_sig.len()).ok()?);
        buf.extend_from_slice(inner_sig.as_bytes());
        buf.push(0);
        Self::marshal(buf, inner, &inner_sig)
    }

    /// Encode an array (`a<elem_sig>`). The length prefix is written as a
    /// placeholder and patched afterwards, since the byte length of the
    /// elements isn't known until they're all encoded — the same
    /// two-pass approach [`crate::message::MessageBuilder`] already uses
    /// for header-fields arrays.
    ///
    /// The element region is padded to `elem_sig`'s alignment even when
    /// `items` is empty, matching [`Unmarshaller::unmarshal_array`]'s
    /// `start` computation: the spec pads array content to the element
    /// alignment regardless of whether the array holds anything.
    fn marshal_array(buf: &mut Vec<u8>, value: &Value, elem_sig: &str) -> Option<()> {
        let Value::Array(items) = value else {
            return None;
        };
        Self::pad_to(buf, 4);
        let len_pos = buf.len();
        buf.extend_from_slice(&0u32.to_le_bytes());
        let elem_align = Unmarshaller::alignment_of(elem_sig);
        Self::pad_to(buf, elem_align);
        let start = buf.len();
        for item in items {
            Self::marshal(buf, item, elem_sig)?;
        }
        let elems_len = u32::try_from(buf.len() - start).ok()?;
        buf[len_pos..len_pos + 4].copy_from_slice(&elems_len.to_le_bytes());
        Some(())
    }

    /// Encode a struct body (the part of `sig` between `(` and `)`), 8-byte
    /// aligned per the D-Bus struct rule, one field per element of
    /// `inner_sig` in order.
    fn marshal_struct(buf: &mut Vec<u8>, value: &Value, inner_sig: &str) -> Option<()> {
        let Value::Struct(items) = value else {
            return None;
        };
        Self::pad_to(buf, 8);
        let mut rest = inner_sig;
        let mut fields = items.iter();
        while !rest.is_empty() {
            let (field_sig, remainder) = Unmarshaller::split_first_type(rest);
            let field = fields.next()?;
            Self::marshal(buf, field, field_sig)?;
            rest = remainder;
        }
        // Extra `Value`s beyond what `inner_sig` describes would silently
        // truncate on the real Unmarshaller side too, so treat a leftover
        // field as the same kind of caller error as a missing one.
        if fields.next().is_some() {
            return None;
        }
        Some(())
    }

    /// Encode a dict-entry body (the part of `sig` between `{` and `}`) —
    /// always exactly one key then one value, 8-byte aligned.
    fn marshal_dict_entry(buf: &mut Vec<u8>, value: &Value, inner_sig: &str) -> Option<()> {
        let Value::DictEntry(k, v) = value else {
            return None;
        };
        Self::pad_to(buf, 8);
        let (key_sig, rest) = Unmarshaller::split_first_type(inner_sig);
        Self::marshal(buf, k, key_sig)?;
        let (val_sig, _) = Unmarshaller::split_first_type(rest);
        Self::marshal(buf, v, val_sig)
    }

    /// Encode every value in `values` against the (possibly multi-type)
    /// top-level signature `sig`, e.g. a method-return body of signature
    /// `"sa{sv}"` needing one `Value::Str` followed by one
    /// `Value::Array` of dict entries.
    #[must_use]
    pub fn marshal_body(values: &[Value], sig: &str) -> Option<Vec<u8>> {
        let mut buf = Vec::new();
        let mut rest = sig;
        let mut values = values.iter();
        while !rest.is_empty() {
            let (t, r) = Unmarshaller::split_first_type(rest);
            let v = values.next()?;
            Self::marshal(&mut buf, v, t)?;
            rest = r;
        }
        if values.next().is_some() {
            return None;
        }
        Some(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip a single value through `Marshaller` then `Unmarshaller`
    /// and assert it comes back unchanged, consuming exactly the bytes
    /// written.
    fn roundtrip(value: &Value, sig: &str) {
        let mut buf = Vec::new();
        Marshaller::marshal(&mut buf, value, sig).expect("marshal should succeed");
        let (decoded, end) =
            Unmarshaller::unmarshal(&buf, 0, sig).expect("unmarshal should succeed");
        assert_eq!(&decoded, value, "roundtrip mismatch for signature {sig:?}");
        assert_eq!(
            end,
            buf.len(),
            "unmarshal should consume every written byte"
        );
    }

    #[test]
    fn roundtrip_fixed_width_scalars() {
        roundtrip(&Value::Byte(7), "y");
        roundtrip(&Value::Bool(true), "b");
        roundtrip(&Value::Bool(false), "b");
        roundtrip(&Value::I16(-30_000), "n");
        roundtrip(&Value::U16(60_000), "q");
        roundtrip(&Value::I32(-2_000_000_000), "i");
        roundtrip(&Value::U32(4_000_000_000), "u");
        roundtrip(&Value::I64(-9_000_000_000_000), "x");
        roundtrip(&Value::U64(18_000_000_000_000_000_000), "t");
        roundtrip(&Value::Double(std::f64::consts::PI), "d");
    }

    #[test]
    fn roundtrip_unix_fd_index() {
        roundtrip(&Value::UnixFd(0), "h");
        roundtrip(&Value::UnixFd(4_000_000_000), "h");
    }

    /// The shape `OpenFile` and `OpenPipeWriter` reply with: a descriptor
    /// index boxed in a variant, so the variant's inline signature has to
    /// come out as `h` rather than `u`.
    #[test]
    fn roundtrip_variant_wrapping_a_unix_fd() {
        roundtrip(&Value::Variant(Box::new(Value::UnixFd(2))), "v");
    }

    /// `h` and `u` are the same four bytes on the wire, so nothing but the
    /// tag stops a caller sending a number where a descriptor belongs.
    #[test]
    fn a_u32_is_refused_where_a_unix_fd_is_declared() {
        let mut buf = Vec::new();
        assert!(Marshaller::marshal(&mut buf, &Value::U32(2), "h").is_none());

        let mut buf = Vec::new();
        assert!(Marshaller::marshal(&mut buf, &Value::UnixFd(2), "u").is_none());
    }

    #[test]
    fn roundtrip_string_like() {
        roundtrip(&Value::Str("hello, portal".into()), "s");
        roundtrip(
            &Value::ObjectPath("/org/freedesktop/portal/desktop".into()),
            "o",
        );
        roundtrip(&Value::Signature("a{sv}".into()), "g");
    }

    #[test]
    fn roundtrip_empty_array_pads_to_element_alignment() {
        // An empty `at` (array of u64, 8-aligned elements) must still pad
        // the (empty) element region to 8 bytes before the length prefix
        // ends, exactly mirroring what `Unmarshaller::unmarshal_array`
        // expects to read back.
        roundtrip(&Value::Array(vec![]), "at");
        roundtrip(&Value::Array(vec![]), "as");
    }

    #[test]
    fn roundtrip_array_of_string() {
        roundtrip(
            &Value::Array(vec![Value::Str("a".into()), Value::Str("bb".into())]),
            "as",
        );
    }

    #[test]
    fn roundtrip_variant_wrapping_u32() {
        roundtrip(&Value::Variant(Box::new(Value::U32(42))), "v");
    }

    #[test]
    fn roundtrip_variant_wrapping_nested_container() {
        // Exercises the self-describing signature path for a variant whose
        // inner value is itself a container, not just a scalar.
        let inner = Value::Array(vec![Value::Struct(vec![
            Value::Str("k".into()),
            Value::U32(1),
        ])]);
        roundtrip(&Value::Variant(Box::new(inner)), "v");
    }

    #[test]
    fn roundtrip_struct_of_mixed_fields() {
        roundtrip(
            &Value::Struct(vec![
                Value::Str("name".into()),
                Value::Byte(9),
                Value::Bool(true),
            ]),
            "(syb)",
        );
    }

    #[test]
    fn roundtrip_a_sv_dict() {
        // This is the shape every portal `GetAll`/options dict actually
        // uses: an array of dict-entries, string keys, variant values.
        let dict = Value::Array(vec![
            Value::DictEntry(
                Box::new(Value::Str("accent-color".into())),
                Box::new(Value::Variant(Box::new(Value::Double(0.2)))),
            ),
            Value::DictEntry(
                Box::new(Value::Str("scheme".into())),
                Box::new(Value::Variant(Box::new(Value::U32(1)))),
            ),
        ]);
        roundtrip(&dict, "a{sv}");
    }

    #[test]
    fn roundtrip_nested_array_of_struct_of_array() {
        // Covers alignment handling at three nesting boundaries at once:
        // array -> struct (8-aligned) -> array (4-aligned) -> string.
        let value = Value::Array(vec![
            Value::Struct(vec![
                Value::Byte(1),
                Value::Array(vec![Value::Str("x".into()), Value::Str("yz".into())]),
            ]),
            Value::Struct(vec![Value::Byte(2), Value::Array(vec![])]),
        ]);
        roundtrip(&value, "a(yas)");
    }

    #[test]
    fn marshal_body_handles_multiple_top_level_values() {
        let values = vec![
            Value::Str("org.example.App".into()),
            Value::Array(vec![Value::DictEntry(
                Box::new(Value::Str("handle_token".into())),
                Box::new(Value::Variant(Box::new(Value::Str("abc".into())))),
            )]),
        ];
        let buf = Marshaller::marshal_body(&values, "sa{sv}").expect("marshal_body succeeds");
        let decoded =
            Unmarshaller::unmarshal_body(&buf, "sa{sv}").expect("unmarshal_body succeeds");
        assert_eq!(decoded, values);
    }

    #[test]
    fn marshal_rejects_signature_value_mismatch() {
        let mut buf = Vec::new();
        assert!(Marshaller::marshal(&mut buf, &Value::Str("x".into()), "u").is_none());
    }

    #[test]
    fn marshal_body_rejects_wrong_value_count() {
        let values = vec![Value::Str("only one".into())];
        assert!(Marshaller::marshal_body(&values, "ss").is_none());
    }

    #[test]
    fn value_signature_matches_expected_shapes() {
        assert_eq!(Value::U32(1).signature(), "u");
        assert_eq!(Value::Str("a".into()).signature(), "s");
        assert_eq!(Value::Array(vec![Value::Byte(1)]).signature(), "ay");
        assert_eq!(
            Value::Struct(vec![Value::Str("a".into()), Value::U32(1)]).signature(),
            "(su)"
        );
        assert_eq!(
            Value::DictEntry(Box::new(Value::Str("k".into())), Box::new(Value::U32(1))).signature(),
            "{su}"
        );
    }
}

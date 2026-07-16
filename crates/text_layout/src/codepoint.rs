//! A raw Unicode scalar value, newtyped so the range-table predicates in
//! [`crate::measurement`] and [`crate::bidi`] read as method calls
//! (`cp.is_wide()`) instead of free functions taking a bare `u32`
//! (`is_wide(cp)`) — while staying `const fn` so the tables still fold to
//! compile-time comparisons, same as the free-function form.

/// A Unicode code point, as the `u32` extracted from a `char`.
///
/// Not `char` itself: the range tables below are easier to write and audit
/// against the Unicode Character Database's numeric ranges directly, and a
/// couple of the surrogate-adjacent ranges used for bookkeeping aren't valid
/// `char`s.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CodePoint(pub u32);

impl CodePoint {
    /// Extract the code point of `c`.
    #[inline]
    #[must_use]
    pub const fn of(c: char) -> Self {
        Self(c as u32)
    }
}

impl From<char> for CodePoint {
    #[inline]
    fn from(c: char) -> Self {
        Self::of(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn of_extracts_scalar_value() {
        assert_eq!(CodePoint::of('A').0, 0x41);
        assert_eq!(CodePoint::of('中').0, 0x4e2d);
    }

    #[test]
    fn from_char_matches_of() {
        let c = '€';
        assert_eq!(CodePoint::from(c), CodePoint::of(c));
    }
}

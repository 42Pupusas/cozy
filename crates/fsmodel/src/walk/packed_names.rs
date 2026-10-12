use std::ffi::CStr;

/// Iterates names stored back to back, each ending in a nul byte.
#[derive(Debug, Clone)]
pub struct PackedNames<'n> {
    rest: &'n [u8],
}

impl<'n> PackedNames<'n> {
    #[must_use]
    pub const fn new(packed: &'n [u8]) -> Self {
        Self { rest: packed }
    }
}

impl<'n> Iterator for PackedNames<'n> {
    type Item = &'n CStr;

    fn next(&mut self) -> Option<Self::Item> {
        let name = CStr::from_bytes_until_nul(self.rest).ok()?;
        self.rest = &self.rest[name.to_bytes_with_nul().len()..];
        Some(name)
    }
}

#[cfg(test)]
mod tests {
    use super::PackedNames;

    #[test]
    fn names_come_back_in_order() {
        let names: Vec<_> = PackedNames::new(b"a\0bc\0def\0").collect();
        assert_eq!(names, [c"a", c"bc", c"def"]);
    }

    #[test]
    fn an_empty_buffer_has_no_names() {
        assert_eq!(PackedNames::new(b"").count(), 0);
    }

    #[test]
    fn an_unterminated_tail_is_ignored() {
        let names: Vec<_> = PackedNames::new(b"a\0tail").collect();
        assert_eq!(names, [c"a"]);
    }
}

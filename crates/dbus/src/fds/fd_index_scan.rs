//! Finding the `h` values inside an outgoing message body.
//!
//! A body carries a descriptor as an *index* ([`Value::UnixFd`]) into the
//! array that rides beside the message. The two are built separately, so
//! nothing structural stops a caller emitting `UnixFd(3)` with two
//! descriptors attached. The peer then resolves that index to nothing.
//!
//! An `h` may sit at any depth: inside an array, a struct, a variant, or
//! either half of a dict entry. A scan must therefore walk the whole tree,
//! not just the top-level arguments.

use crate::value::Value;

/// The `h` indexes a value tree references.
///
/// Separate from the sender so the walk is provable on its own, against
/// value trees no socket needs to exist for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FdIndexScan {
    count: usize,
    highest: Option<u32>,
}

impl FdIndexScan {
    /// Scan a complete message body.
    #[must_use]
    pub fn of(values: &[Value]) -> Self {
        let mut scan = Self::default();
        for value in values {
            scan.visit(value);
        }
        scan
    }

    fn visit(&mut self, value: &Value) {
        match value {
            Value::UnixFd(index) => {
                self.count += 1;
                self.highest = Some(self.highest.map_or(*index, |high| high.max(*index)));
            }
            Value::Array(items) | Value::Struct(items) => {
                for item in items {
                    self.visit(item);
                }
            }
            Value::Variant(inner) => self.visit(inner),
            Value::DictEntry(key, val) => {
                self.visit(key);
                self.visit(val);
            }
            _ => {}
        }
    }

    /// How many `h` values the tree holds. Two of them may name the same
    /// descriptor, so this is not the number of descriptors to attach.
    #[must_use]
    pub const fn count(&self) -> usize {
        self.count
    }

    /// The largest index referenced, or `None` when the tree holds no `h`.
    #[must_use]
    pub const fn highest(&self) -> Option<u32> {
        self.highest
    }

    /// Can every referenced index be resolved against `attached`
    /// descriptors?
    #[must_use]
    pub const fn fits(&self, attached: usize) -> bool {
        match self.highest {
            None => true,
            Some(high) => (high as usize) < attached,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::FdIndexScan;
    use crate::value::Value;

    #[test]
    fn a_body_without_descriptors_references_none() {
        let scan = FdIndexScan::of(&[Value::Str("hello".into()), Value::U32(7)]);
        assert_eq!(scan.count(), 0);
        assert_eq!(scan.highest(), None);
    }

    #[test]
    fn a_body_holding_no_descriptors_fits_an_empty_attachment() {
        assert!(FdIndexScan::of(&[Value::U32(1)]).fits(0));
    }

    #[test]
    fn a_top_level_handle_is_found() {
        let scan = FdIndexScan::of(&[Value::UnixFd(0)]);
        assert_eq!(scan.count(), 1);
        assert_eq!(scan.highest(), Some(0));
    }

    /// The index is a slot number, so the largest one decides how many
    /// descriptors must ride along, not how many `h` values appear.
    #[test]
    fn the_highest_index_is_reported_not_the_last() {
        let scan = FdIndexScan::of(&[Value::UnixFd(2), Value::UnixFd(0)]);
        assert_eq!(scan.highest(), Some(2));
    }

    #[test]
    fn a_handle_inside_an_array_is_found() {
        let scan = FdIndexScan::of(&[Value::Array(vec![Value::UnixFd(1), Value::UnixFd(4)])]);
        assert_eq!(scan.count(), 2);
        assert_eq!(scan.highest(), Some(4));
    }

    #[test]
    fn a_handle_inside_a_struct_is_found() {
        let scan = FdIndexScan::of(&[Value::Struct(vec![
            Value::Str("name".into()),
            Value::UnixFd(3),
        ])]);
        assert_eq!(scan.highest(), Some(3));
    }

    /// A portal hands descriptors back inside `a{sv}`, so the walk must
    /// pass through a variant rather than stopping at it.
    #[test]
    fn a_handle_wrapped_in_a_variant_is_found() {
        let scan = FdIndexScan::of(&[Value::Variant(Box::new(Value::UnixFd(5)))]);
        assert_eq!(scan.count(), 1);
        assert_eq!(scan.highest(), Some(5));
    }

    #[test]
    fn a_handle_in_a_dict_entry_value_is_found() {
        let scan = FdIndexScan::of(&[Value::Array(vec![Value::DictEntry(
            Box::new(Value::Str("fd".into())),
            Box::new(Value::Variant(Box::new(Value::UnixFd(6)))),
        )])]);
        assert_eq!(scan.highest(), Some(6));
    }

    /// The shape a real `Screenshot` reply takes: `a{sv}` holding one
    /// handle beside ordinary settings.
    #[test]
    fn a_handle_is_found_among_unrelated_dictionary_entries() {
        let scan = FdIndexScan::of(&[Value::Array(vec![
            Value::DictEntry(
                Box::new(Value::Str("modal".into())),
                Box::new(Value::Variant(Box::new(Value::Bool(true)))),
            ),
            Value::DictEntry(
                Box::new(Value::Str("fd".into())),
                Box::new(Value::Variant(Box::new(Value::UnixFd(0)))),
            ),
        ])]);
        assert_eq!(scan.count(), 1);
        assert_eq!(scan.highest(), Some(0));
    }

    #[test]
    fn two_handles_may_name_the_same_descriptor() {
        let scan = FdIndexScan::of(&[Value::UnixFd(0), Value::UnixFd(0)]);
        assert_eq!(scan.count(), 2);
        assert_eq!(scan.highest(), Some(0));
    }

    /// The defect this type exists to catch: an index the attached array
    /// cannot resolve.
    #[test]
    fn an_index_past_the_attached_descriptors_does_not_fit() {
        assert!(!FdIndexScan::of(&[Value::UnixFd(2)]).fits(2));
    }

    #[test]
    fn the_last_valid_index_fits() {
        assert!(FdIndexScan::of(&[Value::UnixFd(1)]).fits(2));
    }

    /// Attaching descriptors nothing names wastes them but cannot
    /// misresolve, so it stays legal.
    #[test]
    fn more_descriptors_than_indexes_still_fits() {
        assert!(FdIndexScan::of(&[Value::UnixFd(0)]).fits(4));
    }

    #[test]
    fn a_nested_index_past_the_attachment_is_caught() {
        let scan = FdIndexScan::of(&[Value::Array(vec![Value::Variant(Box::new(Value::UnixFd(
            9,
        )))])]);
        assert!(!scan.fits(1));
    }
}

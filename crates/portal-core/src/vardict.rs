use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

use dbus::value::Value;

/// Read-only view of an `a{sv}` argument with variant-unwrapping accessors.
pub struct VardictReader<'a> {
    dict: &'a Value,
}

impl<'a> VardictReader<'a> {
    #[must_use]
    pub const fn new(dict: &'a Value) -> Self {
        Self { dict }
    }

    #[must_use]
    pub fn value(&self, key: &str) -> Option<Value> {
        self.dict.dict_get(key).map(|v| v.clone().into_inner())
    }

    #[must_use]
    pub fn text(&self, key: &str) -> Option<String> {
        self.value(key)
            .and_then(|v| match v {
                Value::Str(text) | Value::ObjectPath(text) => Some(text),
                _ => None,
            })
            .filter(|text| !text.is_empty())
    }

    #[must_use]
    pub fn flag(&self, key: &str) -> Option<bool> {
        self.value(key).and_then(|v| v.as_bool())
    }

    #[must_use]
    pub fn text_list(&self, key: &str) -> Vec<String> {
        let Some(value) = self.value(key) else {
            return Vec::new();
        };
        value.as_items().map_or_else(Vec::new, |items| {
            items
                .iter()
                .filter_map(|item| match item {
                    Value::Str(text) | Value::ObjectPath(text) if !text.is_empty() => {
                        Some(text.clone())
                    }
                    _ => None,
                })
                .collect()
        })
    }

    #[must_use]
    pub fn path(&self, key: &str) -> Option<PathBuf> {
        self.value(key).and_then(|v| NulTerminatedPath::decode(&v))
    }

    #[must_use]
    pub fn path_list(&self, key: &str) -> Vec<PathBuf> {
        let Some(value) = self.value(key) else {
            return Vec::new();
        };
        value.as_items().map_or_else(Vec::new, |items| {
            items.iter().filter_map(NulTerminatedPath::decode).collect()
        })
    }
}

/// The `ay` encoding of a path: raw bytes with a trailing NUL.
pub struct NulTerminatedPath;

impl NulTerminatedPath {
    #[must_use]
    pub fn decode(value: &Value) -> Option<PathBuf> {
        let items = value.as_items()?;
        let mut bytes = items
            .iter()
            .map(|item| match item {
                Value::Byte(byte) => Some(*byte),
                _ => None,
            })
            .collect::<Option<Vec<u8>>>()?;
        while bytes.last() == Some(&0) {
            bytes.pop();
        }
        (!bytes.is_empty()).then(|| PathBuf::from(OsString::from_vec(bytes)))
    }
}

/// Builds an `a{sv}` value entry by entry.
#[derive(Default)]
pub struct VardictBuilder {
    entries: Vec<Value>,
}

impl VardictBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn entry(mut self, key: &str, value: Value) -> Self {
        self.entries.push(Value::DictEntry(
            Box::new(Value::Str(key.to_owned())),
            Box::new(Value::Variant(Box::new(value))),
        ));
        self
    }

    #[must_use]
    pub fn build(self) -> Value {
        Value::Array(self.entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(text: &[u8]) -> Value {
        Value::Array(text.iter().map(|b| Value::Byte(*b)).collect())
    }

    #[test]
    fn text_unwraps_the_variant() {
        let dict = VardictBuilder::new()
            .entry("title", Value::Str("Open".into()))
            .build();
        assert_eq!(
            VardictReader::new(&dict).text("title"),
            Some("Open".to_owned())
        );
    }

    #[test]
    fn empty_text_is_absent() {
        let dict = VardictBuilder::new()
            .entry("title", Value::Str(String::new()))
            .build();
        assert_eq!(VardictReader::new(&dict).text("title"), None);
    }

    #[test]
    fn text_of_the_wrong_type_is_absent() {
        let dict = VardictBuilder::new().entry("title", Value::U32(1)).build();
        assert_eq!(VardictReader::new(&dict).text("title"), None);
    }

    #[test]
    fn a_missing_key_is_absent_everywhere() {
        let dict = VardictBuilder::new().build();
        let reader = VardictReader::new(&dict);
        assert_eq!(reader.text("k"), None);
        assert_eq!(reader.flag("k"), None);
        assert!(reader.text_list("k").is_empty());
        assert_eq!(reader.path("k"), None);
        assert!(reader.path_list("k").is_empty());
    }

    #[test]
    fn flag_reads_a_boolean() {
        let dict = VardictBuilder::new()
            .entry("modal", Value::Bool(false))
            .build();
        assert_eq!(VardictReader::new(&dict).flag("modal"), Some(false));
    }

    #[test]
    fn text_list_skips_empty_and_non_string_items() {
        let dict = VardictBuilder::new()
            .entry(
                "cc",
                Value::Array(vec![Value::Str("a".into()), Value::Str(String::new())]),
            )
            .build();
        assert_eq!(VardictReader::new(&dict).text_list("cc"), vec!["a"]);
    }

    #[test]
    fn a_path_drops_its_trailing_nul() {
        let dict = VardictBuilder::new()
            .entry("current_folder", bytes(b"/home/me\0"))
            .build();
        assert_eq!(
            VardictReader::new(&dict).path("current_folder"),
            Some(PathBuf::from("/home/me"))
        );
    }

    #[test]
    fn a_path_without_a_nul_is_still_read() {
        let dict = VardictBuilder::new()
            .entry("current_folder", bytes(b"/tmp"))
            .build();
        assert_eq!(
            VardictReader::new(&dict).path("current_folder"),
            Some(PathBuf::from("/tmp"))
        );
    }

    #[test]
    fn a_path_keeps_bytes_that_are_not_utf8() {
        let dict = VardictBuilder::new()
            .entry("current_folder", bytes(b"/tmp/\xff\0"))
            .build();
        let path = VardictReader::new(&dict)
            .path("current_folder")
            .expect("path");
        assert_eq!(path.as_os_str().as_encoded_bytes(), b"/tmp/\xff");
    }

    #[test]
    fn an_empty_path_is_absent() {
        let dict = VardictBuilder::new()
            .entry("current_folder", bytes(b"\0"))
            .build();
        assert_eq!(VardictReader::new(&dict).path("current_folder"), None);
    }

    #[test]
    fn a_path_list_reads_each_element() {
        let dict = VardictBuilder::new()
            .entry("files", Value::Array(vec![bytes(b"a.txt\0"), bytes(b"b\0")]))
            .build();
        assert_eq!(
            VardictReader::new(&dict).path_list("files"),
            vec![PathBuf::from("a.txt"), PathBuf::from("b")]
        );
    }

    #[test]
    fn the_builder_wraps_each_value_in_a_variant() {
        let dict = VardictBuilder::new().entry("k", Value::U32(7)).build();
        assert_eq!(
            dict,
            Value::Array(vec![Value::DictEntry(
                Box::new(Value::Str("k".into())),
                Box::new(Value::Variant(Box::new(Value::U32(7)))),
            )])
        );
    }
}

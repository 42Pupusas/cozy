use std::path::PathBuf;

use dbus::value::Value;

use crate::chooser_choice::ChoiceSelection;
use crate::file_filter::FileFilter;
use crate::response_code::ResponseCode;
use crate::uri_component::UriComponent;
use crate::vardict::VardictBuilder;

/// How a `FileChooser` interaction ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChooserOutcome {
    Picked(Selection),
    Cancelled,
    Failed,
}

/// What the user picked.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Selection {
    pub paths: Vec<PathBuf>,
    pub choices: Vec<ChoiceSelection>,
    pub current_filter: Option<FileFilter>,
    pub writable: bool,
}

impl ChooserOutcome {
    /// The `(response, results)` pair for the method reply.
    #[must_use]
    pub fn into_response(self) -> (ResponseCode, Value) {
        match self {
            Self::Picked(selection) => (ResponseCode::Success, selection.into_results()),
            Self::Cancelled => (ResponseCode::Cancelled, VardictBuilder::new().build()),
            Self::Failed => (ResponseCode::Other, VardictBuilder::new().build()),
        }
    }
}

impl Selection {
    const URI_KEEP: &'static [u8] = b"/";

    /// The `file://` URI for `path`, percent-encoding its raw bytes.
    #[must_use]
    pub fn uri_of(path: &std::path::Path) -> String {
        use std::os::unix::ffi::OsStrExt;
        format!(
            "file://{}",
            UriComponent::encode_bytes(path.as_os_str().as_bytes(), Self::URI_KEEP)
        )
    }

    fn into_results(self) -> Value {
        let mut results = VardictBuilder::new().entry(
            "uris",
            Value::Array(
                self.paths
                    .iter()
                    .map(|path| Value::Str(Self::uri_of(path)))
                    .collect(),
            ),
        );
        if !self.choices.is_empty() {
            results = results.entry(
                "choices",
                Value::Array(self.choices.iter().map(ChoiceSelection::to_value).collect()),
            );
        }
        if let Some(wire) = self.current_filter.as_ref().and_then(FileFilter::to_value) {
            results = results.entry("current_filter", wire);
        }
        if self.writable {
            results = results.entry("writable", Value::Bool(true));
        }
        results.build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_filter::FilterRule;

    fn picked(paths: &[&str]) -> Selection {
        Selection {
            paths: paths.iter().map(PathBuf::from).collect(),
            ..Selection::default()
        }
    }

    fn results_of(selection: Selection) -> Value {
        ChooserOutcome::Picked(selection).into_response().1
    }

    #[test]
    fn a_pick_answers_success_with_file_uris() {
        let (code, results) = ChooserOutcome::Picked(picked(&["/home/me/a b.txt"])).into_response();
        assert_eq!(code, ResponseCode::Success);
        assert_eq!(
            VardictReaderProbe::uris(&results),
            vec!["file:///home/me/a%20b.txt"]
        );
    }

    #[test]
    fn uris_keep_the_order_of_the_paths() {
        let results = results_of(picked(&["/b", "/a"]));
        assert_eq!(VardictReaderProbe::uris(&results), vec!["file:///b", "file:///a"]);
    }

    #[test]
    fn a_path_with_non_utf8_bytes_is_percent_encoded_per_byte() {
        use std::os::unix::ffi::OsStringExt;
        let path = PathBuf::from(std::ffi::OsString::from_vec(b"/tmp/\xff".to_vec()));
        assert_eq!(Selection::uri_of(&path), "file:///tmp/%FF");
    }

    #[test]
    fn a_unicode_name_is_percent_encoded_as_utf8() {
        assert_eq!(
            Selection::uri_of(std::path::Path::new("/tmp/é")),
            "file:///tmp/%C3%A9"
        );
    }

    #[test]
    fn cancelling_answers_one_with_no_results() {
        let (code, results) = ChooserOutcome::Cancelled.into_response();
        assert_eq!(code, ResponseCode::Cancelled);
        assert_eq!(results, Value::Array(vec![]));
    }

    #[test]
    fn failing_answers_two_with_no_results() {
        let (code, results) = ChooserOutcome::Failed.into_response();
        assert_eq!(code, ResponseCode::Other);
        assert_eq!(results, Value::Array(vec![]));
    }

    #[test]
    fn choices_are_returned_only_when_there_are_some() {
        let mut selection = picked(&["/a"]);
        assert!(results_of(selection.clone()).dict_get("choices").is_none());
        selection.choices.push(ChoiceSelection {
            id: "ro".into(),
            selected: "true".into(),
        });
        assert!(results_of(selection).dict_get("choices").is_some());
    }

    #[test]
    fn the_chosen_filter_is_returned_when_it_has_rules() {
        let mut selection = picked(&["/a"]);
        selection.current_filter = Some(FileFilter::new("Text", vec![FilterRule::Glob("*.txt".into())]));
        assert!(results_of(selection).dict_get("current_filter").is_some());
    }

    #[test]
    fn a_filter_without_rules_is_left_out() {
        let mut selection = picked(&["/a"]);
        selection.current_filter = Some(FileFilter::new("Empty", vec![]));
        assert!(results_of(selection).dict_get("current_filter").is_none());
    }

    #[test]
    fn writable_is_returned_only_when_true() {
        let mut selection = picked(&["/a"]);
        assert!(results_of(selection.clone()).dict_get("writable").is_none());
        selection.writable = true;
        assert!(results_of(selection).dict_get("writable").is_some());
    }

    struct VardictReaderProbe;

    impl VardictReaderProbe {
        fn uris(results: &Value) -> Vec<String> {
            crate::vardict::VardictReader::new(results).text_list("uris")
        }
    }
}

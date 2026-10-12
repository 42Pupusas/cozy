use std::path::PathBuf;

use dbus::value::Value;

use crate::chooser_choice::Choice;
use crate::file_filter::FileFilter;
use crate::vardict::{NulTerminatedPath, VardictReader};

/// Which `impl.portal.FileChooser` method was called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChooserMethod {
    OpenFile,
    SaveFile,
    SaveFiles,
}

impl ChooserMethod {
    #[must_use]
    pub const fn member(self) -> &'static str {
        match self {
            Self::OpenFile => "OpenFile",
            Self::SaveFile => "SaveFile",
            Self::SaveFiles => "SaveFiles",
        }
    }
}

/// What the dialog is being asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChooserKind {
    Open {
        multiple: bool,
        directory: bool,
    },
    Save {
        name: Option<String>,
        file: Option<PathBuf>,
    },
    SaveMany {
        names: Vec<PathBuf>,
    },
}

/// A decoded `FileChooser` call: the arguments every method shares, plus the
/// method's own options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChooserRequest {
    pub app_id: String,
    pub parent_window: String,
    pub title: String,
    pub kind: ChooserKind,
    pub accept_label: Option<String>,
    pub modal: bool,
    pub filters: Vec<FileFilter>,
    pub current_filter: Option<FileFilter>,
    pub choices: Vec<Choice>,
    pub current_folder: Option<PathBuf>,
}

impl ChooserRequest {
    const ARGUMENT_COUNT: usize = 5;
    const OPTIONS_INDEX: usize = 4;

    /// Decode the five arguments `(handle, app_id, parent_window, title,
    /// options)`.
    ///
    /// # Errors
    ///
    /// Fails when an argument is missing or has the wrong type.
    pub fn from_call(method: ChooserMethod, args: &[Value]) -> Result<Self, String> {
        if args.len() < Self::ARGUMENT_COUNT {
            return Err(format!(
                "{} expects {} arguments, got {}",
                method.member(),
                Self::ARGUMENT_COUNT,
                args.len()
            ));
        }
        let text = |index: usize, name: &str| {
            args[index]
                .as_display_string()
                .ok_or_else(|| format!("{} argument '{name}' must be a string", method.member()))
        };
        let app_id = text(1, "app_id")?;
        let parent_window = text(2, "parent_window")?;
        let title = text(3, "title")?;
        let options = &args[Self::OPTIONS_INDEX];
        let reader = VardictReader::new(options);
        Ok(Self {
            app_id,
            parent_window,
            title,
            kind: Self::kind_of(method, &reader),
            accept_label: reader.text("accept_label"),
            modal: reader.flag("modal").unwrap_or(true),
            filters: reader
                .value("filters")
                .map_or_else(Vec::new, |v| FileFilter::list_from_value(&v)),
            current_filter: reader
                .value("current_filter")
                .and_then(|v| FileFilter::from_value(&v)),
            choices: reader
                .value("choices")
                .map_or_else(Vec::new, |v| Choice::list_from_value(&v)),
            current_folder: reader.path("current_folder"),
        })
    }

    fn kind_of(method: ChooserMethod, reader: &VardictReader<'_>) -> ChooserKind {
        match method {
            ChooserMethod::OpenFile => ChooserKind::Open {
                multiple: reader.flag("multiple").unwrap_or(false),
                directory: reader.flag("directory").unwrap_or(false),
            },
            ChooserMethod::SaveFile => ChooserKind::Save {
                name: reader.text("current_name"),
                file: reader.path("current_file"),
            },
            ChooserMethod::SaveFiles => ChooserKind::SaveMany {
                names: reader.value("files").map_or_else(Vec::new, |files| {
                    files.as_items().map_or_else(Vec::new, |items| {
                        items.iter().filter_map(NulTerminatedPath::decode).collect()
                    })
                }),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vardict::VardictBuilder;

    struct Call;

    impl Call {
        fn args(options: Value) -> Vec<Value> {
            vec![
                Value::ObjectPath("/r".into()),
                Value::Str("org.example.App".into()),
                Value::Str("x11:1".into()),
                Value::Str("Pick".into()),
                options,
            ]
        }

        fn bytes(text: &[u8]) -> Value {
            Value::Array(text.iter().map(|b| Value::Byte(*b)).collect())
        }
    }

    #[test]
    fn open_defaults_follow_the_spec() {
        let args = Call::args(VardictBuilder::new().build());
        let request = ChooserRequest::from_call(ChooserMethod::OpenFile, &args).expect("request");
        assert_eq!(request.app_id, "org.example.App");
        assert_eq!(request.parent_window, "x11:1");
        assert_eq!(request.title, "Pick");
        assert!(request.modal);
        assert_eq!(
            request.kind,
            ChooserKind::Open {
                multiple: false,
                directory: false
            }
        );
        assert!(request.filters.is_empty());
        assert_eq!(request.current_filter, None);
        assert_eq!(request.current_folder, None);
    }

    #[test]
    fn open_reads_multiple_directory_and_modal() {
        let args = Call::args(
            VardictBuilder::new()
                .entry("multiple", Value::Bool(true))
                .entry("directory", Value::Bool(true))
                .entry("modal", Value::Bool(false))
                .entry("accept_label", Value::Str("_Choose".into()))
                .build(),
        );
        let request = ChooserRequest::from_call(ChooserMethod::OpenFile, &args).expect("request");
        assert!(!request.modal);
        assert_eq!(request.accept_label.as_deref(), Some("_Choose"));
        assert_eq!(
            request.kind,
            ChooserKind::Open {
                multiple: true,
                directory: true
            }
        );
    }

    #[test]
    fn save_reads_name_file_and_folder() {
        let args = Call::args(
            VardictBuilder::new()
                .entry("current_name", Value::Str("notes.txt".into()))
                .entry("current_file", Call::bytes(b"/home/me/notes.txt\0"))
                .entry("current_folder", Call::bytes(b"/home/me\0"))
                .build(),
        );
        let request = ChooserRequest::from_call(ChooserMethod::SaveFile, &args).expect("request");
        assert_eq!(
            request.kind,
            ChooserKind::Save {
                name: Some("notes.txt".into()),
                file: Some(PathBuf::from("/home/me/notes.txt")),
            }
        );
        assert_eq!(request.current_folder, Some(PathBuf::from("/home/me")));
    }

    #[test]
    fn save_files_reads_every_name_in_order() {
        let args = Call::args(
            VardictBuilder::new()
                .entry(
                    "files",
                    Value::Array(vec![Call::bytes(b"a.png\0"), Call::bytes(b"b.png\0")]),
                )
                .build(),
        );
        let request = ChooserRequest::from_call(ChooserMethod::SaveFiles, &args).expect("request");
        assert_eq!(
            request.kind,
            ChooserKind::SaveMany {
                names: vec![PathBuf::from("a.png"), PathBuf::from("b.png")]
            }
        );
    }

    #[test]
    fn filters_and_the_current_filter_are_read() {
        let filter = Value::Struct(vec![
            Value::Str("Text".into()),
            Value::Array(vec![Value::Struct(vec![
                Value::U32(0),
                Value::Str("*.txt".into()),
            ])]),
        ]);
        let args = Call::args(
            VardictBuilder::new()
                .entry("filters", Value::Array(vec![filter.clone()]))
                .entry("current_filter", filter)
                .build(),
        );
        let request = ChooserRequest::from_call(ChooserMethod::OpenFile, &args).expect("request");
        assert_eq!(request.filters.len(), 1);
        assert_eq!(
            request.current_filter.as_ref().map(FileFilter::name),
            Some("Text")
        );
    }

    #[test]
    fn choices_are_read() {
        let choice = Value::Struct(vec![
            Value::Str("ro".into()),
            Value::Str("Read only".into()),
            Value::Array(vec![]),
            Value::Str("false".into()),
        ]);
        let args = Call::args(
            VardictBuilder::new()
                .entry("choices", Value::Array(vec![choice]))
                .build(),
        );
        let request = ChooserRequest::from_call(ChooserMethod::OpenFile, &args).expect("request");
        assert_eq!(request.choices.len(), 1);
        assert!(request.choices[0].is_checkbox());
    }

    #[test]
    fn too_few_arguments_are_refused() {
        let error = ChooserRequest::from_call(ChooserMethod::OpenFile, &[]).unwrap_err();
        assert!(error.contains("OpenFile"), "got {error}");
    }

    #[test]
    fn a_non_string_title_is_refused() {
        let mut args = Call::args(VardictBuilder::new().build());
        args[3] = Value::U32(1);
        let error = ChooserRequest::from_call(ChooserMethod::SaveFile, &args).unwrap_err();
        assert!(error.contains("title"), "got {error}");
    }

    #[test]
    fn the_method_names_match_the_wire() {
        assert_eq!(ChooserMethod::OpenFile.member(), "OpenFile");
        assert_eq!(ChooserMethod::SaveFile.member(), "SaveFile");
        assert_eq!(ChooserMethod::SaveFiles.member(), "SaveFiles");
    }
}

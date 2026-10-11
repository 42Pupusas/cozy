use dbus::value::Value;

/// One selectable option of a [`Choice`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChoiceOption {
    pub id: String,
    pub label: String,
}

/// An extra control the app asks the dialog to show: `(s, s, a(ss), s)`.
///
/// A choice with no options is a checkbox whose value is `"true"` or
/// `"false"`; otherwise it is a combo box over the options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    id: String,
    label: String,
    options: Vec<ChoiceOption>,
    initial: String,
}

impl Choice {
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        options: Vec<ChoiceOption>,
        initial: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            options,
            initial: initial.into(),
        }
    }

    #[must_use]
    pub fn from_value(value: &Value) -> Option<Self> {
        let value = value.clone().into_inner();
        let [
            Value::Str(id),
            Value::Str(label),
            options,
            Value::Str(initial),
        ] = value.as_items()?
        else {
            return None;
        };
        let options = options
            .as_items()?
            .iter()
            .filter_map(|option| match option.as_items()? {
                [Value::Str(id), Value::Str(label)] => Some(ChoiceOption {
                    id: id.clone(),
                    label: label.clone(),
                }),
                _ => None,
            })
            .collect();
        Some(Self {
            id: id.clone(),
            label: label.clone(),
            options,
            initial: initial.clone(),
        })
    }

    #[must_use]
    pub fn list_from_value(value: &Value) -> Vec<Self> {
        value
            .clone()
            .into_inner()
            .as_items()
            .map_or_else(Vec::new, |items| {
                items.iter().filter_map(Self::from_value).collect()
            })
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    #[must_use]
    pub fn options(&self) -> &[ChoiceOption] {
        &self.options
    }

    #[must_use]
    pub const fn is_checkbox(&self) -> bool {
        self.options.is_empty()
    }

    /// The selection to start from: the app's initial value when it names a
    /// real option (or a checkbox state), otherwise the first option.
    #[must_use]
    pub fn starting_selection(&self) -> String {
        if self.is_checkbox() {
            return if self.initial == "true" { "true" } else { "false" }.to_owned();
        }
        if self.options.iter().any(|option| option.id == self.initial) {
            return self.initial.clone();
        }
        self.options
            .first()
            .map_or_else(String::new, |option| option.id.clone())
    }
}

/// What the user left a [`Choice`] set to: `(s, s)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChoiceSelection {
    pub id: String,
    pub selected: String,
}

impl ChoiceSelection {
    #[must_use]
    pub fn to_value(&self) -> Value {
        Value::Struct(vec![
            Value::Str(self.id.clone()),
            Value::Str(self.selected.clone()),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Wire;

    impl Wire {
        fn choice(id: &str, label: &str, options: &[(&str, &str)], initial: &str) -> Value {
            Value::Struct(vec![
                Value::Str(id.into()),
                Value::Str(label.into()),
                Value::Array(
                    options
                        .iter()
                        .map(|(id, label)| {
                            Value::Struct(vec![
                                Value::Str((*id).into()),
                                Value::Str((*label).into()),
                            ])
                        })
                        .collect(),
                ),
                Value::Str(initial.into()),
            ])
        }
    }

    #[test]
    fn a_combo_choice_is_read_with_its_options() {
        let wire = Wire::choice("enc", "Encoding", &[("u8", "UTF-8"), ("l1", "Latin-1")], "l1");
        let choice = Choice::from_value(&wire).expect("choice");
        assert_eq!(choice.id(), "enc");
        assert_eq!(choice.label(), "Encoding");
        assert_eq!(choice.options().len(), 2);
        assert!(!choice.is_checkbox());
        assert_eq!(choice.starting_selection(), "l1");
    }

    #[test]
    fn a_choice_without_options_is_a_checkbox() {
        let wire = Wire::choice("ro", "Read only", &[], "true");
        let choice = Choice::from_value(&wire).expect("choice");
        assert!(choice.is_checkbox());
        assert_eq!(choice.starting_selection(), "true");
    }

    #[test]
    fn a_checkbox_with_an_odd_initial_value_starts_unchecked() {
        let wire = Wire::choice("ro", "Read only", &[], "yes");
        let choice = Choice::from_value(&wire).expect("choice");
        assert_eq!(choice.starting_selection(), "false");
    }

    #[test]
    fn an_initial_value_naming_no_option_falls_back_to_the_first() {
        let wire = Wire::choice("enc", "Encoding", &[("u8", "UTF-8"), ("l1", "Latin-1")], "zz");
        let choice = Choice::from_value(&wire).expect("choice");
        assert_eq!(choice.starting_selection(), "u8");
    }

    #[test]
    fn a_variant_wrapped_choice_is_read() {
        let wire = Value::Variant(Box::new(Wire::choice("a", "A", &[], "false")));
        assert!(Choice::from_value(&wire).is_some());
    }

    #[test]
    fn a_malformed_choice_is_rejected() {
        assert_eq!(Choice::from_value(&Value::Str("x".into())), None);
        assert_eq!(
            Choice::from_value(&Value::Struct(vec![Value::Str("only".into())])),
            None
        );
    }

    #[test]
    fn list_from_value_skips_malformed_entries() {
        let wire = Value::Array(vec![
            Wire::choice("a", "A", &[], "false"),
            Value::U32(1),
            Wire::choice("b", "B", &[("x", "X")], "x"),
        ]);
        let ids: Vec<_> = Choice::list_from_value(&wire)
            .iter()
            .map(|choice| choice.id().to_owned())
            .collect();
        assert_eq!(ids, ["a", "b"]);
    }

    #[test]
    fn a_selection_goes_on_the_wire_as_a_string_pair() {
        let selection = ChoiceSelection {
            id: "enc".into(),
            selected: "u8".into(),
        };
        assert_eq!(
            selection.to_value(),
            Value::Struct(vec![Value::Str("enc".into()), Value::Str("u8".into())])
        );
    }
}

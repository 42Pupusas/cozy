//! The message `impl.portal.Email.ComposeEmail` asks a mail client to open,
//! and the `mailto:` URL that expresses it.

use dbus::value::Value;

use crate::uri_component::UriComponent;

/// A message to compose, read from a `ComposeEmail` options vardict.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EmailMessage {
    to: Vec<String>,
    cc: Vec<String>,
    bcc: Vec<String>,
    subject: Option<String>,
    body: Option<String>,
    attachments: Vec<String>,
    activation_token: Option<String>,
}

impl EmailMessage {
    const ADDRESS_KEEP: &'static [u8] = b"@+";
    const ATTACHMENT_KEEP: &'static [u8] = b"%:/@";

    /// Read the documented keys out of an `a{sv}` options value.
    ///
    /// `address` comes first, followed by `addresses`. Keys of the wrong
    /// type and empty strings are treated as absent.
    #[must_use]
    pub fn from_options(options: &Value) -> Self {
        let mut to = Self::text_option(options, "address")
            .into_iter()
            .collect::<Vec<_>>();
        to.extend(Self::list_option(options, "addresses"));
        Self {
            to,
            cc: Self::list_option(options, "cc"),
            bcc: Self::list_option(options, "bcc"),
            subject: Self::text_option(options, "subject"),
            body: Self::text_option(options, "body"),
            attachments: Self::list_option(options, "attachments"),
            activation_token: Self::text_option(options, "activation_token"),
        }
    }

    /// The token that lets the launched mail client raise its window.
    #[must_use]
    pub fn activation_token(&self) -> Option<&str> {
        self.activation_token.as_deref()
    }

    /// The `mailto:` URL for this message.
    ///
    /// Parameters with no value are left out.
    #[must_use]
    pub fn mailto_url(&self) -> String {
        let mut url = format!("mailto:{}", Self::address_list(&self.to));
        let mut parameters = Vec::new();
        if !self.cc.is_empty() {
            parameters.push(format!("cc={}", Self::address_list(&self.cc)));
        }
        if !self.bcc.is_empty() {
            parameters.push(format!("bcc={}", Self::address_list(&self.bcc)));
        }
        if let Some(subject) = &self.subject {
            parameters.push(format!("subject={}", UriComponent::encode(subject, b"")));
        }
        if let Some(body) = &self.body {
            parameters.push(format!("body={}", UriComponent::encode(body, b"")));
        }
        for attachment in &self.attachments {
            parameters.push(format!(
                "attachment={}",
                UriComponent::encode(attachment, Self::ATTACHMENT_KEEP)
            ));
        }
        if !parameters.is_empty() {
            url.push('?');
            url.push_str(&parameters.join("&"));
        }
        url
    }

    fn address_list(addresses: &[String]) -> String {
        addresses
            .iter()
            .map(|address| UriComponent::encode(address, Self::ADDRESS_KEEP))
            .collect::<Vec<_>>()
            .join(",")
    }

    fn text_option(options: &Value, key: &str) -> Option<String> {
        options
            .dict_get(key)
            .and_then(Value::as_display_string)
            .filter(|text| !text.is_empty())
    }

    fn list_option(options: &Value, key: &str) -> Vec<String> {
        let Some(value) = options.dict_get(key) else {
            return Vec::new();
        };
        let value = value.clone().into_inner();
        value.as_items().map_or_else(Vec::new, |items| {
            items
                .iter()
                .filter_map(Value::as_display_string)
                .filter(|text| !text.is_empty())
                .collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Options(Vec<Value>);

    impl Options {
        fn new() -> Self {
            Self(Vec::new())
        }

        fn text(mut self, key: &str, text: &str) -> Self {
            self.0.push(Self::entry(key, Value::Str(text.into())));
            self
        }

        fn list(mut self, key: &str, items: &[&str]) -> Self {
            let items = items.iter().map(|i| Value::Str((*i).into())).collect();
            self.0.push(Self::entry(key, Value::Array(items)));
            self
        }

        fn raw(mut self, key: &str, value: Value) -> Self {
            self.0.push(Self::entry(key, value));
            self
        }

        fn build(self) -> Value {
            Value::Array(self.0)
        }

        fn entry(key: &str, value: Value) -> Value {
            Value::DictEntry(
                Box::new(Value::Str(key.into())),
                Box::new(Value::Variant(Box::new(value))),
            )
        }
    }

    #[test]
    fn no_options_is_a_bare_mailto() {
        let message = EmailMessage::from_options(&Options::new().build());
        assert_eq!(message.mailto_url(), "mailto:");
    }

    #[test]
    fn one_address_is_the_recipient() {
        let options = Options::new().text("address", "a@example.com").build();
        let message = EmailMessage::from_options(&options);
        assert_eq!(message.mailto_url(), "mailto:a@example.com");
    }

    #[test]
    fn address_precedes_addresses() {
        let options = Options::new()
            .list("addresses", &["b@example.com", "c@example.com"])
            .text("address", "a@example.com")
            .build();
        let message = EmailMessage::from_options(&options);
        assert_eq!(
            message.mailto_url(),
            "mailto:a@example.com,b@example.com,c@example.com"
        );
    }

    #[test]
    fn cc_and_bcc_become_parameters() {
        let options = Options::new()
            .text("address", "a@example.com")
            .list("cc", &["b@example.com"])
            .list("bcc", &["c@example.com", "d@example.com"])
            .build();
        let message = EmailMessage::from_options(&options);
        assert_eq!(
            message.mailto_url(),
            "mailto:a@example.com?cc=b@example.com&bcc=c@example.com,d@example.com"
        );
    }

    #[test]
    fn subject_and_body_are_percent_encoded() {
        let options = Options::new()
            .text("subject", "Q&A: 100% sure?")
            .text("body", "line one\nline two")
            .build();
        let message = EmailMessage::from_options(&options);
        assert_eq!(
            message.mailto_url(),
            "mailto:?subject=Q%26A%3A%20100%25%20sure%3F&body=line%20one%0Aline%20two"
        );
    }

    #[test]
    fn empty_subject_and_body_are_left_out() {
        let options = Options::new()
            .text("address", "a@example.com")
            .text("subject", "")
            .text("body", "")
            .build();
        let message = EmailMessage::from_options(&options);
        assert_eq!(message.mailto_url(), "mailto:a@example.com");
    }

    #[test]
    fn each_attachment_is_its_own_parameter_with_existing_escapes_kept() {
        let options = Options::new()
            .list(
                "attachments",
                &["file:///tmp/a%20b.txt", "file:///tmp/c&d.txt"],
            )
            .build();
        let message = EmailMessage::from_options(&options);
        assert_eq!(
            message.mailto_url(),
            "mailto:?attachment=file:///tmp/a%20b.txt&attachment=file:///tmp/c%26d.txt"
        );
    }

    #[test]
    fn an_address_with_a_display_name_is_encoded() {
        let options = Options::new()
            .text("address", "Ann, Lee <a@example.com>")
            .build();
        let message = EmailMessage::from_options(&options);
        assert_eq!(
            message.mailto_url(),
            "mailto:Ann%2C%20Lee%20%3Ca@example.com%3E"
        );
    }

    #[test]
    fn a_plus_in_an_address_is_kept() {
        let options = Options::new().text("address", "a+tag@example.com").build();
        let message = EmailMessage::from_options(&options);
        assert_eq!(message.mailto_url(), "mailto:a+tag@example.com");
    }

    #[test]
    fn a_value_of_the_wrong_type_is_ignored() {
        let options = Options::new()
            .raw("subject", Value::U32(7))
            .raw("cc", Value::Str("not-a-list".into()))
            .build();
        let message = EmailMessage::from_options(&options);
        assert_eq!(message.mailto_url(), "mailto:");
    }

    #[test]
    fn the_activation_token_is_exposed_and_kept_out_of_the_url() {
        let options = Options::new().text("activation_token", "tok-1").build();
        let message = EmailMessage::from_options(&options);
        assert_eq!(message.activation_token(), Some("tok-1"));
        assert_eq!(message.mailto_url(), "mailto:");
    }

    #[test]
    fn an_empty_activation_token_is_absent() {
        let options = Options::new().text("activation_token", "").build();
        let message = EmailMessage::from_options(&options);
        assert_eq!(message.activation_token(), None);
    }
}

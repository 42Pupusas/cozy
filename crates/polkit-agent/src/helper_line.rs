//! One line of the polkit helper's PAM conversation.
//!
//! The helper speaks newline-delimited text. Each line is a prefix and a
//! payload, and the payload is C-escaped, so `\n` arrives as a literal
//! backslash followed by `n` and must be unescaped before it is shown.
//!
//! The prefixes come from polkit's own `polkitagentsession.c`:
//!
//! ```text
//! PAM_PROMPT_ECHO_OFF <text>   ask, and do not echo (a password)
//! PAM_PROMPT_ECHO_ON <text>    ask, and echo (a username)
//! PAM_ERROR_MSG <text>         show an error
//! PAM_TEXT_INFO <text>         show a notice
//! SUCCESS                      authentication succeeded
//! FAILURE                      authentication failed
//! ```

/// A single decoded line from the helper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelperLine {
    /// Ask the user for `prompt`. `echo` is false for a password, which must
    /// never be drawn to the screen.
    Prompt { prompt: String, echo: bool },
    /// Show an error to the user.
    Error(String),
    /// Show an informational notice to the user.
    Info(String),
    /// The conversation ended successfully.
    Success,
    /// The conversation ended in failure.
    Failure,
}

impl HelperLine {
    /// Parse one line, or `None` if the helper said something unknown.
    ///
    /// An unknown line is deliberately not an error variant: polkit treats it
    /// as a failed session, and so must the caller, but the distinction
    /// between "said FAILURE" and "said gibberish" is worth keeping.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let line = line.strip_suffix('\n').unwrap_or(line);
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(rest) = line.strip_prefix("PAM_PROMPT_ECHO_OFF ") {
            return Some(Self::Prompt {
                prompt: Self::unescape(rest),
                echo: false,
            });
        }
        if let Some(rest) = line.strip_prefix("PAM_PROMPT_ECHO_ON ") {
            return Some(Self::Prompt {
                prompt: Self::unescape(rest),
                echo: true,
            });
        }
        if let Some(rest) = line.strip_prefix("PAM_ERROR_MSG ") {
            return Some(Self::Error(Self::unescape(rest)));
        }
        if let Some(rest) = line.strip_prefix("PAM_TEXT_INFO ") {
            return Some(Self::Info(Self::unescape(rest)));
        }
        if line.starts_with("SUCCESS") {
            return Some(Self::Success);
        }
        if line.starts_with("FAILURE") {
            return Some(Self::Failure);
        }
        None
    }

    /// Whether this line ends the conversation.
    #[must_use]
    pub const fn is_final(&self) -> bool {
        matches!(self, Self::Success | Self::Failure)
    }

    /// Whether this line asks for a secret that must not be echoed.
    #[must_use]
    pub const fn is_secret_prompt(&self) -> bool {
        matches!(self, Self::Prompt { echo: false, .. })
    }

    /// Undo the C escaping glib's `g_strcompress` applies, which is what
    /// polkit uses on the other side.
    fn unescape(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut chars = text.chars();
        while let Some(ch) = chars.next() {
            if ch != '\\' {
                out.push(ch);
                continue;
            }
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('b') => out.push('\u{8}'),
                Some('f') => out.push('\u{c}'),
                Some('v') => out.push('\u{b}'),
                Some('\\') | None => out.push('\\'),
                Some(other) => out.push(other),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The line this machine's helper actually sends first, captured from a
    /// live socket by `agent-probe`.
    #[test]
    fn the_real_password_prompt_parses() {
        let line = HelperLine::parse("PAM_PROMPT_ECHO_OFF Password: ").expect("a known line");
        assert_eq!(
            line,
            HelperLine::Prompt {
                prompt: "Password: ".into(),
                echo: false,
            }
        );
    }

    /// The distinction that keeps a password off the screen.
    #[test]
    fn echo_off_is_a_secret_and_echo_on_is_not() {
        let secret = HelperLine::parse("PAM_PROMPT_ECHO_OFF Password: ").expect("known");
        let plain = HelperLine::parse("PAM_PROMPT_ECHO_ON Username: ").expect("known");
        assert!(secret.is_secret_prompt(), "a password must never echo");
        assert!(!plain.is_secret_prompt());
        assert_eq!(
            plain,
            HelperLine::Prompt {
                prompt: "Username: ".into(),
                echo: true,
            }
        );
    }

    /// The two terminal lines must be recognised, or the caller waits forever.
    #[test]
    fn success_and_failure_end_the_conversation() {
        let success = HelperLine::parse("SUCCESS").expect("known");
        let failure = HelperLine::parse("FAILURE").expect("known");
        assert_eq!(success, HelperLine::Success);
        assert_eq!(failure, HelperLine::Failure);
        assert!(success.is_final());
        assert!(failure.is_final());
    }

    /// A prompt is not final; treating it as final would abandon a live
    /// authentication.
    #[test]
    fn a_prompt_does_not_end_the_conversation() {
        let line = HelperLine::parse("PAM_PROMPT_ECHO_OFF Password: ").expect("known");
        assert!(!line.is_final());
        assert!(!HelperLine::parse("PAM_TEXT_INFO hello").expect("known").is_final());
    }

    /// Errors and notices carry their text for display.
    #[test]
    fn errors_and_notices_keep_their_text() {
        assert_eq!(
            HelperLine::parse("PAM_ERROR_MSG Authentication failure").expect("known"),
            HelperLine::Error("Authentication failure".into())
        );
        assert_eq!(
            HelperLine::parse("PAM_TEXT_INFO Password expires soon").expect("known"),
            HelperLine::Info("Password expires soon".into())
        );
    }

    /// The escaping is real: a multi-line PAM message arrives escaped and
    /// must be restored, or the prompt shows a literal backslash-n.
    #[test]
    fn escaped_text_is_restored() {
        let line = HelperLine::parse("PAM_ERROR_MSG first\\nsecond\\ttab").expect("known");
        assert_eq!(line, HelperLine::Error("first\nsecond\ttab".into()));
    }

    /// A literal backslash survives its own escape.
    #[test]
    fn an_escaped_backslash_becomes_one_backslash() {
        let line = HelperLine::parse("PAM_TEXT_INFO back\\\\slash").expect("known");
        assert_eq!(line, HelperLine::Info("back\\slash".into()));
    }

    /// A trailing newline from the socket must not become part of the text.
    #[test]
    fn the_line_terminator_is_stripped() {
        let line = HelperLine::parse("PAM_PROMPT_ECHO_OFF Password: \n").expect("known");
        assert_eq!(
            line,
            HelperLine::Prompt {
                prompt: "Password: ".into(),
                echo: false,
            }
        );
    }

    /// Anything unrecognised must be reported as unknown rather than guessed
    /// at -- guessing here could mean treating a failure as a success.
    #[test]
    fn an_unknown_line_is_refused() {
        assert!(HelperLine::parse("WHAT IS THIS").is_none());
        assert!(HelperLine::parse("").is_none());
        assert!(HelperLine::parse("PAM_PROMPT_ECHO_OFF").is_none());
    }

    /// A prefix must not match a longer word by accident.
    #[test]
    fn a_prompt_prefix_requires_its_separating_space() {
        assert!(HelperLine::parse("PAM_ERROR_MSGX oops").is_none());
    }

    /// An empty payload after the space is legal and must not be dropped.
    #[test]
    fn an_empty_payload_is_still_a_line() {
        assert_eq!(
            HelperLine::parse("PAM_ERROR_MSG ").expect("known"),
            HelperLine::Error(String::new())
        );
    }

    /// A dangling backslash at the end must not panic.
    #[test]
    fn a_trailing_backslash_is_harmless() {
        let line = HelperLine::parse("PAM_TEXT_INFO ends\\").expect("known");
        assert_eq!(line, HelperLine::Info("ends\\".into()));
    }
}

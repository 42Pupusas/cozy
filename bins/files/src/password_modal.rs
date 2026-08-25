//! [`PasswordModal`] -- the prompt polkit provokes when a mount needs a
//! password.
//!
//! # What this owns, and what it refuses to own
//!
//! It owns the *prompt session*: the question polkit asked, the secret typed
//! so far, any notice the helper sent back, and whether the modal is open at
//! all. It performs no authentication, draws nothing, and never touches a
//! bus. `window.rs` paints it, `browser.rs` hands the answer to the backend.
//!
//! Modelled on [`crate::rename::Rename`] -- `None` inside means closed, and
//! every method is a no-op in that state rather than a panic, so the window
//! routes keys through unconditionally and asks afterwards what happened.
//!
//! # Why the prompt text comes from polkit
//!
//! The wording is **not ours to write**. polkit chooses it, it is localised,
//! and it may name the specific administrator whose password is wanted -- on
//! a machine with several admins, "Password:" alone is a question the user
//! cannot answer correctly. So the text is stored verbatim and displayed
//! verbatim.
//!
//! # The echo flag is obeyed, not assumed
//!
//! PAM says per-message whether a reply may be shown. Nearly always it is
//! `false` and the field draws bullets, but a conversation can legitimately
//! ask for something visible -- a one-time token, a username. Hard-coding
//! concealment would mask a field where the user needs to see typos, so
//! [`Self::echo`] carries the flag through and [`Self::display_text`] obeys
//! it.
//!
//! Concealment is a display decision only. The buffer is a
//! [`SecretInput`] in both cases, because an echoed reply is still a reply
//! to an authentication conversation and there is no reason to leave it in
//! freed heap.
//!
//! # Dismissal is a normal outcome
//!
//! Escape closes the modal and answers `None`, which the backend reports as
//! [`MountOutcome::Cancelled`] rather than a denial. A user who changes their
//! mind has not failed anything, and must not be told they lack a permission
//! they hold.

use crate::secret_input::SecretInput;

/// How a closed prompt ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptOutcome {
    /// The user submitted a reply.
    Answered,
    /// The user dismissed the prompt.
    Dismissed,
}

/// One open password prompt.
#[derive(Debug, Default)]
pub struct PasswordModal {
    session: Option<Session>,
}

#[derive(Debug)]
struct Session {
    /// The question, exactly as polkit worded it.
    prompt: String,
    /// Whether the reply may be shown on screen.
    echo: bool,
    /// The reply being typed.
    input: SecretInput,
    /// The most recent word from the helper, e.g. "2 attempts remaining".
    notice: Option<String>,
}

impl PasswordModal {
    /// Shown above the prompt so the user knows what is asking.
    pub const TITLE: &'static str = "Authentication required";

    /// A closed modal.
    #[must_use]
    pub const fn new() -> Self {
        Self { session: None }
    }

    /// Open a prompt asking `prompt`, echoing the reply if `echo`.
    ///
    /// Opening while one is already open **replaces** it and discards the
    /// half-typed reply. That is the correct response to a second question:
    /// a conversation asks in sequence, so a new question means the previous
    /// answer was already sent or abandoned, and carrying its text over would
    /// submit it as the answer to a different question.
    pub fn open(&mut self, prompt: &str, echo: bool) {
        self.session = Some(Session {
            prompt: prompt.to_owned(),
            echo,
            input: SecretInput::new(),
            notice: None,
        });
    }

    /// Whether a prompt is open.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.session.is_some()
    }

    /// The question polkit asked, if one is open.
    #[must_use]
    pub fn prompt(&self) -> Option<&str> {
        self.session.as_ref().map(|s| s.prompt.as_str())
    }

    /// Whether the reply may be shown on screen.
    ///
    /// False when nothing is open: with no session there is nothing to echo,
    /// and the safe answer to "may I show this" is no.
    #[must_use]
    pub fn echo(&self) -> bool {
        self.session.as_ref().is_some_and(|s| s.echo)
    }

    /// What the painter draws in the field.
    ///
    /// Bullets unless the conversation asked for an echoed reply. This is the
    /// only text the painter is given; there is no path from the painter to
    /// the plaintext of a concealed field.
    #[must_use]
    pub fn display_text(&self) -> String {
        self.session.as_ref().map_or_else(String::new, |s| {
            if s.echo {
                s.input.expose().to_owned()
            } else {
                s.input.masked()
            }
        })
    }

    /// How many characters have been typed, for placing a caret.
    #[must_use]
    pub fn typed_count(&self) -> usize {
        self.session.as_ref().map_or(0, |s| s.input.char_count())
    }

    /// Whether anything has been typed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.session.as_ref().is_none_or(|s| s.input.is_empty())
    }

    /// The helper's most recent notice, if any.
    #[must_use]
    pub fn notice(&self) -> Option<&str> {
        self.session.as_ref().and_then(|s| s.notice.as_deref())
    }

    /// Record a notice from the authentication helper.
    ///
    /// Kept because "2 attempts remaining" is the only warning a user gets
    /// before the account locks, and it arrives *between* prompts.
    pub fn set_notice(&mut self, message: &str) {
        if let Some(session) = self.session.as_mut() {
            session.notice = Some(message.to_owned());
        }
    }

    /// Type one character, reporting whether the field changed.
    pub fn insert(&mut self, c: char) -> bool {
        self.session.as_mut().is_some_and(|s| s.input.insert(c))
    }

    /// Erase the character before the caret, reporting whether it changed.
    pub fn backspace(&mut self) -> bool {
        self.session.as_mut().is_some_and(|s| s.input.backspace())
    }

    /// Erase everything typed, keeping the prompt open.
    pub fn clear(&mut self) -> bool {
        let Some(session) = self.session.as_mut() else {
            return false;
        };
        let had = !session.input.is_empty();
        session.input.clear();
        had
    }

    /// Close the prompt and yield the reply.
    ///
    /// An **empty reply is submitted**, not refused. An empty password is a
    /// legal, if unwise, password, and refusing to send it would hang the
    /// conversation on a modal that appears to do nothing when Enter is
    /// pressed. Let the helper reject it.
    pub fn submit(&mut self) -> Option<String> {
        let mut session = self.session.take()?;
        Some(session.input.take())
    }

    /// Close the prompt, discarding the reply and zeroing its buffer.
    ///
    /// Returns whether a prompt was actually open, so the window repaints
    /// only when something closed.
    pub fn dismiss(&mut self) -> bool {
        self.session.take().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::{PasswordModal, PromptOutcome};

    fn opened() -> PasswordModal {
        let mut modal = PasswordModal::new();
        modal.open("Password: ", false);
        modal
    }

    fn type_text(modal: &mut PasswordModal, text: &str) {
        for c in text.chars() {
            modal.insert(c);
        }
    }

    #[test]
    fn a_fresh_modal_is_closed() {
        let modal = PasswordModal::new();
        assert!(!modal.is_open());
        assert_eq!(modal.prompt(), None);
        assert_eq!(modal.display_text(), "");
    }

    /// polkit chooses the wording and it may be localised or name a specific
    /// administrator, so it must reach the screen unchanged.
    #[test]
    fn the_prompt_text_is_kept_verbatim() {
        let mut modal = PasswordModal::new();
        modal.open("Contraseña para root: ", false);
        assert!(modal.is_open());
        assert_eq!(modal.prompt(), Some("Contraseña para root: "));
    }

    /// The whole point: a password must not appear on screen.
    #[test]
    fn a_concealed_reply_paints_as_bullets() {
        let mut modal = opened();
        type_text(&mut modal, "hunter2");
        assert!(!modal.echo());
        assert_eq!(modal.display_text(), "•••••••");
        assert!(!modal.display_text().contains('h'));
    }

    /// PAM can legitimately ask for something visible, such as a one-time
    /// token; masking it would hide typos the user needs to see.
    #[test]
    fn an_echoed_reply_paints_as_typed() {
        let mut modal = PasswordModal::new();
        modal.open("One-time token: ", true);
        type_text(&mut modal, "482913");
        assert!(modal.echo());
        assert_eq!(modal.display_text(), "482913");
    }

    /// A closed modal must never claim a reply may be shown.
    #[test]
    fn a_closed_modal_does_not_echo() {
        let modal = PasswordModal::new();
        assert!(!modal.echo());
    }

    #[test]
    fn typing_accumulates_and_reports_a_change() {
        let mut modal = opened();
        assert!(modal.insert('a'));
        assert!(!modal.is_empty());
        assert_eq!(modal.typed_count(), 1);
    }

    /// The caret must count characters, or a CJK passphrase would place it
    /// three cells along per keystroke and leak the composition.
    #[test]
    fn the_typed_count_is_characters_not_bytes() {
        let mut modal = opened();
        type_text(&mut modal, "密码");
        assert_eq!(modal.typed_count(), 2);
        assert_eq!(modal.display_text().chars().count(), 2);
    }

    #[test]
    fn backspace_removes_one_character() {
        let mut modal = opened();
        type_text(&mut modal, "abc");
        assert!(modal.backspace());
        assert_eq!(modal.typed_count(), 2);
    }

    /// Keys routed at a closed modal must be inert, not a panic: the window
    /// routes unconditionally and asks afterwards.
    #[test]
    fn keys_are_inert_while_the_modal_is_closed() {
        let mut modal = PasswordModal::new();
        assert!(!modal.insert('a'));
        assert!(!modal.backspace());
        assert!(!modal.clear());
        assert!(!modal.dismiss());
        assert_eq!(modal.submit(), None);
        assert!(modal.is_empty());
    }

    #[test]
    fn submitting_yields_the_reply_and_closes() {
        let mut modal = opened();
        type_text(&mut modal, "hunter2");
        assert_eq!(modal.submit(), Some(String::from("hunter2")));
        assert!(!modal.is_open(), "submitting closes the prompt");
    }

    /// An empty password is legal. Refusing to submit it would make Enter
    /// appear to do nothing, with no way forward.
    #[test]
    fn an_empty_reply_is_submitted_rather_than_refused() {
        let mut modal = opened();
        assert_eq!(modal.submit(), Some(String::new()));
        assert!(!modal.is_open());
    }

    /// Dismissal is a normal outcome, and must yield no reply at all -- the
    /// backend turns this into a cancellation rather than a denial.
    #[test]
    fn dismissing_closes_without_a_reply() {
        let mut modal = opened();
        type_text(&mut modal, "hunter2");
        assert!(modal.dismiss());
        assert!(!modal.is_open());
        assert_eq!(modal.submit(), None);
    }

    #[test]
    fn dismissing_a_closed_modal_reports_no_change() {
        let mut modal = PasswordModal::new();
        assert!(!modal.dismiss());
    }

    #[test]
    fn clearing_keeps_the_prompt_open() {
        let mut modal = opened();
        type_text(&mut modal, "wrong");
        assert!(modal.clear());
        assert!(modal.is_open(), "the user retypes into the same prompt");
        assert!(modal.is_empty());
        assert!(!modal.clear(), "clearing an empty field changes nothing");
    }

    /// "2 attempts remaining" is the only warning before an account locks,
    /// and it arrives between prompts.
    #[test]
    fn a_notice_from_the_helper_is_kept_for_display() {
        let mut modal = opened();
        assert_eq!(modal.notice(), None);
        modal.set_notice("2 attempts remaining");
        assert_eq!(modal.notice(), Some("2 attempts remaining"));
    }

    /// A second question means the previous answer was already sent or
    /// abandoned; carrying its text over would answer the wrong question.
    #[test]
    fn a_second_prompt_replaces_the_first_and_discards_its_text() {
        let mut modal = opened();
        type_text(&mut modal, "first");
        modal.set_notice("try again");
        modal.open("Password: ", false);
        assert!(modal.is_empty(), "the previous reply must not carry over");
        assert_eq!(modal.notice(), None, "nor the previous notice");
    }

    /// A password field must not store Enter or Escape as text: they are
    /// commands, and a control byte in a password guarantees a failure.
    #[test]
    fn control_characters_are_not_typed_into_the_reply() {
        let mut modal = opened();
        for c in ['\n', '\r', '\t', '\u{1b}'] {
            assert!(!modal.insert(c), "{c:?} must be refused");
        }
        assert!(modal.is_empty());
    }

    /// A passphrase may contain spaces, unlike the filter box where space is
    /// a selection gesture.
    #[test]
    fn a_space_is_typed_into_the_reply() {
        let mut modal = opened();
        type_text(&mut modal, "correct horse battery");
        assert_eq!(modal.typed_count(), 21);
    }

    /// The reply must not reach a log line through the modal either.
    #[test]
    fn the_debug_output_does_not_contain_the_reply() {
        let mut modal = opened();
        type_text(&mut modal, "hunter2");
        let shown = format!("{modal:?}");
        assert!(!shown.contains("hunter2"), "got {shown}");
    }

    #[test]
    fn an_outcome_distinguishes_an_answer_from_a_dismissal() {
        assert_ne!(PromptOutcome::Answered, PromptOutcome::Dismissed);
    }
}

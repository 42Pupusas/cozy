//! [`SecretInput`] -- a text field for something that must not be echoed,
//! copied, or left behind in freed memory.
//!
//! # Why not [`widget::TextInput`]
//!
//! `TextInput` is the right field for a filename. It is the wrong one for a
//! password, and not because of the caret:
//!
//! * Its buffer is a `String`, which **reallocates** as it grows. The old
//!   allocation is freed with the password still in it, so a twelve-character
//!   password can leave several plaintext copies scattered through the heap,
//!   none of which anything will ever overwrite on purpose.
//! * Nothing erases the contents when the field goes away. A dropped `String`
//!   returns its bytes to the allocator as-is, to be handed to whatever asks
//!   next.
//! * Its whole reason for existing is to be *rendered*, and the renderer is
//!   handed `text()`.
//!
//! This field is the opposite trade in every one of those. It allocates its
//! buffer **once** at [`Self::CAPACITY`] and never grows it, so the bytes
//! never move. It zeroes that buffer on drop and on [`Self::clear`]. It has
//! no accessor that returns the secret for painting -- the painter is given
//! [`Self::masked`], which is bullets and a length and nothing else.
//!
//! # What this does not claim
//!
//! It is not a defence against a process that can read this one's memory: at
//! that point the password is readable while it is being typed, whatever we
//! do. It narrows the *window* to the time the field is open, and keeps the
//! secret out of freed heap, out of a `Debug` line, and out of the painter.
//!
//! Swapping is a separate matter -- pinning the pages needs `mlock(2)` and a
//! syscall this binary has no other reason to make. It is worth doing and it
//! is not done here.
//!
//! # Refusing rather than truncating
//!
//! A full field ignores further characters instead of dropping the earliest.
//! Silently truncating a password produces an authentication failure the user
//! cannot explain, and they will retype the same thing and fail again.

/// A fixed-capacity, self-erasing text field for a secret.
///
/// No `Clone`: every copy is another plaintext buffer to erase, and nothing
/// here needs one. No `Debug` that reveals the contents -- see the manual
/// impl below.
pub struct SecretInput {
    /// The typed bytes. Allocated once at [`Self::CAPACITY`]; `len` is the
    /// used prefix, and the tail is always zero.
    buffer: Vec<u8>,
    /// Bytes used in `buffer`. Always `<= CAPACITY` and always on a UTF-8
    /// character boundary.
    len: usize,
}

impl SecretInput {
    /// The longest secret accepted, in bytes.
    ///
    /// Generous enough for a passphrase rather than a password: the limit
    /// exists to keep the allocation fixed, not to police what the user
    /// chooses. `pam_unix` truncates far shorter than this, but other modules
    /// do not, so the cap is ours and deliberately not a policy.
    pub const CAPACITY: usize = 512;

    /// The character shown in place of each typed one.
    pub const BULLET: char = '•';

    /// An empty field with its buffer already allocated.
    #[must_use]
    pub fn new() -> Self {
        Self {
            buffer: vec![0; Self::CAPACITY],
            len: 0,
        }
    }

    /// Whether nothing has been typed.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// How many bytes are held.
    ///
    /// Bytes, not characters -- the capacity limit is a byte limit, so this
    /// is what the overflow tests measure. Nothing on screen is derived from
    /// it; the painter counts characters (see [`Self::char_count`]).
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// How many characters are held -- what the painter draws bullets for.
    ///
    /// Counted in characters, not bytes, so a password of accented or CJK
    /// text shows one bullet per keystroke rather than two or three, which
    /// would leak its composition.
    #[must_use]
    pub fn char_count(&self) -> usize {
        self.as_str().chars().count()
    }

    /// The field as the painter must see it: one bullet per character.
    ///
    /// This is the only rendering path. There is deliberately no method that
    /// hands the plaintext to a painter.
    #[must_use]
    pub fn masked(&self) -> String {
        std::iter::repeat_n(Self::BULLET, self.char_count()).collect()
    }

    /// Append one character, reporting whether it was accepted.
    ///
    /// Refuses control characters -- a password field receives Enter, Tab and
    /// Escape as *commands*, and storing them as text would put a control
    /// byte into the secret. Refuses anything that would exceed the capacity,
    /// rather than truncating.
    pub fn insert(&mut self, c: char) -> bool {
        if c.is_control() {
            return false;
        }
        let width = c.len_utf8();
        if self.len + width > Self::CAPACITY {
            return false;
        }
        c.encode_utf8(&mut self.buffer[self.len..]);
        self.len += width;
        true
    }

    /// Erase the character before the end, reporting whether anything went.
    ///
    /// Zeroes the bytes it removes; they are not merely forgotten, since the
    /// buffer outlives them and is reused as the user retypes.
    pub fn backspace(&mut self) -> bool {
        if self.len == 0 {
            return false;
        }
        let mut start = self.len - 1;
        while start > 0 && !self.as_str_upto(self.len).is_char_boundary(start) {
            start -= 1;
        }
        self.buffer[start..self.len].fill(0);
        self.len = start;
        true
    }

    /// Erase everything, zeroing the whole buffer.
    ///
    /// Zeroes the full capacity rather than the used prefix, so a shorter
    /// second attempt cannot leave the tail of a longer first one behind.
    pub fn clear(&mut self) {
        self.buffer.fill(0);
        self.len = 0;
    }

    /// The secret itself.
    ///
    /// Named to be conspicuous at the call site: the one legitimate caller is
    /// the code handing the password to the authentication helper.
    #[must_use]
    pub fn expose(&self) -> &str {
        self.as_str()
    }

    /// Take the secret out, leaving the field empty and its buffer zeroed.
    ///
    /// The returned `String` is a fresh plaintext copy and becomes the
    /// caller's problem to drop promptly -- unavoidable, because the helper
    /// session's API takes an owned `String`.
    pub fn take(&mut self) -> String {
        let secret = self.as_str().to_owned();
        self.clear();
        secret
    }

    /// The used prefix as a string.
    fn as_str(&self) -> &str {
        self.as_str_upto(self.len)
    }

    /// The first `end` bytes as a string.
    ///
    /// Every write goes through `char::encode_utf8`, so the prefix is always
    /// valid UTF-8; the lossy fallback is there so a bug cannot become a
    /// panic in a modal the user cannot then dismiss.
    fn as_str_upto(&self, end: usize) -> &str {
        std::str::from_utf8(&self.buffer[..end]).unwrap_or("")
    }
}

impl Default for SecretInput {
    fn default() -> Self {
        Self::new()
    }
}

/// Erases the buffer when the field goes away.
///
/// `Vec`'s own drop hands the allocation back with the password still in it.
/// This is the difference between the secret living as long as the modal and
/// living until something else happens to reuse that heap.
impl Drop for SecretInput {
    fn drop(&mut self) {
        self.buffer.fill(0);
    }
}

/// Prints the length, never the contents.
///
/// Manual because a derived `Debug` would put the password into any log line,
/// panic message or `dbg!` that ever touches the modal.
impl std::fmt::Debug for SecretInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretInput")
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::SecretInput;

    fn typed(text: &str) -> SecretInput {
        let mut input = SecretInput::new();
        for c in text.chars() {
            assert!(input.insert(c), "{c:?} must be accepted");
        }
        input
    }

    #[test]
    fn a_fresh_field_is_empty() {
        let input = SecretInput::new();
        assert!(input.is_empty());
        assert_eq!(input.len(), 0);
        assert_eq!(input.masked(), "");
    }

    #[test]
    fn typing_accumulates_the_secret() {
        let input = typed("hunter2");
        assert_eq!(input.expose(), "hunter2");
        assert!(!input.is_empty());
    }

    /// The painter must never receive the plaintext, and the mask must not
    /// reveal the password by its own content.
    #[test]
    fn the_mask_is_bullets_and_carries_no_plaintext() {
        let input = typed("hunter2");
        assert_eq!(input.masked(), "•••••••");
        assert!(!input.masked().contains('h'));
    }

    /// One bullet per *character*: counting bytes would show three bullets
    /// for one CJK keystroke and leak the password's composition.
    #[test]
    fn a_multibyte_character_shows_one_bullet_not_one_per_byte() {
        let input = typed("密码");
        assert_eq!(input.char_count(), 2);
        assert_eq!(input.masked().chars().count(), 2);
        assert_eq!(input.len(), 6, "but it really is six bytes");
    }

    #[test]
    fn backspace_removes_the_last_character() {
        let mut input = typed("abc");
        assert!(input.backspace());
        assert_eq!(input.expose(), "ab");
    }

    /// Backspace must step a whole character: removing one byte of a
    /// three-byte character would leave the buffer invalid UTF-8.
    #[test]
    fn backspace_removes_a_whole_multibyte_character() {
        let mut input = typed("a密");
        assert!(input.backspace());
        assert_eq!(input.expose(), "a");
        assert_eq!(input.len(), 1);
    }

    #[test]
    fn backspace_on_an_empty_field_reports_no_change() {
        let mut input = SecretInput::new();
        assert!(!input.backspace());
        assert!(input.is_empty());
    }

    /// The bytes must be *erased*, not merely excluded by the length -- the
    /// buffer is reused as the user retypes.
    #[test]
    fn backspace_zeroes_the_bytes_it_removes() {
        let mut input = typed("abc");
        input.backspace();
        assert_eq!(input.buffer[2], 0);
    }

    #[test]
    fn clearing_empties_the_field() {
        let mut input = typed("hunter2");
        input.clear();
        assert!(input.is_empty());
        assert_eq!(input.expose(), "");
    }

    /// A short second attempt must not leave the tail of a long first one in
    /// the buffer.
    #[test]
    fn clearing_zeroes_the_whole_buffer_not_just_the_used_prefix() {
        let mut input = typed("a-very-long-first-attempt");
        input.clear();
        assert!(
            input.buffer.iter().all(|b| *b == 0),
            "no byte of the previous secret may survive"
        );
    }

    #[test]
    fn taking_the_secret_yields_it_and_empties_the_field() {
        let mut input = typed("hunter2");
        assert_eq!(input.take(), "hunter2");
        assert!(input.is_empty());
        assert!(input.buffer.iter().all(|b| *b == 0));
    }

    /// Enter, Escape and Tab arrive as commands; storing them as text would
    /// put a control byte into the password.
    #[test]
    fn control_characters_are_refused() {
        let mut input = SecretInput::new();
        for c in ['\n', '\r', '\t', '\0', '\u{1b}'] {
            assert!(!input.insert(c), "{c:?} must be refused");
        }
        assert!(input.is_empty());
    }

    /// A space is legal in a passphrase and must be typed, unlike in the
    /// filter box where it is a selection gesture.
    #[test]
    fn a_space_is_accepted_because_a_passphrase_may_contain_one() {
        let input = typed("correct horse");
        assert_eq!(input.expose(), "correct horse");
    }

    /// Truncating silently produces an authentication failure the user cannot
    /// explain, so the field refuses instead.
    #[test]
    fn a_full_field_refuses_more_rather_than_dropping_the_start() {
        let mut input = SecretInput::new();
        for _ in 0..SecretInput::CAPACITY {
            assert!(input.insert('x'));
        }
        assert!(!input.insert('y'));
        assert_eq!(input.len(), SecretInput::CAPACITY);
        assert!(input.expose().ends_with('x'));
        assert!(input.expose().starts_with('x'));
    }

    /// The check is on bytes, not characters: a multi-byte character that
    /// would straddle the end must be refused whole.
    #[test]
    fn a_multibyte_character_that_would_overflow_is_refused_whole() {
        let mut input = SecretInput::new();
        for _ in 0..SecretInput::CAPACITY - 1 {
            assert!(input.insert('x'));
        }
        assert!(!input.insert('密'), "3 bytes cannot fit in 1");
        assert!(input.insert('y'), "but 1 byte can");
        assert_eq!(input.len(), SecretInput::CAPACITY);
    }

    /// The buffer must never be reallocated: a realloc copies the password to
    /// a new address and frees the old one with the plaintext still in it.
    #[test]
    fn the_buffer_address_never_moves_while_typing() {
        let mut input = SecretInput::new();
        let address = input.buffer.as_ptr();
        for _ in 0..SecretInput::CAPACITY {
            input.insert('x');
        }
        assert_eq!(
            input.buffer.as_ptr(),
            address,
            "the buffer must be allocated once and never grow"
        );
        assert_eq!(input.buffer.capacity(), SecretInput::CAPACITY);
    }

    /// A derived `Debug` would put the password into every log line and panic
    /// message that touches the modal.
    #[test]
    fn the_debug_output_does_not_contain_the_secret() {
        let input = typed("hunter2");
        let shown = format!("{input:?}");
        assert!(!shown.contains("hunter2"), "got {shown}");
        assert!(shown.contains('7'), "the length is fine to show: {shown}");
    }
}

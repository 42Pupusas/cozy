//! Carrying an activation token from a launcher to the program it launches.
//!
//! # What a token is for
//!
//! A compositor refuses to raise a window for a process that cannot show
//! the user asked for it -- otherwise any background program could steal
//! focus. An activation token is that proof, minted by the compositor for a
//! client that currently holds focus and an input event serial.
//!
//! The token travels in the environment variable `XDG_ACTIVATION_TOKEN`.
//! This module owns that hand-off, and nothing else: minting belongs to the
//! Wayland client (`platform::toplevel::activation`), and presenting the
//! token belongs to the child's own toolkit.
//!
//! # Why a launcher passes one on rather than only minting one
//!
//! `open` is a short-lived command with no Wayland connection, so it has no
//! focus of its own to prove and cannot mint anything. But whatever ran it
//! -- a terminal, a launcher, a file browser -- may have put a token in its
//! environment. Forwarding that token is the whole reason a browser already
//! running on another workspace comes to the front instead of opening a tab
//! where nobody is looking.
//!
//! # Single use
//!
//! The xdg-activation specification requires a child to unset the variable
//! immediately after reading it, so a token is never inherited by a
//! grandchild. [`ActivationToken::take_from_env`] does exactly that: it
//! removes the variable as it reads it, so one token activates one window.

/// A token being carried to a child process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationToken(String);

impl ActivationToken {
    /// The environment variable an activation token travels in, named by
    /// the xdg-activation specification.
    pub const ENV: &'static str = "XDG_ACTIVATION_TOKEN";

    /// Wrap a token string minted by the compositor.
    #[must_use]
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    /// Take the token out of this process's environment, removing it.
    ///
    /// Removing rather than reading is the specified behaviour: a token
    /// authorises exactly one activation, so leaving it in place would let
    /// every later child inherit a spent token and race for the same raise.
    ///
    /// Returns `None` when the variable is absent or empty. An empty value
    /// is treated as absent because it cannot activate anything, and
    /// carrying it forward would only make a child present a token the
    /// compositor must reject.
    #[must_use]
    pub fn take_from_env() -> Option<Self> {
        let raw = std::env::var(Self::ENV).ok()?;
        // SAFETY: single-threaded process startup. `bins/open` reads this
        // before it spawns anything, and the variable must be gone from our
        // environment before a child could inherit it.
        unsafe {
            std::env::remove_var(Self::ENV);
        }
        if raw.is_empty() {
            return None;
        }
        Some(Self(raw))
    }

    /// The token string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_carries_its_string() {
        assert_eq!(ActivationToken::new("tok-1").as_str(), "tok-1");
    }

    /// The name is specified, not ours to pick: a child looks for exactly
    /// this variable and nothing else.
    #[test]
    fn the_variable_is_the_specified_name() {
        assert_eq!(ActivationToken::ENV, "XDG_ACTIVATION_TOKEN");
    }
}

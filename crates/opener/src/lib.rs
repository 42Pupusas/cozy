//! `opener` -- "which application opens this file, and how do I start it?".
//!
//! # Why this is a crate and not part of the file browser
//!
//! Four programs ask the same question:
//!
//! * `bins/files`, when the user activates a row.
//! * `bins/terminal`, when the user Ctrl+clicks a link.
//! * `bins/open`, the `xdg-open` replacement.
//! * the portal's `OpenURI`, on behalf of a sandboxed application.
//!
//! The policy that answers it -- walk every candidate, skip the ones that no
//! longer resolve -- is small, load-bearing, and easy to get subtly wrong.
//! It lives here once, headlessly tested, rather than three times.
//!
//! # Layering
//!
//! ```text
//! Target                              a file, or a URL
//! mime::MimeDb + mime::Associations   type -> desktop id
//! launcher::DesktopEntry              desktop id -> Exec=
//! Handlers                            the candidate-walking policy
//! ActivationToken                     proof the user asked for this
//! Opener                              spawn
//! ```

mod activation_token;
mod handlers;
mod launch;
mod target;

pub use activation_token::ActivationToken;
pub use handlers::Handlers;
pub use launch::{OpenError, Opener};
pub use target::Target;

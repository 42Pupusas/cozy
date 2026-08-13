//! `opener` -- "which application opens this file, and how do I start it?".
//!
//! # Why this is a crate and not part of the file browser
//!
//! Three programs ask the same question:
//!
//! * `bins/files`, when the user activates a row.
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
//! mime::MimeDb + mime::Associations   type -> desktop id
//! launcher::DesktopEntry              desktop id -> Exec=
//! Handlers                            the candidate-walking policy
//! Opener                              spawn, with an xdg-open fallback
//! ```

mod handlers;
mod launch;

pub use handlers::Handlers;
pub use launch::Opener;

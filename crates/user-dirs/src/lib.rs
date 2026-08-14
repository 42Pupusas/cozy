//! The XDG user directories -- Desktop, Downloads, Documents, and the rest.
//!
//! # What lives here
//!
//! ```text
//! UserDir      one directory: its key, CLI name, and fallback
//! ConfigPaths  where user-dirs.dirs and its siblings live
//! UserDirs     the parsed file
//! DirsWriter   the parsed file, written back atomically
//! DirLookup    one directory, with `xdg-user-dir`'s fallback rule
//! ```
//!
//! [`UserDirs`] and [`DirLookup`] answer deliberately different questions.
//! See [`DirLookup`] for why a caller must not substitute one for the other.
//!
//! # Why a crate
//!
//! These directories are not fixed. The spec lets them be renamed, moved,
//! and localized -- a German desktop has `Schreibtisch`, not `Desktop` -- and
//! the answer lives in `~/.config/user-dirs.dirs`. Hardcoding `~/Desktop`
//! shows a broken entry to anyone whose setup differs.
//!
//! Reading started in the file browser's sidebar. `bins/xdg-user-dirs` needs
//! the same parser plus a writer, and a second copy of the parser would let
//! the reader and the writer disagree about the format.
//!
//! Parsing is split from the environment lookup ([`UserDirs::parse`] against
//! [`UserDirs::load`]) so the format is testable without a real home
//! directory.

mod config_paths;
mod lookup;
mod user_dir;
mod user_dirs;
mod writer;

pub use config_paths::ConfigPaths;
pub use lookup::DirLookup;
pub use user_dir::UserDir;
pub use user_dirs::UserDirs;
pub use writer::{DirsWriter, WriteError};

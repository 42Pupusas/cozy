//! Filesystem *mutation* for a file browser: trashing, deleting, copying,
//! and moving files and directory trees.
//!
//! Deliberately separate from `crates/fsmodel`, which only ever *reads*.
//! Reading a directory and mutating it are different responsibilities with
//! very different risk profiles: a bug in the reader shows a wrong list, a
//! bug in here destroys someone's data. Keeping them apart means this crate
//! can carry the paranoid tests it needs without dragging sort policies and
//! view state into scope.

pub mod conflict;
pub mod progress;
pub mod survey;
pub mod transfer;
pub mod trash;
pub mod trash_dir;
pub mod trash_info;

pub use conflict::{ConflictPolicy, ConflictResolver, Resolution};
pub use progress::{Progress, ProgressSnapshot};
pub use survey::{Survey, Surveyor};
pub use transfer::{Transfer, TransferError, Transferred};
pub use trash::{Trash, TrashError, Trashed};
pub use trash_dir::TrashDir;
pub use trash_info::TrashInfo;

#[cfg(test)]
mod test_support;

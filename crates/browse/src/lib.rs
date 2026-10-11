//! Directory browsing shared by the file manager and the portal file chooser.

pub mod dirs_watch;
pub mod history;

#[cfg(test)]
mod test_support;

pub use dirs_watch::DirsWatch;
pub use history::{History, Trail};

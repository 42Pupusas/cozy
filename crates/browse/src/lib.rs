//! Directory browsing shared by the file manager and the portal file chooser.

pub mod directory_browser;
pub mod dirs_watch;
pub mod history;
pub mod reaction;

#[cfg(test)]
mod test_support;

pub use directory_browser::DirectoryBrowser;
pub use dirs_watch::DirsWatch;
pub use history::{History, Trail};
pub use reaction::Reaction;

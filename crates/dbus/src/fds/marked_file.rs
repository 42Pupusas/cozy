//! A temp file whose contents name it, for proving descriptor identity.
//!
//! A received descriptor is easy to check for validity and hard to check
//! for *identity*: any open file gives a plausible number. Writing a unique
//! marker into each file lets a test assert that the descriptor it got back
//! is the same open file the sender attached, not merely a working one.

use std::io::{Read as _, Seek as _, Write as _};
use std::os::fd::OwnedFd;

/// A temp file identified by its own contents.
pub struct MarkedFile {
    path: std::path::PathBuf,
}

impl MarkedFile {
    pub fn new(marker: &str) -> Self {
        let path = std::env::temp_dir().join(format!("cozy-conn-fd-{}-{marker}", std::process::id()));
        let mut file = std::fs::File::create(&path).expect("create");
        file.write_all(marker.as_bytes()).expect("write");
        Self { path }
    }

    pub fn open(&self) -> std::fs::File {
        std::fs::File::open(&self.path).expect("reopen")
    }

    /// Read a descriptor's file back through a duplicate, so the original
    /// keeps its own seek position.
    pub fn read_through(fd: &OwnedFd) -> String {
        let mut file = std::fs::File::from(fd.try_clone().expect("dup"));
        file.rewind().expect("rewind");
        let mut text = String::new();
        file.read_to_string(&mut text).expect("read");
        text
    }
}

impl Drop for MarkedFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

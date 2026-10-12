use std::path::{Path, PathBuf};

use super::packed_names::PackedNames;

/// Names read from one directory, handed to another thread to `statx`.
#[derive(Debug)]
pub struct NameChunk {
    dir: PathBuf,
    names: Vec<u8>,
}

impl NameChunk {
    #[must_use]
    pub(crate) const fn new(dir: PathBuf, names: Vec<u8>) -> Self {
        Self { dir, names }
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    #[must_use]
    pub fn names(&self) -> PackedNames<'_> {
        PackedNames::new(&self.names)
    }

    pub(crate) fn into_names(self) -> Vec<u8> {
        self.names
    }
}

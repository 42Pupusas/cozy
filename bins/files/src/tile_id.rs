//! [`TileId`] -- what makes a tile the same tile after the listing changes.
//!
//! A row number names a position in the current listing, so it changes for
//! every entry after one that is created or removed. The name is what the
//! user recognises a tile by, and it is unique within a folder.

use std::hash::{DefaultHasher, Hash, Hasher};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileId(u64);

impl TileId {
    #[must_use]
    pub fn of(name: &str) -> Self {
        let mut hasher = DefaultHasher::new();
        name.hash(&mut hasher);
        Self(hasher.finish())
    }
}

#[cfg(test)]
mod tests {
    use super::TileId;

    #[test]
    fn a_name_always_gives_the_same_id() {
        assert_eq!(TileId::of("notes.md"), TileId::of("notes.md"));
    }

    #[test]
    fn different_names_give_different_ids() {
        assert_ne!(TileId::of("notes.md"), TileId::of("notes.txt"));
        assert_ne!(TileId::of("a"), TileId::of("A"));
    }
}

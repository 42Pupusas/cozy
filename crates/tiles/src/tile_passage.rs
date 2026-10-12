//! [`Passage`] -- how the folder now shown relates to the one before it.

use std::path::{Component, Path};

use crate::tile_id::TileId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Passage {
    /// Opened a folder inside the old one, through the tile with this id.
    Into(TileId),
    /// Left the old folder for one holding it, where it is the tile with
    /// this id.
    OutOf(TileId),
    Elsewhere,
}

impl Passage {
    #[must_use]
    pub fn between(from: Option<&Path>, to: &Path) -> Self {
        let Some(from) = from else {
            return Self::Elsewhere;
        };
        if let Some(name) = Self::first_step(from, to) {
            return Self::Into(TileId::of(&name));
        }
        if let Some(name) = Self::first_step(to, from) {
            return Self::OutOf(TileId::of(&name));
        }
        Self::Elsewhere
    }

    fn first_step(outer: &Path, inner: &Path) -> Option<String> {
        let rest = inner.strip_prefix(outer).ok()?;
        match rest.components().next()? {
            Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Passage;
    use crate::tile_id::TileId;
    use std::path::Path;

    #[test]
    fn the_first_folder_comes_from_nowhere() {
        assert_eq!(Passage::between(None, Path::new("/a")), Passage::Elsewhere);
    }

    #[test]
    fn opening_a_child_passes_through_its_tile() {
        assert_eq!(
            Passage::between(Some(Path::new("/a")), Path::new("/a/b")),
            Passage::Into(TileId::of("b"))
        );
    }

    #[test]
    fn opening_a_grandchild_passes_through_the_child_in_view() {
        assert_eq!(
            Passage::between(Some(Path::new("/a")), Path::new("/a/b/c")),
            Passage::Into(TileId::of("b"))
        );
    }

    #[test]
    fn going_up_returns_to_the_tile_that_was_open() {
        assert_eq!(
            Passage::between(Some(Path::new("/a/b")), Path::new("/a")),
            Passage::OutOf(TileId::of("b"))
        );
        assert_eq!(
            Passage::between(Some(Path::new("/a/b")), Path::new("/")),
            Passage::OutOf(TileId::of("a"))
        );
    }

    #[test]
    fn a_sibling_or_the_same_folder_is_elsewhere() {
        assert_eq!(
            Passage::between(Some(Path::new("/a/b")), Path::new("/a/c")),
            Passage::Elsewhere
        );
        assert_eq!(
            Passage::between(Some(Path::new("/a")), Path::new("/a")),
            Passage::Elsewhere
        );
    }
}

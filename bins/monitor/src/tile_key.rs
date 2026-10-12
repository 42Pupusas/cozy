//! [`TileKey`] -- what makes a process tile the same tile across layouts:
//! its name, or being the "more" tile whatever count it shows.

use crate::tile_layout::TileKind;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TileKey {
    Process(String),
    More,
}

impl TileKey {
    /// The key of a process or "more" tile; the fixed tiles have none.
    #[must_use]
    pub fn of(kind: &TileKind) -> Option<Self> {
        match kind {
            TileKind::Process(name) => Some(Self::Process(name.clone())),
            TileKind::More { .. } => Some(Self::More),
            TileKind::Info | TileKind::Chart(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TileKey;
    use crate::tile_layout::{ChartKind, TileKind};

    #[test]
    fn the_more_tile_keeps_its_key_whatever_its_count() {
        assert_eq!(
            TileKey::of(&TileKind::More { count: 3 }),
            TileKey::of(&TileKind::More { count: 40 })
        );
    }

    #[test]
    fn processes_are_keyed_by_name_and_fixed_tiles_not_at_all() {
        assert_eq!(
            TileKey::of(&TileKind::Process("a".to_owned())),
            Some(TileKey::Process("a".to_owned()))
        );
        assert_eq!(TileKey::of(&TileKind::Info), None);
        assert_eq!(TileKey::of(&TileKind::Chart(ChartKind::Cpu)), None);
    }
}

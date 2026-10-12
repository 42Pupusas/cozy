//! [`CardBounds`] -- where the file browser's card sits in its tile field,
//! read from `[files]` and written back there when the user moves it.

use std::path::Path;

use crate::table_edit::TableEdit;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CardBounds {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl CardBounds {
    pub const TABLE: &'static str = "files";
    pub const KEYS: [&'static str; 4] = ["card_x", "card_y", "card_width", "card_height"];
    pub const FIELDS: [&'static str; 4] = [
        "files.card_x",
        "files.card_y",
        "files.card_width",
        "files.card_height",
    ];
    pub const LOWEST: [i64; 4] = [0, 0, 1, 1];
    pub const LIMIT: i64 = 65_535;

    #[must_use]
    pub const fn values(self) -> [i32; 4] {
        [self.x, self.y, self.width, self.height]
    }

    #[must_use]
    pub const fn from_values([x, y, width, height]: [i32; 4]) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// `text` with these bounds written into its `[files]` table.
    #[must_use]
    pub fn written_into(self, text: &str) -> String {
        let mut edit = TableEdit::new(text);
        for (key, value) in Self::KEYS.into_iter().zip(self.values()) {
            edit.set(Self::TABLE, key, &value.to_string());
        }
        edit.text()
    }

    /// Write these bounds into the config file at `path`, keeping the rest
    /// of it as it was. Written in place rather than renamed over, so a
    /// config file that is a symlink stays one.
    pub fn save_to(self, path: &Path) -> std::io::Result<()> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e),
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, self.written_into(&text))
    }
}

#[cfg(test)]
mod tests {
    use super::CardBounds;
    use crate::Config;

    const BOUNDS: CardBounds = CardBounds {
        x: 40,
        y: 30,
        width: 320,
        height: 240,
    };

    #[test]
    fn written_bounds_read_back_the_same() {
        let text = BOUNDS.written_into("[font]\nsize = 14\n\n[files]\ndir = \"#89b4fa\"\n");
        let cfg = Config::from_bytes(text.as_bytes()).expect("still valid toml");
        assert_eq!(cfg.files.card, Some(BOUNDS));
        assert_eq!(cfg.font.size, 14);
    }

    #[test]
    fn writing_twice_replaces_rather_than_repeats() {
        let once = BOUNDS.written_into("");
        let moved = CardBounds { x: 1, ..BOUNDS };
        let twice = moved.written_into(&once);
        assert_eq!(twice.matches("card_x").count(), 1);
        let cfg = Config::from_bytes(twice.as_bytes()).expect("still valid toml");
        assert_eq!(cfg.files.card, Some(moved));
    }

    #[test]
    fn saving_keeps_the_rest_of_the_file() {
        let dir = std::env::temp_dir().join(format!("cozy-card-bounds-{}", std::process::id()));
        let path = dir.join("config.toml");
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(&path, "# mine\n[window]\nopacity = 0.5\n").expect("seed");
        BOUNDS.save_to(&path).expect("save");
        let text = std::fs::read_to_string(&path).expect("read back");
        std::fs::remove_dir_all(&dir).expect("clean up");
        assert!(text.starts_with("# mine\n[window]\nopacity = 0.5\n"));
        let cfg = Config::from_bytes(text.as_bytes()).expect("still valid toml");
        assert_eq!(cfg.files.card, Some(BOUNDS));
    }

    #[test]
    fn saving_creates_a_missing_file() {
        let dir = std::env::temp_dir().join(format!("cozy-card-new-{}", std::process::id()));
        let path = dir.join("terminal").join("config.toml");
        BOUNDS.save_to(&path).expect("save");
        let text = std::fs::read_to_string(&path).expect("read back");
        std::fs::remove_dir_all(&dir).expect("clean up");
        assert_eq!(
            text,
            "[files]\ncard_x = 40\ncard_y = 30\ncard_width = 320\ncard_height = 240\n"
        );
    }
}

//! [`CardStore`] -- loads and saves where the user left the card, as
//! `[files]` keys in the config file, and notices when the file is edited.

use std::os::fd::RawFd;
use std::path::PathBuf;

use config::CardBounds;
use inotify_events::FileWatch;
use platform::draw::Rect;
use tiles::card_placement::CardPlacement;

#[derive(Default)]
pub struct CardStore {
    path: Option<PathBuf>,
    watch: Option<FileWatch>,
}

impl CardStore {
    /// A store that keeps nothing, for windows that must not touch disk.
    #[must_use]
    pub const fn detached() -> Self {
        Self {
            path: None,
            watch: None,
        }
    }

    #[must_use]
    pub fn at(path: PathBuf) -> Self {
        let watch = FileWatch::on(&path);
        Self {
            path: Some(path),
            watch,
        }
    }

    /// The placement the config describes.
    #[must_use]
    pub fn placement(bounds: Option<CardBounds>) -> CardPlacement {
        bounds.map_or_else(CardPlacement::new, |b| {
            CardPlacement::at(Rect::new(b.x, b.y, b.width, b.height))
        })
    }

    /// Record `offset`, the card's place measured from the field's corner.
    pub fn save(&mut self, offset: Rect) {
        let Some(path) = &self.path else {
            return;
        };
        let bounds = CardBounds {
            x: offset.x.max(0),
            y: offset.y.max(0),
            width: offset.w.max(1),
            height: offset.h.max(1),
        };
        if let Err(e) = bounds.save_to(path) {
            eprintln!("files: could not save the card's place: {e}");
        }
        if self.watch.is_none() {
            self.watch = FileWatch::on(path);
        }
    }

    /// The watched config file's descriptor, for the event loop to wait on.
    #[must_use]
    pub fn watch_fd(&self) -> Option<RawFd> {
        self.watch.as_ref().map(FileWatch::raw_fd)
    }

    /// The placement in the config file when it was saved since the last
    /// call. A file that no longer parses changes nothing.
    pub fn reload(&self) -> Option<CardPlacement> {
        let (Some(path), Some(watch)) = (&self.path, &self.watch) else {
            return None;
        };
        if !watch.touched() {
            return None;
        }
        match config::Config::load_from(path) {
            Ok(cfg) => Some(Self::placement(cfg.files.card)),
            Err(e) => {
                eprintln!("files: config error: {e} — keeping the card where it is");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CardStore;
    use config::CardBounds;
    use platform::draw::Rect;

    #[test]
    fn configured_bounds_become_the_cards_offset() {
        let bounds = CardBounds {
            x: 100,
            y: 50,
            width: 300,
            height: 200,
        };
        let placement = CardStore::placement(Some(bounds));
        assert_eq!(placement.offset(), Some(Rect::new(100, 50, 300, 200)));
        assert_eq!(CardStore::placement(None).offset(), None);
    }

    #[test]
    fn a_saved_card_reads_back_from_the_config_file() {
        let dir = std::env::temp_dir().join(format!("cozy-card-store-{}", std::process::id()));
        let path = dir.join("config.toml");
        CardStore::at(path.clone()).save(Rect::new(100, 50, 300, 200));
        let cfg = config::Config::load_from(&path).expect("a valid config");
        std::fs::remove_dir_all(&dir).expect("clean up");
        assert_eq!(
            CardStore::placement(cfg.files.card).offset(),
            Some(Rect::new(100, 50, 300, 200))
        );
    }

    #[test]
    fn an_edit_to_the_config_file_is_reloaded() {
        let dir = std::env::temp_dir().join(format!("cozy-card-reload-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("config.toml");
        let store = CardStore::at(path.clone());
        assert!(store.watch_fd().is_some());
        assert!(store.reload().is_none(), "nothing saved yet");

        std::fs::write(
            &path,
            "[files]\ncard_x = 5\ncard_y = 6\ncard_width = 70\ncard_height = 80\n",
        )
        .expect("edit");
        let placement = store.reload().expect("the edit is seen");
        assert_eq!(placement.offset(), Some(Rect::new(5, 6, 70, 80)));
        assert!(store.reload().is_none(), "seen once");

        std::fs::write(&path, "[files]\ncard_x = 5\n").expect("broken edit");
        assert!(store.reload().is_none(), "a broken file changes nothing");

        std::fs::write(&path, "").expect("keys removed");
        assert_eq!(store.reload().expect("seen").offset(), None);
        std::fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_store_whose_directory_appears_on_first_save_starts_watching() {
        let dir = std::env::temp_dir().join(format!("cozy-card-late-{}", std::process::id()));
        let path = dir.join("terminal").join("config.toml");
        let mut store = CardStore::at(path);
        assert!(store.watch_fd().is_none());
        store.save(Rect::new(1, 2, 30, 40));
        assert!(store.watch_fd().is_some());
        std::fs::remove_dir_all(&dir).expect("clean up");
    }
}

//! Resolve a `.desktop` file's `Icon=` field to an actual PNG file on disk.
//!
//! The XDG Icon Theme spec allows `Icon=` to be either an absolute path or
//! a bare *icon name* that must be searched for across icon theme
//! directories at various sizes/categories. Implementing the full spec
//! (theme inheritance, `index.theme` parsing, SVG/XPM support) is out of
//! scope for v1 -- this covers the overwhelmingly common real-world case:
//! an absolute path, or a bare name found under `hicolor` (the theme every
//! icon-shipping package installs into as a fallback, spec-mandated) or
//! `/usr/share/pixmaps`, PNG only (matching this workspace's new
//! `ixiptla-core` PNG-only decoder -- see `crates/wallpaper`).

use std::path::{Path, PathBuf};

/// Sizes to look for under `hicolor/<size>x<size>/apps/`, largest first --
/// a launcher row is small, so this picks the smallest size at least as
/// big as what's likely to be displayed, falling back to the largest
/// available rather than upscaling a tiny icon blurrily. Kept as a fixed
/// list (not a directory scan) since `hicolor`'s standard sizes are fixed
/// by the spec, not per-theme.
const PREFERRED_SIZES: [u32; 6] = [32, 48, 64, 24, 16, 128];

/// Resolves `Icon=` values to PNG file paths, given the search roots the
/// XDG Base Directory spec defines (so tests can point it at a temp dir
/// instead of the real filesystem).
pub struct IconLookup {
    /// `applications`'s sibling `icons` dirs, highest priority first:
    /// `$XDG_DATA_HOME/icons`, then each `$XDG_DATA_DIRS` entry's `icons`.
    icon_theme_roots: Vec<PathBuf>,
    /// The legacy flat icon dump most distros still populate:
    /// `/usr/share/pixmaps`.
    pixmaps_dir: PathBuf,
}

impl IconLookup {
    /// Build a lookup from the real environment, mirroring
    /// [`crate::app_index::AppIndex::search_dirs`]'s XDG resolution.
    #[must_use]
    pub fn from_env() -> Self {
        Self::with_roots(&Self::xdg_data_dirs(), Path::new("/usr/share/pixmaps"))
    }

    /// Build a lookup over explicit roots -- the seam tests use to avoid
    /// touching the real filesystem.
    fn with_roots(data_dirs: &[PathBuf], pixmaps_dir: &Path) -> Self {
        Self {
            icon_theme_roots: data_dirs.iter().map(|d| d.join("icons")).collect(),
            pixmaps_dir: pixmaps_dir.to_owned(),
        }
    }

    /// Same directory list [`crate::app_index::AppIndex`] scans
    /// `applications` under, reused here for `icons`: XDG defines both as
    /// siblings under the same `$XDG_DATA_HOME`/`$XDG_DATA_DIRS` roots.
    fn xdg_data_dirs() -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        let xdg_data_home = std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_default();
            format!("{home}/.local/share")
        });
        dirs.push(PathBuf::from(xdg_data_home));
        let xdg_data_dirs = std::env::var("XDG_DATA_DIRS")
            .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_owned());
        for dir in xdg_data_dirs.split(':').filter(|d| !d.is_empty()) {
            dirs.push(PathBuf::from(dir));
        }
        dirs
    }

    /// Resolve `icon` (a `.desktop` file's raw `Icon=` value) to a PNG path
    /// that exists on disk, or `None` if nothing matched.
    #[must_use]
    pub fn resolve(&self, icon: &str) -> Option<PathBuf> {
        if icon.contains('/') {
            // Spec: an absolute path is used as-is (no search, no
            // extension guessing) -- only accept it if it's actually a PNG,
            // matching this launcher's PNG-only decoder.
            let path = Path::new(icon);
            return (path.extension().and_then(|e| e.to_str()) == Some("png")
                && path.is_file())
            .then(|| path.to_owned());
        }

        for root in &self.icon_theme_roots {
            for size in PREFERRED_SIZES {
                let candidate = root
                    .join("hicolor")
                    .join(format!("{size}x{size}"))
                    .join("apps")
                    .join(format!("{icon}.png"));
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }

        let flat = self.pixmaps_dir.join(format!("{icon}.png"));
        flat.is_file().then_some(flat)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("launcher_icon_test_{name}_{}", std::process::id()))
    }

    fn touch(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"not really a png, path existence is all that's checked").unwrap();
    }

    #[test]
    fn absolute_png_path_is_used_directly() {
        let tmp = tmp_dir("absolute");
        let icon = tmp.join("some-app.png");
        touch(&icon);

        let lookup = IconLookup::with_roots(&[], Path::new("/nonexistent"));
        assert_eq!(lookup.resolve(icon.to_str().unwrap()), Some(icon));

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn absolute_non_png_path_is_rejected() {
        let tmp = tmp_dir("absolute_svg");
        let icon = tmp.join("some-app.svg");
        touch(&icon);

        let lookup = IconLookup::with_roots(&[], Path::new("/nonexistent"));
        assert_eq!(lookup.resolve(icon.to_str().unwrap()), None);

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn finds_bare_name_in_hicolor_theme_at_a_preferred_size() {
        let tmp = tmp_dir("hicolor");
        let data_dir = tmp.join("data");
        touch(&data_dir.join("icons/hicolor/48x48/apps/firefox.png"));

        let lookup = IconLookup::with_roots(&[data_dir.clone()], Path::new("/nonexistent"));
        assert_eq!(
            lookup.resolve("firefox"),
            Some(data_dir.join("icons/hicolor/48x48/apps/firefox.png"))
        );

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn prefers_smaller_size_over_larger_when_both_present() {
        let tmp = tmp_dir("size_pref");
        let data_dir = tmp.join("data");
        touch(&data_dir.join("icons/hicolor/128x128/apps/spotify.png"));
        touch(&data_dir.join("icons/hicolor/32x32/apps/spotify.png"));

        let lookup = IconLookup::with_roots(&[data_dir.clone()], Path::new("/nonexistent"));
        assert_eq!(
            lookup.resolve("spotify"),
            Some(data_dir.join("icons/hicolor/32x32/apps/spotify.png"))
        );

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn falls_back_to_pixmaps_when_not_in_any_theme() {
        let tmp = tmp_dir("pixmaps");
        let pixmaps = tmp.join("pixmaps");
        touch(&pixmaps.join("htop.png"));

        let lookup = IconLookup::with_roots(&[], &pixmaps);
        assert_eq!(lookup.resolve("htop"), Some(pixmaps.join("htop.png")));

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn earlier_root_wins_over_later_root() {
        let tmp = tmp_dir("priority");
        let user_dir = tmp.join("user");
        let sys_dir = tmp.join("sys");
        touch(&user_dir.join("icons/hicolor/48x48/apps/mattermost.png"));
        touch(&sys_dir.join("icons/hicolor/48x48/apps/mattermost.png"));

        let lookup =
            IconLookup::with_roots(&[user_dir.clone(), sys_dir], Path::new("/nonexistent"));
        assert_eq!(
            lookup.resolve("mattermost"),
            Some(user_dir.join("icons/hicolor/48x48/apps/mattermost.png"))
        );

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn missing_icon_resolves_to_none() {
        let lookup = IconLookup::with_roots(&[], Path::new("/nonexistent"));
        assert_eq!(lookup.resolve("does-not-exist-anywhere"), None);
    }
}

use std::path::{Path, PathBuf};

pub struct StartFolder;

impl StartFolder {
    #[must_use]
    pub fn resolve(requested: Option<&Path>, home: Option<PathBuf>) -> PathBuf {
        requested
            .filter(|path| path.is_dir())
            .map(Path::to_path_buf)
            .or_else(|| home.filter(|path| path.is_dir()))
            .unwrap_or_else(|| PathBuf::from("/"))
    }

    #[must_use]
    pub fn from_environment(requested: Option<&Path>) -> PathBuf {
        Self::resolve(requested, std::env::var_os("HOME").map(PathBuf::from))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_existing_requested_folder_wins() {
        let requested = std::env::temp_dir();
        let home = PathBuf::from("/");
        assert_eq!(
            StartFolder::resolve(Some(&requested), Some(home)),
            requested
        );
    }

    #[test]
    fn a_missing_requested_folder_falls_back_to_home() {
        let missing = Path::new("/nonexistent/cozy/start");
        let home = std::env::temp_dir();
        assert_eq!(
            StartFolder::resolve(Some(missing), Some(home.clone())),
            home
        );
    }

    #[test]
    fn no_request_falls_back_to_home() {
        let home = std::env::temp_dir();
        assert_eq!(StartFolder::resolve(None, Some(home.clone())), home);
    }

    #[test]
    fn nothing_usable_falls_back_to_the_root() {
        let missing = PathBuf::from("/nonexistent/cozy/home");
        assert_eq!(
            StartFolder::resolve(None, Some(missing)),
            PathBuf::from("/")
        );
        assert_eq!(StartFolder::resolve(None, None), PathBuf::from("/"));
    }
}

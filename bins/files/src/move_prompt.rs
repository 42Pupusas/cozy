//! [`MovePrompt`] -- the text box a tile's "move to" opens: where to send
//! the entry, typed as a path.
//!
//! It owns the editing session and the reading of what was typed. It does
//! not touch the filesystem: the browser checks the folder exists and does
//! the move.
//!
//! A leading `~` means the home folder, an absolute path stands as typed, and
//! anything else is relative to the folder being browsed.

use std::path::{Path, PathBuf};
use widget::TextInput;

#[derive(Debug)]
struct Session {
    sources: Vec<PathBuf>,
    input: TextInput,
}

#[derive(Debug, Default)]
pub struct MovePrompt {
    session: Option<Session>,
}

impl MovePrompt {
    #[must_use]
    pub const fn new() -> Self {
        Self { session: None }
    }

    /// Open a prompt for moving `sources`, pre-filled with the folder being
    /// browsed and a trailing slash so typing continues the path.
    pub fn begin(&mut self, sources: Vec<PathBuf>, start: &Path) {
        if sources.is_empty() {
            return;
        }
        let mut text = start.display().to_string();
        if !text.ends_with('/') {
            text.push('/');
        }
        self.session = Some(Session {
            sources,
            input: TextInput::with_text(text),
        });
    }

    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.session.is_some()
    }

    #[must_use]
    pub fn input(&self) -> Option<&TextInput> {
        self.session.as_ref().map(|s| &s.input)
    }

    pub fn input_mut(&mut self) -> Option<&mut TextInput> {
        self.session.as_mut().map(|s| &mut s.input)
    }

    /// What the entries being moved are called, for the prompt's label.
    #[must_use]
    pub fn subject(&self) -> Option<String> {
        let session = self.session.as_ref()?;
        let first = session.sources.first()?;
        let name = first.file_name()?.to_string_lossy().into_owned();
        Some(match session.sources.len() {
            1 => name,
            more => format!("{name} and {} more", more - 1),
        })
    }

    /// The folder the typed text names, read against `base` and `home`.
    /// `None` while nothing is typed.
    #[must_use]
    pub fn destination(&self, base: &Path, home: Option<&Path>) -> Option<PathBuf> {
        let typed = self.session.as_ref()?.input.text().trim();
        if typed.is_empty() {
            return None;
        }
        let from_home = |rest: &str| home.map(|home| home.join(rest.trim_start_matches('/')));
        let path = match typed {
            "~" => home.map(Path::to_path_buf),
            _ => match typed.strip_prefix("~/") {
                Some(rest) => from_home(rest),
                None if typed.starts_with('/') => Some(PathBuf::from(typed)),
                None => Some(base.join(typed)),
            },
        }?;
        Some(Self::tidy(&path))
    }

    /// Close the prompt, handing back what it was going to move.
    pub fn finish(&mut self) -> Vec<PathBuf> {
        self.session.take().map(|s| s.sources).unwrap_or_default()
    }

    pub fn cancel(&mut self) {
        self.session = None;
    }

    /// `path` without `.` parts, empty parts or a trailing slash, and with
    /// `..` resolved by name, which is how the user read it.
    fn tidy(path: &Path) -> PathBuf {
        use std::path::Component;
        let mut tidy = PathBuf::new();
        for part in path.components() {
            match part {
                Component::CurDir => {}
                Component::ParentDir => {
                    tidy.pop();
                }
                other => tidy.push(other.as_os_str()),
            }
        }
        tidy
    }
}

#[cfg(test)]
mod tests {
    use super::MovePrompt;
    use std::path::{Path, PathBuf};

    const BASE: &str = "/data/photos";
    const HOME: &str = "/home/u";

    fn opened(sources: &[&str]) -> MovePrompt {
        let mut prompt = MovePrompt::new();
        prompt.begin(sources.iter().map(PathBuf::from).collect(), Path::new(BASE));
        prompt
    }

    fn typed(prompt: &mut MovePrompt, text: &str) {
        let input = prompt.input_mut().expect("a session is open");
        input.clear();
        for c in text.chars() {
            input.insert(c);
        }
    }

    fn resolve(prompt: &MovePrompt) -> Option<PathBuf> {
        prompt.destination(Path::new(BASE), Some(Path::new(HOME)))
    }

    #[test]
    fn a_fresh_prompt_is_closed() {
        let prompt = MovePrompt::new();
        assert!(!prompt.is_active());
        assert_eq!(prompt.subject(), None);
        assert_eq!(resolve(&prompt), None);
    }

    #[test]
    fn nothing_to_move_opens_nothing() {
        let mut prompt = MovePrompt::new();
        prompt.begin(Vec::new(), Path::new(BASE));
        assert!(!prompt.is_active());
    }

    #[test]
    fn it_opens_on_the_browsed_folder_with_the_caret_after_the_slash() {
        let prompt = opened(&["/data/photos/a.png"]);
        let input = prompt.input().expect("open");
        assert_eq!(input.text(), "/data/photos/");
        assert_eq!(input.caret(), input.text().len());
    }

    #[test]
    fn the_subject_names_one_entry_or_counts_the_rest() {
        assert_eq!(
            opened(&["/data/photos/a.png"]).subject().as_deref(),
            Some("a.png")
        );
        assert_eq!(
            opened(&["/a/x", "/a/y", "/a/z"]).subject().as_deref(),
            Some("x and 2 more")
        );
    }

    #[test]
    fn an_absolute_path_stands_as_typed() {
        let mut prompt = opened(&["/data/photos/a.png"]);
        typed(&mut prompt, "/srv/backup");
        assert_eq!(resolve(&prompt), Some(PathBuf::from("/srv/backup")));
    }

    #[test]
    fn a_relative_path_is_read_from_the_browsed_folder() {
        let mut prompt = opened(&["/data/photos/a.png"]);
        typed(&mut prompt, "archive/2024");
        assert_eq!(
            resolve(&prompt),
            Some(PathBuf::from("/data/photos/archive/2024"))
        );
    }

    #[test]
    fn a_tilde_means_the_home_folder() {
        let mut prompt = opened(&["/data/photos/a.png"]);
        typed(&mut prompt, "~");
        assert_eq!(resolve(&prompt), Some(PathBuf::from(HOME)));
        typed(&mut prompt, "~/Pictures");
        assert_eq!(resolve(&prompt), Some(PathBuf::from("/home/u/Pictures")));
    }

    #[test]
    fn a_tilde_without_a_home_names_nothing() {
        let mut prompt = opened(&["/data/photos/a.png"]);
        typed(&mut prompt, "~/x");
        assert_eq!(prompt.destination(Path::new(BASE), None), None);
    }

    #[test]
    fn dots_and_trailing_slashes_are_tidied() {
        let mut prompt = opened(&["/data/photos/a.png"]);
        typed(&mut prompt, "../other/./sub/");
        assert_eq!(resolve(&prompt), Some(PathBuf::from("/data/other/sub")));
    }

    #[test]
    fn blank_text_names_nothing() {
        let mut prompt = opened(&["/data/photos/a.png"]);
        typed(&mut prompt, "   ");
        assert_eq!(resolve(&prompt), None);
    }

    #[test]
    fn finishing_closes_the_prompt_and_returns_the_sources() {
        let mut prompt = opened(&["/data/photos/a.png", "/data/photos/b.png"]);
        let sources = prompt.finish();
        assert_eq!(sources.len(), 2);
        assert!(!prompt.is_active());
        assert!(prompt.finish().is_empty());
    }

    #[test]
    fn cancelling_closes_the_prompt() {
        let mut prompt = opened(&["/data/photos/a.png"]);
        prompt.cancel();
        assert!(!prompt.is_active());
    }
}

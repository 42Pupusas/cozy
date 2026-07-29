//! Deterministic file-browser workflow without Wayland or a rendered window.
use crate::browser::Browser;
use crate::keymap::{Action, EditAction};
use headless_stress::StressOptions;
use std::path::Path;

/// Runs the same directory open, filtering, sorting, refresh, and navigation
/// state transitions exercised by a busy user, while omitting platform work.
pub struct FileStress;

impl FileStress {
    /// Parse and run the file-browser stress mode when requested.
    pub fn maybe_run(arguments: &[String]) -> Result<bool, String> {
        let Some(options) = StressOptions::parse(arguments.iter().cloned())? else {
            return Ok(false);
        };
        let root = arguments
            .windows(2)
            .find(|pair| pair[0] == "--stress-path")
            .map_or_else(
                || std::env::current_dir().unwrap_or_else(|_| "/".into()),
                |pair| pair[1].clone().into(),
            );
        let mut browser = Browser::open(Path::new(&root), 900, 600, 24)
            .map_err(|error| format!("could not open stress path: {error}"))?;
        let completed = options.run(|round| {
            let query = ['t', 'e', 'r', 'm', 'i', 'n', 'a', 'l']
                [usize::try_from(round % 8).unwrap_or_default()];
            browser.act(Action::Insert(query));
            browser.act(Action::Edit(EditAction::Backspace));
            browser.act(Action::ToggleHidden);
            browser.act(Action::Refresh);
            browser.act(Action::ToggleHidden);
            let width_offset = i32::try_from(round % 5).unwrap_or_default() * 20;
            browser.resize(900 + width_offset, 600);
        });
        println!(
            "files headless stress: {completed} rounds, {} visible entries",
            browser.len()
        );
        Ok(true)
    }
}

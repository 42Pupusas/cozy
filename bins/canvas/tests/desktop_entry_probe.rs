//! Check the shipped desktop entry against the parser that will read it.
//!
//! A `.desktop` file is data, not code, so nothing about it is checked by
//! `cargo build`. A typo in `Exec=` does not fail to compile -- the entry
//! simply stops appearing in the menu, or appears and refuses to launch.
//! These tests read `data/cozy.canvas.desktop.in` back with
//! `launcher::DesktopEntry`, the same parser `bins/launcher` uses.

use launcher::desktop_entry::DesktopEntry;
use launcher::launch::ExecCommand;
use std::path::{Path, PathBuf};

/// The shipped template, plus the substitution the installer applies.
struct EntryTemplate {
    source: String,
}

impl EntryTemplate {
    /// The placeholder `scripts/install.sh` replaces with the binary path.
    const PLACEHOLDER: &'static str = "@EXEC@";

    /// A stand-in for the install root: absolute, and free of characters
    /// that would need quoting.
    const INSTALLED_BIN: &'static str = "/var/cache/example/cargo/bin/canvas";

    fn load() -> Self {
        let path = Self::path();
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        Self { source }
    }

    fn path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("data")
            .join("cozy.canvas.desktop.in")
    }

    fn raw(&self) -> &str {
        &self.source
    }

    fn installed(&self) -> String {
        self.source.replace(Self::PLACEHOLDER, Self::INSTALLED_BIN)
    }

    fn parsed(&self) -> DesktopEntry {
        DesktopEntry::parse(&self.installed())
            .expect("the launcher's own parser must accept the shipped entry")
    }

    /// One key's value from the installed form's `[Desktop Entry]` group.
    fn value(&self, key: &str) -> Option<String> {
        let text = self.installed();
        let prefix = format!("{key}=");
        text.lines()
            .map(str::trim)
            .find_map(|line| line.strip_prefix(&prefix).map(str::to_owned))
    }
}

/// The installer, read rather than restated.
struct Installer;

impl Installer {
    const ENTRY: &'static str = "canvas";

    fn source() -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/install.sh");
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
    }

    /// Whether the installer's entry list names `entry`.
    ///
    /// The list is a shell word list, so a bare `contains` would also match
    /// a longer name that happens to end in the same letters.
    fn installs_entry(script: &str, entry: &str) -> bool {
        script
            .lines()
            .filter_map(|line| line.trim().strip_prefix("desktop_entries="))
            .any(|list| {
                list.trim_matches(['"', '\''])
                    .split_whitespace()
                    .any(|name| name == entry)
            })
    }
}

#[test]
fn the_shipped_entry_parses() {
    let entry = EntryTemplate::load();
    assert_eq!(entry.parsed().name, "Canvas");
}

#[test]
fn the_template_still_carries_the_placeholder() {
    let entry = EntryTemplate::load();
    assert!(
        entry.raw().contains(EntryTemplate::PLACEHOLDER),
        "the installer substitutes {}; without it Exec= is not absolute",
        EntryTemplate::PLACEHOLDER
    );
}

#[test]
fn the_entry_is_shown_in_menus() {
    let entry = EntryTemplate::load();
    assert!(
        entry.value("NoDisplay").is_none(),
        "NoDisplay would hide the entry from every menu"
    );
    assert_eq!(entry.value("Terminal").as_deref(), Some("false"));
}

#[test]
fn exec_runs_the_installed_binary_and_nothing_else() {
    let entry = EntryTemplate::load();
    let command = ExecCommand::parse(&entry.parsed().exec).expect("Exec= must parse");
    assert_eq!(command.program, EntryTemplate::INSTALLED_BIN);
}

#[test]
fn launching_from_a_menu_yields_a_bare_command() {
    let entry = EntryTemplate::load();
    let command = ExecCommand::parse(&entry.parsed().exec).expect("Exec= must parse");
    // No file selected: the %f field code must drop, not become an empty
    // argument the binary would try to open.
    assert!(
        command.args.is_empty(),
        "menu launch passed stray arguments: {:?}",
        command.args
    );
}

#[test]
fn opening_a_document_passes_its_path() {
    let entry = EntryTemplate::load();
    let command = ExecCommand::for_file(&entry.parsed(), Path::new("/tmp/notes.json"))
        .expect("Exec= must accept a file");
    assert_eq!(command.args, vec!["/tmp/notes.json".to_owned()]);
}

/// Claiming `application/json` would make `canvas` a candidate handler for
/// every JSON file on the system. See `data/README.md`.
#[test]
fn the_entry_claims_no_mime_type() {
    let entry = EntryTemplate::load();
    assert!(
        entry.value("MimeType").is_none(),
        "canvas must not register as a handler for a shared type"
    );
}

#[test]
fn the_entry_basename_matches_the_wayland_app_id() {
    let main = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/main.rs"))
        .expect("read src/main.rs");
    assert!(
        main.contains("\"cozy.canvas\""),
        "src/main.rs no longer sets the app_id this entry is named for"
    );
    let install = Installer::source();
    assert!(
        Installer::installs_entry(&install, Installer::ENTRY),
        "the installer must write an entry for canvas"
    );
}

#[test]
fn the_installer_covers_the_canvas() {
    let install = Installer::source();
    assert!(
        install.contains("bins/canvas"),
        "install.sh must build and install the canvas package"
    );
    assert!(
        Installer::installs_entry(&install, Installer::ENTRY),
        "install.sh must list canvas among the entries it installs"
    );
}

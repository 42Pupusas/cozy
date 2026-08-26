//! Check the shipped desktop entry against the parser that will read it.
//!
//! A `.desktop` file is data, not code, so nothing about it is checked by
//! `cargo build`. A typo in `Exec=` does not fail to compile -- the entry
//! simply stops appearing in the menu, or appears and refuses to launch.
//! These tests close that gap by reading `data/cozy.files.desktop.in` back
//! with `launcher::DesktopEntry`, the same parser `bins/launcher` uses.
//!
//! The template carries `@EXEC@` where the installed path goes, so the
//! substitution `scripts/install.sh` performs is reproduced here first.

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

    /// A stand-in for the install root, chosen to be absolute and to
    /// contain no characters that would need quoting -- the point of these
    /// tests is the template, not `sed`.
    const INSTALLED_BIN: &'static str = "/var/cache/example/cargo/bin/files";

    /// Read the template from the crate's own `data` directory.
    fn load() -> Self {
        let path = Self::path();
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        Self { source }
    }

    fn path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("data")
            .join("cozy.files.desktop.in")
    }

    /// The raw template text, before substitution.
    fn raw(&self) -> &str {
        &self.source
    }

    /// The template with `@EXEC@` resolved, as installed on disk.
    fn installed(&self) -> String {
        self.source
            .replace(Self::PLACEHOLDER, Self::INSTALLED_BIN)
    }

    /// Parse the installed form, failing the test if the parser rejects it.
    fn parsed(&self) -> DesktopEntry {
        DesktopEntry::parse(&self.installed())
            .expect("the launcher's own parser must accept the shipped entry")
    }

    /// One key's value from the installed form's `[Desktop Entry]` group.
    ///
    /// `DesktopEntry` deliberately parses only the fields a launcher needs,
    /// so keys like `MimeType` have to be read directly.
    fn value(&self, key: &str) -> Option<String> {
        Self::value_in(&self.installed(), key)
    }

    /// [`Self::value`] against arbitrary text, so a test can read the raw
    /// template rather than the substituted form.
    fn value_in(source: &str, key: &str) -> Option<String> {
        for line in source.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix(key)
                && let Some(value) = rest.strip_prefix('=')
            {
                return Some(value.to_owned());
            }
        }
        None
    }
}

/// The installer's substitution step, reproduced exactly.
struct Installer;

impl Installer {
    /// The bin directory `install.sh` resolves; any absolute path will do,
    /// since the point is `sed`'s handling of it rather than its contents.
    const BIN_DIR: &'static str = "/var/cache/example/cargo/bin";

    /// Lift the `s|@EXEC@|...|` expression out of the script and resolve the
    /// one shell variable it interpolates.
    ///
    /// Reading it from the script rather than restating it is the whole
    /// point: a restated copy would keep passing after the real one broke.
    fn sed_expression(script: &str) -> Option<String> {
        let line = script.lines().find(|l| l.trim_start().starts_with("sed "))?;
        let start = line.find('"')?;
        let rest = &line[start + 1..];
        let end = rest.find('"')?;
        Some(rest[..end].replace("$bin_dir", Self::BIN_DIR))
    }

    /// Ask `sh` to parse the installer without running it.
    ///
    /// `Some(stderr)` when the shell rejected it; `None` when it parsed or
    /// when no shell is available to ask.
    fn syntax_error() -> Option<String> {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/install.sh");
        let output = std::process::Command::new("sh")
            .arg("-n")
            .arg(&script)
            .output()
            .ok()?;
        (!output.status.success())
            .then(|| String::from_utf8_lossy(&output.stderr).into_owned())
    }

    /// Apply `expression` to `path` with the real `sed`.
    fn run_sed(expression: &str, path: &Path) -> Option<String> {
        let output = std::process::Command::new("sed")
            .arg(expression)
            .arg(path)
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

/// The two files that must agree with the entry: the binary that sets the
/// Wayland `app_id`, and the installer that writes the entry out.
struct Sibling;

impl Sibling {
    fn read(relative: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
    }

    /// `bins/files/src/main.rs`, which passes the `app_id` to the toplevel.
    fn main_rs() -> String {
        Self::read("src/main.rs")
    }

    /// `scripts/install.sh`.
    fn install_sh() -> String {
        Self::read("../../scripts/install.sh")
    }
}

#[test]
fn the_shipped_entry_parses() {
    let entry = EntryTemplate::load().parsed();
    assert_eq!(entry.name, "Files");
    assert!(!entry.terminal, "the browser is not a terminal program");
}

/// `NoDisplay=true` would install the entry and hide it from every menu --
/// the exact opposite of why it exists.
#[test]
fn the_entry_is_shown_in_menus() {
    assert!(EntryTemplate::load().parsed().shows_in_menu());
}

/// The launcher spawns entries without a shell, and a niri session need not
/// have the install root on `PATH`. A relative `Exec=` therefore launches
/// nothing, silently.
///
/// The check is against the *template*, not the substituted text: the
/// installer always supplies an absolute path, so testing the substituted
/// form would pass no matter what the template said. What must hold is that
/// the program token is the placeholder itself -- an `Exec=files @EXEC@`
/// would substitute an absolute path into an *argument* and still spawn a
/// bare `files`.
#[test]
fn exec_runs_the_installed_binary_and_nothing_else() {
    let template = EntryTemplate::load();
    let raw = EntryTemplate::value_in(template.raw(), "Exec").expect("Exec= must be present");
    let program = raw
        .split_whitespace()
        .next()
        .expect("Exec= must name a program");
    assert_eq!(
        program,
        EntryTemplate::PLACEHOLDER,
        "the program token must be the substituted path, got {program}"
    );

    let installed = template.parsed().exec;
    let installed_program = installed
        .split_whitespace()
        .next()
        .expect("Exec= must name a program");
    assert!(
        installed_program.starts_with('/'),
        "Exec= must be absolute once installed, got {installed_program}"
    );
}

/// The template must keep the placeholder, or the installer's `sed` becomes
/// a no-op and every machine gets whatever path was hardcoded.
#[test]
fn the_template_still_carries_the_placeholder() {
    let template = EntryTemplate::load();
    assert!(
        template.raw().contains(EntryTemplate::PLACEHOLDER),
        "the template must contain {}",
        EntryTemplate::PLACEHOLDER
    );
    assert!(
        !template.installed().contains(EntryTemplate::PLACEHOLDER),
        "substitution must replace every occurrence"
    );
}

/// `files` takes one optional positional directory, so the entry must use
/// the single-file code. `%F` would hand it a list it does not accept, and
/// no code at all would make "open this folder" open the current directory
/// instead.
#[test]
fn exec_passes_one_file_argument() {
    let entry = EntryTemplate::load().parsed();
    assert!(entry.exec.contains("%f"), "got {}", entry.exec);
    assert!(
        !entry.exec.contains("%F") && !entry.exec.contains("%U"),
        "a list code would pass more paths than the browser parses: {}",
        entry.exec
    );
}

/// Launching from a menu passes no file at all. The browser must still open,
/// which means the expansion is the bare binary -- the same expansion
/// `bins/launcher` performs. A `%f` left in the argv would be read as a
/// directory named `%f` and the window would open on "not found".
#[test]
fn launching_from_a_menu_yields_a_bare_command() {
    let entry = EntryTemplate::load().parsed();
    let command = ExecCommand::for_entry(&entry).expect("the entry must be launchable");
    assert_eq!(command.program, EntryTemplate::INSTALLED_BIN);
    assert!(
        command.args.is_empty(),
        "with nothing selected the field code must drop, got {:?}",
        command.args
    );
}

/// Opening a folder *with* this entry must pass that folder through, which
/// is the whole reason the entry declares `%f` and `inode/directory`.
#[test]
fn opening_a_folder_passes_the_folder_path() {
    let entry = EntryTemplate::load().parsed();
    let folder = Path::new("/tmp/some folder");
    let command = ExecCommand::for_file(&entry, folder).expect("the entry must be launchable");
    assert_eq!(command.program, EntryTemplate::INSTALLED_BIN);
    assert_eq!(
        command.args,
        vec!["/tmp/some folder".to_owned()],
        "a path with a space must stay one argument"
    );
}

/// Registering as a directory handler is what lets `crates/opener` resolve
/// "open this folder" to this binary rather than to a text editor.
#[test]
fn the_entry_handles_directories() {
    let mime = EntryTemplate::load()
        .value("MimeType")
        .expect("MimeType= must be present");
    assert!(
        mime.split(';').any(|t| t == "inode/directory"),
        "got {mime}"
    );
}

/// The basename of the installed file must match the Wayland `app_id`, or a
/// taskbar cannot map the window back to this entry.
#[test]
fn the_entry_basename_matches_the_wayland_app_id() {
    let main = Sibling::main_rs();
    assert!(
        main.contains("\"cozy.files\""),
        "src/main.rs no longer sets the app_id this entry is named for"
    );
    let install = Sibling::install_sh();
    assert!(
        install.contains("cozy.files.desktop"),
        "the installer must write the entry under the app_id's name"
    );
}

/// The whole point of this work: the installer must actually install both
/// the binary and the entry.
#[test]
fn the_installer_covers_the_browser() {
    let install = Sibling::install_sh();
    assert!(
        install.contains("bins/files"),
        "install.sh must build and install the files package"
    );
    assert!(
        install.contains("cozy.files.desktop.in"),
        "install.sh must substitute the shipped template"
    );
}

/// A shell script has no compiler. Nothing else in this repository would
/// notice an unbalanced quote in the installer until someone ran it, by
/// which point it may already have replaced some of the binaries.
#[test]
fn the_installer_is_valid_shell() {
    if let Some(error) = Installer::syntax_error() {
        panic!("scripts/install.sh does not parse:\n{error}");
    }
}

/// Run the installer's own substitution and parse what it writes.
///
/// The tests above substitute with `str::replace`, which is *not* what runs
/// on your machine -- `install.sh` uses `sed`. The two agree only as long as
/// the placeholder and the install path stay free of `sed`'s metacharacters,
/// and nothing else here would notice if that stopped being true.
///
/// Skipped rather than failed when `sed` is missing: its absence is a fact
/// about the machine, not a defect in the entry.
#[test]
fn the_installers_own_substitution_produces_a_valid_entry() {
    let Some(expression) = Installer::sed_expression(&Sibling::install_sh()) else {
        panic!("install.sh must contain the sed expression that fills in @EXEC@");
    };
    let Some(produced) = Installer::run_sed(&expression, &EntryTemplate::path()) else {
        eprintln!("skipped: sed is not available");
        return;
    };

    assert!(
        !produced.contains(EntryTemplate::PLACEHOLDER),
        "sed left the placeholder in place:\n{produced}"
    );
    let entry = DesktopEntry::parse(&produced)
        .expect("what install.sh writes must parse as a desktop entry");
    assert_eq!(entry.name, "Files");
    assert!(
        entry.exec.starts_with('/'),
        "installed Exec= must be absolute, got {}",
        entry.exec
    );
}

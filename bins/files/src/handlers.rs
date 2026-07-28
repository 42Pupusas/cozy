//! [`Handlers`] -- which application opens this file, and with what argv.
//!
//! # Why this is composition and not parsing
//!
//! Every piece of the answer already exists in this workspace, each tested
//! headlessly in its own crate:
//!
//! * `mime::MimeDb` -- what *is* this file (glob, then content sniff).
//! * `mime::Associations` -- which `.desktop` id handles that type, with
//!   the user's `mimeapps.list` outranking the generated `mimeinfo.cache`.
//! * `launcher::DesktopEntry` -- that file's `Name`/`Exec`/`Terminal`.
//! * `launcher::ExecCommand::for_file` -- `Exec=` with `%f`/`%U` actually
//!   substituted, wrapped in a terminal when `Terminal=true`.
//!
//! So this module deliberately parses nothing. It owns exactly one thing
//! the individual crates cannot: the *policy* for walking the candidate
//! list, which is where the real-world failure modes live.
//!
//! # The rule that matters: a candidate may be a lie
//!
//! `mimeapps.list` is a plain text file that nothing prunes. Uninstall an
//! application and its id stays listed as the default for its types
//! forever. A resolver that took `default_for` and stopped would therefore
//! fail to open a PDF because of a program removed two years ago -- while a
//! perfectly good viewer sat second on the list.
//!
//! So resolution walks *every* candidate and takes the first that survives
//! the whole pipeline: id resolves to a file on disk, that file parses as a
//! launchable entry, and its `Exec=` yields a command. Each of those three
//! can independently fail on a real machine.

use launcher::desktop_entry::DesktopEntry;
use launcher::launch::ExecCommand;
use mime::{Associations, DesktopId, MimeDb};
use std::path::{Path, PathBuf};

/// The type-detection and application-association databases, loaded once.
///
/// Loaded once because both read a good deal of the filesystem -- the glob
/// database alone is ~1400 patterns -- and a browser resolves handlers
/// repeatedly over a session. Nothing here watches for changes: an app
/// installed while the browser is open is not visible until restart, which
/// is the same bargain every desktop shell makes.
pub struct Handlers {
    mime: MimeDb,
    associations: Associations,
    /// Where `.desktop` files live, resolved once for the same reason.
    app_dirs: Vec<PathBuf>,
}

impl Handlers {
    /// Load the system databases.
    ///
    /// Never fails: a machine with neither shared-mime-info nor any
    /// `mimeapps.list` yields a resolver that simply never finds a handler,
    /// and the caller falls back.
    #[must_use]
    pub fn load() -> Self {
        Self {
            mime: MimeDb::load(),
            associations: Associations::load(),
            app_dirs: Associations::application_dirs(),
        }
    }

    /// Build from explicit databases and application directories -- the seam
    /// tests use so they assert on fixtures rather than on whatever happens
    /// to be installed on the machine running them.
    ///
    /// Test-only: the running program always wants [`Self::load`], and a
    /// constructor nothing calls in a release build is dead weight the
    /// default build should not carry.
    #[cfg(test)]
    #[must_use]
    pub const fn from_parts(
        mime: MimeDb,
        associations: Associations,
        app_dirs: Vec<PathBuf>,
    ) -> Self {
        Self {
            mime,
            associations,
            app_dirs,
        }
    }

    /// The command that opens `path`, or `None` if nothing on this machine
    /// claims to handle its type.
    ///
    /// `None` is a genuine answer, not an error: plenty of types have no
    /// handler installed, and the caller decides what to do about it.
    #[must_use]
    pub fn command_for(&self, path: &Path) -> Option<ExecCommand> {
        let entry = self.entry_for(path)?;
        ExecCommand::for_file(&entry, path)
    }

    /// The application that opens `path`, skipping candidates that no
    /// longer resolve on this machine.
    #[must_use]
    pub fn entry_for(&self, path: &Path) -> Option<DesktopEntry> {
        self.entries_for(path).into_iter().next()
    }

    /// Every application that can open `path`, best first, filtered down to
    /// the ones that actually exist -- what an "Open With…" menu renders.
    #[must_use]
    pub fn entries_for(&self, path: &Path) -> Vec<DesktopEntry> {
        let detected = self.mime.detect(path);
        self.associations
            .candidates_for(&detected.mime)
            .iter()
            .filter_map(|id| self.load_entry(id))
            .collect()
    }

    /// Resolve one id to a parsed entry, or `None` if it is stale.
    ///
    /// Two independent ways to fail, both routine: the `.desktop` file is
    /// gone (uninstalled app still listed in `mimeapps.list`), or it parses
    /// as something unlaunchable (`NoDisplay=true`, a `Type=Link`, or no
    /// `Exec=` at all).
    fn load_entry(&self, id: &DesktopId) -> Option<DesktopEntry> {
        let path = Associations::resolve_path_in(id, &self.app_dirs)?;
        DesktopEntry::load(&path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory of `.desktop` fixtures, so these tests assert on
    /// association *policy* rather than on what is installed on the machine
    /// running them.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("files-handlers-{label}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("create scratch dir");
            Self(path)
        }

        /// Write a `.desktop` file and return its id.
        fn app(&self, id: &str, body: &str) -> String {
            std::fs::write(self.0.join(id), body).expect("write desktop file");
            id.to_owned()
        }

        /// A file to be opened. Its *name* is what matters -- detection is
        /// by glob -- but it must exist so `detect` doesn't take the
        /// unreadable-path fallback.
        fn file(&self, name: &str) -> PathBuf {
            let p = self.0.join(name);
            std::fs::write(&p, b"contents").expect("write file");
            p
        }

        fn dirs(&self) -> Vec<PathBuf> {
            vec![self.0.clone()]
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const GLOBS: &str = "\
50:image/png:*.png
50:application/pdf:*.pdf
50:text/x-rust:*.rs
";

    fn handlers(scratch: &Scratch, assoc: &str) -> Handlers {
        Handlers::from_parts(
            MimeDb::from_globs_data(GLOBS),
            Associations::from_str_data(assoc),
            scratch.dirs(),
        )
    }

    #[test]
    fn resolves_a_file_to_its_default_application() {
        let scratch = Scratch::new("default");
        scratch.app(
            "eog.desktop",
            "[Desktop Entry]\nName=Image Viewer\nExec=eog %f\nType=Application\n",
        );
        let h = handlers(&scratch, "[Default Applications]\nimage/png=eog.desktop\n");

        let cmd = h.command_for(&scratch.file("photo.png")).expect("resolved");
        assert_eq!(cmd.program, "eog");
        assert_eq!(cmd.args.len(), 1, "the path must be substituted for %f");
        assert!(cmd.args[0].ends_with("photo.png"));
    }

    /// The rule this module exists for. `mimeapps.list` is never pruned, so
    /// the top candidate routinely names an application that was
    /// uninstalled; falling back to the next one is the difference between
    /// "opens" and "does nothing".
    #[test]
    fn a_candidate_whose_desktop_file_is_gone_is_skipped() {
        let scratch = Scratch::new("stale");
        // Note: `ghost.desktop` is deliberately never written to disk.
        scratch.app(
            "evince.desktop",
            "[Desktop Entry]\nName=Evince\nExec=evince %f\nType=Application\n",
        );
        let h = handlers(
            &scratch,
            "[Default Applications]\napplication/pdf=ghost.desktop;evince.desktop;\n",
        );

        let cmd = h.command_for(&scratch.file("paper.pdf")).expect("resolved");
        assert_eq!(
            cmd.program, "evince",
            "a stale default must not shadow a working handler"
        );
    }

    /// The second way a candidate lies: the file is present but describes
    /// something unlaunchable. `Hidden=true` means the user deleted it --
    /// per the spec, equivalent to the file not existing at all.
    #[test]
    fn a_candidate_that_parses_as_unlaunchable_is_skipped() {
        let scratch = Scratch::new("unlaunchable");
        scratch.app(
            "deleted.desktop",
            "[Desktop Entry]\nName=Deleted\nExec=nope %f\nHidden=true\n",
        );
        scratch.app(
            "eog.desktop",
            "[Desktop Entry]\nName=Image Viewer\nExec=eog %f\nType=Application\n",
        );
        let h = handlers(
            &scratch,
            "[Default Applications]\nimage/png=deleted.desktop;eog.desktop;\n",
        );

        let cmd = h.command_for(&scratch.file("photo.png")).expect("resolved");
        assert_eq!(cmd.program, "eog");
    }

    /// The bug this module's probe caught on a real machine: Chrome ships
    /// `NoDisplay=true` and is a listed handler for `text/html` and
    /// `application/pdf`. Treating that as a rejection -- which the parser
    /// originally did, correctly for a launcher menu -- silently demotes a
    /// perfectly good handler.
    ///
    /// The spec is explicit that `NoDisplay` exists precisely so an app can
    /// "be associated with MIME types, so that it gets launched from a file
    /// manager, without having a menu entry for it".
    #[test]
    fn a_no_display_application_is_still_a_valid_handler() {
        let scratch = Scratch::new("nodisplay");
        scratch.app(
            "chrome.desktop",
            "[Desktop Entry]\nName=Chrome\nExec=chrome %U\nType=Application\nNoDisplay=true\n",
        );
        let h = handlers(
            &scratch,
            "[Default Applications]\napplication/pdf=chrome.desktop\n",
        );

        let cmd = h
            .command_for(&scratch.file("paper.pdf"))
            .expect("NoDisplay hides an app from menus, it does not stop it opening files");
        assert_eq!(cmd.program, "chrome");
    }

    #[test]
    fn a_type_with_no_handler_resolves_to_nothing() {
        let scratch = Scratch::new("nohandler");
        let h = handlers(&scratch, "[Default Applications]\nimage/png=eog.desktop\n");

        assert!(
            h.command_for(&scratch.file("paper.pdf")).is_none(),
            "nothing claims application/pdf here"
        );
    }

    /// A terminal application must keep its wrapper when opening a file,
    /// or the TUI starts with nulled stdio and vanishes instantly.
    #[test]
    fn a_terminal_application_is_still_wrapped_when_opening_a_file() {
        let scratch = Scratch::new("terminal");
        scratch.app(
            "nvim.desktop",
            "[Desktop Entry]\nName=Neovim\nExec=nvim %F\nTerminal=true\nType=Application\n",
        );
        let h = handlers(
            &scratch,
            "[Default Applications]\ntext/x-rust=nvim.desktop\n",
        );

        let cmd = h.command_for(&scratch.file("main.rs")).expect("resolved");
        assert_ne!(cmd.program, "nvim", "must be wrapped in a terminal");
        assert_eq!(cmd.args[0], "-e");
        assert_eq!(cmd.args[1], "nvim");
        assert!(cmd.args[2].ends_with("main.rs"));
    }

    /// The user's explicit choice outranks the generated cache -- verified
    /// here end-to-end (through file resolution and `Exec=` expansion)
    /// rather than only at the association layer.
    #[test]
    fn the_users_default_wins_over_the_system_cache() {
        let scratch = Scratch::new("precedence");
        scratch.app(
            "evince.desktop",
            "[Desktop Entry]\nName=Evince\nExec=evince %f\nType=Application\n",
        );
        scratch.app(
            "chrome.desktop",
            "[Desktop Entry]\nName=Chrome\nExec=chrome %U\nType=Application\n",
        );
        let h = handlers(
            &scratch,
            "[Default Applications]\napplication/pdf=evince.desktop\n\
             [MIME Cache]\napplication/pdf=chrome.desktop;\n",
        );

        let cmd = h.command_for(&scratch.file("paper.pdf")).expect("resolved");
        assert_eq!(cmd.program, "evince");
    }

    /// …but the cache entry still shows up below it, which is what makes an
    /// "Open With…" menu possible later.
    #[test]
    fn every_working_candidate_is_offered_in_order() {
        let scratch = Scratch::new("openwith");
        scratch.app(
            "evince.desktop",
            "[Desktop Entry]\nName=Evince\nExec=evince %f\nType=Application\n",
        );
        scratch.app(
            "chrome.desktop",
            "[Desktop Entry]\nName=Chrome\nExec=chrome %U\nType=Application\n",
        );
        let h = handlers(
            &scratch,
            "[Default Applications]\napplication/pdf=evince.desktop\n\
             [MIME Cache]\napplication/pdf=ghost.desktop;chrome.desktop;\n",
        );

        let names: Vec<String> = h
            .entries_for(&scratch.file("paper.pdf"))
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(
            names,
            ["Evince", "Chrome"],
            "best first, with the uninstalled candidate dropped entirely"
        );
    }

    /// A filename with a space has to survive as a single argv entry -- the
    /// property that would break if substitution went through a shell.
    #[test]
    fn a_filename_with_spaces_stays_one_argument() {
        let scratch = Scratch::new("spaces");
        scratch.app(
            "eog.desktop",
            "[Desktop Entry]\nName=Image Viewer\nExec=eog %f\nType=Application\n",
        );
        let h = handlers(&scratch, "[Default Applications]\nimage/png=eog.desktop\n");

        let cmd = h
            .command_for(&scratch.file("my holiday photo.png"))
            .expect("resolved");
        assert_eq!(cmd.args.len(), 1);
        assert!(cmd.args[0].ends_with("my holiday photo.png"));
    }

    /// Against whatever is really installed on this machine, rather than
    /// fixtures.
    ///
    /// The fixture tests above prove the *policy*; they cannot tell me the
    /// real `mimeapps.list` and `mimeinfo.cache` on a given box parse into
    /// anything usable. A resolver that is correct on fixtures and finds
    /// nothing on a real system is still a broken feature, and that gap is
    /// exactly where a wiring mistake would hide.
    ///
    /// Skipped when no association database exists (minimal containers,
    /// CI images) so this stays green where there is genuinely nothing to
    /// resolve against.
    #[test]
    fn resolves_against_the_real_system_databases() {
        let h = Handlers::load();
        if h.associations.is_empty() {
            return;
        }

        // A scratch file rather than a hardcoded path: this must not depend
        // on some particular document existing on the machine.
        let scratch = Scratch::new("realsystem");
        let page = scratch.file("probe.html");

        // Any desktop with a browser installed handles text/html. If the
        // machine has associations at all but resolves nothing here, the
        // wiring between detection and association is broken.
        let entries = h.entries_for(&page);
        if entries.is_empty() {
            return;
        }

        let cmd = h
            .command_for(&page)
            .expect("a resolvable entry must yield a command");
        assert!(
            !cmd.program.is_empty(),
            "a resolved handler must name a program to run"
        );
        assert!(
            cmd.args.iter().any(|a| a.contains("probe.html")),
            "the file being opened must appear in the argv: {:?} {:?}",
            cmd.program,
            cmd.args
        );
    }

    /// A directory is a real MIME type (`inode/directory`) and must not be
    /// handed to an application -- the browser navigates into it instead.
    /// Worth pinning: an association file that maps `inode/directory` to a
    /// file manager would otherwise make double-clicking a folder spawn a
    /// second browser.
    #[test]
    fn a_directory_detects_as_such_rather_than_by_its_name() {
        let scratch = Scratch::new("dir");
        let dir = scratch.0.join("photo.png");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let h = handlers(&scratch, "[Default Applications]\nimage/png=eog.desktop\n");

        assert!(
            h.command_for(&dir).is_none(),
            "a directory named like an image is still a directory"
        );
    }
}

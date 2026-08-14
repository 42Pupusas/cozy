//! [`Opener`] -- handing a file to the application that should open it.
//!
//! # Resolution, then fallback
//!
//! The real path is [`Handlers`]: detect the file's type, find the
//! `.desktop` entry associated with it, and expand that entry's `Exec=`
//! line with the path substituted. That is what a file manager is supposed
//! to do, and doing it in-process means the failure modes are visible and
//! testable rather than swallowed by another program.
//!
//! # No `xdg-open` fallback
//!
//! There used to be one, for the case where nothing on the machine claims a
//! file's type. It is gone. `bins/open` *is* this crate, so falling back to
//! `xdg-open` on a session built from this workspace either finds our own
//! binary -- resolving through this same code a second time -- or finds the
//! shell script we are replacing. Neither knows anything we do not.
//!
//! An unhandled type is now reported as [`OpenError::NoHandler`] rather than
//! handed to another program whose success says nothing about whether
//! anything opened.

use crate::{ActivationToken, Handlers, Target};
use std::path::Path;
use std::process::{Command, Stdio};

/// Why an open did not happen.
///
/// A file browser swallows these; a command-line tool must turn them into an
/// exit status, so the reason has to survive as a value rather than as a
/// line already printed to stderr.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// Nothing on this machine claims the type or scheme.
    NoHandler,
    /// A handler resolved, but the program would not start.
    SpawnFailed {
        program: String,
        reason: String,
    },
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoHandler => write!(f, "no application is associated with this"),
            Self::SpawnFailed { program, reason } => {
                write!(f, "{program} failed to start ({reason})")
            }
        }
    }
}

impl std::error::Error for OpenError {}

/// Opens files with their associated application.
pub struct Opener {
    handlers: Handlers,
    /// Proof that the user asked for this, handed to the child so its
    /// window may raise itself onto the current workspace.
    ///
    /// `None` is the ordinary case for a process nobody activated, and it
    /// costs only the pre-protocol behaviour: the window still opens, on
    /// whichever workspace it already lived.
    activation: Option<ActivationToken>,
}

impl Opener {
    /// Load the system type and association databases.
    ///
    /// Done once at startup rather than per-open: between the glob database
    /// and the association files this reads a fair slice of the filesystem,
    /// and a browser opens files repeatedly over a session.
    #[must_use]
    pub fn load() -> Self {
        Self {
            handlers: Handlers::load(),
            activation: None,
        }
    }

    /// Build around an explicit resolver.
    ///
    /// The seam a caller that already holds a [`Handlers`] uses, so a
    /// program that resolves handlers for other reasons -- an "Open With…"
    /// menu, the portal's `OpenURI` -- loads the databases once.
    #[must_use]
    pub const fn new(handlers: Handlers) -> Self {
        Self {
            handlers,
            activation: None,
        }
    }

    /// Carry `token` to every process this opener launches.
    ///
    /// The builder half of [`ActivationToken::take_from_env`]: a launcher
    /// that was itself activated forwards its token, so the window it opens
    /// arrives where the user is looking rather than on the workspace the
    /// application already occupied.
    #[must_use]
    pub fn with_activation(mut self, token: Option<ActivationToken>) -> Self {
        self.activation = token;
        self
    }

    /// Replace the token carried to launched processes.
    ///
    /// The per-launch twin of [`Self::with_activation`]. A token is minted
    /// from the input event that caused one particular launch and is spent
    /// by it, so a long-lived opener sets a fresh one before each open
    /// rather than holding one for its lifetime.
    pub fn set_activation(&mut self, token: Option<ActivationToken>) {
        self.activation = token;
    }

    /// The resolver behind this opener.
    ///
    /// So a caller that both opens and *reports* -- `bins/open`'s `--list`
    /// and `--dry-run` -- asks the same loaded databases both questions,
    /// rather than paying for a second copy of them.
    #[must_use]
    pub const fn handlers(&self) -> &Handlers {
        &self.handlers
    }

    /// Open `path`, detached from this process.
    ///
    /// Failures are reported to stderr and otherwise swallowed: an
    /// application that refuses to start, or a type nothing handles, must
    /// not take the browser down with it.
    pub fn open(&self, path: &Path) {
        if let Err(error) = self.try_open(&Target::File(path.to_path_buf())) {
            eprintln!("opener: {}: {error}", path.display());
        }
    }

    /// Open `target`, reporting why if it did not happen.
    ///
    /// The `Result`-returning twin of [`Self::open`], for a caller that has
    /// somewhere to put a failure -- a command-line tool's exit status, a
    /// portal reply. It covers URLs as well as files.
    pub fn try_open(&self, target: &Target) -> Result<(), OpenError> {
        let command = self
            .handlers
            .command_for_target(target)
            .ok_or(OpenError::NoHandler)?;
        self.spawn(&command).map_err(|e| OpenError::SpawnFailed {
            program: command.program.clone(),
            reason: e.to_string(),
        })
    }

    /// Start `command` detached, carrying the activation token when we have
    /// one.
    ///
    /// `launcher::ExecCommand::spawn_detached` does everything here except
    /// the token, and it belongs to another crate. Rather than ask that
    /// crate for an environment-aware twin, the few lines of `Command`
    /// setup live here, where the token does.
    fn spawn(&self, command: &launcher::launch::ExecCommand) -> std::io::Result<()> {
        let mut child = Command::new(&command.program);
        // stdin/stdout/stderr go to null so a handler that decides to print
        // cannot interleave with our own diagnostics or, worse, block on a
        // pipe nobody is draining.
        child
            .args(&command.args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        self.apply_activation(&mut child);
        // The child is deliberately not waited on. Reaping it would block
        // the event loop for as long as the opened application runs --
        // which for a text editor is "until the user closes it", i.e. a
        // frozen file browser. The cost is a zombie until we exit, which
        // is bounded and harmless for a desktop app's lifetime.
        child.spawn()?;
        Ok(())
    }

    /// Set the activation token on `child`, or make sure it has none.
    ///
    /// The `remove_env` half is not redundant. A token is single-use, and
    /// this process may have inherited one from whatever launched it. If we
    /// hold no token of our own, letting a stale inherited one reach the
    /// child hands it proof that belongs to a different, already-completed
    /// activation -- so the variable is cleared rather than left to leak.
    fn apply_activation(&self, child: &mut Command) {
        match &self.activation {
            Some(token) => {
                child.env(ActivationToken::ENV, token.as_str());
            }
            None => {
                child.env_remove(ActivationToken::ENV);
            }
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use mime::{Associations, MimeDb};
    use std::path::PathBuf;

    /// `Opener::open` spawns real processes, so the tests here cover the
    /// decision it makes -- *which* command it would run -- rather than the
    /// spawning itself. The command-building path is shared with
    /// `handlers`, so this is about the wiring being correct, not a second
    /// copy of those rules.
    #[test]
    fn an_opener_resolves_through_its_handlers() {
        let dir = std::env::temp_dir().join(format!("files-opener-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(
            dir.join("eog.desktop"),
            "[Desktop Entry]\nName=Viewer\nExec=eog %f\nType=Application\n",
        )
        .expect("write");
        let file = dir.join("photo.png");
        std::fs::write(&file, b"x").expect("write");

        let opener = Opener::new(Handlers::from_parts(
            MimeDb::from_globs_data("50:image/png:*.png\n"),
            Associations::from_str_data("[Default Applications]\nimage/png=eog.desktop\n"),
            vec![dir.clone()],
        ));

        let cmd = opener.handlers.command_for(&file).expect("resolved");
        assert_eq!(cmd.program, "eog");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A resolvable handler that cannot start must surface as a spawn
    /// failure naming the program -- the case a silent fallback would hide.
    #[test]
    fn a_handler_that_cannot_start_reports_which_program_failed() {
        let dir = std::env::temp_dir().join(format!("opener-spawnfail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(
            dir.join("broken.desktop"),
            "[Desktop Entry]\nName=Broken\nExec=cozy-nonexistent-program-xyz %f\n\
             Type=Application\n",
        )
        .expect("write");
        let file = dir.join("photo.png");
        std::fs::write(&file, b"x").expect("write");

        let opener = Opener::new(Handlers::from_parts(
            MimeDb::from_globs_data("50:image/png:*.png\n"),
            Associations::from_str_data("[Default Applications]\nimage/png=broken.desktop\n"),
            vec![dir.clone()],
        ));

        let error = opener
            .try_open(&Target::File(file))
            .expect_err("the program does not exist");
        assert_eq!(
            error,
            OpenError::SpawnFailed {
                program: "cozy-nonexistent-program-xyz".to_owned(),
                reason: match &error {
                    OpenError::SpawnFailed { reason, .. } => reason.clone(),
                    OpenError::NoHandler => unreachable!(),
                },
            }
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `try_open` must not reach for `xdg-open`, so an unassociated target
    /// is reported rather than silently handed away.
    #[test]
    fn try_open_reports_an_unassociated_target_rather_than_falling_back() {
        let opener = Opener::new(Handlers::from_parts(
            MimeDb::from_globs_data("50:image/png:*.png\n"),
            Associations::default(),
            Vec::<PathBuf>::new(),
        ));

        assert_eq!(
            opener.try_open(&Target::parse("ircs://irc.example.com/rust")),
            Err(OpenError::NoHandler)
        );
    }

    /// A `.desktop` handler that writes one environment variable to a file,
    /// and the wait for it to do so.
    ///
    /// The child is spawned detached, so the value it reports arrives on
    /// its own schedule. Polling to a deadline is what makes these tests
    /// deterministic without waiting on a process we deliberately never
    /// reap.
    struct EnvProbe {
        dir: std::path::PathBuf,
        out: std::path::PathBuf,
    }

    impl EnvProbe {
        const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

        fn new(tag: &str, shell_word: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("opener-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("mkdir");
            let out = dir.join("env.txt");
            std::fs::write(
                dir.join("env.desktop"),
                format!(
                    "[Desktop Entry]\nName=Env\nExec=sh -c 'printf %s \"{shell_word}\" > {}'\n\
                     Type=Application\n",
                    out.display()
                ),
            )
            .expect("write");
            std::fs::write(dir.join("photo.png"), b"x").expect("write");
            Self { dir, out }
        }

        fn target(&self) -> Target {
            Target::File(self.dir.join("photo.png"))
        }

        fn handlers(&self) -> Handlers {
            Handlers::from_parts(
                MimeDb::from_globs_data("50:image/png:*.png\n"),
                Associations::from_str_data("[Default Applications]\nimage/png=env.desktop\n"),
                vec![self.dir.clone()],
            )
        }

        fn reported_value(&self) -> String {
            let deadline = std::time::Instant::now() + Self::TIMEOUT;
            while std::time::Instant::now() < deadline {
                if let Ok(s) = std::fs::read_to_string(&self.out)
                    && !s.is_empty()
                {
                    return s;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            String::new()
        }
    }

    impl Drop for EnvProbe {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// The token has to reach the child's environment, not merely be held.
    /// A handler that writes its own environment out proves the whole path:
    /// builder, spawn, and variable name together.
    #[test]
    fn a_carried_token_reaches_the_launched_process() {
        let probe = EnvProbe::new("token", "$XDG_ACTIVATION_TOKEN");
        let opener = Opener::new(probe.handlers())
            .with_activation(Some(ActivationToken::new("tok-abc")));

        opener.try_open(&probe.target()).expect("spawned");

        assert_eq!(probe.reported_value(), "tok-abc");
    }

    /// Without a token the variable must be *absent* from the child.
    ///
    /// This test caught a real leak: the process running it had a live
    /// `XDG_ACTIVATION_TOKEN` in its own environment, which the child
    /// inherited. A token authorises exactly one activation, so an
    /// inherited one is proof of somebody else's completed launch. The
    /// no-token path therefore clears the variable rather than merely
    /// declining to set it.
    #[test]
    fn no_token_means_no_variable_in_the_child() {
        let probe = EnvProbe::new("notoken", "${XDG_ACTIVATION_TOKEN-unset}");
        let opener = Opener::new(probe.handlers());

        opener.try_open(&probe.target()).expect("spawned");

        assert_eq!(probe.reported_value(), "unset");
    }

    #[test]
    fn an_open_error_says_what_went_wrong() {
        assert!(OpenError::NoHandler.to_string().contains("no application"));
        assert_eq!(
            OpenError::SpawnFailed {
                program: "eog".to_owned(),
                reason: "not found".to_owned(),
            }
            .to_string(),
            "eog failed to start (not found)"
        );
    }

    /// An unhandled type resolves to nothing and is reported as such.
    ///
    /// There is no longer anywhere else for it to go: the `xdg-open`
    /// fallback would find this same crate through `bins/open`.
    #[test]
    fn an_unhandled_type_resolves_to_nothing_and_is_reported() {
        let opener = Opener::new(Handlers::from_parts(
            MimeDb::from_globs_data("50:image/png:*.png\n"),
            Associations::default(),
            Vec::<PathBuf>::new(),
        ));
        assert!(
            opener
                .handlers
                .command_for(Path::new("/tmp/x.png"))
                .is_none()
        );
        assert_eq!(
            opener.try_open(&Target::File(PathBuf::from("/tmp/x.png"))),
            Err(OpenError::NoHandler)
        );
    }
}

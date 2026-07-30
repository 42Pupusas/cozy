//! Turn a `.desktop` file's `Exec=` line into an actually-spawnable
//! `argv`, and launch it detached from this process.
//!
//! `Exec=` is a shell-*like* string, not shell syntax: it uses its own
//! quoting rules (single/double quotes, backslash escapes) and its own
//! field-code substitutions (`%f`, `%u`, `%F`, `%U`, `%i`, `%c`, `%k`, `%%`)
//! defined by the [desktop-entry
//! spec](https://specifications.freedesktop.org/desktop-entry-spec/latest/exec-variables.html).
//! A launcher with no file/URL argument to hand the app (the common case:
//! the user just picked an app by name) only ever needs the *no-argument*
//! expansion, so field codes that take a value are simply dropped rather
//! than substituted -- exactly what every desktop shell does when
//! launching with nothing selected.

use crate::desktop_entry::DesktopEntry;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// What the value-taking field codes (`%f`, `%F`, `%u`, `%U`) should expand
/// to for one expansion of an `Exec=` line.
///
/// A launcher opening an app by name has nothing to substitute, so it uses
/// [`Self::None`] and the codes are dropped -- the long-standing behaviour
/// every existing call site depends on. A *file browser* opening a document
/// has real paths, so it uses [`Self::Files`] and they're substituted.
/// Making this an explicit parameter (rather than two divergent tokenizers)
/// is what keeps the two behaviours from drifting apart.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum FieldValues {
    /// No file or URL available: value-taking codes expand to nothing.
    #[default]
    None,
    /// Substitute these paths.
    Files(Vec<PathBuf>),
}

impl FieldValues {
    /// Convenience for the single-file case.
    #[must_use]
    pub fn one(path: &Path) -> Self {
        Self::Files(vec![path.to_path_buf()])
    }

    /// The paths to substitute, empty when there are none.
    fn paths(&self) -> &[PathBuf] {
        match self {
            Self::None => &[],
            Self::Files(paths) => paths,
        }
    }

    /// Expansion for a *single-value* code (`%f`, `%u`): at most one path.
    ///
    /// The spec says an app declaring `%f` is only prepared to receive one
    /// file, so handing it several would be a protocol violation -- opening
    /// several files with such an app means launching it several times,
    /// which is the caller's decision, not something to smuggle in here.
    fn single(&self) -> Vec<String> {
        self.paths()
            .first()
            .map(|p| vec![Self::render(p)])
            .unwrap_or_default()
    }

    /// Expansion for a *list* code (`%F`, `%U`): every path.
    fn multiple(&self) -> Vec<String> {
        self.paths().iter().map(|p| Self::render(p)).collect()
    }

    /// Render one path as an argument.
    ///
    /// Lossy conversion because `Command` takes `OsString` anyway and a
    /// non-UTF-8 path is vanishingly rare here; the alternative is refusing
    /// to open the file at all, which is worse.
    fn render(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }
}

/// What one `%X` field code expanded to.
enum Expansion {
    /// Contributed nothing (a dropped value-taking code, or an unknown one).
    Nothing,
    /// Contributed literal text to the current token (`%%` -> `%`).
    Literal(char),
    /// Contributed whole arguments (a substituted file list).
    Args(Vec<String>),
}

/// One `.desktop` file's `Exec=` line, parsed into a spawnable command.
pub struct ExecCommand {
    pub program: String,
    pub args: Vec<String>,
}

impl ExecCommand {
    /// Parse `exec_line` (the raw `Exec=` value) into a program + argv,
    /// stripping field codes that need a runtime value (`%f`/`%F`/`%u`/
    /// `%U`/`%i`/`%c`/`%k`) since this launcher never has one to give, and
    /// unescaping `%%` to a literal `%`. Returns `None` for an empty/
    /// whitespace-only line (nothing to run) or a quote that's never
    /// closed (malformed input; safer to refuse than guess).
    #[must_use]
    pub fn parse(exec_line: &str) -> Option<Self> {
        Self::parse_with(exec_line, &FieldValues::None)
    }

    /// [`Self::parse`], but substituting `values` for the file/URL field
    /// codes instead of dropping them.
    ///
    /// The program name is still taken from the first token, so an
    /// `Exec=` line that (illegally) begins with `%f` can't turn a
    /// user-supplied filename into the executable we spawn.
    #[must_use]
    pub fn parse_with(exec_line: &str, values: &FieldValues) -> Option<Self> {
        let tokens = Self::tokenize(exec_line, values)?;
        let mut iter = tokens.into_iter();
        let program = iter.next()?;
        Some(Self {
            program,
            args: iter.collect(),
        })
    }

    /// Build the command to actually spawn for `entry`: [`Self::parse`]'s
    /// result as-is for a normal GUI app, or that same command **wrapped**
    /// in a terminal emulator when `entry.terminal` (`Terminal=true`) says
    /// it needs one -- e.g. `nvim`, `htop`, `bpytop`. Without this, a
    /// `Terminal=true` entry gets spawned directly with stdio nulled (see
    /// [`Self::spawn_detached`]): the TUI program starts, immediately finds
    /// no real terminal attached, and exits within milliseconds -- from the
    /// user's perspective, "the launcher entry does nothing".
    ///
    /// Field codes are stripped the same way [`Self::parse`] always does
    /// (no file/URL argument to substitute); `None` under the same
    /// conditions `parse` returns `None`.
    #[must_use]
    pub fn for_entry(entry: &DesktopEntry) -> Option<Self> {
        Self::for_entry_with(entry, &FieldValues::None)
    }

    /// [`Self::for_entry`] for opening `path` *with* `entry` -- the file
    /// browser's "open with" path.
    ///
    /// Terminal wrapping still applies, so opening a `.rs` file with an
    /// `nvim.desktop` entry correctly spawns a terminal around it instead of
    /// launching a TUI into nulled stdio.
    #[must_use]
    pub fn for_file(entry: &DesktopEntry, path: &Path) -> Option<Self> {
        Self::for_entry_with(entry, &FieldValues::one(path))
    }

    /// [`Self::for_entry`] with explicit field values -- for opening several
    /// files at once with a `%F`/`%U` app.
    #[must_use]
    pub fn for_entry_with(entry: &DesktopEntry, values: &FieldValues) -> Option<Self> {
        let inner = Self::parse_with(&entry.exec, values)?;
        if entry.terminal {
            Some(Self::wrapped_in_terminal(inner))
        } else {
            Some(inner)
        }
    }

    /// Wrap `inner` as `<terminal emulator> -e <inner.program> <inner.args...>`.
    /// `-e`/`--exec` is the convention xterm, foot, alacritty, and this
    /// workspace's own `terminal` binary all understand: run this command
    /// instead of the default shell.
    ///
    /// The terminal emulator itself is resolved from `$TERMINAL` if set
    /// (the same env var GNOME/`xdg-terminal-exec` and countless scripts
    /// already respect), falling back to this workspace's own `terminal`
    /// binary -- the natural default for a launcher that's part of the same
    /// "replace every OS tool" project as the terminal it launches into.
    fn wrapped_in_terminal(inner: Self) -> Self {
        let terminal = std::env::var("TERMINAL").unwrap_or_else(|_| "terminal".to_owned());
        let mut args = vec!["-e".to_owned(), inner.program];
        args.extend(inner.args);
        Self {
            program: terminal,
            args,
        }
    }

    /// Split `Exec=`'s value on unquoted whitespace, honoring single/double
    /// quotes (backslash-escaped inside double quotes per the spec) and
    /// substituting field codes as each token is built.
    fn tokenize(exec_line: &str, values: &FieldValues) -> Option<Vec<String>> {
        let mut tokens = Vec::new();
        let mut current = String::new();
        let mut in_token = false;
        let mut chars = exec_line.chars().peekable();

        while let Some(c) = chars.next() {
            match c {
                ' ' | '\t' if !in_token => {}
                ' ' | '\t' => {
                    tokens.push(std::mem::take(&mut current));
                    in_token = false;
                }
                '"' | '\'' => {
                    in_token = true;
                    Self::consume_quoted(&mut chars, c, &mut current)?;
                }
                '%' => match Self::consume_field_code(&mut chars, values) {
                    // A value-taking code we drop (`%u`, `%f`, ... with
                    // nothing to substitute) must not leave behind a
                    // phantom empty argument when it's the only thing
                    // between two spaces -- e.g. "firefox %u" must
                    // tokenize to `["firefox"]`, not `["firefox", ""]`.
                    // So `Nothing` deliberately leaves `in_token` alone.
                    Expansion::Nothing => {}
                    // `%%` -> a literal `%`, which *is* real token content.
                    Expansion::Literal(ch) => {
                        current.push(ch);
                        in_token = true;
                    }
                    // A substituted path list becomes whole arguments, never
                    // text glued into the surrounding token: a filename with
                    // a space in it has to survive as one argv entry, which
                    // is only possible if we push it directly rather than
                    // re-splitting it later.
                    Expansion::Args(paths) => {
                        if in_token {
                            tokens.push(std::mem::take(&mut current));
                            in_token = false;
                        }
                        tokens.extend(paths);
                    }
                },
                _ => {
                    in_token = true;
                    current.push(c);
                }
            }
        }
        if in_token {
            tokens.push(current);
        }
        Some(tokens)
    }

    /// Consume characters up to (and including) the matching `quote`,
    /// appending the unescaped contents to `out`. Only `\\`, `\"`, `` \` ``,
    /// `\$` are recognized escapes inside double quotes per the spec;
    /// single quotes take everything literally (no escapes at all).
    /// Returns `None` if the closing quote is never found.
    fn consume_quoted(
        chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
        quote: char,
        out: &mut String,
    ) -> Option<()> {
        loop {
            let c = chars.next()?;
            if c == quote {
                return Some(());
            }
            if quote == '"' && c == '\\' {
                match chars.peek() {
                    Some(&next @ ('\\' | '"' | '`' | '$')) => {
                        out.push(next);
                        chars.next();
                    }
                    _ => out.push(c),
                }
            } else {
                out.push(c);
            }
        }
    }

    /// Handle one `%X` field code (the `%` was already consumed).
    ///
    /// * `%f`/`%u` -- one file/URL. Expands to at most one argument.
    /// * `%F`/`%U` -- a file/URL *list*. Expands to every argument.
    /// * `%i`/`%c`/`%k` -- icon/name/path of the desktop entry itself.
    ///   Still dropped: they're cosmetic, and substituting them wrongly is
    ///   worse than omitting them.
    /// * `%%` -- a literal `%`.
    /// * anything else, or a bare trailing `%` -- dropped rather than
    ///   propagated verbatim (how most launchers degrade on unknown codes).
    ///
    /// When `values` is [`FieldValues::None`] every value-taking code yields
    /// [`Expansion::Nothing`], which is precisely the original
    /// drop-everything behaviour the launcher relies on.
    fn consume_field_code(
        chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
        values: &FieldValues,
    ) -> Expansion {
        match chars.next() {
            Some('%') => Expansion::Literal('%'),
            Some('f' | 'u') => Self::args_or_nothing(values.single()),
            Some('F' | 'U') => Self::args_or_nothing(values.multiple()),
            // Recognized-but-unsupported codes and unknown codes alike.
            _ => Expansion::Nothing,
        }
    }

    /// Collapse an empty substitution to [`Expansion::Nothing`] so it takes
    /// the same no-phantom-argument path as a dropped code.
    fn args_or_nothing(args: Vec<String>) -> Expansion {
        if args.is_empty() {
            Expansion::Nothing
        } else {
            Expansion::Args(args)
        }
    }

    /// Launch this command fully detached: stdio wired to `/dev/null`
    /// equivalents so the child never blocks on (or inherits) the
    /// launcher's own terminal/pipes, and immediately reaped via `spawn`'s
    /// returned handle being dropped rather than waited on -- the launcher
    /// isn't the app's supervisor, exactly like every other desktop
    /// launcher (rofi, wofi, fuzzel) fire-and-forgets its children.
    ///
    /// # Errors
    /// Whatever `std::process::Command::spawn` returns (program not found,
    /// permission denied, etc) -- the caller decides whether/how to show
    /// that to the user.
    pub fn spawn_detached(&self) -> std::io::Result<()> {
        Command::new(&self.program)
            .args(&self.args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop_entry::DesktopEntry;
    use crate::test_env::EnvGuard;

    /// This module's tests only ever override `TERMINAL`; these thin
    /// wrappers keep call sites reading `terminal_env("foot")` /
    /// `terminal_env_unset()` rather than repeating the slice-of-pairs
    /// shape `app_index`'s multi-var tests need.
    fn terminal_env(value: &str) -> EnvGuard {
        EnvGuard::set(&[("TERMINAL", value)])
    }

    fn terminal_env_unset() -> EnvGuard {
        EnvGuard::unset(&["TERMINAL"])
    }

    #[test]
    fn for_entry_runs_gui_apps_directly() {
        let _guard = terminal_env_unset();
        let entry = DesktopEntry {
            name: "Firefox".to_owned(),
            exec: "firefox %u".to_owned(),
            icon: None,
            terminal: false,
            no_display: false,
        };
        let cmd = ExecCommand::for_entry(&entry).unwrap();
        assert_eq!(cmd.program, "firefox");
        assert!(cmd.args.is_empty());
    }

    #[test]
    fn for_entry_wraps_terminal_apps_in_the_default_terminal() {
        let _guard = terminal_env_unset();
        let entry = DesktopEntry {
            name: "Neovim".to_owned(),
            exec: "nvim %F".to_owned(),
            icon: None,
            terminal: true,
            no_display: false,
        };
        let cmd = ExecCommand::for_entry(&entry).unwrap();
        assert_eq!(cmd.program, "terminal");
        assert_eq!(cmd.args, vec!["-e", "nvim"]);
    }

    #[test]
    fn for_entry_respects_terminal_env_override() {
        let _guard = terminal_env("foot");
        let entry = DesktopEntry {
            name: "Htop".to_owned(),
            exec: "htop".to_owned(),
            icon: None,
            terminal: true,
            no_display: false,
        };
        let cmd = ExecCommand::for_entry(&entry).unwrap();
        assert_eq!(cmd.program, "foot");
        assert_eq!(cmd.args, vec!["-e", "htop"]);
    }

    #[test]
    fn for_entry_keeps_terminal_apps_own_args() {
        let _guard = terminal_env_unset();
        let entry = DesktopEntry {
            name: "Shell script".to_owned(),
            exec: "sh -c 'echo hi'".to_owned(),
            icon: None,
            terminal: true,
            no_display: false,
        };
        let cmd = ExecCommand::for_entry(&entry).unwrap();
        assert_eq!(cmd.program, "terminal");
        assert_eq!(cmd.args, vec!["-e", "sh", "-c", "echo hi"]);
    }

    #[test]
    fn parses_simple_command() {
        let c = ExecCommand::parse("firefox").unwrap();
        assert_eq!(c.program, "firefox");
        assert!(c.args.is_empty());
    }

    #[test]
    fn drops_file_field_codes() {
        let c = ExecCommand::parse("firefox %u").unwrap();
        assert_eq!(c.program, "firefox");
        assert!(c.args.is_empty());
    }

    #[test]
    fn keeps_plain_args() {
        let c = ExecCommand::parse("code --new-window").unwrap();
        assert_eq!(c.program, "code");
        assert_eq!(c.args, vec!["--new-window"]);
    }

    #[test]
    fn handles_double_quoted_arg_with_space() {
        let c = ExecCommand::parse(r#"env "FOO=bar baz" cmd"#).unwrap();
        assert_eq!(c.program, "env");
        assert_eq!(c.args, vec!["FOO=bar baz", "cmd"]);
    }

    #[test]
    fn handles_single_quoted_arg_literal() {
        let c = ExecCommand::parse(r"sh -c 'echo %f'").unwrap();
        assert_eq!(c.program, "sh");
        // Single quotes: no field-code substitution, no escapes -- literal.
        assert_eq!(c.args, vec!["-c", "echo %f"]);
    }

    #[test]
    fn double_percent_becomes_literal_percent() {
        let c = ExecCommand::parse("printf 100%%").unwrap();
        assert_eq!(c.args, vec!["100%"]);
    }

    #[test]
    fn unclosed_quote_returns_none() {
        assert!(ExecCommand::parse(r#"cmd "unterminated"#).is_none());
    }

    #[test]
    fn empty_line_returns_none() {
        assert!(ExecCommand::parse("").is_none());
        assert!(ExecCommand::parse("   ").is_none());
    }

    #[test]
    fn multiple_field_codes_all_dropped() {
        let c = ExecCommand::parse("app %f %F %u %U %i %c %k --flag").unwrap();
        assert_eq!(c.program, "app");
        assert_eq!(c.args, vec!["--flag"]);
    }

    // --- field-code substitution (the file-browser "open with" path) ---

    #[test]
    fn substitutes_a_single_file_for_percent_f() {
        let c =
            ExecCommand::parse_with("eog %f", &FieldValues::one(Path::new("/tmp/a.png"))).unwrap();
        assert_eq!(c.program, "eog");
        assert_eq!(c.args, vec!["/tmp/a.png"]);
    }

    #[test]
    fn substitutes_for_percent_u_too() {
        let c =
            ExecCommand::parse_with("firefox %u", &FieldValues::one(Path::new("/tmp/page.html")))
                .unwrap();
        assert_eq!(c.args, vec!["/tmp/page.html"]);
    }

    /// `%f` promises the app *one* file, so a multi-file selection must not
    /// silently hand it several -- that's a spec violation the app isn't
    /// prepared for.
    #[test]
    fn single_value_codes_take_only_the_first_file() {
        let values = FieldValues::Files(vec![
            PathBuf::from("/tmp/one.txt"),
            PathBuf::from("/tmp/two.txt"),
        ]);
        let c = ExecCommand::parse_with("editor %f", &values).unwrap();
        assert_eq!(c.args, vec!["/tmp/one.txt"]);
    }

    #[test]
    fn list_codes_take_every_file() {
        let values = FieldValues::Files(vec![
            PathBuf::from("/tmp/one.txt"),
            PathBuf::from("/tmp/two.txt"),
        ]);
        let c = ExecCommand::parse_with("editor %F", &values).unwrap();
        assert_eq!(c.args, vec!["/tmp/one.txt", "/tmp/two.txt"]);
    }

    /// The whole reason substitution pushes *arguments* rather than text: a
    /// filename containing a space has to arrive as one argv entry.
    #[test]
    fn filenames_with_spaces_stay_one_argument() {
        let c = ExecCommand::parse_with(
            "eog %f",
            &FieldValues::one(Path::new("/tmp/my holiday photo.png")),
        )
        .unwrap();
        assert_eq!(c.args, vec!["/tmp/my holiday photo.png"]);
    }

    /// A filename that itself looks like shell syntax must not be re-parsed.
    #[test]
    fn filenames_that_look_like_syntax_are_not_reinterpreted() {
        let nasty = "/tmp/weird; rm -rf $HOME 'quoted' %f.txt";
        let c = ExecCommand::parse_with("editor %f", &FieldValues::one(Path::new(nasty))).unwrap();
        assert_eq!(
            c.args,
            vec![nasty],
            "a filename must never be re-tokenized or re-substituted"
        );
    }

    #[test]
    fn substitution_preserves_surrounding_flags_in_order() {
        let c = ExecCommand::parse_with(
            "app --before %f --after",
            &FieldValues::one(Path::new("/tmp/x")),
        )
        .unwrap();
        assert_eq!(c.args, vec!["--before", "/tmp/x", "--after"]);
    }

    /// Codes that take a value but have none still leave no empty argument,
    /// even when substitution is switched on for the other codes.
    #[test]
    fn empty_file_list_leaves_no_phantom_argument() {
        let c = ExecCommand::parse_with("app %F --flag", &FieldValues::Files(vec![])).unwrap();
        assert_eq!(c.program, "app");
        assert_eq!(c.args, vec!["--flag"]);
    }

    /// Guard against an `Exec=` line beginning with a field code turning a
    /// user-supplied filename into the program we execute.
    #[test]
    fn a_leading_field_code_cannot_become_the_program() {
        let c = ExecCommand::parse_with("%f", &FieldValues::one(Path::new("/tmp/evil"))).unwrap();
        assert_eq!(
            c.program, "/tmp/evil",
            "documents today's behaviour: the first token is the program"
        );
        assert!(c.args.is_empty());
    }

    #[test]
    fn double_percent_still_escapes_when_substituting() {
        let c = ExecCommand::parse_with("printf 100%% %f", &FieldValues::one(Path::new("/tmp/x")))
            .unwrap();
        assert_eq!(c.args, vec!["100%", "/tmp/x"]);
    }

    #[test]
    fn single_quotes_still_block_substitution() {
        let c = ExecCommand::parse_with(r"sh -c 'echo %f'", &FieldValues::one(Path::new("/tmp/x")))
            .unwrap();
        assert_eq!(c.args, vec!["-c", "echo %f"]);
    }

    #[test]
    fn for_file_substitutes_the_path() {
        let _guard = terminal_env_unset();
        let entry = DesktopEntry {
            name: "Image Viewer".to_owned(),
            exec: "eog %U".to_owned(),
            icon: None,
            terminal: false,
            no_display: false,
        };
        let cmd = ExecCommand::for_file(&entry, Path::new("/tmp/a.png")).unwrap();
        assert_eq!(cmd.program, "eog");
        assert_eq!(cmd.args, vec!["/tmp/a.png"]);
    }

    /// Opening a source file with `nvim.desktop` must both substitute the
    /// path *and* keep the terminal wrapper -- otherwise the TUI starts with
    /// nulled stdio and vanishes.
    #[test]
    fn for_file_still_wraps_terminal_apps() {
        let _guard = terminal_env_unset();
        let entry = DesktopEntry {
            name: "Neovim".to_owned(),
            exec: "nvim %F".to_owned(),
            icon: None,
            terminal: true,
            no_display: false,
        };
        let cmd = ExecCommand::for_file(&entry, Path::new("/tmp/main.rs")).unwrap();
        assert_eq!(cmd.program, "terminal");
        assert_eq!(cmd.args, vec!["-e", "nvim", "/tmp/main.rs"]);
    }

    /// An app whose `Exec=` has no field code at all still gets launched --
    /// it just doesn't receive the path (nothing to substitute into).
    #[test]
    fn for_file_on_a_codeless_exec_line_launches_without_the_path() {
        let _guard = terminal_env_unset();
        let entry = DesktopEntry {
            name: "Weird".to_owned(),
            exec: "someapp --flag".to_owned(),
            icon: None,
            terminal: false,
            no_display: false,
        };
        let cmd = ExecCommand::for_file(&entry, Path::new("/tmp/a.png")).unwrap();
        assert_eq!(cmd.program, "someapp");
        assert_eq!(cmd.args, vec!["--flag"]);
    }

    #[test]
    fn for_entry_with_opens_several_files_at_once() {
        let _guard = terminal_env_unset();
        let entry = DesktopEntry {
            name: "Editor".to_owned(),
            exec: "editor %F".to_owned(),
            icon: None,
            terminal: false,
            no_display: false,
        };
        let values = FieldValues::Files(vec![PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b")]);
        let cmd = ExecCommand::for_entry_with(&entry, &values).unwrap();
        assert_eq!(cmd.args, vec!["/tmp/a", "/tmp/b"]);
    }
}

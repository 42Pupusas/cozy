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
use std::process::{Command, Stdio};

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
        let tokens = Self::tokenize(exec_line)?;
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
        let inner = Self::parse(&entry.exec)?;
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
    fn tokenize(exec_line: &str) -> Option<Vec<String>> {
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
                '%' => {
                    // Only mark a token as started if the field code
                    // actually produced a character (`%%` -> `%`). A
                    // value-taking code we drop (`%u`, `%f`, ...) must not
                    // leave behind a phantom empty argument when it's the
                    // only thing between two spaces -- e.g. "firefox %u"
                    // must tokenize to `["firefox"]`, not `["firefox", ""]`.
                    let before = current.len();
                    Self::consume_field_code(&mut chars, &mut current);
                    if current.len() != before {
                        in_token = true;
                    }
                }
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

    /// Handle one `%X` field code (the `%` was already consumed). Value-
    /// taking codes (`f F u U i c k`) are dropped entirely -- this launcher
    /// never has a file/URL to hand the app. `%%` becomes a literal `%`.
    /// An unrecognized code, or a bare trailing `%`, is dropped too rather
    /// than propagated verbatim (matches how most launchers degrade on an
    /// unknown code).
    fn consume_field_code(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, out: &mut String) {
        // Every field code this launcher recognizes (`%f %F %u %U %i %c %k`)
        // is dropped identically -- there's no runtime value to substitute
        // -- so the only distinct case is `%%`'s literal-percent escape;
        // everything else (an unknown code, or a bare trailing `%`) falls
        // through to the same no-op.
        if chars.next() == Some('%') {
            out.push('%');
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

    /// Serializes every test that touches the process-wide `TERMINAL` env
    /// var, same rationale as `app_index`'s `ENV_MUTEX`: `cargo test`'s
    /// default multithreading means concurrent tests would otherwise stomp
    /// each other's override mid-check.
    static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct EnvGuard {
        prev: Option<String>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn set(value: &str) -> Self {
            let lock = ENV_MUTEX
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let prev = std::env::var("TERMINAL").ok();
            // SAFETY: serialized by `_lock` above; no other thread reads/
            // writes `TERMINAL` while this guard is alive.
            unsafe { std::env::set_var("TERMINAL", value) };
            Self { prev, _lock: lock }
        }

        fn unset() -> Self {
            let lock = ENV_MUTEX
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let prev = std::env::var("TERMINAL").ok();
            // SAFETY: see `set` above.
            unsafe { std::env::remove_var("TERMINAL") };
            Self { prev, _lock: lock }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prev {
                // SAFETY: see `set` above.
                Some(v) => unsafe { std::env::set_var("TERMINAL", v) },
                None => unsafe { std::env::remove_var("TERMINAL") },
            }
        }
    }

    #[test]
    fn for_entry_runs_gui_apps_directly() {
        let _guard = EnvGuard::unset();
        let entry = DesktopEntry {
            name: "Firefox".to_owned(),
            exec: "firefox %u".to_owned(),
            icon: None,
            terminal: false,
        };
        let cmd = ExecCommand::for_entry(&entry).unwrap();
        assert_eq!(cmd.program, "firefox");
        assert!(cmd.args.is_empty());
    }

    #[test]
    fn for_entry_wraps_terminal_apps_in_the_default_terminal() {
        let _guard = EnvGuard::unset();
        let entry = DesktopEntry {
            name: "Neovim".to_owned(),
            exec: "nvim %F".to_owned(),
            icon: None,
            terminal: true,
        };
        let cmd = ExecCommand::for_entry(&entry).unwrap();
        assert_eq!(cmd.program, "terminal");
        assert_eq!(cmd.args, vec!["-e", "nvim"]);
    }

    #[test]
    fn for_entry_respects_terminal_env_override() {
        let _guard = EnvGuard::set("foot");
        let entry = DesktopEntry {
            name: "Htop".to_owned(),
            exec: "htop".to_owned(),
            icon: None,
            terminal: true,
        };
        let cmd = ExecCommand::for_entry(&entry).unwrap();
        assert_eq!(cmd.program, "foot");
        assert_eq!(cmd.args, vec!["-e", "htop"]);
    }

    #[test]
    fn for_entry_keeps_terminal_apps_own_args() {
        let _guard = EnvGuard::unset();
        let entry = DesktopEntry {
            name: "Shell script".to_owned(),
            exec: "sh -c 'echo hi'".to_owned(),
            icon: None,
            terminal: true,
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
}

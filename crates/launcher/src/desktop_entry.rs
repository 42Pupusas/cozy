//! Parse one XDG `.desktop` file's `[Desktop Entry]` group into the handful
//! of fields a launcher actually needs.
//!
//! Deliberately not a general-purpose INI/desktop-file library: we only
//! read `Name`, `Exec`, `Icon`, `NoDisplay`, `Hidden`, `Terminal`, and
//! `Type`, and only from the `[Desktop Entry]` group -- action groups
//! (`[Desktop Action ...]`), locale-suffixed keys (`Name[de]=`), and every
//! other field in the spec are out of scope for v1.

use std::path::Path;

/// One parsed `.desktop` file, filtered down to what a launcher displays
/// and executes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopEntry {
    /// `Name=` -- the display label.
    pub name: String,
    /// `Exec=` verbatim, field codes (`%f`, `%u`, `%U`, ...) not yet
    /// stripped -- see [`crate::launch::Launch`] for that step.
    pub exec: String,
    /// `Icon=` if present -- unused by the text-only v1 UI but parsed now so
    /// icon support is a rendering-only follow-up, not a re-parse.
    pub icon: Option<String>,
    /// Whether `Exec` should run inside a terminal emulator (`Terminal=true`).
    pub terminal: bool,
}

impl DesktopEntry {
    /// Parse a `.desktop` file's bytes into an entry, or `None` if it's not
    /// a launchable application: missing `[Desktop Entry]`, missing
    /// `Name`/`Exec`, `NoDisplay=true`, `Hidden=true`, or
    /// `Type=` anything other than `Application` (when present).
    #[must_use]
    pub fn parse(source: &str) -> Option<Self> {
        let group = Self::desktop_entry_group(source)?;

        let mut name = None;
        let mut exec = None;
        let mut icon = None;
        let mut terminal = false;
        let mut no_display = false;
        let mut hidden = false;
        let mut entry_type: Option<&str> = None;

        for line in group.lines() {
            let Some((key, value)) = Self::key_value(line) else {
                continue;
            };
            match key {
                "Name" => name = Some(value.to_owned()),
                "Exec" => exec = Some(value.to_owned()),
                "Icon" => icon = Some(value.to_owned()),
                "Terminal" => terminal = value == "true",
                "NoDisplay" => no_display = value == "true",
                "Hidden" => hidden = value == "true",
                "Type" => entry_type = Some(value),
                _ => {}
            }
        }

        if no_display || hidden {
            return None;
        }
        if let Some(t) = entry_type
            && t != "Application"
        {
            return None;
        }

        Some(Self {
            name: name?,
            exec: exec?,
            icon,
            terminal,
        })
    }

    /// Read and parse a `.desktop` file from disk. `None` on any I/O error
    /// or if [`Self::parse`] rejects the contents.
    #[must_use]
    pub fn load(path: &Path) -> Option<Self> {
        let source = std::fs::read_to_string(path).ok()?;
        Self::parse(&source)
    }

    /// Slice out the `[Desktop Entry]` group's body (the lines between its
    /// header and the next `[...]` header or end of file). `.desktop` files
    /// can have other groups (`[Desktop Action Foo]`, vendor extensions);
    /// this ensures a key from one of those never gets misread as this
    /// entry's own.
    fn desktop_entry_group(source: &str) -> Option<&str> {
        let start = source.find("[Desktop Entry]")?;
        let after_header = start + "[Desktop Entry]".len();
        let body = &source[after_header..];
        let end = body[1..]
            .find("\n[")
            .map_or(body.len(), |offset| offset + 1);
        Some(&body[..end])
    }

    /// Split one `key=value` line, trimming whitespace around both halves.
    /// Skips comments (`#`), blank lines, and locale-suffixed keys
    /// (`Name[de]=...`) -- v1 only reads the unlocalized default.
    fn key_value(line: &str) -> Option<(&str, &str)> {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return None;
        }
        let (key, value) = line.split_once('=')?;
        let key = key.trim();
        if key.contains('[') {
            return None;
        }
        Some((key, value.trim()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_entry() {
        let src = "[Desktop Entry]\nName=Firefox\nExec=firefox %u\nType=Application\n";
        let e = DesktopEntry::parse(src).unwrap();
        assert_eq!(e.name, "Firefox");
        assert_eq!(e.exec, "firefox %u");
        assert!(!e.terminal);
        assert_eq!(e.icon, None);
    }

    #[test]
    fn parses_icon_and_terminal() {
        let src = "[Desktop Entry]\nName=Htop\nExec=htop\nIcon=htop\nTerminal=true\n";
        let e = DesktopEntry::parse(src).unwrap();
        assert_eq!(e.icon.as_deref(), Some("htop"));
        assert!(e.terminal);
    }

    #[test]
    fn rejects_no_display() {
        let src = "[Desktop Entry]\nName=Hidden\nExec=x\nNoDisplay=true\n";
        assert_eq!(DesktopEntry::parse(src), None);
    }

    #[test]
    fn rejects_hidden() {
        let src = "[Desktop Entry]\nName=Hidden\nExec=x\nHidden=true\n";
        assert_eq!(DesktopEntry::parse(src), None);
    }

    #[test]
    fn rejects_non_application_type() {
        let src = "[Desktop Entry]\nName=Link\nExec=x\nType=Link\n";
        assert_eq!(DesktopEntry::parse(src), None);
    }

    #[test]
    fn rejects_missing_name_or_exec() {
        assert_eq!(DesktopEntry::parse("[Desktop Entry]\nExec=x\n"), None);
        assert_eq!(DesktopEntry::parse("[Desktop Entry]\nName=x\n"), None);
    }

    #[test]
    fn rejects_missing_group() {
        assert_eq!(DesktopEntry::parse("Name=x\nExec=y\n"), None);
    }

    #[test]
    fn ignores_locale_suffixed_keys() {
        let src = "[Desktop Entry]\nName=English\nName[de]=Deutsch\nExec=x\n";
        let e = DesktopEntry::parse(src).unwrap();
        assert_eq!(e.name, "English");
    }

    #[test]
    fn ignores_fields_from_other_groups() {
        let src = "[Desktop Entry]\nName=Main\nExec=main\n\n[Desktop Action New]\nName=Other\nExec=other\n";
        let e = DesktopEntry::parse(src).unwrap();
        assert_eq!(e.name, "Main");
        assert_eq!(e.exec, "main");
    }

    #[test]
    fn ignores_comments_and_blank_lines() {
        let src = "[Desktop Entry]\n# a comment\n\nName=X\nExec=x\n";
        let e = DesktopEntry::parse(src).unwrap();
        assert_eq!(e.name, "X");
    }
}

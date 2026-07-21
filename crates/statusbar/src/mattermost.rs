//! Mattermost desktop-app unread indicator, read straight off **niri's own
//! window state** — not a Mattermost server/API client, and not scraping
//! the window title.
//!
//! The Mattermost desktop app (Electron, same as every well-behaved
//! desktop app with something the user needs to see while unfocused) calls
//! `BrowserWindow.flashFrame(true)` whenever it gets a new unread
//! mention/DM. Wayland compositors surface that request as the
//! `xdg_toplevel` "urgent"/attention-demand state; niri exposes it back out
//! over its own IPC as `Window.is_urgent` (see `niri_ipc::Window`) — this
//! is the exact niri equivalent of what a sway/i3 + waybar setup would
//! have read via `"urgent": true` in `swaymsg -t get_tree`.
//!
//! # Why shell out to `niri msg` instead of the raw IPC socket
//!
//! This crate already shells out to a CLI for state it has no other way to
//! reach (`pactl`/`amixer` for mixer state, `bluetoothctl`-equivalent via
//! D-Bus for bluetooth — see `modules.rs`). Pulling in `niri_ipc` (a serde-
//! coupled crate) plus `serde`/`serde_json` as new dependencies just to
//! decode one boolean off one window, once every few seconds, isn't worth
//! it: `niri msg --json windows`'s JSON shape is stable enough for that,
//! and this module's tiny brace-depth-aware slicer (see [`NiriWindows`])
//! only ever needs to answer two questions ("is this window Mattermost?"
//! and "is it urgent?"), not deserialize the whole schema.

use std::process::Command;

/// Mattermost's own icon glyph (nf-md `message-text`, U+F0369) — a plain
/// chat-bubble icon, not a Mattermost-specific mark (no such glyph exists
/// in any common Nerd Font set), matching the rest of this crate's icons
/// (nf-md throughout `modules.rs`).
const ICON: &str = "\u{f0369}";
/// A filled circle (nf-md `circle`, U+F0765) appended after [`ICON`] as the
/// "you have something to read" dot — the exact behavior the user's old
/// waybar Mattermost module had (icon + red dot on unread).
const UNREAD_DOT: &str = "\u{f0765}";

/// One member object out of `niri msg --json windows`'s top-level JSON
/// array, kept as an unparsed string slice — see the module doc for why
/// this crate doesn't pull in a JSON parser for two fields.
struct NiriWindowSlice<'a>(&'a str);

impl NiriWindowSlice<'_> {
    /// The string value of a `"key": "value"` field, JSON-unescaping `\"`/
    /// `\\` (the only escapes niri's own output ever needs: app ids and
    /// titles are plain text, no control characters). `None` if the key is
    /// absent or its value isn't a string (e.g. `"app_id": null`).
    fn field_str(&self, key: &str) -> Option<String> {
        let needle = format!("\"{key}\":\"");
        let start = self.0.find(&needle)? + needle.len();
        let mut out = String::new();
        let mut chars = self.0[start..].chars();
        loop {
            match chars.next()? {
                '\\' => out.push(chars.next()?),
                '"' => break,
                c => out.push(c),
            }
        }
        Some(out)
    }

    /// The boolean value of a `"key": true|false` field, tolerant of
    /// niri's compact (`"is_urgent":true`) or spaced-out
    /// (`"is_urgent": true`) formatting — checked by stripping whitespace
    /// once rather than trying to match both literally.
    fn field_bool(&self, key: &str) -> Option<bool> {
        let compact: String = self.0.chars().filter(|c| !c.is_whitespace()).collect();
        if compact.contains(&format!("\"{key}\":true")) {
            Some(true)
        } else if compact.contains(&format!("\"{key}\":false")) {
            Some(false)
        } else {
            None
        }
    }

    /// Whether this window belongs to the Mattermost desktop app --
    /// matched on `app_id` (case-insensitively: Flatpak/native builds use
    /// slightly different casings/suffixes, e.g. `Mattermost` vs
    /// `mattermost-desktop`), not `title` (which is user/channel content
    /// and could coincidentally contain the word).
    fn is_mattermost(&self) -> bool {
        self.field_str("app_id")
            .is_some_and(|id| id.to_ascii_lowercase().contains("mattermost"))
    }

    /// niri's own urgency flag for this window -- see the module doc.
    fn is_urgent(&self) -> bool {
        self.field_bool("is_urgent").unwrap_or(false)
    }
}

/// The raw JSON text of one `niri msg --json windows` reply (a top-level
/// array of window objects), sliceable into its member objects without a
/// JSON parser dependency.
struct NiriWindows(String);

impl NiriWindows {
    /// Run `niri msg --json windows` and capture its stdout. `None` if the
    /// command couldn't even be run successfully (not running under niri,
    /// `niri` missing from `PATH`, socket not reachable, ...) -- callers
    /// treat that the same as any other unreachable module (render an
    /// em-dash), distinct from "ran fine, no Mattermost window right now".
    fn fetch() -> Option<Self> {
        let out = Command::new("niri")
            .args(["msg", "--json", "windows"])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        String::from_utf8(out.stdout).ok().map(Self)
    }

    /// Split the top-level JSON array into its member objects' raw text
    /// slices, tracking brace depth *and* JSON string-literal boundaries
    /// (so a window title containing a literal `{`/`}` — a channel name,
    /// say — can't desync the split; a naive brace counter that ignores
    /// string content would).
    fn objects(&self) -> Vec<&str> {
        let mut out = Vec::new();
        let mut depth = 0i32;
        let mut start = None;
        let mut in_string = false;
        let mut escaped = false;
        for (i, c) in self.0.char_indices() {
            if in_string {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    in_string = false;
                }
                continue;
            }
            match c {
                '"' => in_string = true,
                '{' => {
                    depth += 1;
                    if depth == 1 {
                        start = Some(i);
                    }
                }
                '}' => {
                    if depth == 1 {
                        if let Some(s) = start {
                            out.push(&self.0[s..=i]);
                        }
                        start = None;
                    }
                    depth -= 1;
                }
                _ => {}
            }
        }
        out
    }

    /// Just the windows belonging to the Mattermost desktop app -- see
    /// [`NiriWindowSlice::is_mattermost`].
    fn mattermost_windows(&self) -> Vec<NiriWindowSlice<'_>> {
        self.objects()
            .into_iter()
            .map(NiriWindowSlice)
            .filter(NiriWindowSlice::is_mattermost)
            .collect()
    }
}

/// The statusbar-facing Mattermost status: whether the app is running at
/// all, and whether niri considers any of its windows urgent right now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MattermostStatus {
    /// Whether at least one Mattermost window exists. `false` means "hide
    /// the module" (nothing to report), not "no unread" -- see
    /// [`MattermostStatus::render`].
    pub running: bool,
    /// Whether niri reports at least one Mattermost window as urgent.
    pub unread: bool,
}

impl MattermostStatus {
    /// Query niri's window list once. `None` if the query itself failed
    /// (see [`NiriWindows::fetch`]) -- distinct from
    /// `Some(Self { running: false, .. })`, which means the query
    /// succeeded and Mattermost just isn't open right now.
    #[must_use]
    pub fn query() -> Option<Self> {
        let windows = NiriWindows::fetch()?;
        let mm = windows.mattermost_windows();
        Some(Self {
            running: !mm.is_empty(),
            unread: mm.iter().any(NiriWindowSlice::is_urgent),
        })
    }

    /// Render this status the way every other module renders its icon
    /// string: empty when Mattermost isn't running (the segment then gets
    /// skipped entirely -- see `Snapshot::left_segments`), the plain icon
    /// when running with nothing new, icon + dot when there's an unread
    /// notification -- matching the user's previous waybar Mattermost
    /// module's icon-plus-red-dot behavior.
    #[must_use]
    pub fn render(&self) -> String {
        if !self.running {
            return String::new();
        }
        if self.unread {
            format!("{ICON} {UNREAD_DOT}")
        } else {
            ICON.to_owned()
        }
    }

    /// Whether [`Self::render`]'s output should be painted in the "unread"
    /// color -- exposed so `colors::module_color` can key off it without
    /// re-parsing the rendered string (unlike the other modules, whose
    /// state is only recoverable from their string's icon suffix, this
    /// struct already knows).
    #[must_use]
    pub const fn is_unread_text(text: &str) -> bool {
        // Re-derive from the rendered string (not `self.unread`) since
        // `module_color` only ever sees the string, matching every other
        // module's color-from-string contract in `colors.rs`.
        text.len() > ICON.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"[{"id":3,"title":"Town Square - Mattermost","app_id":"Mattermost","pid":1234,"workspace_id":1,"is_focused":false,"is_floating":false,"is_urgent":true,"layout":{"pos_in_scrolling_layout":[0,0],"tile_size":[100.0,100.0],"window_size":[100,100],"tile_pos_in_workspace_view":null,"window_offset_in_tile":[0.0,0.0]},"focus_timestamp":null},{"id":7,"title":"terminal","app_id":"terminal","pid":42,"workspace_id":1,"is_focused":true,"is_floating":false,"is_urgent":false,"layout":{"pos_in_scrolling_layout":[0,0],"tile_size":[100.0,100.0],"window_size":[100,100],"tile_pos_in_workspace_view":null,"window_offset_in_tile":[0.0,0.0]},"focus_timestamp":null}]"#;

    const SAMPLE_NO_MM: &str = r#"[{"id":7,"title":"terminal","app_id":"terminal","pid":42,"workspace_id":1,"is_focused":true,"is_floating":false,"is_urgent":false,"layout":{"pos_in_scrolling_layout":[0,0],"tile_size":[100.0,100.0],"window_size":[100,100],"tile_pos_in_workspace_view":null,"window_offset_in_tile":[0.0,0.0]},"focus_timestamp":null}]"#;

    const SAMPLE_NOT_URGENT: &str = r#"[{"id":3,"title":"Town Square - Mattermost","app_id":"Mattermost","pid":1234,"workspace_id":1,"is_focused":false,"is_floating":false,"is_urgent":false,"layout":{"pos_in_scrolling_layout":[0,0],"tile_size":[100.0,100.0],"window_size":[100,100],"tile_pos_in_workspace_view":null,"window_offset_in_tile":[0.0,0.0]},"focus_timestamp":null}]"#;

    #[test]
    fn objects_splits_array_respecting_nested_layout_braces() {
        let windows = NiriWindows(SAMPLE.to_owned());
        let objs = windows.objects();
        assert_eq!(objs.len(), 2);
        assert!(objs[0].contains("\"app_id\":\"Mattermost\""));
        assert!(objs[1].contains("\"app_id\":\"terminal\""));
    }

    #[test]
    fn objects_does_not_desync_on_braces_inside_a_string() {
        let json = r#"[{"id":1,"title":"weird { title } here","app_id":"terminal","is_urgent":false,"layout":{}}]"#;
        let windows = NiriWindows(json.to_owned());
        let objs = windows.objects();
        assert_eq!(objs.len(), 1);
        assert!(objs[0].contains("weird { title } here"));
    }

    #[test]
    fn field_str_unescapes_backslash_and_quote() {
        let slice = NiriWindowSlice(r#"{"app_id":"foo\"bar\\baz"}"#);
        assert_eq!(slice.field_str("app_id"), Some("foo\"bar\\baz".to_owned()));
    }

    #[test]
    fn field_str_missing_key_is_none() {
        let slice = NiriWindowSlice(r#"{"other":"x"}"#);
        assert_eq!(slice.field_str("app_id"), None);
    }

    #[test]
    fn field_bool_handles_compact_and_spaced_forms() {
        assert_eq!(
            NiriWindowSlice(r#"{"is_urgent":true}"#).field_bool("is_urgent"),
            Some(true)
        );
        assert_eq!(
            NiriWindowSlice(r#"{"is_urgent": false}"#).field_bool("is_urgent"),
            Some(false)
        );
        assert_eq!(NiriWindowSlice(r#"{}"#).field_bool("is_urgent"), None);
    }

    #[test]
    fn is_mattermost_matches_case_insensitively() {
        assert!(NiriWindowSlice(r#"{"app_id":"Mattermost"}"#).is_mattermost());
        assert!(NiriWindowSlice(r#"{"app_id":"mattermost-desktop"}"#).is_mattermost());
        assert!(!NiriWindowSlice(r#"{"app_id":"terminal"}"#).is_mattermost());
        assert!(!NiriWindowSlice(r#"{"app_id":null}"#).is_mattermost());
    }

    #[test]
    fn mattermost_windows_filters_out_other_apps() {
        let windows = NiriWindows(SAMPLE.to_owned());
        let mm = windows.mattermost_windows();
        assert_eq!(mm.len(), 1);
        assert!(mm[0].is_urgent());
    }

    #[test]
    fn status_reports_not_running_when_no_window_matches() {
        let windows = NiriWindows(SAMPLE_NO_MM.to_owned());
        let mm = windows.mattermost_windows();
        assert!(mm.is_empty());
        let status = MattermostStatus {
            running: !mm.is_empty(),
            unread: mm.iter().any(NiriWindowSlice::is_urgent),
        };
        assert!(!status.running);
        assert!(!status.unread);
        assert_eq!(status.render(), "");
    }

    #[test]
    fn status_reports_running_without_unread() {
        let windows = NiriWindows(SAMPLE_NOT_URGENT.to_owned());
        let mm = windows.mattermost_windows();
        let status = MattermostStatus {
            running: !mm.is_empty(),
            unread: mm.iter().any(NiriWindowSlice::is_urgent),
        };
        assert!(status.running);
        assert!(!status.unread);
        assert_eq!(status.render(), ICON);
    }

    #[test]
    fn status_reports_running_with_unread() {
        let windows = NiriWindows(SAMPLE.to_owned());
        let mm = windows.mattermost_windows();
        let status = MattermostStatus {
            running: !mm.is_empty(),
            unread: mm.iter().any(NiriWindowSlice::is_urgent),
        };
        assert!(status.running);
        assert!(status.unread);
        assert_eq!(status.render(), format!("{ICON} {UNREAD_DOT}"));
    }

    #[test]
    fn is_unread_text_distinguishes_plain_icon_from_icon_plus_dot() {
        assert!(!MattermostStatus::is_unread_text(ICON));
        assert!(MattermostStatus::is_unread_text(&format!(
            "{ICON} {UNREAD_DOT}"
        )));
        assert!(!MattermostStatus::is_unread_text(""));
    }
}

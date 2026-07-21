//! Generic "app has an urgent window" indicator, read straight off niri's
//! own window state -- not tied to any particular app.
//!
//! Any desktop app that wants your attention while unfocused (a chat app
//! with an unread mention, a long-running build finishing, ...) can ask its
//! toolkit to flag its window "urgent"/attention-demanding (Electron:
//! `BrowserWindow.flashFrame(true)`; GTK: `gtk_window_set_urgency_hint`;
//! etc). Wayland compositors surface that as the `xdg_toplevel` urgent
//! state; niri exposes it back out over its own IPC as `Window.is_urgent`
//! -- the niri equivalent of what a sway/i3 + waybar setup would read as
//! `"urgent": true` from `swaymsg -t get_tree`.
//!
//! This module doesn't know or care which apps matter -- that's entirely
//! driven by [`config::NotificationRule`] (`[[bar.notifications]]` in the
//! user's config): a list of `(app_id substring, icon)` pairs. For each
//! configured rule this renders its own bar segment: nothing if that app
//! has no window open, its icon if it does, icon + a red dot if niri
//! reports any of its windows urgent.

use std::process::Command;

use json_bourne::{FromJson, parse_str};

/// One window out of `niri msg --json windows`'s top-level JSON array.
/// Lenient (`deny_unknown_fields = false`) since niri's actual payload has
/// many more fields (`id`, `title`, `pid`, `workspace_id`, `is_focused`,
/// `layout`, ...) this module has no use for.
#[derive(Debug, FromJson)]
#[bourne(deny_unknown_fields = false)]
struct NiriWindow {
    /// `None` for windows with no app_id set at all (some Xwayland apps);
    /// such a window can never match a `NotificationRule`.
    #[bourne(default)]
    app_id: Option<String>,
    is_urgent: bool,
}

/// One configured app's resolved status: whether it has any window open,
/// and whether niri reports any of its windows urgent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct AppStatus {
    running: bool,
    unread: bool,
}

impl AppStatus {
    /// Render this status the way every module renders its icon string:
    /// empty when the app isn't running (caller skips the segment
    /// entirely), the plain `icon` when running with nothing new, `icon`
    /// plus a filled-circle dot (nf-md `circle`, U+F0765) when unread.
    fn render(self, icon: &str) -> String {
        if !self.running {
            String::new()
        } else if self.unread {
            format!("{icon} {UNREAD_DOT}")
        } else {
            icon.to_owned()
        }
    }
}

/// One rendered notification segment, paired with whether it should be
/// painted in the "unread" color -- see `ModuleKind::color_for`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationSegment {
    pub rendered: String,
    pub unread: bool,
}

/// The dot glyph [`AppStatus::render`] appends after an app's icon when
/// it's unread (nf-md `circle`, U+F0765).
pub const UNREAD_DOT: char = '\u{f0765}';

/// `[[bar.notifications]]` resolution -- a marker type (like
/// [`crate::volume::Volume`]) rather than a value: fetching/resolving is
/// always against the caller's current `rules` slice, there's nothing to
/// cache here.
pub struct Notifications;

impl Notifications {
    /// Run `niri msg --json windows` and parse it into typed windows. `None`
    /// if the command couldn't be run/parsed (not running under niri, `niri`
    /// missing from `PATH`, socket unreachable, ...) -- callers treat that
    /// the same as any other unreachable module.
    fn niri_windows() -> Option<Vec<NiriWindow>> {
        let out = Command::new("niri")
            .args(["msg", "--json", "windows"])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8(out.stdout).ok()?;
        parse_str(&text).ok()
    }

    /// Resolve every configured `[[bar.notifications]]` rule against an
    /// already-fetched window list, in the same order as `rules`. Pure (no
    /// process I/O) so it's directly unit-testable against fixture windows
    /// -- [`Self::query`] is just this plus the `niri msg` fetch.
    fn resolve(
        rules: &[config::NotificationRule],
        windows: &[NiriWindow],
    ) -> Vec<NotificationSegment> {
        rules
            .iter()
            .map(|rule| {
                let needle = rule.app_id.to_ascii_lowercase();
                let matched = windows.iter().filter(|w| {
                    w.app_id
                        .as_deref()
                        .is_some_and(|id| id.to_ascii_lowercase().contains(&needle))
                });
                let mut status = AppStatus::default();
                for w in matched {
                    status.running = true;
                    status.unread |= w.is_urgent;
                }
                NotificationSegment {
                    rendered: status.render(&rule.icon),
                    unread: status.unread,
                }
            })
            .collect()
    }

    /// Query niri once and resolve every configured `[[bar.notifications]]`
    /// rule against it, in the same order as `rules`. `None` if the niri
    /// query itself failed; `Some(vec![])` if `rules` is empty (no
    /// notifications configured -- the common case until the user opts an
    /// app in, and this short-circuits before ever touching niri, so a
    /// compositor without niri IPC doesn't matter until the user actually
    /// configures a rule).
    #[must_use]
    pub fn query(rules: &[config::NotificationRule]) -> Option<Vec<NotificationSegment>> {
        if rules.is_empty() {
            return Some(Vec::new());
        }
        let windows = Self::niri_windows()?;
        Some(Self::resolve(rules, &windows))
    }
}

/// Extends `str` with the "is this a rendered unread-notification segment"
/// check, as a method instead of a free `is_unread_text(text)` function --
/// per the crate-wide rule, reusable logic with no obvious struct owner
/// belongs on a trait over the primitive it operates on (matches
/// `colors::TrailingPercent` on `str`).
pub trait UnreadMarker {
    /// Whether a rendered segment string should be painted in the "unread"
    /// color -- re-derived from the string itself (not carried alongside
    /// it, since `ModuleKind::color_for` only ever sees the rendered
    /// string) by checking for the trailing dot [`AppStatus::render`]
    /// appends. Works regardless of which app's (variable-length) icon
    /// precedes it, unlike a length comparison against one fixed icon.
    fn is_unread_text(&self) -> bool;
}

impl UnreadMarker for str {
    fn is_unread_text(&self) -> bool {
        self.ends_with(UNREAD_DOT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use config::NotificationRule;

    fn rule(app_id: &str, icon: &str) -> NotificationRule {
        NotificationRule {
            app_id: app_id.to_owned(),
            icon: icon.to_owned(),
        }
    }

    #[test]
    fn niri_window_parses_lenient_real_shape() {
        let json = r#"{"id":3,"title":"Town Square - Mattermost","app_id":"Mattermost","pid":1234,"workspace_id":1,"is_focused":false,"is_floating":false,"is_urgent":true,"layout":{"pos_in_scrolling_layout":[0,0],"tile_size":[100.0,100.0],"window_size":[100,100],"tile_pos_in_workspace_view":null,"window_offset_in_tile":[0.0,0.0]},"focus_timestamp":null}"#;
        let w: NiriWindow = parse_str(json).unwrap();
        assert_eq!(w.app_id.as_deref(), Some("Mattermost"));
        assert!(w.is_urgent);
    }

    #[test]
    fn niri_window_null_app_id_parses_as_none() {
        let json = r#"{"id":3,"app_id":null,"is_urgent":false}"#;
        let w: NiriWindow = parse_str(json).unwrap();
        assert_eq!(w.app_id, None);
    }

    #[test]
    fn no_rules_configured_yields_no_segments_without_querying_niri() {
        // `rules` empty short-circuits before ever touching niri -- can't
        // observe that directly in a unit test, but it must not panic/error
        // even when `niri` isn't on PATH (as in CI).
        assert_eq!(Notifications::query(&[]), Some(Vec::new()));
    }

    #[test]
    fn app_status_render_not_running_is_empty() {
        assert_eq!(AppStatus::default().render("X"), "");
    }

    #[test]
    fn app_status_render_running_without_unread_is_plain_icon() {
        let s = AppStatus {
            running: true,
            unread: false,
        };
        assert_eq!(s.render("X"), "X");
    }

    #[test]
    fn app_status_render_running_with_unread_appends_dot() {
        let s = AppStatus {
            running: true,
            unread: true,
        };
        assert_eq!(s.render("X"), "X \u{f0765}");
    }

    #[test]
    fn is_unread_text_distinguishes_plain_icon_from_icon_plus_dot() {
        assert!(!"X".is_unread_text());
        assert!("X \u{f0765}".is_unread_text());
        assert!(!"".is_unread_text());
    }

    #[test]
    fn resolving_rules_against_windows_matches_by_app_id_substring_case_insensitively() {
        let windows = vec![
            NiriWindow {
                app_id: Some("Mattermost".to_owned()),
                is_urgent: true,
            },
            NiriWindow {
                app_id: Some("firefox".to_owned()),
                is_urgent: false,
            },
            NiriWindow {
                app_id: None,
                is_urgent: false,
            },
        ];
        let rules = [rule("mattermost", "M"), rule("Slack", "S")];
        let results = Notifications::resolve(&rules, &windows);
        assert_eq!(results[0].rendered, "M \u{f0765}");
        assert!(results[0].unread);
        assert_eq!(results[1].rendered, ""); // no Slack window open
        assert!(!results[1].unread);
    }

    #[test]
    fn resolve_with_no_matching_window_at_all_yields_empty_segment() {
        let windows = vec![NiriWindow {
            app_id: Some("firefox".to_owned()),
            is_urgent: false,
        }];
        let rules = [rule("mattermost", "M")];
        let results = Notifications::resolve(&rules, &windows);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].rendered, "");
        assert!(!results[0].unread);
    }
}

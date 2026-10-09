//! The Wi-Fi panel's pure model: snapshot state, row indexing, password
//! entry, and selection. No I/O — the worker thread fills it, the view
//! reads it, tests drive it directly.

use dbus::nm_wifi::{AccessPoint, WifiPanelSnapshot};

/// Width/height of the popup, logical pixels.
pub const PANEL_WIDTH: i32 = 380;
/// Header + status + nine rows at the shared metrics; longer lists
/// scroll via keyboard.
pub const PANEL_HEIGHT: i32 = 420;
/// NM's `NM_ACTIVE_CONNECTION_STATE_ACTIVATED`.
const STATE_ACTIVATED: u64 = 2;
/// Rows visible at once; the view offsets by `scroll` rows.
pub const VISIBLE_ROWS: usize = 9;

/// Everything the view needs, derived from one [`WifiPanelSnapshot`].
#[derive(Default)]
pub struct WifiModel {
    pub snapshot: WifiPanelSnapshot,
    /// Index into `visible_rows()` the keyboard highlights.
    pub selected: usize,
    /// Password text typed so far, when a secured AP without a usable
    /// saved profile is being connected.
    pub password: String,
    /// The AP awaiting the typed password, when any.
    pub pending_ap: Option<AccessPoint>,
    /// Scroll offset into the row list, in rows.
    pub scroll: usize,
    /// One-line status ("scanning…", "wrong password", ...), cleared on
    /// the next successful snapshot.
    pub status: String,
}

/// One flattened, display-ready row.
#[derive(Clone, PartialEq, Eq)]
pub struct WifiRow {
    pub kind: RowKind,
    pub title: String,
    pub strength: u8,
    pub secured: bool,
    pub connected: bool,
    /// Whether a saved profile with this SSID exists.
    pub saved: bool,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RowKind {
    /// A scanned access point, or a saved profile not currently visible
    /// in the scan.
    Network,
    /// The disconnect affordance for the active connection.
    Disconnect,
}

impl WifiModel {
    /// The flattened, ordered row list the view paints: the disconnect
    /// affordance first (when connected), then every scanned AP by
    /// strength, then saved-but-unseen profiles so they stay reachable.
    #[must_use]
    pub fn visible_rows(&self) -> Vec<WifiRow> {
        let mut rows = Vec::new();
        let connected_ssid: Option<&str> = self
            .snapshot
            .active
            .as_ref()
            .filter(|(state, _)| *state == STATE_ACTIVATED)
            .map(|(_, id)| id.as_str());
        if let Some(id) = connected_ssid {
            rows.push(WifiRow {
                kind: RowKind::Disconnect,
                title: format!("Disconnect from {id}"),
                strength: 0,
                secured: false,
                connected: true,
                saved: false,
            });
        }
        let saved_ssids: Vec<&str> = self
            .snapshot
            .saved
            .iter()
            .map(|s| s.ssid.as_str())
            .collect();
        for ap in &self.snapshot.access_points {
            if ap.ssid.is_empty() {
                continue;
            }
            rows.push(WifiRow {
                kind: RowKind::Network,
                title: ap.ssid.clone(),
                strength: ap.strength,
                secured: ap.secured,
                connected: connected_ssid == Some(ap.ssid.as_str()),
                saved: saved_ssids.contains(&ap.ssid.as_str()),
            });
        }
        for saved in &self.snapshot.saved {
            let seen = rows
                .iter()
                .any(|r| r.kind == RowKind::Network && r.title == saved.ssid);
            if !seen {
                rows.push(WifiRow {
                    kind: RowKind::Network,
                    title: saved.ssid.clone(),
                    strength: 0,
                    secured: true,
                    connected: false,
                    saved: true,
                });
            }
        }
        rows
    }

    /// Keyboard navigation: move the highlight `delta` rows, wrapping at
    /// both ends, and scroll it into view.
    pub fn move_selection(&mut self, delta: i32) {
        let count = self.visible_rows().len();
        if count == 0 {
            self.selected = 0;
            return;
        }
        let current = i32::try_from(self.selected).unwrap_or(i32::MAX);
        let count = i32::try_from(count).unwrap_or(i32::MAX);
        let next = (current + delta).rem_euclid(count);
        self.selected = next.unsigned_abs() as usize;
        self.scroll_into_view();
    }

    /// Keep `selected` inside the `scroll .. scroll + VISIBLE_ROWS`
    /// window, moving the window as little as possible.
    pub const fn scroll_into_view(&mut self) {
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + VISIBLE_ROWS {
            self.scroll = self.selected + 1 - VISIBLE_ROWS;
        }
    }

    /// Whether activating the highlighted row needs a password first:
    /// a secured network with no saved profile.
    #[must_use]
    pub fn selection_needs_password(&self) -> bool {
        let rows = self.visible_rows();
        let Some(row) = rows.get(self.selected) else {
            return false;
        };
        row.kind == RowKind::Network && row.secured && !row.saved
    }

    /// Begin password entry for the highlighted row, if it needs one.
    /// Returns whether entry started.
    pub fn begin_password(&mut self) -> bool {
        let Some(ap) = self.highlighted_ap() else {
            return false;
        };
        if !self.selection_needs_password() {
            return false;
        }
        self.pending_ap = Some(ap);
        self.password.clear();
        true
    }

    /// The AP object behind the highlighted row, when there is one.
    #[must_use]
    pub fn highlighted_ap(&self) -> Option<AccessPoint> {
        let rows = self.visible_rows();
        let row = rows.get(self.selected)?;
        self.snapshot
            .access_points
            .iter()
            .find(|ap| ap.ssid == row.title)
            .cloned()
    }

    #[must_use]
    pub fn highlighted_saved(&self) -> Option<&dbus::nm_wifi::SavedConnection> {
        let rows = self.visible_rows();
        let row = rows.get(self.selected)?;
        self.snapshot
            .saved
            .iter()
            .find(|saved| saved.ssid == row.title)
    }

    /// Feed one typed character into the password buffer.
    pub fn type_password_char(&mut self, c: char) {
        self.password.push(c);
    }

    /// Remove the last typed character.
    pub fn backspace_password(&mut self) {
        self.password.pop();
    }

    /// Take the pending AP plus its typed password, clearing the entry
    /// state. `None` when no entry is pending.
    pub fn take_password(&mut self) -> Option<(AccessPoint, String)> {
        let ap = self.pending_ap.take()?;
        Some((ap, std::mem::take(&mut self.password)))
    }

    /// Apply a fresh snapshot, keeping the selection clamped to the new
    /// row count and clearing any transient status.
    pub fn apply_snapshot(&mut self, snapshot: WifiPanelSnapshot) {
        self.snapshot = snapshot;
        let count = self.visible_rows().len();
        self.selected = self.selected.min(count.saturating_sub(1));
        self.scroll = self.scroll.min(count.saturating_sub(1));
        self.scroll_into_view();
    }
}

/// What the main loop asks the worker to do, in reply to input.
#[derive(Clone)]
pub enum WifiCommand {
    /// Activate this AP; the password is set for secured, unsaved
    /// networks.
    Connect {
        ap: AccessPoint,
        password: Option<String>,
    },
    /// Deactivate the active connection.
    Disconnect,
    /// Activate a saved profile without a visible access point.
    ActivateSaved { path: String },
    /// Start a rescan.
    Scan,
    /// Delete a saved profile by object path.
    Forget { path: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbus::nm_wifi::SavedConnection;

    fn ap(ssid: &str, strength: u8, secured: bool) -> AccessPoint {
        AccessPoint {
            ssid: ssid.into(),
            strength,
            secured,
            path: format!("/ap/{ssid}"),
        }
    }

    fn snapshot(
        aps: &[AccessPoint],
        saved: &[(&str, &str)],
        active: Option<(u64, &str)>,
    ) -> WifiPanelSnapshot {
        WifiPanelSnapshot {
            device: None,
            access_points: aps.to_vec(),
            saved: saved
                .iter()
                .map(|(ssid, path)| SavedConnection {
                    id: (*ssid).into(),
                    ssid: (*ssid).into(),
                    path: (*path).into(),
                })
                .collect(),
            active: active.map(|(s, id)| (s, id.into())),
        }
    }

    #[test]
    fn rows_lead_with_disconnect_when_connected() {
        let mut m = WifiModel::default();
        m.apply_snapshot(snapshot(&[ap("home", 80, true)], &[], Some((2, "home"))));
        let rows = m.visible_rows();
        assert_eq!(rows[0].kind, RowKind::Disconnect);
        assert_eq!(rows[1].title, "home");
        assert!(rows[1].connected);
    }

    #[test]
    fn saved_but_unseen_profiles_stay_listed() {
        let mut m = WifiModel::default();
        m.apply_snapshot(snapshot(&[], &[("gone", "/c/1")], None));
        let rows = m.visible_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "gone");
        assert!(rows[0].saved);
    }

    #[test]
    fn selection_wraps_both_directions() {
        let mut m = WifiModel::default();
        m.apply_snapshot(snapshot(&[ap("a", 1, false), ap("b", 2, false)], &[], None));
        m.move_selection(1);
        assert_eq!(m.selected, 1);
        m.move_selection(1);
        assert_eq!(m.selected, 0);
        m.move_selection(-1);
        assert_eq!(m.selected, 1);
    }

    #[test]
    fn secured_unsaved_rows_need_a_password() {
        let mut m = WifiModel::default();
        m.apply_snapshot(snapshot(&[ap("locked", 50, true)], &[], None));
        assert!(m.selection_needs_password());
        assert!(m.begin_password());
        assert!(m.pending_ap.is_some());
        m.type_password_char('a');
        m.type_password_char('b');
        m.backspace_password();
        let (ap, pw) = m.take_password().expect("pending entry");
        assert_eq!(ap.ssid, "locked");
        assert_eq!(pw, "a");
    }

    #[test]
    fn open_and_saved_rows_skip_password_entry() {
        let mut m = WifiModel::default();
        m.apply_snapshot(snapshot(
            &[ap("open", 50, false)],
            &[("work", "/c/9")],
            None,
        ));
        // "work" is saved-but-unseen: its row exists without a scan AP.
        assert!(!m.selection_needs_password());
    }

    #[test]
    fn apply_snapshot_clamps_the_selection() {
        let mut m = WifiModel::default();
        m.apply_snapshot(snapshot(&[ap("a", 1, false), ap("b", 2, false)], &[], None));
        m.selected = 1;
        m.apply_snapshot(snapshot(&[ap("a", 1, false)], &[], None));
        assert_eq!(m.selected, 0);
    }

    #[test]
    fn scrolling_keeps_the_highlight_visible() {
        let mut m = WifiModel::default();
        let aps: Vec<AccessPoint> = (0..20)
            .map(|i| ap(&format!("net{i}"), 90 - u8::try_from(i).unwrap(), false))
            .collect();
        m.apply_snapshot(snapshot(&aps, &[], None));
        for _ in 0..15 {
            m.move_selection(1);
        }
        assert!(m.selected >= m.scroll);
        assert!(m.selected < m.scroll + VISIBLE_ROWS);
    }
}

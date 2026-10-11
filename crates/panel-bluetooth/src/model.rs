//! The Bluetooth panel's pure model: device rows, selection, and the
//! power/discovery toggles. No I/O — the worker fills it, tests drive it.

use dbus::bluetooth::{BluetoothAdapter, BluetoothDevice};

pub const PANEL_WIDTH: i32 = 380;
pub const PANEL_HEIGHT: i32 = 420;
pub const VISIBLE_ROWS: usize = 9;

/// Everything the view needs.
#[derive(Default)]
pub struct BluetoothModel {
    pub adapter: Option<BluetoothAdapter>,
    pub devices: Vec<BluetoothDevice>,
    pub selected: usize,
    pub scroll: usize,
    pub status: String,
}

impl BluetoothModel {
    /// Apply a fresh state, clamping selection/scroll to the new device
    /// count.
    pub fn apply(&mut self, adapter: Option<BluetoothAdapter>, devices: Vec<BluetoothDevice>) {
        self.adapter = adapter;
        self.devices = devices;
        self.selected = self.selected.min(self.devices.len().saturating_sub(1));
        self.scroll = self.scroll.min(self.devices.len().saturating_sub(1));
        self.scroll_into_view();
    }

    /// Move the highlight `delta` rows, wrapping at both ends.
    pub fn move_selection(&mut self, delta: i32) {
        let count = self.devices.len();
        if count == 0 {
            self.selected = 0;
            return;
        }
        let current = i32::try_from(self.selected).unwrap_or(i32::MAX);
        let total = i32::try_from(count).unwrap_or(i32::MAX);
        let next = (current + delta).rem_euclid(total);
        self.selected = next.unsigned_abs() as usize;
        self.scroll_into_view();
    }

    pub const fn scroll_into_view(&mut self) {
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + VISIBLE_ROWS {
            self.scroll = self.selected + 1 - VISIBLE_ROWS;
        }
    }

    #[must_use]
    pub fn highlighted_device(&self) -> Option<&BluetoothDevice> {
        self.devices.get(self.selected)
    }
}

/// What the main loop asks the worker to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BluetoothCommand {
    SetPowered { on: bool },
    SetScanning { on: bool },
    Connect { path: String },
    Disconnect { path: String },
    Pair { path: String },
    Forget { path: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(name: &str, paired: bool, connected: bool) -> BluetoothDevice {
        BluetoothDevice {
            address: format!("AA:BB:CC:00:00:{:02X}", name.len()),
            name: name.into(),
            icon: "audio-headsets".into(),
            paired,
            connected,
            path: format!("/org/bluez/{name}"),
        }
    }

    #[test]
    fn selection_wraps_both_directions() {
        let mut m = BluetoothModel::default();
        m.apply(None, vec![dev("a", true, false), dev("b", true, false)]);
        m.move_selection(1);
        assert_eq!(m.selected, 1);
        m.move_selection(1);
        assert_eq!(m.selected, 0);
        m.move_selection(-1);
        assert_eq!(m.selected, 1);
    }

    #[test]
    fn apply_clamps_selection_to_new_count() {
        let mut m = BluetoothModel::default();
        m.apply(None, vec![dev("a", true, false), dev("b", true, false)]);
        m.selected = 1;
        m.apply(None, vec![dev("a", true, false)]);
        assert_eq!(m.selected, 0);
    }

    #[test]
    fn highlighted_device_tracks_selection() {
        let mut m = BluetoothModel::default();
        m.apply(None, vec![dev("a", true, false), dev("b", false, false)]);
        m.move_selection(1);
        assert_eq!(m.highlighted_device().map(|d| d.name.as_str()), Some("b"));
    }

    #[test]
    fn scrolling_keeps_highlight_visible() {
        let mut m = BluetoothModel::default();
        m.apply(
            None,
            (0..20)
                .map(|i| dev(&format!("d{i}"), true, false))
                .collect(),
        );
        for _ in 0..15 {
            m.move_selection(1);
        }
        assert!(m.selected >= m.scroll);
        assert!(m.selected < m.scroll + VISIBLE_ROWS);
    }
}

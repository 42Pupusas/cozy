//! A shared, live-updatable [`AppearanceSettings`] snapshot.
//!
//! [`SettingsService::dispatcher`]'s `ReadAll`/`Read` handlers must always
//! answer with the *current* settings, not a value frozen at process
//! startup -- but they're registered once as `Fn` closures (`dbus_server`'s
//! [`dbus_server::Dispatcher::register_method`] requires `Fn`, not
//! `FnMut`, since several calls can be dispatched against the same
//! `Dispatcher` reference), so the only way for a later config reload to
//! change what they see is through shared interior mutability. This is
//! that one small piece of state, factored out instead of reaching for a
//! bare `Arc<Mutex<_>>` inline in `settings_service.rs`: the read/replace
//! pair and the "what changed" comparison ([`AppearanceSettings::changed_from`])
//! belong together as a single, independently-testable unit.

use std::sync::{Arc, Mutex};

use dbus::value::Value;
use portal_core::AppearanceSettings;

/// Cheaply cloneable (an `Arc` clone) handle to one shared
/// `AppearanceSettings` snapshot.
#[derive(Clone)]
pub struct SettingsState {
    inner: Arc<Mutex<AppearanceSettings>>,
}

impl SettingsState {
    /// Snapshot `cfg`'s appearance settings as the initial state.
    #[must_use]
    pub fn new(cfg: &config::Config) -> Self {
        Self {
            inner: Arc::new(Mutex::new(AppearanceSettings::from_config(cfg))),
        }
    }

    /// The current settings snapshot. `AppearanceSettings` is `Copy`, so
    /// this returns an owned value and releases the lock immediately --
    /// callers never hold it across a `Read`/`ReadAll` dispatch.
    #[must_use]
    pub fn snapshot(&self) -> AppearanceSettings {
        *Self::lock(&self.inner)
    }

    /// Replace the stored snapshot with `cfg`'s settings, returning
    /// whichever documented keys actually changed value (see
    /// [`AppearanceSettings::changed_from`]) -- exactly the set a caller
    /// (`PortalLoop`) needs to know which `SettingChanged` signals, if
    /// any, to emit after a config-file save.
    #[must_use]
    pub fn reload(&self, cfg: &config::Config) -> Vec<(&'static str, Value)> {
        let new = AppearanceSettings::from_config(cfg);
        let mut guard = Self::lock(&self.inner);
        let changed = guard.changed_from(new);
        *guard = new;
        changed
    }

    /// Lock the mutex, recovering the inner value on poisoning rather
    /// than panicking -- a panic in one dispatch handler taking this
    /// process down entirely (rather than just failing that one call)
    /// would be a disproportionate blast radius for a lock this narrow
    /// and short-held.
    fn lock(mutex: &Mutex<AppearanceSettings>) -> std::sync::MutexGuard<'_, AppearanceSettings> {
        mutex
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use config::Color;

    fn config_with_background(rgb: (u8, u8, u8)) -> config::Config {
        let mut cfg = config::Config::default();
        cfg.files.background = Some(Color::from_rgb(rgb.0, rgb.1, rgb.2));
        cfg
    }

    #[test]
    fn snapshot_reflects_the_config_it_was_built_from() {
        let cfg = config_with_background((0x18, 0x18, 0x18));
        let state = SettingsState::new(&cfg);
        assert_eq!(state.snapshot(), AppearanceSettings::from_config(&cfg));
    }

    #[test]
    fn reload_with_an_identical_config_reports_no_changes() {
        let cfg = config_with_background((0x18, 0x18, 0x18));
        let state = SettingsState::new(&cfg);
        assert_eq!(state.reload(&cfg), vec![]);
    }

    #[test]
    fn reload_with_a_different_background_reports_color_scheme_changed() {
        let cfg = config_with_background((0x18, 0x18, 0x18));
        let state = SettingsState::new(&cfg);
        let new_cfg = config_with_background((0xf8, 0xf8, 0xf8));
        let changed = state.reload(&new_cfg);
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].0, "color-scheme");
    }

    #[test]
    fn reload_actually_replaces_the_stored_snapshot() {
        let cfg = config_with_background((0x18, 0x18, 0x18));
        let state = SettingsState::new(&cfg);
        let new_cfg = config_with_background((0xf8, 0xf8, 0xf8));
        let _ = state.reload(&new_cfg);
        assert_eq!(state.snapshot(), AppearanceSettings::from_config(&new_cfg));
    }

    #[test]
    fn cloned_handles_share_the_same_underlying_state() {
        let cfg = config_with_background((0x18, 0x18, 0x18));
        let state = SettingsState::new(&cfg);
        let cloned = state.clone();
        let new_cfg = config_with_background((0xf8, 0xf8, 0xf8));
        let _ = state.reload(&new_cfg);
        assert_eq!(cloned.snapshot(), AppearanceSettings::from_config(&new_cfg));
    }
}

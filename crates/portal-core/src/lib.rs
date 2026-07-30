//! Pure protocol/domain logic for this workspace's native
//! `xdg-desktop-portal` backend interfaces -- no D-Bus wiring, no Wayland,
//! no UI. See `docs/portal-scoping.md` for the overall architecture: this
//! crate is deliberately the layer that stays exhaustively unit-testable
//! without a live bus connection, with `bins/portal` doing nothing more
//! than gluing these types to a real `dbus_server::Dispatcher`.
//!
//! # `impl.portal.Settings`
//!
//! [`AppearanceSettings`] is the whole of this crate's first backend
//! interface (`docs/portal-scoping.md` step 5): the `org.freedesktop.
//! appearance` namespace's four documented keys, derived from a
//! `config::Config` snapshot. [`ColorScheme`] and [`AccentColor`] hold the
//! two keys with a genuine local source (background luma, the file
//! browser's accent); [`NamespaceFilter`] is `ReadAll`'s glob-matching
//! rules, factored out because they're independent of what a namespace
//! actually contains; [`SettingChangedSignal`] builds the one signal this
//! interface emits.

pub mod accent_color;
pub mod appearance_settings;
pub mod color_scheme;
pub mod namespace_filter;
pub mod setting_changed;

pub use accent_color::AccentColor;
pub use appearance_settings::AppearanceSettings;
pub use color_scheme::ColorScheme;
pub use namespace_filter::NamespaceFilter;
pub use setting_changed::SettingChangedSignal;

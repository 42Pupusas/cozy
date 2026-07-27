//! `wallpaper` — the headless core of the swaybg-replacement bin
//! (`bins/wallpaper`, not yet built): decoding a background image and
//! resampling it to fit an output, with zero Wayland/`platform`
//! dependency.
//!
//! This mirrors `crates/launcher`'s split (pure catalog/fuzzy-match logic,
//! no Wayland) and `crates/greetd_ipc`'s planned split for the greeter:
//! everything here is a pure function of bytes/dimensions, so it's
//! unit-testable without a live compositor. The eventual `bins/wallpaper`
//! binary owns the Wayland side (per-output `zwlr_layer_shell_v1`
//! `Layer::Background` surfaces — see `wallpaper_image.rs`/`scale_mode.rs`'s
//! doc comments for the split rationale) and converts this crate's plain
//! RGBA8 output into whatever pixel format `platform::draw::Canvas` wants.

mod scale_mode;
mod wallpaper_image;

pub use scale_mode::ScaleMode;
pub use wallpaper_image::{LoadError, WallpaperImage};

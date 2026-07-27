//! Decode and cache `DesktopEntry::icon`s as RGBA8 pixels for
//! `Painter::paint` to blit each frame.
//!
//! Decoding a PNG is real work (inflate + defilter + palette/bit-depth
//! expansion) that a launcher's every-keystroke repaint must not redo --
//! this is the one place `bins/launcher` touches `ixiptla-core` at all,
//! mirroring `crates/wallpaper`'s equally narrow decode boundary. Lookup
//! (which file, if any, an icon *name* maps to) stays in
//! `launcher::icon_lookup::IconLookup`, which is pure/headless; only the
//! actual byte decoding needs a real PNG crate.

use std::collections::HashMap;

use launcher::icon_lookup::IconLookup;

/// Generous but bounded decode limit -- app icons are tiny (tens of KB at
/// most), so this exists only to reject a pathological/corrupt file, not
/// to accommodate real icon sizes. Far smaller than `bins/wallpaper`'s
/// bound since a launcher icon is never a multi-megapixel image.
const MAX_DECOMPRESSED_BYTES: usize = 16 << 20;

/// One decoded icon: flat RGBA8 pixels plus dimensions, ready for
/// [`platform::draw::Canvas::blit_rgba_scaled`].
pub struct DecodedIcon {
    pub width: i32,
    pub height: i32,
    pub rgba: Vec<u8>,
}

/// Per-run cache: resolves + decodes each distinct icon name at most once.
/// `None` is cached too (an icon that failed to resolve or decode stays
/// failed for the process's lifetime), so a broken/missing icon doesn't
/// retry a filesystem walk and a failed decode on every single repaint.
pub struct IconCache {
    lookup: IconLookup,
    decoded: HashMap<String, Option<DecodedIcon>>,
}

impl IconCache {
    #[must_use]
    pub fn new() -> Self {
        Self {
            lookup: IconLookup::from_env(),
            decoded: HashMap::new(),
        }
    }

    /// The decoded icon for `name` (a `.desktop` file's `Icon=` value),
    /// resolving and decoding it on first request and serving the cached
    /// result (`Some` or `None`) on every request after.
    pub fn get(&mut self, name: &str) -> Option<&DecodedIcon> {
        self.decoded
            .entry(name.to_owned())
            .or_insert_with(|| Self::load(&self.lookup, name))
            .as_ref()
    }

    fn load(lookup: &IconLookup, name: &str) -> Option<DecodedIcon> {
        let path = lookup.resolve(name)?;
        let bytes = std::fs::read(&path).ok()?;
        let png = ixiptla_core::Png::decode(&bytes, MAX_DECOMPRESSED_BYTES).ok()?;
        let expander = ixiptla_core::RgbaExpander::new(
            &png.ihdr,
            png.palette.as_ref(),
            png.transparency.as_ref(),
        );
        let rgba = expander.expand(&png.pixels).ok()?;
        Some(DecodedIcon {
            #[allow(clippy::cast_possible_wrap)]
            width: png.ihdr.width as i32,
            #[allow(clippy::cast_possible_wrap)]
            height: png.ihdr.height as i32,
            rgba,
        })
    }
}

impl Default for IconCache {
    fn default() -> Self {
        Self::new()
    }
}

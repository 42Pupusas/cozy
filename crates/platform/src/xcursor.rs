//! Loading real system cursor themes (Xcursor format) so our hardware cursor
//! matches the user's actual desktop pointer instead of a hand-drawn bitmap.
//!
//! This implements just enough of the on-disk Xcursor format (see
//! `Xcursor(3)`) to find a theme's `left_ptr` image and decode it: no
//! animation, no cursor caching daemon, no `Xcursor.so` — we just read the
//! file ourselves.
//!
//! Resolution follows the same rules `libXcursor` uses:
//! * `$XCURSOR_THEME` (falls back to the conventional `"default"` theme,
//!   which on most distros is an `index.theme` that `Inherits=` the real
//!   theme, e.g. Adwaita).
//! * `$XCURSOR_SIZE` (falls back to 24, the classic X default).
//! * Search order: `$HOME/.icons`, `$XDG_DATA_HOME/icons` (or
//!   `~/.local/share/icons`), then the system dirs
//!   `/usr/share/icons`, `/usr/local/share/icons`.
//! * A theme's `index.theme` may declare `Inherits=other,theme,list`; we
//!   walk that chain breadth-first looking for the cursor file in every
//!   theme before giving up.

use std::fs;
use std::path::PathBuf;

/// A single decoded cursor image: dimensions, hotspot, and premultiplied
/// ARGB8888 pixels (native-endian `u32`s) — exactly the pixel format
/// `wl_shm`'s `Argb8888` format expects, so callers can copy this straight
/// into a shm buffer with no conversion.
#[derive(Debug, Clone)]
pub struct CursorImage {
    pub width: u32,
    pub height: u32,
    pub hotspot_x: u32,
    pub hotspot_y: u32,
    pub pixels: Vec<u32>,
}

const XCUR_MAGIC: u32 = 0x7275_6358; // "Xcur" little-endian
const CHUNK_IMAGE: u32 = 0xfffd_0002;

/// Names an Xcursor file might be published under for "the normal arrow
/// pointer" — themes vary in which alias they actually ship.
const POINTER_NAMES: &[&str] = &[
    "left_ptr",
    "default",
    "left-arrow",
    "arrow",
    "top_left_arrow",
];

/// Read `$XCURSOR_THEME` (default `"default"`) and `$XCURSOR_SIZE` (default
/// 24), and try to load a pointer cursor at (approximately) `size * scale`
/// physical pixels. Returns `None` if no theme on disk has a decodable
/// pointer image — callers should fall back to a built-in bitmap.
pub fn load_system_pointer(scale: i32) -> Option<CursorImage> {
    let theme = std::env::var("XCURSOR_THEME").unwrap_or_else(|_| "default".to_string());
    let base_size: u32 = std::env::var("XCURSOR_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(24);
    let want_size = base_size.saturating_mul(scale.max(1) as u32);
    load_pointer(&theme, want_size)
}

/// Same as [`load_system_pointer`] but with an explicit theme/size, for
/// testing without depending on process environment.
pub fn load_pointer(theme: &str, want_size: u32) -> Option<CursorImage> {
    let dirs = search_dirs();
    let chain = resolve_theme_chain(theme, &dirs);

    for theme_name in &chain {
        for dir in &dirs {
            for name in POINTER_NAMES {
                let path = dir.join(theme_name).join("cursors").join(name);
                if let Ok(bytes) = fs::read(&path) {
                    if let Some(img) = parse_best(&bytes, want_size) {
                        return Some(img);
                    }
                }
            }
        }
    }
    None
}

/// Directories that may contain `<theme>/cursors/<name>` files, in the
/// order `libXcursor` searches them (user overrides before system dirs).
fn search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        dirs.push(PathBuf::from(&home).join(".icons"));
        let xdg_data =
            std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| format!("{home}/.local/share"));
        dirs.push(PathBuf::from(xdg_data).join("icons"));
    }
    dirs.push(PathBuf::from("/usr/share/icons"));
    dirs.push(PathBuf::from("/usr/local/share/icons"));
    dirs
}

/// Breadth-first walk of `Inherits=` chains starting at `theme`, across all
/// search dirs, so `"default" -> Adwaita -> hicolor` resolves even though
/// each theme's `index.theme` lives in a different directory tree.
fn resolve_theme_chain(theme: &str, dirs: &[PathBuf]) -> Vec<String> {
    let mut seen = vec![theme.to_string()];
    let mut queue = vec![theme.to_string()];
    while let Some(cur) = queue.pop() {
        for dir in dirs {
            let idx = dir.join(&cur).join("index.theme");
            if let Ok(text) = fs::read_to_string(&idx) {
                for parent in parse_inherits(&text) {
                    if !seen.contains(&parent) {
                        seen.push(parent.clone());
                        queue.push(parent);
                    }
                }
            }
        }
    }
    seen
}

/// Extract the comma-separated `Inherits=` value from an `index.theme` INI
/// file's `[Icon Theme]` section (we don't care about other sections, and
/// this key is unique enough not to need real INI parsing).
fn parse_inherits(text: &str) -> Vec<String> {
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Inherits=") {
            return rest.split(',').map(str::trim).map(str::to_string).collect();
        }
    }
    Vec::new()
}

/// Parse an Xcursor file and return the image whose nominal size (the TOC
/// `subtype` for image chunks) is closest to `want_size`.
fn parse_best(bytes: &[u8], want_size: u32) -> Option<CursorImage> {
    let mut best: Option<(u32, CursorImage)> = None;
    for img in parse_all_images(bytes) {
        let diff = img.width.max(want_size) - img.width.min(want_size);
        match &best {
            Some((best_diff, _)) if *best_diff <= diff => {}
            _ => best = Some((diff, img)),
        }
    }
    best.map(|(_, img)| img)
}

/// Decode every image chunk in an Xcursor file. Malformed/truncated chunks
/// are skipped rather than aborting the whole file, matching the reference
/// parser's tolerance for partially-broken theme files.
fn parse_all_images(bytes: &[u8]) -> Vec<CursorImage> {
    let mut out = Vec::new();
    let Some(mut r) = Reader::new(bytes) else {
        return out;
    };
    let Some(magic) = r.u32() else { return out };
    let Some(header) = r.u32() else { return out };
    let Some(_version) = r.u32() else { return out };
    let Some(ntoc) = r.u32() else { return out };
    if magic != XCUR_MAGIC || header != 16 {
        return out;
    }

    // TOC entries: (type, subtype, position), 12 bytes each.
    let mut positions = Vec::with_capacity(ntoc as usize);
    for _ in 0..ntoc {
        let Some(chunk_type) = r.u32() else { break };
        let Some(_subtype) = r.u32() else { break };
        let Some(position) = r.u32() else { break };
        if chunk_type == CHUNK_IMAGE {
            positions.push(position as usize);
        }
    }

    for pos in positions {
        if let Some(img) = parse_image_chunk(bytes, pos) {
            out.push(img);
        }
    }
    out
}

fn parse_image_chunk(bytes: &[u8], pos: usize) -> Option<CursorImage> {
    let mut r = Reader::new(bytes.get(pos..)?)?;
    let img_header = r.u32()?;
    let _img_type = r.u32()?;
    let _img_subtype = r.u32()?;
    let img_version = r.u32()?;
    let width = r.u32()?;
    let height = r.u32()?;
    let xhot = r.u32()?;
    let yhot = r.u32()?;
    let _delay = r.u32()?;

    if img_header != 36 || img_version != 1 {
        return None;
    }
    // Sanity bound: refuse absurd sizes rather than trying to allocate
    // gigabytes from a corrupt/malicious file.
    if width == 0 || height == 0 || width > 4096 || height > 4096 {
        return None;
    }

    let npixels = (width as usize).checked_mul(height as usize)?;
    let nbytes = npixels.checked_mul(4)?;
    let pixel_bytes = r.take(nbytes)?;

    // Xcursor pixel data is already premultiplied ARGB8888, little-endian —
    // exactly what wl_shm's Argb8888 format wants, so this is a straight
    // byte-to-u32 reinterpretation, no math needed.
    let mut pixels = Vec::with_capacity(npixels);
    for chunk in pixel_bytes.chunks_exact(4) {
        pixels.push(u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }

    Some(CursorImage {
        width,
        height,
        hotspot_x: xhot.min(width - 1),
        hotspot_y: yhot.min(height - 1),
        pixels,
    })
}

/// Minimal little-endian cursor over a byte slice.
struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Option<Self> {
        Some(Self { bytes, pos: 0 })
    }

    fn u32(&mut self) -> Option<u32> {
        let b = self.bytes.get(self.pos..self.pos + 4)?;
        self.pos += 4;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let b = self.bytes.get(self.pos..self.pos + n)?;
        self.pos += n;
        Some(b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_xcursor_file(images: &[(u32, u32, u32, u32, u32)]) -> Vec<u8> {
        // images: (width, height, xhot, yhot, pixel_fill)
        let mut buf = Vec::new();
        buf.extend_from_slice(&XCUR_MAGIC.to_le_bytes());
        buf.extend_from_slice(&16u32.to_le_bytes());
        buf.extend_from_slice(&1u32.to_le_bytes());
        buf.extend_from_slice(&(images.len() as u32).to_le_bytes());

        let toc_end = 16 + images.len() * 12;
        let mut chunk_data = Vec::new();
        let mut positions = Vec::new();
        for &(w, h, _, _, _) in images {
            positions.push(toc_end + chunk_data.len());
            chunk_data.resize(chunk_data.len() + 36 + (w * h * 4) as usize, 0);
        }

        for (i, &(w, _h, _, _, _)) in images.iter().enumerate() {
            buf.extend_from_slice(&CHUNK_IMAGE.to_le_bytes());
            buf.extend_from_slice(&w.to_le_bytes()); // subtype == nominal size
            buf.extend_from_slice(&(positions[i] as u32).to_le_bytes());
        }

        // Now actually fill each chunk's header + pixels at its position.
        let mut file = buf;
        file.resize(toc_end, 0);
        for (i, &(w, h, xhot, yhot, fill)) in images.iter().enumerate() {
            let mut chunk = Vec::new();
            chunk.extend_from_slice(&36u32.to_le_bytes());
            chunk.extend_from_slice(&CHUNK_IMAGE.to_le_bytes());
            chunk.extend_from_slice(&w.to_le_bytes());
            chunk.extend_from_slice(&1u32.to_le_bytes());
            chunk.extend_from_slice(&w.to_le_bytes());
            chunk.extend_from_slice(&h.to_le_bytes());
            chunk.extend_from_slice(&xhot.to_le_bytes());
            chunk.extend_from_slice(&yhot.to_le_bytes());
            chunk.extend_from_slice(&0u32.to_le_bytes());
            for _ in 0..(w * h) {
                chunk.extend_from_slice(&fill.to_le_bytes());
            }
            assert_eq!(positions[i], file.len());
            file.extend_from_slice(&chunk);
        }
        file
    }

    #[test]
    fn parses_single_image_chunk() {
        let file = build_xcursor_file(&[(4, 4, 1, 1, 0xffaa_bbcc)]);
        let imgs = parse_all_images(&file);
        assert_eq!(imgs.len(), 1);
        assert_eq!(imgs[0].width, 4);
        assert_eq!(imgs[0].height, 4);
        assert_eq!(imgs[0].hotspot_x, 1);
        assert_eq!(imgs[0].hotspot_y, 1);
        assert_eq!(imgs[0].pixels.len(), 16);
        assert!(imgs[0].pixels.iter().all(|&p| p == 0xffaa_bbcc));
    }

    #[test]
    fn picks_closest_size_to_requested() {
        let file = build_xcursor_file(&[(16, 16, 0, 0, 1), (32, 32, 0, 0, 2), (48, 48, 0, 0, 3)]);
        let best = parse_best(&file, 30).unwrap();
        assert_eq!(best.width, 32);
    }

    #[test]
    fn rejects_bad_magic() {
        let mut file = build_xcursor_file(&[(4, 4, 0, 0, 1)]);
        file[0] = 0; // corrupt magic
        assert!(parse_all_images(&file).is_empty());
    }

    #[test]
    fn truncated_file_yields_no_images_not_a_panic() {
        let file = build_xcursor_file(&[(4, 4, 0, 0, 1)]);
        for cut in 0..file.len() {
            let _ = parse_all_images(&file[..cut]);
        }
    }

    #[test]
    fn parse_inherits_reads_comma_list() {
        let ini = "[Icon Theme]\nName=default\nInherits=Adwaita,hicolor\n";
        assert_eq!(parse_inherits(ini), vec!["Adwaita", "hicolor"]);
    }

    #[test]
    fn parse_inherits_missing_key_is_empty() {
        assert!(parse_inherits("[Icon Theme]\nName=foo\n").is_empty());
    }

    #[test]
    fn oversized_dimensions_are_rejected() {
        // Hand-craft a chunk claiming a huge width without actually
        // allocating that much pixel data.
        let mut buf = Vec::new();
        buf.extend_from_slice(&XCUR_MAGIC.to_le_bytes());
        buf.extend_from_slice(&16u32.to_le_bytes());
        buf.extend_from_slice(&1u32.to_le_bytes());
        buf.extend_from_slice(&1u32.to_le_bytes());
        buf.extend_from_slice(&CHUNK_IMAGE.to_le_bytes());
        buf.extend_from_slice(&999_999u32.to_le_bytes());
        buf.extend_from_slice(&28u32.to_le_bytes());
        // chunk header at offset 28
        buf.extend_from_slice(&36u32.to_le_bytes());
        buf.extend_from_slice(&CHUNK_IMAGE.to_le_bytes());
        buf.extend_from_slice(&999_999u32.to_le_bytes());
        buf.extend_from_slice(&1u32.to_le_bytes());
        buf.extend_from_slice(&999_999u32.to_le_bytes()); // width: huge
        buf.extend_from_slice(&999_999u32.to_le_bytes()); // height: huge
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        assert!(parse_all_images(&buf).is_empty());
    }
}

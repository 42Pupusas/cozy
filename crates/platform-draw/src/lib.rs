//! The software rasterizer: pixels, shapes, and text, decoupled from Wayland.
//!
//! Audit finding #3 (god-crate), third cut: after `sys.rs` (raw syscalls)
//! came out as `platform-sys` and the wire protocol (`id`/`protocol`/`wire`)
//! came out as `platform-wire`, this is the third self-contained seam in
//! `platform` -- everything that turns colors and shapes into `u32` pixel
//! words, with no knowledge of Wayland objects, connections, or windows.
//!
//! * [`draw`] -- [`draw::Canvas`], the pixel buffer + drawing primitives
//!   (`fill_rect`, `clear`, lines, nearest-neighbor image blit).
//! * [`round_rect`] -- rounded-rectangle scanline geometry, the shape math
//!   behind `Canvas::fill_round_rect`/`draw_round_rect`.
//! * [`surface_format`] -- [`surface_format::SurfaceFormat`], the
//!   opaque-vs-translucent pixel-packing rule a `Canvas` carries.
//! * [`font`] -- the embedded 8x16 bitmap ASCII fallback font.
//! * [`text`] -- [`text::GlyphBlit`], the shared TTF/OTF glyph-blend kernel
//!   used by both the terminal renderer and the status bar.
//!
//! This crate depends on `font` (for [`text::GlyphBlit`]'s `Atlas`/
//! `GlyphEntry` types) and `config` (for `Color`'s `From<config::Color>`),
//! but nothing in `platform` proper -- exactly the same "already a closed
//! dependency subgraph, just not drawn as a crate boundary" shape as the
//! two splits before it. `platform` re-exports this crate's public surface
//! under the names every existing call site already writes
//! (`platform::draw`, `platform::round_rect`, `platform::SurfaceFormat`,
//! `platform::font`, `platform::GlyphBlit`), so the split is invisible from
//! the outside.

pub mod clip;
pub mod draw;
pub mod ellipse;
pub mod font;
pub mod line;
pub mod round_rect;
pub mod surface_format;
pub mod text;

pub use surface_format::SurfaceFormat;
pub use text::GlyphBlit;

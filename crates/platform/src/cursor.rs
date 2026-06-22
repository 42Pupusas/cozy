//! Hardware cursor support: a left-arrow pointer rendered into a shared-memory
//! buffer, attached to a dedicated `wl_surface`, and activated via
//! `wl_pointer.set_cursor`.
//!
//! For now only one cursor shape exists — the classic X "left_ptr" at 16×16
//! pixels with the hotspot at its tip. As we add more shapes (move, resize,
//! text, …) the surface remains the same; we just attach a different buffer.

use crate::draw::Color;
use crate::id::{BufferId, SurfaceId};
use crate::sys::ShmBuffer;

/// Width and height of the cursor image, in pixels.
pub const CURSOR_SIZE: i32 = 16;

/// Hotspot offset (pixels from top-left) of the left-arrow cursor — the pixel
/// that the compositor aligns with the physical pointer position.
pub const HOTSPOT_X: i32 = 2;
pub const HOTSPOT_Y: i32 = 2;

/// Pixel-layout tag for the bitmap below: 0 → transparent, 1 → black outline,
/// 2 → white fill.
const T: u8 = 0;
const B: u8 = 1;
const W: u8 = 2;

/// The X "left_ptr" cursor at 16×16, as a flat row-major array of pixel tags.
/// Converted to ARGB8888 at runtime by [`write_into`].
const LEFT_PTR_BITMAP: &[u8] = &[
    T, T, T, T, B, B, B, B, T, T, T, T, T, T, T, T, //  0
    T, T, T, T, B, W, W, B, T, T, T, T, T, T, T, T, //  1
    T, T, T, T, B, W, W, B, T, T, T, T, T, T, T, T, //  2
    T, T, T, T, B, W, B, T, T, T, T, T, T, T, T, T, //  3
    T, T, T, T, B, B, T, T, T, T, T, T, T, T, T, T, //  4
    T, T, T, T, B, W, B, T, T, T, T, T, T, T, T, T, //  5
    T, T, T, T, B, W, W, B, T, T, T, T, T, T, T, T, //  6
    T, T, T, B, W, W, W, B, T, T, T, T, T, T, T, T, //  7
    T, T, B, W, W, W, B, W, B, T, T, T, T, T, T, T, //  8
    T, B, W, W, W, B, W, B, T, T, T, T, T, T, T, T, //  9
    B, W, W, W, B, W, B, T, T, T, T, T, T, T, T, T, // 10
    B, W, W, W, B, B, B, B, B, B, B, T, T, T, T, T, // 11
    B, W, W, W, W, W, W, W, W, W, B, T, T, T, T, T, // 12
    B, W, W, W, B, B, B, B, B, B, B, T, T, T, T, T, // 13
    B, B, B, B, B, T, T, T, T, T, T, T, T, T, T, T, // 14
    T, T, T, T, T, T, T, T, T, T, T, T, T, T, T, T, // 15
];

/// Write the left-arrow cursor into `pixels`, which must be at least
/// `CURSOR_SIZE × CURSOR_SIZE` elements. Fills every pixel — call on a
/// zero-initialized or don't-care buffer.
pub fn write_left_ptr(pixels: &mut [u32]) {
    let black = Color::BLACK.to_argb8888();
    let white = Color::WHITE.to_argb8888();

    for (i, &tag) in LEFT_PTR_BITMAP.iter().enumerate() {
        pixels[i] = match tag {
            B => black,
            W => white,
            _ => 0, // transparent
        };
    }
}

/// Everything the `Window` needs to keep alive for the cursor to work.
pub struct Cursor {
    /// The cursor's dedicated `wl_surface`.
    pub surface: SurfaceId,
    /// The buffer currently attached to the cursor surface (holds the
    /// left-arrow pixels). Held for lifetime — never read after setup.
    #[allow(dead_code)]
    pub buffer: BufferId,
    /// Backing shared memory for the cursor buffer — must stay mapped while the
    /// compositor may read it.
    #[allow(dead_code)] // held for the mmap lifetime
    pub shm: ShmBuffer,
}

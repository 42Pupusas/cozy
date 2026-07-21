//! Hardware cursor support: the system's real pointer image (loaded from the
//! user's Xcursor theme, matching every other app on their desktop) rendered
//! into a shared-memory buffer, attached to a dedicated `wl_surface`, and
//! activated via `wl_pointer.set_cursor`.
//!
//! If no system theme can be found or decoded (headless test box, unusual
//! setup, etc.) we fall back to a small hand-drawn "left_ptr" bitmap so the
//! cursor is never simply invisible.

use crate::conn::{ConnError, Sender};
use crate::draw::Color;
use crate::error::{Error, Result};
use crate::id::{BufferId, CompositorId, ShmId, SurfaceId};
use crate::protocol::{CompositorRequest, Format, ShmPoolRequest, ShmRequest, SurfaceRequest};
use crate::sys::ShmBuffer;
use crate::wire::MessageBuilder;
use crate::xcursor::CursorImage;

/// Width and height of the built-in fallback cursor bitmap, in pixels.
pub const FALLBACK_SIZE: i32 = 16;

/// Pixel-layout tag for the bitmap below: 0 → transparent, 1 → black outline,
/// 2 → white fill.
const T: u8 = 0;
const B: u8 = 1;
const W: u8 = 2;

/// The X "left_ptr" cursor at 16×16, as a flat row-major array of pixel tags.
/// Used only when no real system cursor theme could be loaded.
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

/// Everything the `Window` needs to keep alive for the cursor to work.
///
/// Named `PointerCursor` (not the shorter `Cursor`) to stay unambiguous next
/// to `grid::Cursor`, the unrelated terminal-cell cursor-position type --
/// the two used to share a bare name.
pub struct PointerCursor {
    /// The cursor's dedicated `wl_surface`.
    pub surface: SurfaceId,
    /// The buffer currently attached to the cursor surface.
    #[allow(dead_code)]
    pub buffer: BufferId,
    /// Backing shared memory for the cursor buffer — must stay mapped while the
    /// compositor may read it.
    #[allow(dead_code)] // held for the mmap lifetime
    pub shm: ShmBuffer,
    /// Hotspot the compositor should align with the physical pointer
    /// position, in the cursor image's own pixel space.
    pub hotspot_x: i32,
    pub hotspot_y: i32,
}

impl PointerCursor {
    /// Set up the cursor surface: try to load the user's real system cursor
    /// theme at `scale`x density, falling back to the built-in bitmap if
    /// none is found; paint it into an shm buffer; attach + commit the
    /// Wayland surface.
    pub fn create(
        sender: &mut Sender,
        shm: ShmId,
        compositor: CompositorId,
        scale: i32,
    ) -> Result<Self> {
        let image = CursorImage::load_system_pointer(scale);
        let (width, height, hotspot_x, hotspot_y) = match &image {
            Some(img) => (
                img.width as i32,
                img.height as i32,
                img.hotspot_x as i32,
                img.hotspot_y as i32,
            ),
            None => (FALLBACK_SIZE, FALLBACK_SIZE, 2, 2),
        };

        let w = width.max(1) as usize;
        let h = height.max(1) as usize;
        let mut cshm = ShmBuffer::new(w, h, 1).map_err(|e| Error::Conn(ConnError::Sys(e)))?;
        match &image {
            Some(img) => Self::write_image(cshm.pixels(0), img),
            None => Self::write_left_ptr(cshm.pixels(0)),
        }

        let cursor_pool: crate::id::PoolId = sender.alloc_id();
        let msg = MessageBuilder::new(shm, ShmRequest::CreatePool)
            .new_id(cursor_pool)
            .i32(cshm.size_bytes() as i32)
            .finish();
        sender.send_with_fds(msg, &[cshm.fd()])?;
        let cursor_surface: SurfaceId = sender.alloc_id();
        sender.request(
            MessageBuilder::new(compositor, CompositorRequest::CreateSurface)
                .new_id(cursor_surface),
        )?;
        let cursor_buffer: BufferId = sender.alloc_id();
        sender.request(
            MessageBuilder::new(cursor_pool, ShmPoolRequest::CreateBuffer)
                .new_id(cursor_buffer)
                .i32(0)
                .i32(width)
                .i32(height)
                .i32(width * 4)
                .u32(Format::Argb8888.code()),
        )?;
        sender.request(
            MessageBuilder::new(cursor_surface, SurfaceRequest::Attach)
                .object(cursor_buffer)
                .i32(0)
                .i32(0),
        )?;
        sender.request(MessageBuilder::new(cursor_surface, SurfaceRequest::Commit))?;
        Ok(Self {
            surface: cursor_surface,
            buffer: cursor_buffer,
            shm: cshm,
            hotspot_x,
            hotspot_y,
        })
    }

    /// Copy a decoded Xcursor image's premultiplied ARGB8888 pixels straight
    /// into the shm buffer — same pixel format, no conversion needed.
    fn write_image(pixels: &mut [u32], img: &CursorImage) {
        let n = pixels.len().min(img.pixels.len());
        pixels[..n].copy_from_slice(&img.pixels[..n]);
    }

    /// Write the fallback left-arrow cursor into `pixels`, which must be at
    /// least `FALLBACK_SIZE × FALLBACK_SIZE` elements.
    fn write_left_ptr(pixels: &mut [u32]) {
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
}

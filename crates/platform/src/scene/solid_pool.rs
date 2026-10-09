//! [`SolidPool`] -- one `wl_buffer` per distinct colour, shared by every
//! section showing that colour.
//!
//! With `wp_single_pixel_buffer_manager_v1` a colour costs no client memory.
//! Without it (niri) each colour is one 4-byte pixel in a shared shm pool,
//! stretched to size by the section's viewport.

use std::collections::HashMap;

use crate::conn::{ConnError, WindowSender};
use crate::error::{PlatformError, Result};
use crate::id::{BufferId, PoolId, ShmId, SinglePixelBufferManagerId};
use crate::protocol::{
    Format, ShmPoolRequest, ShmRequest, SinglePixelBufferManagerRequest, U32Channel,
};
use crate::sys::ShmBuffer;
use crate::wire::MessageBuilder;

enum SolidBacking {
    SinglePixel(SinglePixelBufferManagerId),
    Shm {
        pixels: ShmBuffer,
        pool: PoolId,
        used: usize,
        capacity: usize,
    },
}

pub struct SolidPool {
    backing: SolidBacking,
    buffers: HashMap<u32, BufferId>,
}

impl SolidPool {
    const INITIAL_COLOURS: usize = 64;

    pub fn new(
        window_tx: &mut WindowSender,
        shm: ShmId,
        single_pixel: Option<SinglePixelBufferManagerId>,
    ) -> Result<Self> {
        let backing = match single_pixel {
            Some(manager) => SolidBacking::SinglePixel(manager),
            None => Self::shm_backing(window_tx, shm)?,
        };
        Ok(Self {
            backing,
            buffers: HashMap::new(),
        })
    }

    fn shm_backing(window_tx: &mut WindowSender, shm: ShmId) -> Result<SolidBacking> {
        let pixels = ShmBuffer::new(1, Self::INITIAL_COLOURS, 1)
            .map_err(|e| PlatformError::Conn(ConnError::Sys(e)))?;
        let fd = pixels
            .dup_fd()
            .map_err(|e| PlatformError::Conn(ConnError::Io(e)))?;
        let pool: PoolId = window_tx.alloc_id();
        let msg = MessageBuilder::new(shm, ShmRequest::CreatePool)
            .new_id(pool)
            .i32(i32::try_from(pixels.size_bytes()).unwrap_or(i32::MAX))
            .finish();
        window_tx.send_with_fds(msg, vec![fd]);
        Ok(SolidBacking::Shm {
            pixels,
            pool,
            used: 0,
            capacity: Self::INITIAL_COLOURS,
        })
    }

    /// Distinct colours created so far.
    #[must_use]
    pub fn colours(&self) -> usize {
        self.buffers.len()
    }

    /// The buffer showing premultiplied `argb`, created on first use.
    pub fn buffer_for(&mut self, window_tx: &mut WindowSender, argb: u32) -> Result<BufferId> {
        if let Some(&buffer) = self.buffers.get(&argb) {
            return Ok(buffer);
        }
        let buffer: BufferId = window_tx.alloc_id();
        match &mut self.backing {
            SolidBacking::SinglePixel(manager) => {
                let [b, g, r, a] = argb.to_le_bytes();
                window_tx.request(
                    MessageBuilder::new(
                        *manager,
                        SinglePixelBufferManagerRequest::CreateU32RgbaBuffer,
                    )
                    .new_id(buffer)
                    .u32(U32Channel::from_u8(r).0)
                    .u32(U32Channel::from_u8(g).0)
                    .u32(U32Channel::from_u8(b).0)
                    .u32(U32Channel::from_u8(a).0),
                );
            }
            SolidBacking::Shm {
                pixels,
                pool,
                used,
                capacity,
            } => {
                if *used == *capacity {
                    *capacity *= 2;
                    let grown = pixels
                        .resize(1, *capacity)
                        .map_err(|e| PlatformError::Conn(ConnError::Sys(e)))?;
                    if let Some(bytes) = grown {
                        window_tx.request(
                            MessageBuilder::new(*pool, ShmPoolRequest::Resize)
                                .i32(i32::try_from(bytes).unwrap_or(i32::MAX)),
                        );
                    }
                }
                pixels.pixels(0)[*used] = argb;
                window_tx.request(
                    MessageBuilder::new(*pool, ShmPoolRequest::CreateBuffer)
                        .new_id(buffer)
                        .i32(i32::try_from(*used * 4).unwrap_or(i32::MAX))
                        .i32(1)
                        .i32(1)
                        .i32(4)
                        .u32(Self::format_for(argb).code()),
                );
                *used += 1;
            }
        }
        self.buffers.insert(argb, buffer);
        Ok(buffer)
    }

    /// An opaque colour uses XRGB so the compositor can skip blending it.
    const fn format_for(argb: u32) -> Format {
        if argb >> 24 == 0xff {
            Format::Xrgb8888
        } else {
            Format::Argb8888
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SolidPool;
    use crate::protocol::Format;

    #[test]
    fn only_a_fully_opaque_colour_skips_blending() {
        assert_eq!(SolidPool::format_for(0xff10_2030), Format::Xrgb8888);
        assert_eq!(SolidPool::format_for(0xcc10_2030), Format::Argb8888);
        assert_eq!(SolidPool::format_for(0), Format::Argb8888);
    }
}

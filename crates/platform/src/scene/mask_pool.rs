//! [`MaskPool`] -- one shm `wl_buffer` per corner mask (colour and size),
//! shared by every corner section that shows it, whichever way it is turned.

use std::collections::HashMap;

use crate::conn::{ConnError, WindowSender};
use crate::error::{PlatformError, Result};
use crate::id::{BufferId, PoolId, ShmId};
use crate::protocol::{Format, ShmPoolRequest, ShmRequest};
use crate::sys::ShmBuffer;
use crate::wire::MessageBuilder;

use super::corner_mask::CornerMask;

pub struct MaskPool {
    pixels: ShmBuffer,
    pool: PoolId,
    used: usize,
    capacity: usize,
    buffers: HashMap<(u32, u16), BufferId>,
}

impl MaskPool {
    const INITIAL_PIXELS: usize = 4096;

    pub fn new(window_tx: &mut WindowSender, shm: ShmId) -> Result<Self> {
        let pixels = ShmBuffer::new(1, Self::INITIAL_PIXELS, 1).map_err(Self::sys)?;
        let fd = pixels
            .dup_fd()
            .map_err(|e| PlatformError::Conn(ConnError::Io(e)))?;
        let pool: PoolId = window_tx.alloc_id();
        let msg = MessageBuilder::new(shm, ShmRequest::CreatePool)
            .new_id(pool)
            .i32(i32::try_from(pixels.size_bytes()).unwrap_or(i32::MAX))
            .finish();
        window_tx.send_with_fds(msg, vec![fd]);
        Ok(Self {
            pixels,
            pool,
            used: 0,
            capacity: Self::INITIAL_PIXELS,
            buffers: HashMap::new(),
        })
    }

    const fn sys(e: crate::sys::SysError) -> PlatformError {
        PlatformError::Conn(ConnError::Sys(e))
    }

    #[must_use]
    pub fn masks(&self) -> usize {
        self.buffers.len()
    }

    /// Bytes of shm the masks occupy.
    #[must_use]
    pub const fn bytes(&self) -> usize {
        self.used * 4
    }

    /// The top-left corner mask of premultiplied `argb`, `side` pixels square.
    pub fn buffer_for(
        &mut self,
        window_tx: &mut WindowSender,
        argb: u32,
        side: u16,
    ) -> Result<BufferId> {
        if let Some(&buffer) = self.buffers.get(&(argb, side)) {
            return Ok(buffer);
        }
        let side_px = usize::from(side.max(1));
        let needed = side_px * side_px;
        if self.used + needed > self.capacity {
            self.capacity = (self.used + needed).max(self.capacity * 2);
            if let Some(bytes) = self.pixels.resize(1, self.capacity).map_err(Self::sys)? {
                window_tx.request(
                    MessageBuilder::new(self.pool, ShmPoolRequest::Resize)
                        .i32(i32::try_from(bytes).unwrap_or(i32::MAX)),
                );
            }
        }
        let start = self.used;
        CornerMask::paint(
            &mut self.pixels.pixels(0)[start..start + needed],
            side_px,
            argb,
        );
        let buffer: BufferId = window_tx.alloc_id();
        let side_i32 = i32::from(side.max(1));
        window_tx.request(
            MessageBuilder::new(self.pool, ShmPoolRequest::CreateBuffer)
                .new_id(buffer)
                .i32(i32::try_from(start * 4).unwrap_or(i32::MAX))
                .i32(side_i32)
                .i32(side_i32)
                .i32(side_i32 * 4)
                .u32(Format::Argb8888.code()),
        );
        self.used += needed;
        self.buffers.insert((argb, side), buffer);
        Ok(buffer)
    }
}

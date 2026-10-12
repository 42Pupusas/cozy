//! A capture left in place in the session's shm pool.

use platform::id::BufferId;

use crate::frame_view::FrameView;

/// The pixels of the latest capture and the `wl_buffer` that holds them.
///
/// Borrowed from the [`crate::ScreencopySession`] that captured it, so the
/// pool, and the buffer the compositor may be displaying, cannot be freed or
/// reused while this is alive.
#[derive(Copy, Clone)]
pub struct HeldFrame<'a> {
    view: FrameView<'a>,
    buffer: BufferId,
}

impl<'a> HeldFrame<'a> {
    pub(crate) const fn new(view: FrameView<'a>, buffer: BufferId) -> Self {
        Self { view, buffer }
    }

    #[must_use]
    pub const fn view(&self) -> &FrameView<'a> {
        &self.view
    }

    /// The `wl_buffer` the capture was written into, attachable to a surface.
    #[must_use]
    pub const fn buffer(&self) -> BufferId {
        self.buffer
    }
}

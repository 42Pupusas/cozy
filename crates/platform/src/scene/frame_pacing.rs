//! [`FramePacing`] -- whether a section-composed window may commit again.
//!
//! It may when something changed and the compositor answered the last frame
//! callback. Sections hold no client-side frames, so there is no buffer
//! release to wait for.

use crate::conn::WindowSender;
use crate::event::RawEvent;
use crate::id::CallbackId;
use crate::surface::{Routed, SurfaceBootstrap};

pub struct FramePacing {
    dirty: bool,
    ready: bool,
    callback: Option<CallbackId>,
}

impl FramePacing {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            dirty: true,
            ready: true,
            callback: None,
        }
    }

    pub const fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    #[must_use]
    pub const fn can_commit(&self) -> bool {
        self.dirty && self.ready
    }

    pub const fn committed(&mut self, callback: CallbackId) {
        self.dirty = false;
        self.ready = false;
        self.callback = Some(callback);
    }

    /// `wl_display` errors, id recycling, and the frame callback.
    pub fn route(&mut self, ev: &RawEvent, window_tx: &mut WindowSender) -> Routed {
        SurfaceBootstrap::route_common(ev, window_tx, &mut [], &mut self.ready, &mut self.callback)
    }
}

impl Default for FramePacing {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::FramePacing;
    use crate::id::CallbackId;

    #[test]
    fn a_new_window_commits_at_once() {
        assert!(FramePacing::new().can_commit());
    }

    #[test]
    fn a_commit_waits_for_both_a_change_and_the_callback() {
        let mut pacing = FramePacing::new();
        pacing.committed(CallbackId(9));
        assert!(!pacing.can_commit());
        pacing.mark_dirty();
        assert!(!pacing.can_commit());
    }

    #[test]
    fn the_frame_callback_reopens_pacing() {
        use crate::event::RawEvent;
        let mut pacing = FramePacing::new();
        pacing.committed(CallbackId(9));
        pacing.mark_dirty();
        let done = RawEvent::Wire {
            object_id: 9,
            opcode: 0,
            args: [0; 4],
            nargs: 1,
            bytes: Box::new([]),
        };
        let (tx, _rx) = quetzalcoatl::mpsc::RingBuffer::<crate::conn::WindowRequest>::new(
            quetzalcoatl::capacity::Capacity::exact(4),
        )
        .split();
        let mut window_tx = crate::conn::WindowSender {
            tx,
            next_id: 100,
            id_end: 200,
            free_ids: Vec::new(),
        };
        assert!(matches!(
            pacing.route(&done, &mut window_tx),
            crate::surface::Routed::Handled
        ));
        assert!(pacing.can_commit());
    }
}

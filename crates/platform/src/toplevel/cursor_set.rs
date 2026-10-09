//! [`CursorSet`] -- one cursor surface per shape, created on demand.
//!
//! `wl_pointer.set_cursor` names a surface, not a shape, so switching the
//! pointer image means switching which surface is attached. Cursor images
//! differ in size, and a `wl_buffer` has a fixed size for its lifetime, so a
//! single reusable buffer cannot hold them all: each shape gets its own
//! surface, built the first time it is asked for and kept afterwards.
//!
//! A shape whose image cannot be loaded is remembered as absent, so a
//! missing theme file costs one failed lookup rather than one per motion
//! event.

use std::collections::HashMap;

use crate::conn::WindowSender;
use crate::id::{BufferId, CompositorId, PointerId, ShmId, SurfaceId};
use crate::protocol::{
    CompositorRequest, Format, PointerRequest, ShmPoolRequest, ShmRequest, SurfaceRequest,
};
use crate::sys::ShmBuffer;
use crate::wire::MessageBuilder;
use crate::xcursor::CursorImage;

/// One loaded cursor: its surface and the hotspot to align with the pointer.
struct Loaded {
    surface: SurfaceId,
    hotspot_x: i32,
    hotspot_y: i32,
    /// Held so the compositor's mapping of the pixels stays valid.
    #[allow(dead_code)]
    shm: ShmBuffer,
    /// Held so the buffer is not garbage-collected by our own id allocator.
    #[allow(dead_code)]
    buffer: BufferId,
}

/// A lazily-populated cache of cursor surfaces, keyed by the theme name list
/// that produced them.
pub struct CursorSet {
    shm: ShmId,
    compositor: CompositorId,
    scale: i32,
    /// `None` marks a shape whose image could not be loaded, so the failed
    /// lookup is not retried on every pointer motion.
    loaded: HashMap<&'static str, Option<Loaded>>,
    /// The key currently applied, to skip redundant `set_cursor` requests.
    current: Option<&'static str>,
}

impl CursorSet {
    /// An empty set. Surfaces are created on the first [`Self::apply`].
    #[must_use]
    pub fn new(shm: ShmId, compositor: CompositorId, scale: i32) -> Self {
        Self {
            shm,
            compositor,
            scale: scale.max(1),
            loaded: HashMap::new(),
            current: None,
        }
    }

    /// Forget every cached surface, so the next apply reloads at the new
    /// density. The caller is responsible for the (now unused) surfaces;
    /// they are left to the compositor to clean up when the client exits.
    pub fn set_scale(&mut self, scale: i32) {
        let scale = scale.max(1);
        if scale != self.scale {
            self.scale = scale;
            self.loaded.clear();
            self.current = None;
        }
    }

    /// Point `pointer` at the cursor for `key`, loading it if needed.
    ///
    /// `key` identifies the shape and must be stable for it; `names` are the
    /// Xcursor aliases to try. `serial` must be the latest pointer-enter
    /// serial, which is what the protocol requires.
    ///
    /// A shape with no loadable image leaves the current cursor alone rather
    /// than blanking the pointer.
    ///
    /// Every object here is allocated from the *window's* id block via
    /// [`WindowSender`]. Reaching for `Sender::alloc_id` instead would name
    /// an id the window already reserved, and the compositor answers that
    /// with a protocol error that kills the connection.
    pub fn apply(
        &mut self,
        sender: &mut WindowSender,
        pointer: PointerId,
        serial: u32,
        key: &'static str,
        names: &[&str],
    ) {
        if self.current == Some(key) {
            return;
        }
        if !self.loaded.contains_key(key) {
            let entry = self.build(sender, names);
            self.loaded.insert(key, entry);
        }
        let Some(Some(loaded)) = self.loaded.get(key) else {
            return;
        };
        sender.request(
            MessageBuilder::new(pointer, PointerRequest::SetCursor)
                .u32(serial)
                .object(loaded.surface)
                .i32(loaded.hotspot_x)
                .i32(loaded.hotspot_y),
        );
        self.current = Some(key);
    }

    /// Drop the memo of which cursor is applied, so the next [`Self::apply`]
    /// re-sends even for the same shape.
    ///
    /// The compositor resets the cursor when the pointer leaves and
    /// re-enters a surface, so the client's idea of "already applied" is
    /// stale after every enter.
    pub const fn invalidate(&mut self) {
        self.current = None;
    }

    /// Load one shape's image and wrap it in a surface, or `None` when no
    /// theme provides it or the buffer cannot be allocated.
    ///
    /// Failure is `None` rather than an error: a missing cursor shape must
    /// leave the pointer as it is, never take the window down.
    fn build(&self, sender: &mut WindowSender, names: &[&str]) -> Option<Loaded> {
        let image = CursorImage::load_system_named(names, self.scale)?;
        let width = i32::try_from(image.width).ok()?;
        let height = i32::try_from(image.height).ok()?;
        if width <= 0 || height <= 0 {
            return None;
        }
        let w = usize::try_from(width).ok()?;
        let h = usize::try_from(height).ok()?;
        let mut shm = ShmBuffer::new(w, h, 1).ok()?;
        let pixels = shm.pixels(0);
        let n = pixels.len().min(image.pixels.len());
        pixels[..n].copy_from_slice(&image.pixels[..n]);

        // The pool fd is duplicated: `shm` keeps the original open for as
        // long as the compositor may read the mapping.
        let pool_fd = shm.dup_fd().ok()?;
        let pool: crate::id::PoolId = sender.alloc_id();
        let msg = MessageBuilder::new(self.shm, ShmRequest::CreatePool)
            .new_id(pool)
            .i32(i32::try_from(shm.size_bytes()).unwrap_or(i32::MAX))
            .finish();
        sender.send_with_fds(msg, vec![pool_fd]);

        let surface: SurfaceId = sender.alloc_id();
        sender.request(
            MessageBuilder::new(self.compositor, CompositorRequest::CreateSurface).new_id(surface),
        );
        let buffer: BufferId = sender.alloc_id();
        sender.request(
            MessageBuilder::new(pool, ShmPoolRequest::CreateBuffer)
                .new_id(buffer)
                .i32(0)
                .i32(width)
                .i32(height)
                .i32(width * 4)
                .u32(Format::Argb8888.code()),
        );
        sender.request(
            MessageBuilder::new(surface, SurfaceRequest::Attach)
                .object(buffer)
                .i32(0)
                .i32(0),
        );
        sender.request(MessageBuilder::new(surface, SurfaceRequest::Commit));

        Some(Loaded {
            surface,
            hotspot_x: i32::try_from(image.hotspot_x).unwrap_or(0),
            hotspot_y: i32::try_from(image.hotspot_y).unwrap_or(0),
            shm,
            buffer,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conn::WindowRequest;
    use crate::id::{CompositorId, ShmId};
    use quetzalcoatl::mpsc;

    fn set() -> CursorSet {
        CursorSet::new(ShmId(2), CompositorId(3), 1)
    }

    /// A `WindowSender` over a real ring, allocating from `start..end`.
    fn window_sender(start: u32, end: u32) -> (WindowSender, mpsc::Consumer<WindowRequest>) {
        let (tx, rx) =
            mpsc::RingBuffer::<WindowRequest>::new(quetzalcoatl::capacity::Capacity::exact(64))
                .split();
        (
            WindowSender {
                tx,
                next_id: start,
                id_end: end,
                free_ids: Vec::new(),
            },
            rx,
        )
    }

    /// Every object a cursor creates must come from the window's reserved
    /// block.
    ///
    /// This is the bug that shipped: `build` allocated from the connection's
    /// `Sender` instead, which names ids the window already holds. The
    /// compositor answers with
    /// `invalid arguments for wl_shm#N.create_pool` and drops the
    /// connection, so the window dies as soon as the pointer first enters
    /// it. Nothing but a live compositor catches that, hence this test.
    #[test]
    fn every_cursor_object_is_allocated_from_the_windows_block() {
        let mut cursors = set();
        let (mut sender, _rx) = window_sender(100, 200);
        // A shape the machine really has, so `build` runs all the way
        // through the three allocations. Falls back to a name no theme
        // ships when the box has no cursor theme at all, in which case the
        // assertion below still holds trivially.
        cursors.apply(
            &mut sender,
            PointerId(9),
            1,
            "probe",
            &["left_ptr", "default", "arrow"],
        );
        assert!(
            (100..=200).contains(&sender.next_id),
            "cursor ids escaped the window's block: next_id = {}",
            sender.next_id
        );
        // Guard against a vacuous pass: on a box with no cursor theme
        // `build` bails before allocating, and the bounds check above would
        // hold for the buggy code too. Only assert the strong form when a
        // cursor really loaded.
        if cursors.loaded["probe"].is_some() {
            assert_eq!(
                sender.next_id, 103,
                "a loaded cursor takes exactly a pool, a surface and a buffer"
            );
        }
    }

    /// A shape no installed theme provides must leave the pointer alone
    /// rather than taking the window down with an error.
    #[test]
    fn an_unavailable_shape_is_survivable() {
        let mut cursors = set();
        let (mut sender, _rx) = window_sender(100, 200);
        cursors.apply(
            &mut sender,
            PointerId(9),
            1,
            "probe",
            &["cozy-no-such-cursor"],
        );
        assert!(cursors.current.is_none(), "nothing was applied");
        assert!(cursors.loaded.contains_key("probe"), "the miss is memoized");
    }

    /// Re-applying a shape that failed to load must not re-walk the theme
    /// directories on every pointer motion.
    #[test]
    fn a_failed_shape_is_looked_up_only_once() {
        let mut cursors = set();
        let (mut sender, _rx) = window_sender(100, 200);
        for _ in 0..8 {
            cursors.apply(
                &mut sender,
                PointerId(9),
                1,
                "probe",
                &["cozy-no-such-cursor"],
            );
        }
        assert_eq!(cursors.loaded.len(), 1);
    }

    #[test]
    fn a_fresh_set_has_applied_nothing() {
        assert!(set().current.is_none());
    }

    #[test]
    fn a_zero_or_negative_scale_is_clamped() {
        let s = CursorSet::new(ShmId(2), CompositorId(3), 0);
        assert_eq!(s.scale, 1);
        let s = CursorSet::new(ShmId(2), CompositorId(3), -4);
        assert_eq!(s.scale, 1);
    }

    #[test]
    fn changing_scale_drops_the_cache_so_images_reload_at_the_new_density() {
        let mut s = set();
        s.loaded.insert("default", None);
        s.current = Some("default");
        s.set_scale(2);
        assert!(s.loaded.is_empty(), "cached images are the wrong size now");
        assert!(s.current.is_none());
    }

    #[test]
    fn setting_the_same_scale_keeps_the_cache() {
        let mut s = set();
        s.loaded.insert("default", None);
        s.set_scale(1);
        assert_eq!(s.loaded.len(), 1, "no reason to reload at the same density");
    }

    #[test]
    fn invalidating_forces_the_next_apply_to_resend() {
        let mut s = set();
        s.current = Some("default");
        s.invalidate();
        assert!(s.current.is_none());
    }

    #[test]
    fn an_unloadable_shape_is_remembered_so_it_is_not_retried() {
        // `build` returning None must still occupy the map slot, otherwise
        // every motion event re-walks the theme directories.
        let mut s = set();
        s.loaded.insert("nonexistent", None);
        assert!(s.loaded.contains_key("nonexistent"));
        assert!(s.loaded["nonexistent"].is_none());
    }
}

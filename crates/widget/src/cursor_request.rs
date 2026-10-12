//! [`CursorRequest`] -- the pointer image a window wants, held until the
//! event loop passes it on to the compositor.

use crate::cursor_shape::CursorShape;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorRequest {
    shape: CursorShape,
    dirty: bool,
}

impl CursorRequest {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            shape: CursorShape::Default,
            dirty: true,
        }
    }

    pub fn want(&mut self, shape: CursorShape) {
        if shape != self.shape {
            self.shape = shape;
            self.dirty = true;
        }
    }

    /// The shape to show, once per change.
    pub const fn take(&mut self) -> Option<CursorShape> {
        if self.dirty {
            self.dirty = false;
            return Some(self.shape);
        }
        None
    }
}

impl Default for CursorRequest {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::CursorRequest;
    use crate::cursor_shape::CursorShape;

    #[test]
    fn the_first_take_sets_the_plain_arrow() {
        let mut request = CursorRequest::new();
        assert_eq!(request.take(), Some(CursorShape::Default));
        assert_eq!(request.take(), None);
    }

    #[test]
    fn a_change_is_taken_once_and_a_repeat_is_not() {
        let mut request = CursorRequest::new();
        request.take();
        request.want(CursorShape::Move);
        assert_eq!(request.take(), Some(CursorShape::Move));
        request.want(CursorShape::Move);
        assert_eq!(request.take(), None);
    }
}

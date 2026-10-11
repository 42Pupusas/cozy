use platform::draw::{Canvas, Color, Rect};
use platform::toplevel::{KeyPress, Painter, PhysicalSize};

use crate::cancel_key::CancelKey;

pub struct ChooserPainter {
    cancel_requested: bool,
}

impl ChooserPainter {
    const BACKGROUND: Color = Color::rgb(0x1e, 0x1e, 0x2e);

    #[must_use]
    pub const fn new() -> Self {
        Self {
            cancel_requested: false,
        }
    }

    #[must_use]
    pub const fn cancel_requested(&self) -> bool {
        self.cancel_requested
    }
}

impl Default for ChooserPainter {
    fn default() -> Self {
        Self::new()
    }
}

impl Painter for ChooserPainter {
    fn paint(&mut self, canvas: &mut Canvas<'_>, size: PhysicalSize) {
        canvas.fill_rect(Rect::new(0, 0, size.width, size.height), Self::BACKGROUND);
    }

    fn background(&self) -> Color {
        Self::BACKGROUND
    }

    fn on_key(&mut self, press: KeyPress) {
        if CancelKey::matches(press) {
            self.cancel_requested = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform::toplevel::{Key, ModifierState};

    struct Press;

    impl Press {
        fn of(key: Key) -> KeyPress {
            KeyPress {
                key,
                modifiers: ModifierState::from_wire(0, 0, 0, 0),
                repeat: false,
            }
        }
    }

    #[test]
    fn a_fresh_painter_has_no_cancel_pending() {
        assert!(!ChooserPainter::new().cancel_requested());
    }

    #[test]
    fn escape_marks_the_painter_cancelled() {
        let mut painter = ChooserPainter::new();
        painter.on_key(Press::of(Key::Escape));
        assert!(painter.cancel_requested());
    }

    #[test]
    fn typing_does_not_cancel() {
        let mut painter = ChooserPainter::new();
        painter.on_key(Press::of(Key::Char('a')));
        assert!(!painter.cancel_requested());
    }
}

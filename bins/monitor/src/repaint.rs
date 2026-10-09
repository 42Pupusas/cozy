//! [`Repaint`] -- how much of a frame has to be redrawn.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Repaint {
    full: bool,
    text: bool,
}

impl Repaint {
    /// A full repaint always redraws text too.
    #[must_use]
    pub const fn new(full: bool, text: bool) -> Self {
        Self {
            full,
            text: full || text,
        }
    }

    /// The whole window was cleared, so every tile redraws from scratch.
    #[must_use]
    pub const fn full(self) -> bool {
        self.full
    }

    /// Displayed numbers changed, so text regions redraw.
    #[must_use]
    pub const fn text(self) -> bool {
        self.text
    }
}

#[cfg(test)]
mod tests {
    use super::Repaint;

    #[test]
    fn a_full_repaint_includes_text() {
        let repaint = Repaint::new(true, false);
        assert!(repaint.full() && repaint.text());
    }

    #[test]
    fn a_text_repaint_is_not_full() {
        let repaint = Repaint::new(false, true);
        assert!(!repaint.full() && repaint.text());
        assert!(!Repaint::new(false, false).text());
    }
}

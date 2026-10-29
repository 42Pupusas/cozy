//! Glyphs gathered on one row before they are written in a single store call.

/// Up to [`Self::CAPACITY`] glyphs with their widths, and the cells they span.
pub struct GlyphRun {
    glyphs: [(char, u8); Self::CAPACITY],
    len: usize,
    span: usize,
}

impl GlyphRun {
    pub const CAPACITY: usize = 64;

    pub const fn new() -> Self {
        Self {
            glyphs: [('\0', 0); Self::CAPACITY],
            len: 0,
            span: 0,
        }
    }

    pub const fn is_full(&self) -> bool {
        self.len == Self::CAPACITY
    }

    pub const fn span(&self) -> usize {
        self.span
    }

    pub const fn push(&mut self, ch: char, width: u8) {
        self.glyphs[self.len] = (ch, width);
        self.len += 1;
        self.span += width as usize;
    }

    pub fn glyphs(&self) -> &[(char, u8)] {
        &self.glyphs[..self.len]
    }

    pub fn last(&self) -> Option<char> {
        self.glyphs().last().map(|&(ch, _)| ch)
    }

    pub const fn clear(&mut self) {
        self.len = 0;
        self.span = 0;
    }
}

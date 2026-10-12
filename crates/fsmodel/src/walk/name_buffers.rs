use ququmatz::dir::DirentBuf;

/// Emptied name buffers kept for reuse, so a thread that keeps splitting
/// large directories stops allocating once it holds a few.
#[derive(Debug, Default)]
pub struct NameBuffers {
    spare: Vec<Vec<u8>>,
}

impl NameBuffers {
    const KEPT: usize = 4;

    #[must_use]
    pub const fn new() -> Self {
        Self { spare: Vec::new() }
    }

    pub fn take(&mut self) -> Vec<u8> {
        self.spare
            .pop()
            .unwrap_or_else(|| Vec::with_capacity(DirentBuf::BYTES))
    }

    pub fn give(&mut self, mut names: Vec<u8>) {
        if self.spare.len() < Self::KEPT {
            names.clear();
            self.spare.push(names);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::NameBuffers;

    #[test]
    fn a_returned_buffer_comes_back_empty_with_its_capacity() {
        let mut buffers = NameBuffers::new();
        let mut names = buffers.take();
        names.extend_from_slice(b"x\0");
        let capacity = names.capacity();
        buffers.give(names);
        let again = buffers.take();
        assert!(again.is_empty());
        assert_eq!(again.capacity(), capacity);
    }

    #[test]
    fn only_a_few_buffers_are_kept() {
        let mut buffers = NameBuffers::new();
        for _ in 0..10 {
            buffers.give(Vec::new());
        }
        assert_eq!(buffers.spare.len(), NameBuffers::KEPT);
    }
}

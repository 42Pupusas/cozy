//! Input bytes waiting to be written to the PTY master, as the chunks they
//! arrived in.
//!
//! The write SQE points straight into the front chunk, so no byte is copied
//! on the way to the kernel. A short write only moves the offset into that
//! chunk forward.

use std::collections::VecDeque;

#[derive(Default)]
pub struct WriteQueue {
    chunks: VecDeque<Box<[u8]>>,
    /// Bytes of the front chunk already written.
    offset: usize,
    /// Whether the front chunk's remainder is handed to the kernel: its
    /// write SQE is in flight, or it hit `EAGAIN` and waits for `POLLOUT`.
    /// The front chunk must not be freed or popped while this is set.
    busy: bool,
}

impl WriteQueue {
    pub fn push(&mut self, chunk: Box<[u8]>) {
        if !chunk.is_empty() {
            self.chunks.push_back(chunk);
        }
    }

    /// The unwritten remainder of the front chunk, if there is one and no
    /// write is already in flight. Marks the queue busy.
    ///
    /// The returned pointer stays valid while the queue is busy: the chunk's
    /// heap allocation does not move when the deque grows, and the front is
    /// only popped by [`complete`](Self::complete).
    pub fn start(&mut self) -> Option<(*const u8, u32)> {
        if self.busy {
            return None;
        }
        let rest = self.remainder()?;
        self.busy = true;
        Some(rest)
    }

    /// The in-flight remainder again, for resubmitting the same write after
    /// `EAGAIN` or `EINTR`.
    pub fn retry(&self) -> Option<(*const u8, u32)> {
        if self.busy { self.remainder() } else { None }
    }

    /// The kernel finished with the in-flight write: `Some(n)` bytes went
    /// out, or `None` for a fatal error, which drops the rest of the chunk
    /// since retrying it would fail forever.
    pub fn complete(&mut self, written: Option<usize>) {
        self.busy = false;
        let Some(front) = self.chunks.front() else {
            return;
        };
        let offset = written.map_or(front.len(), |n| self.offset + n);
        if offset < front.len() {
            self.offset = offset;
        } else {
            self.chunks.pop_front();
            self.offset = 0;
        }
    }

    fn remainder(&self) -> Option<(*const u8, u32)> {
        let rest = self.chunks.front()?.get(self.offset..)?;
        Some((rest.as_ptr(), u32::try_from(rest.len()).unwrap_or(u32::MAX)))
    }
}

#[cfg(test)]
mod tests {
    use super::WriteQueue;

    fn queue(chunks: &[&[u8]]) -> WriteQueue {
        let mut queue = WriteQueue::default();
        for chunk in chunks {
            queue.push(Box::from(*chunk));
        }
        queue
    }

    fn started_len(queue: &mut WriteQueue) -> Option<u32> {
        queue.start().map(|(_, len)| len)
    }

    #[test]
    fn writes_whole_chunks_one_at_a_time() {
        let mut queue = queue(&[b"abcd", b"ef"]);
        assert_eq!(started_len(&mut queue), Some(4));
        assert_eq!(started_len(&mut queue), None);
        queue.complete(Some(4));
        assert_eq!(started_len(&mut queue), Some(2));
        queue.complete(Some(2));
        assert_eq!(started_len(&mut queue), None);
    }

    #[test]
    fn a_short_write_resumes_where_it_stopped() {
        let mut queue = queue(&[b"abcdef"]);
        let (head, _) = queue.start().unwrap();
        queue.complete(Some(4));
        let (rest, len) = queue.start().unwrap();
        assert_eq!(len, 2);
        assert_eq!(rest, head.wrapping_add(4));
    }

    #[test]
    fn retry_repeats_the_in_flight_write() {
        let mut queue = queue(&[b"abc"]);
        assert_eq!(queue.retry(), None);
        let started = queue.start();
        assert_eq!(queue.retry(), started);
    }

    #[test]
    fn a_fatal_error_drops_the_chunk() {
        let mut queue = queue(&[b"abc", b"de"]);
        queue.start();
        queue.complete(None);
        assert_eq!(started_len(&mut queue), Some(2));
    }

    #[test]
    fn empty_chunks_are_skipped() {
        let mut queue = queue(&[b""]);
        assert_eq!(started_len(&mut queue), None);
    }
}

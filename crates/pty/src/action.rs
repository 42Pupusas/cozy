//! What the event loop does with one completion.

use crate::inflight::OpTag;
use crate::{EINTR, ENOBUFS};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CqeAction {
    None,
    /// The multishot read filled pool buffer `buf_id` with `len` bytes.
    ReadOk { buf_id: u16, len: u32 },
    /// The `POLLOUT` armed after a write hit `EAGAIN` fired — write again.
    WriteReady,
    /// Master-fd read returned 0 (EOF) or an error (typically EIO once the
    /// child exits and the slave side closes) — both mean "child gone".
    ReadEof,
    /// Write CQE with its raw result (bytes written, or negative errno).
    WriteDone(i32),
    Signal,
    Wake,
}

impl CqeAction {
    pub const fn from_cqe(user_data: u64, result: i32, buffer_id: Option<u16>) -> Self {
        match OpTag::from_user_data(user_data) {
            Some(OpTag::Read) => match (result, buffer_id) {
                // Matched positive, so the sign-preserving cast is exact.
                (1.., Some(buf_id)) => Self::ReadOk {
                    buf_id,
                    len: result.cast_unsigned(),
                },
                // The read stopped while the child is still there: the pool
                // ran dry or a signal interrupted it. The loop re-arms it
                // once its buffers are free. Treating EINTR as EOF (an old
                // bug) tore the whole session down on a stray signal.
                (r, _) if r == -ENOBUFS || r == -EINTR => Self::None,
                // 0 (EOF), a fatal errno (typically -EIO once the child exits
                // and the slave side closes), or bytes without a buffer, which
                // a buffer-select read cannot produce.
                _ => Self::ReadEof,
            },
            Some(OpTag::Write) => Self::WriteDone(result),
            Some(OpTag::Signal) => Self::Signal,
            Some(OpTag::Wake) => Self::Wake,
            // A poll error (POLLHUP/POLLERR, or the fd gone) still retries:
            // the retried write then reports the real errno.
            Some(OpTag::WritePoll) => Self::WriteReady,
            None => Self::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CqeAction;
    use crate::inflight::OpTag;
    use crate::{EINTR, ENOBUFS};

    const READ: u64 = OpTag::Read.as_user_data();

    #[test]
    fn a_chunk_names_its_buffer() {
        let action = CqeAction::from_cqe(READ, 12, Some(3));
        assert_eq!(action, CqeAction::ReadOk { buf_id: 3, len: 12 });
    }

    #[test]
    fn a_dry_pool_or_a_signal_only_stops_the_read() {
        assert_eq!(CqeAction::from_cqe(READ, -ENOBUFS, None), CqeAction::None);
        assert_eq!(CqeAction::from_cqe(READ, -EINTR, None), CqeAction::None);
    }

    #[test]
    fn eof_and_eio_end_the_session() {
        assert_eq!(CqeAction::from_cqe(READ, 0, None), CqeAction::ReadEof);
        assert_eq!(CqeAction::from_cqe(READ, -5, None), CqeAction::ReadEof);
    }
}

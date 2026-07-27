//! Reads one clipboard representation out of a compositor-mediated pipe.
//!
//! `data_control_offer.receive` hands the *owning client* a pipe we created,
//! then that client writes at its own pace. Two consequences shape this type:
//!
//! * The read must be bounded. The peer is an arbitrary application; a hung or
//!   malicious one would otherwise stall a daemon forever. A read timeout is
//!   the only thing standing between "some app froze" and "the clipboard
//!   manager froze".
//! * Our copy of the write end must be dropped before reading, or the pipe
//!   never reports EOF — we would be waiting on ourselves.

use platform::conn::Sender;
use platform::id::DataControlOfferId;
use platform::protocol::DataControlOfferRequest;
use platform::wire::MessageBuilder;
use std::io::Read;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

/// Reads a single MIME representation from an offer.
pub struct MimeReader {
    /// How long the peer may stall before we abandon this representation.
    timeout: Duration,
    /// Refuses payloads that would exhaust memory. A clipboard entry is a
    /// selection or an image, not a filesystem.
    limit: usize,
}

impl MimeReader {
    /// Default bounds: generous for a screenshot, hostile to a runaway peer.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            timeout: Duration::from_secs(3),
            limit: 64 * 1024 * 1024,
        }
    }

    /// Override the stall timeout.
    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Ask `offer` for `mime` and read the bytes the owner writes back.
    ///
    /// Returns `None` when the peer sends nothing, stalls past the timeout, or
    /// exceeds the size limit — all of which mean "skip this representation",
    /// never "abort the daemon".
    pub fn read(
        &self,
        sender: &mut Sender,
        offer: DataControlOfferId,
        mime: &str,
    ) -> Option<Vec<u8>> {
        let (mut reader, writer) = UnixStream::pair().ok()?;
        // A per-read timeout bounds each syscall; `deadline` below bounds the
        // whole transfer, so a peer dribbling one byte per timeout cannot hold
        // the loop open indefinitely.
        reader.set_read_timeout(Some(self.timeout)).ok()?;
        let message = MessageBuilder::new(offer, DataControlOfferRequest::Receive)
            .string(mime)
            .finish();
        sender
            .send_with_fds(&message, &[std::os::fd::AsFd::as_fd(&writer)])
            .ok()?;
        // The compositor now owns a duplicate; ours must go or EOF never
        // arrives.
        drop(writer);

        let deadline = Instant::now() + self.timeout;
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if bytes.len() + n > self.limit {
                        return None;
                    }
                    bytes.extend_from_slice(&chunk[..n]);
                }
                // A timeout surfaces as WouldBlock/TimedOut on a socket with
                // SO_RCVTIMEO; anything else is a broken pipe.
                Err(_) => return None,
            }
            if Instant::now() >= deadline {
                return None;
            }
        }
        (!bytes.is_empty()).then_some(bytes)
    }
}

impl Default for MimeReader {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::MimeReader;
    use std::time::Duration;

    #[test]
    fn bounds_are_configurable_without_touching_wayland() {
        let reader = MimeReader::new().with_timeout(Duration::from_millis(50));
        assert_eq!(reader.timeout, Duration::from_millis(50));
        assert_eq!(reader.limit, 64 * 1024 * 1024);
    }
}

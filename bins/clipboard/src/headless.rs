//! Non-rendering clipboard workflow stress test.
//!
//! This drives the same history and MIME policy used by the Wayland daemon,
//! while replacing compositor notifications with a deterministic copy/paste
//! stream. That keeps perf focused on clipboard policy rather than sockets.

use clipboard::{ClipboardHistory, ClipboardOffer, ClipboardOwner, ClipboardPayload, OwnerRequest};
use headless_stress::StressOptions;

/// Exercises the clipboard manager's real copy, retention, and paste paths.
pub struct ClipboardStress;

impl ClipboardStress {
    /// Run the deterministic workflow and report completed rounds.
    pub fn run(options: StressOptions) {
        let mut history = ClipboardHistory::with_capacity(200);
        let completed = options.run(|round| {
            let payload = Self::payload(round);
            history.push(payload);

            if let Some(entry) = history.entries().first() {
                let owner = ClipboardOwner::new(entry.payload());
                let mime = match round % 4 {
                    0 => "text/plain;charset=utf-8",
                    1 => "text/plain",
                    2 => "text/uri-list",
                    _ => "image/png",
                };
                let _ = owner.handle(OwnerRequest::Send(mime.to_owned()));
                let _ = owner.handle(OwnerRequest::Cancelled);
            }

            // A real manager periodically exposes the history to its picker;
            // walking it here includes the bounded-retention read path without
            // introducing an artificial allocation-heavy serialization loop.
            let _ = history
                .entries()
                .iter()
                .map(|entry| entry.payload().len())
                .sum::<usize>();
        });
        println!("clipboard headless stress: {completed} rounds");
    }

    fn payload(round: u64) -> ClipboardPayload {
        let mut payload = ClipboardPayload::new();
        let text = format!("clipboard stress selection {round}");
        payload.offer(ClipboardOffer::new(
            "text/plain;charset=utf-8",
            text.into_bytes(),
        ));
        payload.offer(ClipboardOffer::new(
            "text/uri-list",
            format!("file:///tmp/stress-{round}.txt\n").into_bytes(),
        ));
        if round.is_multiple_of(4) {
            payload.offer(ClipboardOffer::new(
                "image/png",
                vec![0x89, b'P', b'N', b'G'],
            ));
        }
        payload
    }
}

#[cfg(test)]
mod tests {
    use super::ClipboardStress;
    use headless_stress::StressOptions;

    #[test]
    fn workflow_completes_without_wayland() {
        ClipboardStress::run(StressOptions {
            iterations: 4,
            duration: None,
        });
    }
}

//! [`TextSelection`] -- plain text this window has put on the clipboard.
//!
//! The seat has one `wl_data_device`, and the drag controller already owns it,
//! so this type holds no device of its own: the caller passes the
//! `(manager, device)` pair for each publish. What it does own is the outgoing
//! `wl_data_source` and the text it serves when a peer asks to paste.

use crate::conn::WindowSender;
use crate::id::{DataDeviceId, DataDeviceManagerId, DataSourceId};
use crate::protocol::{
    DataDeviceManagerRequest, DataDeviceRequest, DataSourceEvent, DataSourceRequest,
};
use crate::wire::MessageBuilder;

/// The MIME types offered for the text, most specific first.
const MIME_TYPES: [&str; 2] = ["text/plain;charset=utf-8", "text/plain"];

/// The window's clipboard selection, while it holds one.
pub struct TextSelection {
    id: Option<DataSourceId>,
    body: String,
    awaiting_send_fd: bool,
}

impl TextSelection {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            id: None,
            body: String::new(),
            awaiting_send_fd: false,
        }
    }

    /// Make `text` the seat's selection, under `serial` (the serial of the
    /// input event that asked for the copy).
    ///
    /// A previous source of ours is destroyed once the new one is set, since
    /// the compositor cancels it at that moment. Empty text publishes
    /// nothing, so a copy never clears the clipboard by accident.
    pub fn publish(
        &mut self,
        window_tx: &mut WindowSender,
        data_device: (DataDeviceManagerId, DataDeviceId),
        serial: u32,
        text: String,
    ) {
        if text.is_empty() {
            return;
        }
        let (manager, device) = data_device;
        self.body = text;
        let source: DataSourceId = window_tx.alloc_id();
        window_tx.request(
            MessageBuilder::new(manager, DataDeviceManagerRequest::CreateDataSource).new_id(source),
        );
        for mime in MIME_TYPES {
            window_tx.request(MessageBuilder::new(source, DataSourceRequest::Offer).string(mime));
        }
        window_tx.request(
            MessageBuilder::new(device, DataDeviceRequest::SetSelection)
                .object(source)
                .u32(serial),
        );
        if let Some(previous) = self.id.replace(source) {
            window_tx.request(MessageBuilder::new(previous, DataSourceRequest::Destroy));
        }
    }

    /// The live source's id, for routing `wl_data_source.*` events here.
    #[must_use]
    pub const fn id(&self) -> Option<DataSourceId> {
        self.id
    }

    /// Handle one `wl_data_source` event for the live source.
    ///
    /// `Send` arms the fd wait: the pipe to write into arrives as the next
    /// `RawEvent::Fd`. `Cancelled` means another client took the clipboard.
    pub fn on_event(&mut self, window_tx: &mut WindowSender, event: DataSourceEvent) {
        match event {
            DataSourceEvent::Send => self.awaiting_send_fd = true,
            DataSourceEvent::Cancelled => {
                if let Some(source) = self.id.take() {
                    window_tx.request(MessageBuilder::new(source, DataSourceRequest::Destroy));
                }
            }
            _ => {}
        }
    }

    /// Whether a `send` is in flight and the next fd is the pipe for it.
    #[must_use]
    pub const fn is_awaiting_send_fd(&self) -> bool {
        self.awaiting_send_fd
    }

    /// Write the text into `fd` and close it, which is the peer's EOF.
    pub fn write_send_fd(&mut self, fd: std::os::fd::OwnedFd) {
        use crate::sys::OwnedFdExt as _;
        self.awaiting_send_fd = false;
        fd.write_all_and_close(self.body.as_bytes());
    }
}

impl Default for TextSelection {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conn::WindowRequest;
    use crate::id::ObjectId as _;
    use quetzalcoatl::capacity::Capacity;
    use quetzalcoatl::mpsc;

    const DEVICE: (DataDeviceManagerId, DataDeviceId) = (DataDeviceManagerId(1), DataDeviceId(2));

    /// A sender and the queue its requests land in.
    struct Wire {
        tx: WindowSender,
        rx: mpsc::Consumer<WindowRequest>,
    }

    impl Wire {
        fn new() -> Self {
            let (tx, rx) = mpsc::RingBuffer::<WindowRequest>::new(Capacity::exact(64)).split();
            Self {
                tx: WindowSender {
                    tx,
                    next_id: 100,
                    id_end: 200,
                    free_ids: Vec::new(),
                },
                rx,
            }
        }

        /// `(object, opcode)` of every request sent since the last drain.
        fn drain(&mut self) -> Vec<(u32, u16)> {
            let mut sent = Vec::new();
            while let Some(WindowRequest::Send(msg)) = self.rx.pop() {
                sent.push((
                    u32::from_ne_bytes(msg[0..4].try_into().expect("object")),
                    u16::from_ne_bytes(msg[4..6].try_into().expect("opcode")),
                ));
            }
            sent
        }
    }

    #[test]
    fn a_fresh_selection_holds_nothing() {
        let selection = TextSelection::new();
        assert_eq!(selection.id(), None);
        assert!(!selection.is_awaiting_send_fd());
    }

    #[test]
    fn publishing_sets_the_selection_after_offering_every_type() {
        let mut wire = Wire::new();
        let mut selection = TextSelection::new();
        selection.publish(&mut wire.tx, DEVICE, 7, "/a/b".to_owned());
        let source = selection.id().expect("a source").raw();
        let sent = wire.drain();
        let offers: Vec<usize> = sent
            .iter()
            .enumerate()
            .filter(|(_, m)| **m == (source, DataSourceRequest::Offer as u16))
            .map(|(i, _)| i)
            .collect();
        let set_position = sent
            .iter()
            .position(|m| *m == (2, DataDeviceRequest::SetSelection as u16))
            .expect("set_selection on the device");
        assert_eq!(offers.len(), MIME_TYPES.len());
        assert!(
            offers.iter().all(|&i| i < set_position),
            "offers precede the set"
        );
    }

    #[test]
    fn empty_text_publishes_nothing() {
        let mut wire = Wire::new();
        let mut selection = TextSelection::new();
        selection.publish(&mut wire.tx, DEVICE, 7, String::new());
        assert_eq!(selection.id(), None);
        assert!(wire.drain().is_empty());
    }

    #[test]
    fn replacing_the_selection_destroys_the_old_source() {
        let mut wire = Wire::new();
        let mut selection = TextSelection::new();
        selection.publish(&mut wire.tx, DEVICE, 7, "one".to_owned());
        let first = selection.id().expect("first").raw();
        wire.drain();
        selection.publish(&mut wire.tx, DEVICE, 8, "two".to_owned());
        assert_ne!(
            selection.id().map(platform_wire::id::ObjectId::raw),
            Some(first)
        );
        assert!(
            wire.drain()
                .contains(&(first, DataSourceRequest::Destroy as u16))
        );
    }

    #[test]
    fn send_arms_the_fd_wait_and_writing_clears_it() {
        let mut wire = Wire::new();
        let mut selection = TextSelection::new();
        selection.publish(&mut wire.tx, DEVICE, 7, "x".to_owned());
        selection.on_event(&mut wire.tx, DataSourceEvent::Send);
        assert!(selection.is_awaiting_send_fd());
        let (reader, writer) = std::os::unix::net::UnixStream::pair().expect("pair");
        selection.write_send_fd(writer.into());
        assert!(!selection.is_awaiting_send_fd());
        let mut served = String::new();
        std::io::Read::read_to_string(&mut &reader, &mut served).expect("read");
        assert_eq!(served, "x");
    }

    #[test]
    fn cancellation_releases_the_source() {
        let mut wire = Wire::new();
        let mut selection = TextSelection::new();
        selection.publish(&mut wire.tx, DEVICE, 7, "x".to_owned());
        let source = selection.id().expect("source").raw();
        wire.drain();
        selection.on_event(&mut wire.tx, DataSourceEvent::Cancelled);
        assert_eq!(selection.id(), None);
        assert!(
            wire.drain()
                .contains(&(source, DataSourceRequest::Destroy as u16))
        );
    }
}

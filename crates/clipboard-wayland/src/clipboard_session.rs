//! Persistent Wayland clipboard ownership.
//!
//! This uses `zwlr_data_control_unstable_v1`, rather than core
//! `wl_data_device`, because a keybind-driven publisher has no input serial.
//! The owner stays alive in this process until the compositor reports
//! `cancelled` — that is what "the clipboard holds it" means on Wayland: there
//! is no server-side storage, only a live client promising to serve bytes.

use clipboard::{
    ClipboardOwner as PayloadOwner, ClipboardPayload, OwnerRequest, OwnerResponse, PendingSends,
};
use platform::conn::Sender;
use platform::event::RawEvent;
use platform::id::{
    DataControlDeviceId, DataControlManagerId, DataControlSourceId, RegistryId, SeatId,
};
use platform::protocol::{
    DataControlDeviceRequest, DataControlManagerRequest, DataControlSourceEvent,
    DataControlSourceRequest,
};
use platform::surface::SurfaceBootstrap;
use platform::wire::{MessageBuilder, WireEvent};
use platform::sys::OwnedFdExt as _;
use quetzalcoatl::spsc::Consumer;
use std::os::fd::OwnedFd;
use std::path::PathBuf;

/// Owns one payload until another clipboard source replaces it.
pub struct ClipboardSession<'a> {
    payload: PayloadOwner<'a>,
    source: DataControlSourceId,
    /// Requests whose pipe has not arrived yet, oldest first.
    ///
    /// A queue rather than a single slot because a peer routinely asks for
    /// several representations at once -- a browser pasting a screenshot wants
    /// `image/png` *and* `text/uri-list` -- so two `send` messages can arrive
    /// before either pipe does. See [`PendingSends`] for why order is the only
    /// correlation available.
    pending: PendingSends,
    cleanup_path: Option<PathBuf>,
}

impl<'a> ClipboardSession<'a> {
    /// Publish `payload` using a fresh seat object.
    pub fn publish(
        sender: &mut Sender,
        manager: DataControlManagerId,
        registry: RegistryId,
        seat_global: u32,
        seat_version: u32,
        payload: &'a ClipboardPayload,
        cleanup_path: Option<PathBuf>,
    ) -> platform::Result<Self> {
        let seat: SeatId = SurfaceBootstrap::bind(
            sender,
            registry,
            seat_global,
            platform::protocol::WL_SEAT.name,
            platform::protocol::WL_SEAT.version.min(seat_version),
        )?;
        let device: DataControlDeviceId = sender.alloc_id();
        sender.request(
            MessageBuilder::new(manager, DataControlManagerRequest::GetDevice)
                .new_id(device)
                .object(seat),
        )?;
        let source: DataControlSourceId = sender.alloc_id();
        sender.request(
            MessageBuilder::new(manager, DataControlManagerRequest::CreateSource).new_id(source),
        )?;
        for mime in payload.mimes() {
            sender.request(
                MessageBuilder::new(source, DataControlSourceRequest::Offer).string(mime),
            )?;
        }
        sender.request(
            MessageBuilder::new(device, DataControlDeviceRequest::SetSelection).object(source),
        )?;
        Ok(Self {
            payload: PayloadOwner::new(payload),
            source,
            pending: PendingSends::new(),
            cleanup_path,
        })
    }

    /// Serve requests until the clipboard is replaced or the connection ends.
    pub fn run(&mut self, events: &mut Consumer<RawEvent>) {
        while let Some(event) = events.pop_block() {
            if !self.handle(event) {
                return;
            }
        }
    }

    /// Handle one event; `false` means the owner should stop.
    ///
    /// Split out of [`Self::run`] so a caller with its own event loop — a
    /// daemon that is simultaneously watching for new selections — can drive
    /// the same logic without surrendering its ring.
    pub fn handle(&mut self, event: RawEvent) -> bool {
        if event.is(self.source) {
            match event.event::<DataControlSourceEvent>() {
                Some(DataControlSourceEvent::Send) => {
                    // The mime arrives on the message; the pipe to write it
                    // into arrives out of band in a later `Fd` event. Queue it
                    // rather than overwrite: a second `send` may well land
                    // before the first pipe does.
                    let Some(bytes) = event.wire_bytes() else {
                        return true;
                    };
                    let Some((mut wire, _)) = WireEvent::parse(bytes) else {
                        return true;
                    };
                    self.pending.push(wire.string());
                }
                Some(DataControlSourceEvent::Cancelled) => {
                    let _ = self.payload.handle(OwnerRequest::Cancelled);
                    return false;
                }
                None => {}
            }
            return true;
        }
        // The reader forwards every fd that arrived with one receive batch as
        // a single `Fd` event, so a batch carrying two requests carries two
        // pipes. Serving only the first wrote the wrong payload into it and
        // dropped the other -- closing it unanswered, which the peer reads as
        // zero bytes and renders as an empty image.
        if let RawEvent::Fd(fds) = event {
            for fd in fds {
                self.serve(fd);
            }
        }
        true
    }

    /// Answer one pipe with the bytes for the request it belongs to.
    ///
    /// Dropping `fd` without writing is the correct response to an
    /// unrecognised request: closing the pipe tells the peer "nothing here",
    /// whereas leaving it open would hang a reader waiting for an EOF that
    /// never comes.
    fn serve(&mut self, fd: OwnedFd) {
        let Some(mime) = self.pending.claim() else {
            return;
        };
        let response = self.payload.handle(OwnerRequest::Send(mime));
        let bytes = match response {
            OwnerResponse::Bytes(bytes) => bytes,
            OwnerResponse::Unsupported | OwnerResponse::Stop => return,
        };
        fd.write_all_and_close(bytes);
    }
}

impl Drop for ClipboardSession<'_> {
    fn drop(&mut self) {
        if let Some(path) = self.cleanup_path.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use clipboard::ClipboardPayload;

    #[test]
    fn owner_type_is_built_around_payload_decisions() {
        let payload = ClipboardPayload::new();
        let _ = clipboard::ClipboardOwner::new(&payload);
    }
}

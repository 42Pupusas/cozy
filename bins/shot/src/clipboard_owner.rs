//! Persistent Wayland clipboard ownership for screenshot payloads.
//!
//! This uses `zwlr_data_control_unstable_v1`, rather than core
//! `wl_data_device`, because a keybind-driven screenshot has no input serial.
//! The owner stays in this process until the compositor reports `cancelled`.

use clipboard::{ClipboardOwner as PayloadOwner, ClipboardPayload, OwnerRequest, OwnerResponse};
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
use quetzalcoatl::spsc::Consumer;
use std::io::Write;
use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd};
use std::path::PathBuf;

/// Owns one payload until another clipboard source replaces it.
pub struct ClipboardSession<'a> {
    payload: PayloadOwner<'a>,
    source: DataControlSourceId,
    pending_mime: Option<String>,
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
            pending_mime: None,
            cleanup_path,
        })
    }

    /// Serve requests until the clipboard is replaced or the connection ends.
    pub fn run(&mut self, events: &mut Consumer<RawEvent>) {
        while let Some(event) = events.pop_block() {
            if event.is(self.source) {
                match event.event::<DataControlSourceEvent>() {
                    Some(DataControlSourceEvent::Send) => {
                        let Some(bytes) = event.wire_bytes() else {
                            continue;
                        };
                        let Some((mut wire, _)) = WireEvent::parse(bytes) else {
                            continue;
                        };
                        self.pending_mime = Some(wire.string());
                    }
                    Some(DataControlSourceEvent::Cancelled) => {
                        let _ = self.payload.handle(OwnerRequest::Cancelled);
                        return;
                    }
                    None => {}
                }
                continue;
            }
            if let RawEvent::Fd(fds) = event
                && let Some(fd) = fds.into_iter().next()
            {
                self.serve(fd);
            }
        }
    }

    fn serve(&mut self, fd: OwnedFd) {
        let Some(mime) = self.pending_mime.take() else {
            return;
        };
        let response = self.payload.handle(OwnerRequest::Send(mime));
        let bytes = match response {
            OwnerResponse::Bytes(bytes) => bytes,
            OwnerResponse::Unsupported | OwnerResponse::Stop => return,
        };
        let raw = fd.into_raw_fd();
        // SAFETY: the RawEvent transferred ownership of this descriptor to us.
        let mut file = unsafe { std::fs::File::from_raw_fd(raw) };
        let _ = file.write_all(bytes);
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
    use super::*;

    #[test]
    fn owner_type_is_built_around_payload_decisions() {
        let payload = ClipboardPayload::new();
        let _ = PayloadOwner::new(&payload);
    }
}

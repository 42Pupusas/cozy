use super::Interface;

pub const WL_DATA_DEVICE_MANAGER: Interface = Interface {
    name: "wl_data_device_manager",
    // v3 is the minimum a *drag* works at, even though v1 covers the whole
    // clipboard. From v3 the compositor negotiates a `dnd_action` between
    // source and destination, and it withholds `wl_data_device.drop` from a
    // v3 destination when that negotiation selected nothing. Binding v1
    // leaves us unable to send `wl_data_source.set_actions` at all, so every
    // drag into a modern toolkit shows its "drop here" affordance and then
    // delivers nothing on release. See `DndAction`.
    version: 3,
};

requests! {
    /// `wl_data_device_manager` requests — clipboard/DnD factory.
    DataDeviceManagerRequest {
        /// Create a data source (data we offer to other clients).
        CreateDataSource = 0,
        /// Get the per-seat data device.
        GetDataDevice = 1,
    }
}
requests! {
    /// `wl_data_device` requests.
    DataDeviceRequest {
        /// Start a drag-and-drop (unused here).
        StartDrag = 0,
        /// Set the clipboard selection to our data source: `serial`, `source`.
        SetSelection = 1,
        /// Release the device (v2+).
        Release = 2,
    }
}
events! {
    /// `wl_data_device` events.
    DataDeviceEvent {
        /// A new drag offer appeared (unused here).
        DataOffer = 0,
        /// Keyboard focus entered during `DnD` (unused here).
        Enter = 1,
        /// `DnD` focus left (unused here).
        Leave = 2,
        /// `DnD` motion (unused here).
        Motion = 3,
        /// `DnD` drop (unused here).
        Drop = 4,
        /// The clipboard selection changed: `id` (a `wl_data_offer`, or 0).
        Selection = 5,
    }
}
requests! {
    /// `wl_data_source` requests — data we offer to other clients (our copy).
    DataSourceRequest {
        /// Advertise one MIME type we can provide: `mime_type` (string).
        Offer = 0,
        /// Destroy the source.
        Destroy = 1,
        /// Set the allowed `DnD` actions (v3+; unused for plain clipboard).
        SetActions = 2,
    }
}
events! {
    /// `wl_data_source` events.
    DataSourceEvent {
        /// A destination accepted a MIME type (drag target; unused here).
        Target = 0,
        /// A client requests the data: `mime_type` (string), `fd` — we write
        /// the selection to `fd` and close it. The fd rides in ancillary data.
        Send = 1,
        /// The selection was replaced by another client; destroy our source.
        Cancelled = 2,
        /// `DnD` drop performed (unused here).
        DndDropPerformed = 3,
        /// `DnD` finished (unused here).
        DndFinished = 4,
        /// Action offered (unused here).
        Action = 5,
    }
}
requests! {
    /// `wl_data_offer` requests — data another client offers us (our paste).
    DataOfferRequest {
        /// Accept a MIME type (drag; for clipboard we can send a null serial):
        /// `serial`, `mime_type`.
        Accept = 0,
        /// Ask the source to transfer: `mime_type`, `fd` — we read from `fd`.
        Receive = 1,
        /// Destroy the offer.
        Destroy = 2,
        /// Finish a `DnD` offer (unused here).
        Finish = 3,
        /// Set actions (unused here).
        SetActions = 4,
    }
}
events! {
    /// `wl_data_offer` events.
    DataOfferEvent {
        /// The source advertises one MIME type it can provide: `mime_type`.
        Offer = 0,
        /// Source actions (v3+; unused).
        SourceActions = 1,
        /// Offered action (v3+; unused).
        Action = 2,
    }
}

/// A `wl_data_device_manager.dnd_action` bit: what a drag-and-drop gesture
/// will do to the dragged data.
///
/// # Why a drag must name one
///
/// The compositor matches the source's action mask against the
/// destination's and delivers the intersection. A source that offers no
/// action intersects to nothing, and a compositor then withholds
/// `wl_data_device.drop` from a v3 destination entirely: the target paints
/// its "drop here" affordance from `enter`, then receives nothing when the
/// button is released. So the mask is not an optional refinement -- without
/// it a drag is silently undeliverable to every modern toolkit.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum DndAction {
    None = 0,
    Copy = 1,
    Move = 2,
    Ask = 4,
}

impl DndAction {
    /// The `wl_data_device_manager` version that added `set_actions`,
    /// `accept`'s binding result, and `finish`. Below it none of those
    /// requests exist and sending one is a protocol error.
    pub const SINCE_VERSION: u32 = 3;

    /// This action as a one-bit mask, for building a mask up.
    #[must_use]
    pub const fn mask(self) -> u32 {
        self as u32
    }

    /// Whether `bitmask` includes this action. [`Self::None`] is the empty
    /// bit pattern, so it is never "in" any mask.
    #[must_use]
    pub const fn in_mask(self, bitmask: u32) -> bool {
        bitmask & (self as u32) != 0
    }

    /// Decode the single action a compositor selected (`wl_data_offer.action`
    /// / `wl_data_source.action`). The protocol guarantees at most one bit,
    /// so anything else decodes to [`Self::None`] rather than guessing.
    #[must_use]
    pub const fn from_selected(bitmask: u32) -> Self {
        match bitmask {
            1 => Self::Copy,
            2 => Self::Move,
            4 => Self::Ask,
            _ => Self::None,
        }
    }
}

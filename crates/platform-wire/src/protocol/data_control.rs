//! `zwlr_data_control_unstable_v1`: clipboard access without an input
//! serial, so a keybind-driven tool can set the selection without mapping a
//! surface first.

use super::Interface;

pub const ZWLR_DATA_CONTROL_MANAGER_V1: Interface = Interface {
    name: "zwlr_data_control_manager_v1",
    version: 2,
};

requests! {
    /// `zwlr_data_control_manager_v1` requests.
    DataControlManagerRequest {
        CreateSource = 0,
        GetDevice = 1,
        Destroy = 2,
    }
}

requests! {
    /// `zwlr_data_control_device_v1` requests.
    DataControlDeviceRequest {
        SetSelection = 0,
        Destroy = 1,
    }
}

events! {
    /// `zwlr_data_control_device_v1` events.
    DataControlDeviceEvent {
        DataOffer = 0,
        Selection = 1,
        Finished = 2,
        PrimarySelection = 3,
    }
}

requests! {
    /// `zwlr_data_control_offer_v1` requests.
    DataControlOfferRequest {
        Receive = 0,
        Destroy = 1,
    }
}

events! {
    /// `zwlr_data_control_offer_v1` events.
    DataControlOfferEvent {
        Offer = 0,
    }
}

requests! {
    /// `zwlr_data_control_source_v1` requests.
    DataControlSourceRequest {
        Offer = 0,
        Destroy = 1,
    }
}

events! {
    /// `zwlr_data_control_source_v1` events.
    DataControlSourceEvent {
        Send = 0,
        Cancelled = 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Opcode;

    #[test]
    fn data_control_opcodes_match_the_protocol_xml() {
        assert_eq!(DataControlManagerRequest::CreateSource.opcode(), 0);
        assert_eq!(DataControlManagerRequest::GetDevice.opcode(), 1);
        assert_eq!(DataControlManagerRequest::Destroy.opcode(), 2);
        assert_eq!(DataControlDeviceRequest::SetSelection.opcode(), 0);
        assert_eq!(DataControlDeviceRequest::Destroy.opcode(), 1);
        assert_eq!(
            DataControlDeviceEvent::from_opcode(0),
            Some(DataControlDeviceEvent::DataOffer)
        );
        assert_eq!(
            DataControlDeviceEvent::from_opcode(1),
            Some(DataControlDeviceEvent::Selection)
        );
        assert_eq!(DataControlOfferRequest::Receive.opcode(), 0);
        assert_eq!(DataControlOfferRequest::Destroy.opcode(), 1);
        assert_eq!(
            DataControlOfferEvent::from_opcode(0),
            Some(DataControlOfferEvent::Offer)
        );
        assert_eq!(DataControlSourceRequest::Offer.opcode(), 0);
        assert_eq!(DataControlSourceRequest::Destroy.opcode(), 1);
        assert_eq!(
            DataControlSourceEvent::from_opcode(0),
            Some(DataControlSourceEvent::Send)
        );
        assert_eq!(
            DataControlSourceEvent::from_opcode(1),
            Some(DataControlSourceEvent::Cancelled)
        );
    }
}

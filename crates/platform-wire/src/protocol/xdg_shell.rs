use super::Interface;

pub const XDG_WM_BASE: Interface = Interface {
    name: "xdg_wm_base",
    version: 1,
};

requests! {
    /// `xdg_wm_base` requests — the shell that turns surfaces into windows.
    WmBaseRequest {
        CreatePositioner = 1,
        GetXdgSurface = 2,
        Pong = 3,
    }
}
events! {
    /// `xdg_wm_base` events.
    WmBaseEvent {
        /// Liveness check; we must `Pong` with the same serial or get killed.
        Ping = 0,
    }
}

requests! {
    /// `xdg_surface` requests — bridges a `wl_surface` to xdg-shell roles.
    XdgSurfaceRequest {
        Destroy = 0,
        GetToplevel = 1,
        /// `id` (`new_id`), `parent` (nullable `xdg_surface`), `positioner`.
        /// A layer-shell parent passes null here and attaches via
        /// `zwlr_layer_surface_v1.get_popup` before the first commit.
        GetPopup = 2,
        AckConfigure = 4,
    }
}
events! {
    /// `xdg_surface` events.
    XdgSurfaceEvent {
        /// The compositor proposes a configuration; ack it, then commit.
        Configure = 0,
    }
}

requests! {
    /// `xdg_positioner` requests — placement rules consumed by `get_popup`.
    PositionerRequest {
        Destroy = 0,
        /// `width`, `height` of the popup's window geometry.
        SetSize = 1,
        /// `x`, `y`, `width`, `height` of the rectangle on the parent the
        /// popup is placed against.
        SetAnchorRect = 2,
        /// A [`PositionerAnchor`].
        SetAnchor = 3,
        /// A [`PositionerGravity`].
        SetGravity = 4,
        /// An OR of [`ConstraintAdjustment`] bits.
        SetConstraintAdjustment = 5,
        /// `x`, `y` added to the computed position.
        SetOffset = 6,
    }
}

/// `xdg_positioner.anchor`: which point of the anchor rect the popup
/// attaches to.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum PositionerAnchor {
    None = 0,
    Top = 1,
    Bottom = 2,
    Left = 3,
    Right = 4,
    TopLeft = 5,
    BottomLeft = 6,
    TopRight = 7,
    BottomRight = 8,
}

/// `xdg_positioner.gravity`: the direction the popup extends from the
/// anchor point.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum PositionerGravity {
    None = 0,
    Top = 1,
    Bottom = 2,
    Left = 3,
    Right = 4,
    TopLeft = 5,
    BottomLeft = 6,
    TopRight = 7,
    BottomRight = 8,
}

/// `xdg_positioner.constraint_adjustment` bits, combined with bitwise or.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum ConstraintAdjustment {
    SlideX = 1,
    SlideY = 2,
    FlipX = 4,
    FlipY = 8,
    ResizeX = 16,
    ResizeY = 32,
}

impl ConstraintAdjustment {
    #[must_use]
    pub fn bits(adjustments: &[Self]) -> u32 {
        adjustments.iter().fold(0u32, |acc, a| acc | (*a as u32))
    }
}

requests! {
    /// `xdg_popup` requests.
    PopupRequest {
        Destroy = 0,
        /// `seat`, `serial` of the input event that opened the popup. The
        /// compositor then sends `popup_done` on any click outside it.
        Grab = 1,
    }
}
events! {
    /// `xdg_popup` events.
    PopupEvent {
        /// `x`, `y`, `width`, `height` relative to the parent.
        Configure = 0,
        /// The popup was dismissed (outside click, grab lost, ...).
        PopupDone = 1,
    }
}

requests! {
    /// `xdg_toplevel` requests — a top-level application window.
    ToplevelRequest {
        Destroy = 0,
        SetTitle = 2,
        /// Stable, machine-facing identifier (`app_id`) the compositor uses for
        /// window rules / matching — distinct from the human-facing title,
        /// which apps overwrite at runtime via OSC.
        SetAppId = 3,
    }
}
events! {
    /// `xdg_toplevel` events.
    ToplevelEvent {
        /// Suggested new size and states.
        Configure = 0,
        /// The user (or compositor) asked the window to close.
        Close = 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Opcode;

    #[test]
    fn popup_opcodes_match_the_protocol_xml() {
        assert_eq!(WmBaseRequest::CreatePositioner.opcode(), 1);
        assert_eq!(XdgSurfaceRequest::GetPopup.opcode(), 2);
        assert_eq!(PositionerRequest::SetSize.opcode(), 1);
        assert_eq!(PositionerRequest::SetAnchorRect.opcode(), 2);
        assert_eq!(PositionerRequest::SetAnchor.opcode(), 3);
        assert_eq!(PositionerRequest::SetGravity.opcode(), 4);
        assert_eq!(PositionerRequest::SetConstraintAdjustment.opcode(), 5);
        assert_eq!(PositionerRequest::SetOffset.opcode(), 6);
        assert_eq!(PopupRequest::Destroy.opcode(), 0);
        assert_eq!(PopupRequest::Grab.opcode(), 1);
        assert_eq!(PopupEvent::from_opcode(0), Some(PopupEvent::Configure));
        assert_eq!(PopupEvent::from_opcode(1), Some(PopupEvent::PopupDone));
    }

    #[test]
    fn positioner_enums_match_the_protocol_xml() {
        assert_eq!(PositionerAnchor::BottomLeft as u32, 6);
        assert_eq!(PositionerGravity::BottomRight as u32, 8);
        assert_eq!(
            ConstraintAdjustment::bits(&[
                ConstraintAdjustment::SlideX,
                ConstraintAdjustment::FlipY
            ]),
            9
        );
    }
}

//! `zwlr_layer_shell_v1`: the shell status bars, panels and wallpapers use
//! instead of xdg-shell. A layer surface anchors to screen edges, can reserve
//! an exclusive zone, and stacks in one of four fixed compositor layers.

use super::Interface;

pub const ZWLR_LAYER_SHELL_V1: Interface = Interface {
    name: "zwlr_layer_shell_v1",
    version: 4,
};

requests! {
    /// `zwlr_layer_shell_v1` requests — the factory for layer surfaces.
    LayerShellRequest {
        /// Turn a plain `wl_surface` into a layer surface: `id` (`new_id`),
        /// `surface`, `output` (nullable -- 0 lets the compositor pick),
        /// `layer` (see [`Layer`]), `namespace` (a string identifying this
        /// shell client, e.g. "statusbar").
        GetLayerSurface = 0,
        /// Unbind the factory (existing layer surfaces are unaffected).
        Destroy = 1,
    }
}

/// `zwlr_layer_shell_v1` stacking layer (argument to `get_layer_surface`).
///
/// Bottom-to-top: `Background` sits behind normal windows, `Overlay` sits
/// above everything including lock screens on some compositors.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Layer {
    Background = 0,
    Bottom = 1,
    Top = 2,
    Overlay = 3,
}

requests! {
    /// `zwlr_layer_surface_v1` requests.
    LayerSurfaceRequest {
        /// Suggest a size (0 on an axis defers to the anchor -- e.g. a bar
        /// anchored `Left|Right|Top` with `set_size(0, h)` fills the output's
        /// width and only fixes the height).
        SetSize = 0,
        /// Which edge(s) to anchor to -- an OR of [`Anchor`] bits. Anchoring
        /// to two opposite edges (e.g. `Left|Right`) stretches that axis to
        /// fill the output.
        SetAnchor = 1,
        /// Reserve a strip of `zone` pixels (measured from the anchored
        /// edge) that the compositor keeps other windows' maximized/tiled
        /// geometry clear of. `-1` means "don't reserve any space, but also
        /// don't let other windows overlap us either" (rare); `0` reserves
        /// nothing.
        SetExclusiveZone = 2,
        /// Offset the surface inward from its anchored edge(s): `top`,
        /// `right`, `bottom`, `left`, in pixels.
        SetMargin = 3,
        /// Whether/how this layer surface can receive keyboard focus.
        SetKeyboardInteractivity = 4,
        /// Parent an `xdg_popup` (created with a null parent) to this layer
        /// surface: `popup`. Must precede the popup surface's first commit.
        GetPopup = 5,
        /// Ack a `configure` event by its serial, same contract as
        /// `xdg_surface.ack_configure`.
        AckConfigure = 6,
        Destroy = 7,
        /// Change stacking layer after creation (v2+; unused -- we set it
        /// once at `get_layer_surface` time).
        SetLayer = 8,
    }
}
events! {
    /// `zwlr_layer_surface_v1` events.
    LayerSurfaceEvent {
        /// The compositor proposes `width`/`height` (0 on an axis we didn't
        /// fix ourselves means "you decide", but in practice it echoes back
        /// the anchored/filled dimension) -- ack it, draw, then commit.
        Configure = 0,
        /// The compositor destroyed this layer surface (output removed,
        /// shell client asked it to close, ...); tear down and exit.
        Closed = 1,
    }
}

/// `zwlr_layer_surface_v1` anchor bits (argument to `set_anchor`), `ORed` together.
///
/// Two opposite edges anchored on an axis stretches that axis to fill the
/// output -- a full-width top bar anchors `Top | Left | Right`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Anchor {
    Top = 1,
    Bottom = 2,
    Left = 4,
    Right = 8,
}

impl Anchor {
    /// OR several anchor edges into one bitmask, e.g.
    /// `Anchor::bits(&[Anchor::Top, Anchor::Left, Anchor::Right])` for a
    /// full-width top bar.
    #[must_use]
    pub fn bits(edges: &[Self]) -> u32 {
        edges.iter().fold(0u32, |acc, e| acc | (*e as u32))
    }
}

/// `zwlr_layer_surface_v1` keyboard-interactivity modes (argument to
/// `set_keyboard_interactivity`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum KeyboardInteractivity {
    /// Never receives keyboard focus -- the right choice for a status bar.
    None = 0,
    /// Exclusively grabs the keyboard while mapped (menus/launchers).
    Exclusive = 1,
    /// Can be given focus by the compositor on demand (click-to-focus).
    OnDemand = 2,
}

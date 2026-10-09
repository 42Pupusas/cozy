//! The Wayland objects we speak to, as typed enums.
//!
//! Each interface gets a `#[repr(u16)]` enum for the requests we send and
//! one for the events we receive; the discriminant is the wire opcode (the
//! 0-based declaration index in the protocol XML). Only the messages we use
//! are transcribed. One submodule per protocol extension; everything is
//! re-exported flat so callers keep writing `protocol::SurfaceRequest`.
#![allow(dead_code)]

/// A request enum whose discriminant is its wire opcode. Implemented for every
/// `*Request` enum so [`MessageBuilder::new`](crate::wire::MessageBuilder::new)
/// can accept any of them directly.
pub trait Opcode: Copy {
    fn opcode(self) -> u16;
}

/// An event enum that can be decoded from a wire opcode. Implemented for every
/// `*Event` enum so a generic decoder (e.g. `Event::event::<E>()`) works for any
/// of them.
pub trait FromOpcode: Sized {
    fn from_opcode(op: u16) -> Option<Self>;
}

macro_rules! requests {
    ($(#[$m:meta])* $name:ident {
        $($(#[$vm:meta])* $variant:ident = $op:expr),+ $(,)?
    }) => {
        $(#[$m])*
        #[derive(Copy, Clone, Debug, PartialEq, Eq)]
        #[repr(u16)]
        pub enum $name {
            $($(#[$vm])* $variant = $op),+
        }
        impl $crate::protocol::Opcode for $name {
            fn opcode(self) -> u16 {
                self as u16
            }
        }
    };
}

macro_rules! events {
    ($(#[$m:meta])* $name:ident {
        $($(#[$vm:meta])* $variant:ident = $op:expr),+ $(,)?
    }) => {
        $(#[$m])*
        #[derive(Copy, Clone, Debug, PartialEq, Eq)]
        #[repr(u16)]
        pub enum $name {
            $($(#[$vm])* $variant = $op),+
        }
        impl $name {
            /// Decode a wire opcode, or `None` if it isn't one we recognise.
            pub const fn from_opcode(op: u16) -> Option<Self> {
                match op {
                    $($op => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
        impl $crate::protocol::FromOpcode for $name {
            fn from_opcode(op: u16) -> Option<Self> {
                Self::from_opcode(op)
            }
        }
    };
}

mod activation;
mod compositor;
mod data_control;
mod data_device;
mod decoration;
mod display;
mod dmabuf;
mod interface;
mod key;
mod keyboard;
mod layer_shell;
mod output;
mod pointer;
mod screencopy;
mod seat;
mod shm;
mod single_pixel_buffer;
mod subcompositor;
mod viewporter;
mod xdg_shell;

pub use activation::*;
pub use compositor::*;
pub use data_control::*;
pub use data_device::*;
pub use decoration::*;
pub use display::*;
pub use dmabuf::*;
pub use interface::Interface;
pub use key::Key;
pub use keyboard::*;
pub use layer_shell::*;
pub use output::*;
pub use pointer::*;
pub use screencopy::*;
pub use seat::*;
pub use shm::*;
pub use single_pixel_buffer::*;
pub use subcompositor::*;
pub use viewporter::*;
pub use xdg_shell::*;

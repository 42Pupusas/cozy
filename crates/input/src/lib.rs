//! VT / Kitty keyboard and mouse input encoding.
//!
//! This crate is the **only** place that converts platform key/mouse events
//! into the byte sequences sent down the PTY master fd.  Nothing here does
//! I/O; it just produces `&[u8]` slices (or writes into a caller-supplied
//! buffer) so the event loop can forward them to the PTY.
//!
//! ## Keyboard encoding modes
//!
//! The terminal grid tracks which mode is active (negotiated via XTMODKEYS /
//! Kitty progressive enhancement CSIs); the input crate encodes accordingly.
//!
//! | Mode                  | Negotiation CSI          | Notes                        |
//! |-----------------------|--------------------------|------------------------------|
//! | `Legacy`              | (default)                | Classic VT220 sequences       |
//! | `XtermModifyOtherKeys`| `CSI > 4 ; 2 m`         | Disambiguate modified keys    |
//! | `Kitty`               | `CSI = 1 u`             | Full progressive protocol     |
//!
//! ## Mouse encoding modes
//!
//! | Mode       | Enabled by    | Bytes / event            |
//! |------------|---------------|--------------------------|
//! | `Off`      | (default)     | —                        |
//! | `X10`      | `CSI ? 9 h`   | Press only, 3 bytes      |
//! | `Normal`   | `CSI ? 1000 h`| Press+release, 3 bytes   |
//! | `Sgr`      | `CSI ? 1006 h`| Press+release, variable  |

#![warn(clippy::pedantic)]
#![warn(clippy::nursery)]
#![warn(clippy::perf)]
#![warn(clippy::style)]
#![allow(
    clippy::missing_docs_in_private_items,
    clippy::doc_markdown,
    clippy::module_name_repetitions
)]

pub mod key;
pub mod mouse;
pub mod encode;

pub use key::{Key, KeyState, Modifiers};
pub use mouse::{MouseButton, MouseEvent, MouseMode};
pub use encode::{KeyboardMode, Encoder};

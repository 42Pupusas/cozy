//! `screencopy` — capturing screen contents via `zwlr_screencopy_unstable_v1`,
//! the protocol `grim` uses and the one a screenshot bin needs.
//!
//! Core Wayland gives clients no way to read back other clients' pixels, by
//! design, so capture is necessarily a privileged compositor extension. This
//! crate owns that exchange and nothing else: it does not decide *what* to
//! capture (that's the caller's region/output policy) and does not encode
//! anything (that's `ixiptla-core`).
//!
//! The split within the crate follows this workspace's usual "isolable core,
//! thin compositor shell" rule (`crates/wallpaper`, `crates/launcher`):
//!
//! * [`FrameParams`] and [`CapturedFrame`] are pure byte/geometry logic —
//!   validating what the compositor demands, then normalising the pixels it
//!   returns. Both are fully unit-tested with no compositor.
//! * [`FrameHandshake`] is the protocol conversation as a pure state machine,
//!   deliberately holding no object ids and no socket. The nasty cases — a v1
//!   compositor that never sends `buffer_done`, a missing `flags` event, a
//!   dmabuf-only offer — are exactly where capture clients hang or produce
//!   upside-down images, and keeping them here makes each one an ordinary
//!   unit test rather than something reproducible only on the right machine.
//! * [`ScreencopySession`] is the thin shell that owns object ids and shm
//!   lifetime, and does little more than route wire events into the pieces
//!   above.

mod capture_error;
mod capture_region;
mod captured_frame;
mod frame_handshake;
mod frame_params;
mod frame_view;
mod held_frame;
mod pixel_window;
mod screencopy_session;

pub use capture_error::{Result, ScreencopyError};
pub use capture_region::CaptureRegion;
pub use captured_frame::{CaptureError, CapturedFrame};
pub use frame_handshake::{FrameHandshake, HandshakeError, Step};
pub use frame_params::{FrameParams, ParamsError};
pub use frame_view::FrameView;
pub use held_frame::HeldFrame;
pub use pixel_window::PixelWindow;
pub use screencopy_session::ScreencopySession;

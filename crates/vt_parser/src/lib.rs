//! VT/ANSI escape sequence parser.
//!
//! A zero-allocation state machine that consumes raw bytes from the PTY
//! master fd and emits typed [`Event`]s. Runs on its own thread, consuming
//! from an SPSC ring (raw [`ByteChunk`]s from `crates/pty`) and pushing
//! parsed events onto a second SPSC ring towards `crates/grid`.
//!
//! # State machine
//!
//! Follows the DEC VT / xterm state machine documented at
//! <https://vt100.net/emu/dec_ansi_parser> (Paul Williams' model):
//!
//! ```text
//! Ground → Escape → EscapeIntermediate
//!        → CsiEntry → CsiParam → CsiIntermediate → CsiIgnore
//!        → OscString
//!        → DcsEntry → DcsParam → DcsIntermediate → DcsPassthrough → DcsIgnore
//!        → SosPmApcString
//! ```
//!
//! All allocations happen at startup (SPSC ring). The hot path is entirely
//! allocation-free.

mod event;
mod params;
mod state;

pub use event::{CsiAction, C0, Event, OscCmd, SgrAttr};
pub use params::Params;
pub use state::Parser;

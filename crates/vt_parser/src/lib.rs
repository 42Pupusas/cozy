//! VT/ANSI escape sequence parser.
//!
//! A zero-allocation state machine that consumes raw bytes from the PTY
//! master fd through either typed [`TermEvent`] callbacks ([`Parser::feed`])
//! or a direct [`TerminalSink`] ([`Parser::feed_to`]).
//! Runs on its own thread, consuming raw [`ByteChunk`]s from `crates/pty`
//! over an SPSC ring; each `TermEvent` is applied straight to `crates/grid`
//! inside the callback rather than being queued, since it may borrow from
//! the parser's own internal buffers (OSC text) and can't outlive the call.
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
mod scan;
mod sink;
mod state;

pub use event::{C0, CsiAction, DcsCmd, OscCmd, SgrAttr, SgrAttrs, TermEvent};
pub use params::Params;
use sink::EventCallback;
pub use sink::TerminalSink;
pub use state::Parser;

//! A minimal D-Bus client, purpose-built for polling a couple of system
//! services (`NetworkManager`, bluez) from the statusbar rather than shelling
//! out to `iw`/`bluetoothctl` and scraping their text output.
//!
//! No codegen, no introspection, no signal subscriptions — just enough of
//! the wire protocol (SASL `EXTERNAL` auth, method calls, reply parsing) to
//! make blocking `Get`/`GetAll`/method-call round trips over an `AF_UNIX`
//! socket, with every socket op going through `io_uring` via `ququmatz` (see
//! [`conn`]'s module docs for why, and how that compares to
//! `platform::conn`'s fuller two-ring design).
//!
//! Layout:
//! * [`value`] — the D-Bus type system: [`value::Value`] plus the marshaller/
//!   unmarshaller, generic over any signature (recursive descent, not a
//!   fixed set of hand-coded shapes).
//! * [`message`] — wire-frame (header + body) building and parsing.
//! * [`sys`] — the unavoidable libc sliver (socket creation, `sockaddr_un`,
//!   `geteuid`).
//! * [`session_address`] — [`session_address::SessionAddress`], resolving
//!   where the session bus socket actually lives.
//! * [`conn`] — [`conn::DbusConn`], the actual connection: connect, auth, call.
//! * [`network_manager`] / [`bluez`] / [`logind`] — thin, service-specific
//!   convenience wrappers the statusbar calls directly.

pub mod bluez;
pub mod conn;
pub mod logind;
pub mod message;
pub mod network_manager;
pub mod session_address;
pub mod sys;
pub mod value;

pub use conn::{DbusConn, DbusError};
pub use session_address::SessionAddress;
pub use value::Value;

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
//! * [`marshal`] — [`marshal::Marshaller`], the generic encode-side mirror
//!   of [`value::Unmarshaller`] — needed once a caller must return an
//!   arbitrary signature (e.g. portal replies) rather than the handful of
//!   shapes `message::MessageBuilder` hand-writes for our own outbound calls.
//! * [`fds`] — unix descriptor passing: `SCM_RIGHTS` over the bus socket,
//!   the arrival-ordered staging every `h` index refers into, and the
//!   `AGREE_UNIX_FD` negotiation flag.
//! * [`sys`] — the unavoidable libc sliver (socket creation, `sockaddr_un`,
//!   `geteuid`).
//! * [`session_address`] — [`session_address::SessionAddress`], resolving
//!   where the session bus socket actually lives.
//! * [`conn`] — [`conn::DbusConn`], the actual connection: connect, auth, call.
//! * [`signals`] — typed decode-side wrappers for signal bodies shared by
//!   more than one caller (currently [`signals::NameOwnerChanged`]), so the
//!   positional `Value` layout of a given signal is decoded in one place
//!   rather than re-derived by every subscriber. Callers still own their own
//!   `add_match`/dispatch loop — this crate has no subscription machinery of
//!   its own, matching the "no signal subscriptions" scope above.
//! * [`network_manager`] / [`bluez`] / [`logind`] — thin, service-specific
//!   convenience wrappers the statusbar calls directly.

pub mod bluetooth;
pub mod bluez;
pub mod call_flags;
pub mod call_target;
pub mod conn;
pub mod fds;
pub mod logind;
pub mod marshal;
pub mod message;
pub mod mount_options;
pub mod network_manager;
pub mod nm_wifi;
pub mod session_address;
pub mod signals;
pub mod sys;
pub mod systemd1;
pub mod unit_name;
pub mod value;

pub use call_flags::CallFlags;
pub use conn::{DbusConn, DbusError};
pub use mount_options::MountOptions;
pub use session_address::SessionAddress;
pub use signals::NameOwnerChanged;
pub use systemd1::{MountError, TransientMount, TransientUnmount};
pub use unit_name::UnitName;
pub use value::Value;

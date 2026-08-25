//! A polkit authentication agent.
//!
//! # Why this exists
//!
//! Mounting a removable disk goes through a privileged service, and polkit
//! guards that call with `auth_admin_keep`: the answer is "ask the user for a
//! password", not "no". Somebody must present that prompt. On a desktop that
//! somebody is the authentication agent, and this machine has none installed
//! -- only `pkttyagent`, which needs a terminal.
//!
//! # No setuid helper
//!
//! The documented way to run the PAM conversation is polkit's setuid binary,
//! `/usr/lib/polkit-1/polkit-agent-helper-1`. Modern polkit also listens on
//! `/run/polkit/agent-helper.socket`, which speaks the same protocol over an
//! ordinary unix socket. This crate uses the socket, so it spawns nothing,
//! links no PAM, and never holds privilege of its own.
//!
//! # What it never does
//!
//! It does not store, log, or copy a password anywhere. A secret goes from
//! the caller into the socket and the buffer holding it is wiped.

pub mod agent_identity;
pub mod agent_registration;
pub mod auth_request;
#[cfg(test)]
mod fake_helper;
pub mod helper_line;
pub mod helper_session;

pub use agent_identity::AgentIdentity;
pub use agent_registration::AgentRegistration;
pub use auth_request::AuthRequest;
pub use helper_line::HelperLine;
pub use helper_session::{AuthOutcome, HelperSession, SessionError};

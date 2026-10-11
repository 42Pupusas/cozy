//! The volume statusbar panel: model, `pactl` client, worker, input
//! translation, paint pass, and the [`VolumePanel`] the bar hosts.

pub mod events;
pub mod input;
pub mod model;
pub mod pactl;
pub mod panel;
pub mod pending_edits;
pub mod view;
pub mod worker;

pub use panel::VolumePanel;

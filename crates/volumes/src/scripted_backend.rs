//! [`ScriptedBackend`] -- a mounter that records calls and returns answers
//! chosen in advance.
//!
//! # Why this is not behind `#[cfg(test)]`
//!
//! The interesting tests for mounting are not in this crate. They are in the
//! file browser, which must show a password prompt, retry, and update its
//! sidebar. That code cannot be tested against a real mounter: it would need
//! a bus, a polkit agent, a spare USB stick and a human to type a password.
//!
//! So the fake is public API. A test-only fake would force every downstream
//! crate to write its own, and those copies would drift from the trait.
//!
//! # Why a queue of outcomes
//!
//! The sequence that matters most is `NeedsAuthorization` **then** `Mounted`
//! -- the user is prompted, types a password, and the retry succeeds. A fake
//! returning one fixed value cannot express it, so the outcomes are a queue
//! and the last one repeats once the queue is empty.

use crate::mount_backend::{MountBackend, MountOutcome};
use crate::mount_request::MountRequest;
use std::path::{Path, PathBuf};

/// A mounter with scripted answers and a record of what it was asked.
#[derive(Debug)]
pub struct ScriptedBackend {
    /// Outcomes to return, in order.
    outcomes: Vec<MountOutcome>,
    /// How many mounts have been attempted.
    attempts: usize,
    /// Every request received, in order.
    requests: Vec<MountRequest>,
    /// Every mount point an unmount was asked for.
    unmounted: Vec<PathBuf>,
    /// What [`MountBackend::is_available`] reports.
    available: bool,
}

impl ScriptedBackend {
    /// A backend that answers with `outcomes`, then repeats the last one.
    #[must_use]
    pub const fn new(outcomes: Vec<MountOutcome>) -> Self {
        Self {
            outcomes,
            attempts: 0,
            requests: Vec::new(),
            unmounted: Vec::new(),
            available: true,
        }
    }

    /// A backend that always succeeds.
    #[must_use]
    pub fn always_succeeding() -> Self {
        Self::new(vec![MountOutcome::Mounted])
    }

    /// A backend that asks for a password once, then succeeds.
    ///
    /// The sequence a real first mount produces.
    #[must_use]
    pub fn prompting_once() -> Self {
        Self::new(vec![MountOutcome::NeedsAuthorization, MountOutcome::Mounted])
    }

    /// A backend that reports itself unusable, as on a machine with no
    /// system mounter.
    #[must_use]
    pub fn unavailable() -> Self {
        Self {
            available: false,
            ..Self::new(vec![MountOutcome::Unavailable(String::from("no mounter"))])
        }
    }

    /// How many times a mount was attempted.
    #[must_use]
    pub const fn attempts(&self) -> usize {
        self.attempts
    }

    /// The requests received, in order.
    #[must_use]
    pub fn requests(&self) -> &[MountRequest] {
        &self.requests
    }

    /// The most recent request, if any.
    #[must_use]
    pub fn last_request(&self) -> Option<&MountRequest> {
        self.requests.last()
    }

    /// The mount points an unmount was requested for.
    #[must_use]
    pub fn unmounted(&self) -> &[PathBuf] {
        &self.unmounted
    }

    /// The answer for the current attempt.
    ///
    /// The last outcome repeats rather than the queue running dry, so a test
    /// that retries more than it scripted gets a consistent answer instead of
    /// a panic in unrelated code.
    fn next_outcome(&self) -> MountOutcome {
        let index = self.attempts.min(self.outcomes.len().saturating_sub(1));
        self.outcomes
            .get(index)
            .cloned()
            .unwrap_or(MountOutcome::Mounted)
    }
}

impl MountBackend for ScriptedBackend {
    fn mount(&mut self, request: &MountRequest) -> MountOutcome {
        let outcome = self.next_outcome();
        self.requests.push(request.clone());
        self.attempts += 1;
        outcome
    }

    fn unmount(&mut self, mount_point: &Path) -> MountOutcome {
        self.unmounted.push(mount_point.to_path_buf());
        MountOutcome::Mounted
    }

    fn is_available(&self) -> bool {
        self.available
    }
}

#[cfg(test)]
mod tests {
    use super::ScriptedBackend;
    use crate::mount_backend::{MountBackend, MountOutcome};
    use crate::mount_point::MountPoint;
    use crate::mount_request::MountRequest;
    use std::path::Path;

    fn request() -> MountRequest {
        let point = MountPoint::under(Path::new("/run/media/bob"), Some("K"), "/dev/sdb1")
            .expect("an absolute root");
        MountRequest::new(
            String::from("/dev/sdb1"),
            String::from("vfat"),
            point,
            1000,
            1000,
        )
    }

    /// The sequence this fake exists for: prompt, then succeed on retry.
    #[test]
    fn a_prompting_backend_succeeds_on_the_second_attempt() {
        let mut backend = ScriptedBackend::prompting_once();
        let first = backend.mount(&request());
        assert!(first.is_retryable(), "the first attempt must ask");
        assert!(!first.is_mounted());
        let second = backend.mount(&request());
        assert!(second.is_mounted(), "the retry must succeed");
        assert_eq!(backend.attempts(), 2);
    }

    /// A test that retries more than scripted must get a stable answer
    /// rather than a panic from an exhausted queue.
    #[test]
    fn the_last_outcome_repeats_once_the_script_runs_out() {
        let mut backend = ScriptedBackend::new(vec![MountOutcome::Denied]);
        for _ in 0..5 {
            assert_eq!(backend.mount(&request()), MountOutcome::Denied);
        }
        assert_eq!(backend.attempts(), 5);
    }

    /// The request must be recorded exactly, since the caller's whole job is
    /// building it correctly from a device.
    #[test]
    fn every_request_is_recorded_in_order() {
        let mut backend = ScriptedBackend::always_succeeding();
        backend.mount(&request());
        backend.mount(&request().without_write_access());
        assert_eq!(backend.requests().len(), 2);
        assert!(!backend.requests()[0].read_only());
        assert!(backend.last_request().expect("a request").read_only());
    }

    #[test]
    fn an_unavailable_backend_reports_itself_unusable() {
        let backend = ScriptedBackend::unavailable();
        assert!(!backend.is_available());
    }

    #[test]
    fn unmount_records_the_mount_point() {
        let mut backend = ScriptedBackend::always_succeeding();
        backend.unmount(Path::new("/run/media/bob/K"));
        assert_eq!(backend.unmounted(), [Path::new("/run/media/bob/K")]);
    }

    /// A fresh backend must have recorded nothing, or a test asserting "no
    /// mount was attempted" would pass vacuously.
    #[test]
    fn a_fresh_backend_has_recorded_nothing() {
        let backend = ScriptedBackend::always_succeeding();
        assert_eq!(backend.attempts(), 0);
        assert!(backend.requests().is_empty());
        assert!(backend.last_request().is_none());
        assert!(backend.unmounted().is_empty());
    }
}

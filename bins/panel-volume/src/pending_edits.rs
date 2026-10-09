//! Local audio changes that outrank an in-flight snapshot.
//!
//! `pactl` mutations are fire-and-forget subprocesses and a full snapshot
//! costs six more (~35 ms measured). A snapshot already running when the
//! user drags a slider therefore reports the pre-change value, and
//! applying it verbatim would snap the slider backwards. Each edit is
//! held here and re-asserted over arriving snapshots until the server has
//! had time to confirm it.

use std::time::{Duration, Instant};

use crate::model::VolumeTarget;
use crate::pactl::MixerSnapshot;

/// How long an edit outranks incoming snapshots: long enough to cover a
/// snapshot round trip and the mutation that raced it, short enough that
/// a change made elsewhere still shows up promptly.
const HOLD: Duration = Duration::from_millis(400);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Edit {
    Volume(u32),
    Mute(bool),
}

#[derive(Debug, Default)]
pub struct PendingEdits {
    entries: Vec<(VolumeTarget, Edit, Instant)>,
}

impl PendingEdits {
    /// Record a volume the user asked for.
    pub fn volume(&mut self, target: &VolumeTarget, percent: u32) {
        self.record(target, Edit::Volume(percent));
    }

    /// Record a mute state the user asked for.
    pub fn mute(&mut self, target: &VolumeTarget, muted: bool) {
        self.record(target, Edit::Mute(muted));
    }

    fn record(&mut self, target: &VolumeTarget, edit: Edit) {
        let now = Instant::now();
        if let Some(slot) = self.entries.iter_mut().find(|(t, e, _)| {
            t == target && std::mem::discriminant(e) == std::mem::discriminant(&edit)
        }) {
            *slot = (target.clone(), edit, now);
        } else {
            self.entries.push((target.clone(), edit, now));
        }
    }

    /// Drop expired edits and re-assert the live ones over `snapshot`.
    pub fn reapply(&mut self, snapshot: &mut MixerSnapshot) {
        let now = Instant::now();
        self.entries
            .retain(|(_, _, at)| now.duration_since(*at) < HOLD);
        for (target, edit, _) in &self.entries {
            match *edit {
                Edit::Volume(percent) => snapshot.set_volume(target, percent),
                Edit::Mute(muted) => snapshot.set_mute(target, muted),
            }
        }
    }

    #[cfg(test)]
    const fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pactl::{PactlDevice, PactlStream};

    fn snapshot() -> MixerSnapshot {
        MixerSnapshot {
            sinks: vec![PactlDevice::fixture("sink-a", "Speakers", 60)],
            sources: Vec::new(),
            sink_inputs: vec![PactlStream::stream_fixture(7, "Music", 75)],
            source_outputs: Vec::new(),
            default_sink: "sink-a".into(),
            default_source: String::new(),
        }
    }

    fn sink() -> VolumeTarget {
        VolumeTarget::Sink("sink-a".into())
    }

    #[test]
    fn a_fresh_edit_outranks_a_snapshot_that_predates_it() {
        let mut pending = PendingEdits::default();
        pending.volume(&sink(), 25);
        let mut snap = snapshot();
        pending.reapply(&mut snap);
        assert_eq!(snap.sinks[0].volume_percent(), 25);
    }

    #[test]
    fn an_untouched_target_keeps_whatever_the_server_reported() {
        let mut pending = PendingEdits::default();
        pending.volume(&sink(), 25);
        let mut snap = snapshot();
        pending.reapply(&mut snap);
        assert_eq!(snap.sink_inputs[0].volume_percent(), 75);
    }

    #[test]
    fn the_newest_edit_for_a_target_replaces_the_previous_one() {
        let mut pending = PendingEdits::default();
        pending.volume(&sink(), 25);
        pending.volume(&sink(), 30);
        assert_eq!(pending.len(), 1);
        let mut snap = snapshot();
        pending.reapply(&mut snap);
        assert_eq!(snap.sinks[0].volume_percent(), 30);
    }

    #[test]
    fn a_volume_and_a_mute_on_one_target_are_held_independently() {
        let mut pending = PendingEdits::default();
        pending.volume(&sink(), 25);
        pending.mute(&sink(), true);
        assert_eq!(pending.len(), 2);
        let mut snap = snapshot();
        pending.reapply(&mut snap);
        assert_eq!(snap.sinks[0].volume_percent(), 25);
        assert!(snap.sinks[0].mute);
    }

    #[test]
    fn an_expired_edit_stops_overriding_the_server() {
        let mut pending = PendingEdits::default();
        pending.volume(&sink(), 25);
        pending.entries[0].2 = Instant::now()
            .checked_sub(HOLD + Duration::from_millis(1))
            .expect("a time before now");
        let mut snap = snapshot();
        pending.reapply(&mut snap);
        assert_eq!(snap.sinks[0].volume_percent(), 60);
        assert_eq!(pending.len(), 0);
    }

    #[test]
    fn an_edit_naming_a_vanished_target_is_harmless() {
        let mut pending = PendingEdits::default();
        pending.volume(&VolumeTarget::SinkInput(999), 10);
        let mut snap = snapshot();
        pending.reapply(&mut snap);
        assert_eq!(snap.sink_inputs[0].volume_percent(), 75);
    }
}

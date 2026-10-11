//! Volume panel worker thread.
//!
//! Sleeps in `poll(2)` on two descriptors: the `pactl subscribe` pipe,
//! readable when the audio server changes something, and the command
//! eventfd, readable when the UI sends an action or drops its sender. A
//! mutation needs no follow-up read of its own: the server announces the
//! change once applied, and that announcement triggers the snapshot.

use std::thread::JoinHandle;

use panel_ui::CommandInbox;
use quetzalcoatl::mpsc::Producer;
use waker::{Poller, Waker};

use crate::events::MixerEvents;
use crate::model::{VolumeCommand, VolumeTarget};
use crate::pactl::{MixerSnapshot, Pactl};

/// `poll(2)` timeout meaning "wait until a descriptor is ready".
const BLOCK_FOREVER: i32 = -1;

/// `poll(2)` ignores negative descriptors, which stands in for a missing
/// event stream.
const NO_DESCRIPTOR: i32 = -1;

pub struct VolumeWorker {
    snapshots: Producer<MixerSnapshot>,
    inbox: CommandInbox<VolumeCommand>,
    wake: Waker,
    events: Option<MixerEvents>,
}

impl VolumeWorker {
    /// Spawn the worker. Dropping the matching command sender and joining
    /// the returned handle shuts it down and reaps `pactl subscribe`.
    ///
    /// # Panics
    /// Panics if the thread cannot be spawned.
    #[must_use]
    pub fn spawn(
        snapshot_tx: Producer<MixerSnapshot>,
        inbox: CommandInbox<VolumeCommand>,
        wake: Waker,
    ) -> JoinHandle<()> {
        std::thread::Builder::new()
            .name("panel-volume-worker".into())
            .spawn(move || {
                let events = MixerEvents::spawn()
                    .map_err(|e| eprintln!("panel-volume: cannot watch the audio server: {e}"))
                    .ok();
                Self {
                    snapshots: snapshot_tx,
                    inbox,
                    wake,
                    events,
                }
                .run();
            })
            .expect("spawn panel-volume worker")
    }

    fn run(mut self) {
        if !self.publish() {
            return;
        }
        loop {
            self.sleep_until_work();
            let batch = self.inbox.take();
            for command in batch.commands {
                Self::execute(command);
            }
            if batch.closed {
                return;
            }
            if self.absorb_events() && !self.publish() {
                return;
            }
        }
    }

    fn sleep_until_work(&self) {
        let events_fd = self
            .events
            .as_ref()
            .map_or(NO_DESCRIPTOR, MixerEvents::raw_fd);
        let _ = Poller::two(events_fd, self.inbox.raw_fd()).poll_two(BLOCK_FOREVER);
    }

    /// Read the event stream; returns whether the panel's picture is out
    /// of date. A stream that ended is dropped, since its descriptor would
    /// otherwise report readable forever.
    fn absorb_events(&mut self) -> bool {
        let Some(events) = self.events.as_mut() else {
            return false;
        };
        if !Poller::one(events.raw_fd()).poll(0) {
            return false;
        }
        let absorbed = events.absorb();
        if absorbed.closed {
            eprintln!("panel-volume: audio server event stream ended");
            self.events = None;
        }
        absorbed.changed || absorbed.closed
    }

    /// Send a fresh snapshot; returns false once the UI is gone.
    fn publish(&self) -> bool {
        if self.snapshots.push_block(Pactl::snapshot()).is_err() {
            return false;
        }
        self.wake.signal();
        true
    }

    fn execute(command: VolumeCommand) {
        match command {
            VolumeCommand::SetVolume { target, percent } => match target {
                VolumeTarget::Sink(name) => Pactl::set_sink_volume(&name, percent),
                VolumeTarget::Source(name) => Pactl::set_source_volume(&name, percent),
                VolumeTarget::SinkInput(index) => Pactl::set_sink_input_volume(index, percent),
                VolumeTarget::SourceOutput(index) => {
                    Pactl::set_source_output_volume(index, percent);
                }
            },
            VolumeCommand::ToggleMute { target } => match target {
                VolumeTarget::Sink(name) => Pactl::toggle_sink_mute(&name),
                VolumeTarget::Source(name) => Pactl::toggle_source_mute(&name),
                VolumeTarget::SinkInput(index) => Pactl::toggle_sink_input_mute(index),
                VolumeTarget::SourceOutput(index) => Pactl::toggle_source_output_mute(index),
            },
            VolumeCommand::SetDefaultSink(name) => Pactl::set_default_sink(&name),
            VolumeCommand::SetDefaultSource(name) => Pactl::set_default_source(&name),
        }
    }
}

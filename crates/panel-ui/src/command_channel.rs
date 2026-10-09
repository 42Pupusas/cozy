//! The channel carrying user actions from a panel's UI thread to its worker.
//!
//! A worker that sleeps in `poll(2)` on a socket cannot be woken by a ring
//! push. Each send therefore also signals an eventfd the worker polls
//! alongside the socket.

use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc::{Consumer, Producer, RingBuffer};
use waker::Waker;

/// Signals the worker when it is dropped.
///
/// Declared after the producer in [`CommandSender`], so the ring is already
/// closed when the signal fires and the worker's next read sees the closure.
struct CloseSignal(Waker);

impl Drop for CloseSignal {
    fn drop(&mut self) {
        self.0.signal();
    }
}

/// The UI thread's end: sends a command and wakes the worker.
pub struct CommandSender<C> {
    tx: Producer<C>,
    wake: Waker,
    _closed: CloseSignal,
}

/// The worker's end.
pub struct CommandInbox<C> {
    rx: Consumer<C>,
    wake: Waker,
}

/// Everything that arrived since the last [`CommandInbox::take`].
pub struct CommandBatch<C> {
    pub commands: Vec<C>,
    /// The UI thread dropped its end; no further command can arrive.
    pub closed: bool,
}

/// Constructor for a connected [`CommandSender`] / [`CommandInbox`] pair.
pub struct CommandChannel;

impl CommandChannel {
    const SLOTS: usize = 64;

    /// A connected sender and inbox sharing one eventfd.
    ///
    /// # Panics
    /// Panics if the eventfd cannot be created.
    #[must_use]
    pub fn open<C>() -> (CommandSender<C>, CommandInbox<C>) {
        let (tx, rx) = RingBuffer::new(Capacity::exact(Self::SLOTS)).split();
        let wake = Waker::new().expect("create command waker");
        (
            CommandSender {
                tx,
                wake: wake.clone(),
                _closed: CloseSignal(wake.clone()),
            },
            CommandInbox { rx, wake },
        )
    }
}

impl<C> CommandSender<C> {
    /// Queue `command` and wake the worker. Never blocks the UI thread: a
    /// command that finds the ring full is dropped and `false` is returned.
    pub fn send(&self, command: C) -> bool {
        let queued = self.tx.push(command).is_ok();
        self.wake.signal();
        queued
    }
}

impl<C> CommandInbox<C> {
    /// The eventfd to poll for pending commands.
    #[must_use]
    pub fn raw_fd(&self) -> i32 {
        self.wake.raw_fd()
    }

    /// Collect every queued command and re-arm the eventfd.
    ///
    /// The eventfd is drained before the ring is read: a send landing in
    /// between leaves the counter set, so the next poll returns at once.
    /// Closure is observed before the drain, so every command pushed ahead
    /// of the last producer dropping is still delivered.
    #[must_use]
    pub fn take(&mut self) -> CommandBatch<C> {
        self.wake.drain();
        let closed = self.rx.is_closed();
        let mut commands = Vec::new();
        self.rx.drain(|command| commands.push(command));
        CommandBatch { commands, closed }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_send_makes_the_inbox_pollable() {
        let (commands, inbox) = CommandChannel::open::<u8>();
        assert!(!waker::Poller::one(inbox.raw_fd()).poll(0));

        commands.send(1);

        assert!(waker::Poller::one(inbox.raw_fd()).poll(0));
    }

    #[test]
    fn taking_delivers_in_order_and_quiets_the_fd() {
        let (commands, mut inbox) = CommandChannel::open::<u8>();
        commands.send(1);
        commands.send(2);

        let batch = inbox.take();

        assert_eq!(batch.commands, [1, 2]);
        assert!(!batch.closed);
        assert!(!waker::Poller::one(inbox.raw_fd()).poll(0));
    }

    #[test]
    fn dropping_the_sender_wakes_the_worker_and_reports_closure() {
        let (commands, mut inbox) = CommandChannel::open::<u8>();
        drop(commands);

        assert!(waker::Poller::one(inbox.raw_fd()).poll(0));
        assert!(inbox.take().closed);
    }

    #[test]
    fn commands_sent_before_the_drop_are_still_delivered() {
        let (commands, mut inbox) = CommandChannel::open::<u8>();
        commands.send(7);
        drop(commands);

        let batch = inbox.take();

        assert_eq!(batch.commands, [7]);
        assert!(batch.closed);
    }

    #[test]
    fn a_full_ring_refuses_instead_of_blocking() {
        let (commands, mut inbox) = CommandChannel::open::<usize>();
        for n in 0..CommandChannel::SLOTS {
            assert!(commands.send(n));
        }

        assert!(!commands.send(CommandChannel::SLOTS));
        assert_eq!(inbox.take().commands.len(), CommandChannel::SLOTS);
        assert!(commands.send(0));
    }
}

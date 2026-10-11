//! A panel's two channels to its worker thread, plus the thread itself.
//!
//! Dropping tears them down in the one order that cannot hang: the update
//! ring first, so a worker blocked pushing into a full ring is released;
//! then the command sender, which tells the worker to stop; then the join,
//! so the worker's own cleanup (stopping discovery, reaping a subprocess)
//! finishes before the panel is gone.

use std::thread::JoinHandle;

use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc::{Consumer, Producer, RingBuffer};

use crate::command_channel::{CommandChannel, CommandInbox, CommandSender};

pub struct WorkerLink<U, C> {
    updates: Option<Consumer<U>>,
    commands: Option<CommandSender<C>>,
    thread: Option<JoinHandle<()>>,
}

impl<U, C> WorkerLink<U, C> {
    const UPDATE_SLOTS: usize = 16;

    /// Build both channels and hand the worker ends to `spawn`.
    pub fn start(
        spawn: impl FnOnce(Producer<U>, CommandInbox<C>) -> Option<JoinHandle<()>>,
    ) -> Self {
        let (update_tx, updates) = RingBuffer::new(Capacity::exact(Self::UPDATE_SLOTS)).split();
        let (commands, inbox) = CommandChannel::open();
        let thread = spawn(update_tx, inbox);
        Self {
            updates: Some(updates),
            commands: Some(commands),
            thread,
        }
    }

    pub fn send(&self, command: C) {
        if let Some(commands) = &self.commands {
            commands.send(command);
        }
    }

    /// Pass every queued update to `apply`; `true` when there was any.
    pub fn drain(&mut self, mut apply: impl FnMut(U)) -> bool {
        let Some(updates) = &mut self.updates else {
            return false;
        };
        let mut any = false;
        while let Some(update) = updates.pop() {
            apply(update);
            any = true;
        }
        any
    }
}

impl<U, C> Drop for WorkerLink<U, C> {
    fn drop(&mut self) {
        drop(self.updates.take());
        drop(self.commands.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn updates_drain_in_order() {
        let mut link = WorkerLink::<u8, ()>::start(|tx, _inbox| {
            tx.push(1).ok();
            tx.push(2).ok();
            None
        });
        let mut seen = Vec::new();
        assert!(link.drain(|u| seen.push(u)));
        assert_eq!(seen, [1, 2]);
        assert!(!link.drain(|_| {}));
    }

    #[test]
    fn dropping_stops_a_worker_waiting_on_commands() {
        let link = WorkerLink::<(), u8>::start(|_tx, mut inbox| {
            Some(std::thread::spawn(move || {
                loop {
                    let _ = waker::Poller::one(inbox.raw_fd()).poll(-1);
                    if inbox.take().closed {
                        return;
                    }
                }
            }))
        });
        drop(link);
    }

    #[test]
    fn dropping_releases_a_worker_blocked_on_a_full_ring() {
        let link = WorkerLink::<u32, ()>::start(|tx, _inbox| {
            Some(std::thread::spawn(move || {
                for n in 0.. {
                    if tx.push_block(n).is_err() {
                        return;
                    }
                }
            }))
        });
        std::thread::sleep(std::time::Duration::from_millis(20));
        drop(link);
    }

    #[test]
    fn commands_reach_the_worker() {
        let (seen_tx, seen_rx) = std::sync::mpsc::channel();
        let link = WorkerLink::<(), u8>::start(|_tx, mut inbox| {
            Some(std::thread::spawn(move || {
                loop {
                    let _ = waker::Poller::one(inbox.raw_fd()).poll(-1);
                    let batch = inbox.take();
                    for c in batch.commands {
                        seen_tx.send(c).ok();
                    }
                    if batch.closed {
                        return;
                    }
                }
            }))
        });
        link.send(7);
        drop(link);
        assert_eq!(seen_rx.try_iter().collect::<Vec<_>>(), [7]);
    }
}

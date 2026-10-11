//! Calls accepted by a deferred handler and waiting for their answer.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use dbus::value::Value;

use crate::call_context::CallContext;

/// A call whose reply has not been sent yet.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingCall {
    pub context: CallContext,
    pub args: Vec<Value>,
}

/// A shared first-in first-out list of [`PendingCall`]s.
///
/// A deferred handler runs inside the dispatcher and cannot hold the
/// connection, so it pushes here and the loop that owns the connection pops.
#[derive(Debug, Clone, Default)]
pub struct CallQueue {
    calls: Arc<Mutex<VecDeque<PendingCall>>>,
}

impl CallQueue {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&self, call: PendingCall) {
        self.lock().push_back(call);
    }

    #[must_use]
    pub fn pop(&self) -> Option<PendingCall> {
        self.lock().pop_front()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<PendingCall>> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(serial: u32) -> PendingCall {
        PendingCall {
            context: CallContext::new("/p", ":1.1", serial),
            args: vec![Value::U32(serial)],
        }
    }

    #[test]
    fn calls_come_out_in_arrival_order() {
        let queue = CallQueue::new();
        queue.push(call(1));
        queue.push(call(2));
        assert_eq!(queue.pop().expect("first").context.serial(), 1);
        assert_eq!(queue.pop().expect("second").context.serial(), 2);
        assert_eq!(queue.pop(), None);
    }

    #[test]
    fn clones_share_one_list() {
        let queue = CallQueue::new();
        let producer = queue.clone();
        producer.push(call(7));
        assert_eq!(queue.len(), 1);
        assert!(!queue.is_empty());
    }

    #[test]
    fn a_new_queue_is_empty() {
        assert!(CallQueue::new().is_empty());
    }

    #[test]
    fn a_producer_on_another_thread_is_seen() {
        let queue = CallQueue::new();
        let producer = queue.clone();
        std::thread::spawn(move || producer.push(call(3)))
            .join()
            .expect("thread");
        assert_eq!(queue.pop().expect("call").context.serial(), 3);
    }
}

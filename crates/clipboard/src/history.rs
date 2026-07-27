//! Bounded clipboard-history policy, independent of Wayland and storage.

use crate::ClipboardPayload;

/// One captured clipboard state and its stable history identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryEntry {
    id: u64,
    payload: ClipboardPayload,
}

impl HistoryEntry {
    /// Create an entry with an externally assigned identity.
    #[must_use]
    pub const fn new(id: u64, payload: ClipboardPayload) -> Self {
        Self { id, payload }
    }

    /// The persistent identity of this entry.
    #[must_use]
    pub const fn id(&self) -> u64 {
        self.id
    }

    /// The complete MIME payload owned by this entry.
    #[must_use]
    pub const fn payload(&self) -> &ClipboardPayload {
        &self.payload
    }
}

/// In-memory ordering and retention policy for clipboard history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipboardHistory {
    capacity: usize,
    next_id: u64,
    entries: Vec<HistoryEntry>,
}

impl ClipboardHistory {
    /// Create an empty history retaining at most `capacity` entries.
    #[must_use]
    pub const fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity,
            next_id: 0,
            entries: Vec::new(),
        }
    }

    /// The maximum number of retained entries.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Add a payload as the newest entry, returning its identity.
    ///
    /// Empty payloads are ignored because they cannot be pasted. An identical
    /// payload is promoted instead of duplicated: history should describe
    /// useful user actions, not every compositor notification.
    pub fn push(&mut self, payload: ClipboardPayload) -> Option<u64> {
        if self.capacity == 0 || payload.is_empty() {
            return None;
        }
        if let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.payload == payload)
        {
            let entry = self.entries.remove(index);
            let id = entry.id;
            self.entries.insert(0, entry);
            return Some(id);
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.entries.insert(0, HistoryEntry::new(id, payload));
        self.entries.truncate(self.capacity);
        Some(id)
    }

    /// Entries from newest to oldest.
    #[must_use]
    pub fn entries(&self) -> &[HistoryEntry] {
        &self.entries
    }

    /// Restore one persisted entry in its existing newest-first position.
    pub(crate) fn restore(&mut self, id: u64, payload: ClipboardPayload) {
        if self.capacity == 0 || payload.is_empty() || self.entries.len() >= self.capacity {
            return;
        }
        self.next_id = self.next_id.max(id.wrapping_add(1));
        self.entries.push(HistoryEntry::new(id, payload));
    }

    /// Find an entry by identity without changing its ordering.
    #[must_use]
    pub fn get(&self, id: u64) -> Option<&HistoryEntry> {
        self.entries.iter().find(|entry| entry.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::ClipboardHistory;
    use crate::{ClipboardOffer, ClipboardPayload};

    struct HistoryFixture;

    impl HistoryFixture {
        fn payload(byte: u8) -> ClipboardPayload {
            let mut payload = ClipboardPayload::new();
            payload.offer(ClipboardOffer::new("text/plain;charset=utf-8", vec![byte]));
            payload
        }
    }

    #[test]
    fn newest_entries_are_retained_with_a_bounded_capacity() {
        let mut history = ClipboardHistory::with_capacity(2);
        assert_eq!(history.push(HistoryFixture::payload(1)), Some(0));
        assert_eq!(history.push(HistoryFixture::payload(2)), Some(1));
        assert_eq!(history.push(HistoryFixture::payload(3)), Some(2));
        assert_eq!(history.entries().len(), 2);
        assert_eq!(
            history.entries()[0]
                .payload()
                .requested("text/plain;charset=utf-8"),
            Some(&[3][..])
        );
        assert_eq!(
            history.entries()[1]
                .payload()
                .requested("text/plain;charset=utf-8"),
            Some(&[2][..])
        );
        assert!(history.get(0).is_none());
    }

    #[test]
    fn duplicate_payload_is_promoted_without_duplication() {
        let mut history = ClipboardHistory::with_capacity(3);
        let first = history.push(HistoryFixture::payload(1));
        let second = history.push(HistoryFixture::payload(2));
        assert_eq!(history.push(HistoryFixture::payload(1)), first);
        assert_eq!(
            history
                .entries()
                .iter()
                .map(super::HistoryEntry::id)
                .collect::<Vec<_>>(),
            [first.unwrap(), second.unwrap(),]
        );
    }

    #[test]
    fn zero_capacity_and_empty_payload_are_noops() {
        let mut history = ClipboardHistory::with_capacity(0);
        assert_eq!(history.push(HistoryFixture::payload(1)), None);
        let mut history = ClipboardHistory::with_capacity(1);
        assert_eq!(history.push(ClipboardPayload::new()), None);
        assert!(history.entries().is_empty());
    }
}

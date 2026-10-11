//! Ordering and retention of clipboard history, holding no payload bytes.

use crate::{EntrySummary, PayloadDigest};

/// One history entry as the index sees it: identity, fingerprint, and summary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexEntry {
    id: u64,
    digest: PayloadDigest,
    summary: EntrySummary,
}

impl IndexEntry {
    #[must_use]
    pub const fn new(id: u64, digest: PayloadDigest, summary: EntrySummary) -> Self {
        Self {
            id,
            digest,
            summary,
        }
    }

    #[must_use]
    pub const fn id(&self) -> u64 {
        self.id
    }

    #[must_use]
    pub const fn digest(&self) -> PayloadDigest {
        self.digest
    }

    #[must_use]
    pub const fn summary(&self) -> &EntrySummary {
        &self.summary
    }
}

/// What recording a payload did to the index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Admission {
    /// An identical entry already existed and moved to the front.
    Promoted(u64),
    /// A new entry was added; `evicted` fell off the end.
    Inserted { id: u64, evicted: Vec<u64> },
}

impl Admission {
    #[must_use]
    pub const fn id(&self) -> u64 {
        match self {
            Self::Promoted(id) | Self::Inserted { id, .. } => *id,
        }
    }
}

/// Newest-first history entries bounded by a capacity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntryIndex {
    capacity: usize,
    next_id: u64,
    entries: Vec<IndexEntry>,
}

impl EntryIndex {
    #[must_use]
    pub const fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity,
            next_id: 0,
            entries: Vec::new(),
        }
    }

    pub(crate) fn restore(capacity: usize, next_id: u64, entries: Vec<IndexEntry>) -> Self {
        let next_id = entries
            .iter()
            .map(|entry| entry.id.wrapping_add(1))
            .fold(next_id, u64::max);
        Self {
            capacity,
            next_id,
            entries,
        }
    }

    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    pub(crate) const fn next_id(&self) -> u64 {
        self.next_id
    }

    pub(crate) fn stored(&self) -> &[IndexEntry] {
        &self.entries
    }

    /// Entries from newest to oldest, at most `capacity` of them.
    #[must_use]
    pub fn entries(&self) -> &[IndexEntry] {
        &self.entries[..self.entries.len().min(self.capacity)]
    }

    #[must_use]
    pub fn get(&self, id: u64) -> Option<&IndexEntry> {
        self.entries().iter().find(|entry| entry.id == id)
    }

    /// Add a payload as the newest entry.
    ///
    /// An entry with the same digest for which `is_same` confirms equal
    /// contents is promoted instead of duplicated. Returns `None` when the
    /// capacity is zero.
    pub fn record(
        &mut self,
        digest: PayloadDigest,
        summary: EntrySummary,
        mut is_same: impl FnMut(u64) -> bool,
    ) -> Option<Admission> {
        if self.capacity == 0 {
            return None;
        }
        if let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.digest == digest && is_same(entry.id))
        {
            let entry = self.entries.remove(index);
            let id = entry.id;
            self.entries.insert(0, entry);
            return Some(Admission::Promoted(id));
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.entries.insert(0, IndexEntry::new(id, digest, summary));
        let evicted = self
            .entries
            .drain(self.capacity.min(self.entries.len())..)
            .map(|entry| entry.id)
            .collect();
        Some(Admission::Inserted { id, evicted })
    }

    /// Keep only entries for which `keep` returns true.
    pub fn retain(&mut self, mut keep: impl FnMut(u64) -> bool) {
        self.entries.retain(|entry| keep(entry.id));
    }
}

#[cfg(test)]
mod tests {
    use super::{Admission, EntryIndex};
    use crate::{ClipboardOffer, ClipboardPayload, EntrySummary, PayloadDigest};

    struct IndexFixture;

    impl IndexFixture {
        fn record(index: &mut EntryIndex, byte: u8) -> Option<Admission> {
            let mut payload = ClipboardPayload::new();
            payload.offer(ClipboardOffer::new("text/plain", vec![byte]));
            index.record(
                PayloadDigest::of(&payload),
                EntrySummary::of(&payload),
                |_| true,
            )
        }

        fn ids(index: &EntryIndex) -> Vec<u64> {
            index.entries().iter().map(super::IndexEntry::id).collect()
        }
    }

    #[test]
    fn newest_entries_are_retained_and_evictions_reported() {
        let mut index = EntryIndex::with_capacity(2);
        IndexFixture::record(&mut index, 1);
        IndexFixture::record(&mut index, 2);
        assert_eq!(
            IndexFixture::record(&mut index, 3),
            Some(Admission::Inserted {
                id: 2,
                evicted: vec![0]
            })
        );
        assert_eq!(IndexFixture::ids(&index), [2, 1]);
        assert!(index.get(0).is_none());
    }

    #[test]
    fn duplicate_is_promoted_without_duplication() {
        let mut index = EntryIndex::with_capacity(3);
        IndexFixture::record(&mut index, 1);
        IndexFixture::record(&mut index, 2);
        assert_eq!(
            IndexFixture::record(&mut index, 1),
            Some(Admission::Promoted(0))
        );
        assert_eq!(IndexFixture::ids(&index), [0, 1]);
    }

    #[test]
    fn unconfirmed_digest_match_is_inserted_fresh() {
        let mut index = EntryIndex::with_capacity(3);
        let mut payload = ClipboardPayload::new();
        payload.offer(ClipboardOffer::new("text/plain", vec![1]));
        let digest = PayloadDigest::of(&payload);
        index.record(digest, EntrySummary::of(&payload), |_| true);
        let admission = index.record(digest, EntrySummary::of(&payload), |_| false);
        assert_eq!(admission.map(|a| a.id()), Some(1));
    }

    #[test]
    fn zero_capacity_records_nothing() {
        let mut index = EntryIndex::with_capacity(0);
        assert_eq!(IndexFixture::record(&mut index, 1), None);
        assert!(index.entries().is_empty());
    }

    #[test]
    fn restore_never_reuses_a_stored_id() {
        let mut first = EntryIndex::with_capacity(4);
        IndexFixture::record(&mut first, 1);
        IndexFixture::record(&mut first, 2);
        let mut restored = EntryIndex::restore(4, 0, first.stored().to_vec());
        assert_eq!(
            IndexFixture::record(&mut restored, 3).map(|a| a.id()),
            Some(2)
        );
    }

    #[test]
    fn view_is_capped_when_capacity_shrinks() {
        let mut wide = EntryIndex::with_capacity(4);
        IndexFixture::record(&mut wide, 1);
        IndexFixture::record(&mut wide, 2);
        let narrow = EntryIndex::restore(1, wide.next_id(), wide.stored().to_vec());
        assert_eq!(IndexFixture::ids(&narrow), [1]);
    }
}

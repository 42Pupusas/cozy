//! Bookkeeping for clipboard offers announced before they are selected.
//!
//! Data-control announces an offer object first, then streams its MIME types,
//! and only later names one offer as the selection. Between those points the
//! MIME list has nowhere to live, so it is parked here by object id.
//!
//! The registry is deliberately bounded. A compositor announces an offer for
//! *every* selection change, including the primary selection we never take, so
//! an unbounded map would grow for the whole life of a daemon that runs for
//! weeks.

/// One announced offer and the MIME types advertised for it so far.
#[derive(Clone, Debug, PartialEq, Eq)]
struct AnnouncedOffer {
    id: u32,
    mimes: Vec<String>,
}

/// Tracks announced-but-not-yet-selected clipboard offers by object id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OfferRegistry {
    capacity: usize,
    offers: Vec<AnnouncedOffer>,
}

impl OfferRegistry {
    /// Track at most `capacity` concurrently announced offers.
    #[must_use]
    pub const fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity,
            offers: Vec::new(),
        }
    }

    /// Begin tracking a newly announced offer.
    ///
    /// Re-announcing an id resets it: the compositor is free to reuse an id
    /// once the previous object is destroyed, and carrying stale MIME types
    /// across that boundary would offer types the new owner never advertised.
    pub fn announce(&mut self, id: u32) {
        if self.capacity == 0 {
            return;
        }
        if let Some(existing) = self.offers.iter_mut().find(|offer| offer.id == id) {
            existing.mimes.clear();
            return;
        }
        // Oldest-first eviction: the newest announcement is the one about to
        // become the selection, so dropping the stalest is the safe choice.
        if self.offers.len() >= self.capacity {
            self.offers.remove(0);
        }
        self.offers.push(AnnouncedOffer {
            id,
            mimes: Vec::new(),
        });
    }

    /// Record one MIME type advertised for `id`, ignoring unknown offers.
    pub fn advertise(&mut self, id: u32, mime: impl Into<String>) {
        let Some(offer) = self.offers.iter_mut().find(|offer| offer.id == id) else {
            return;
        };
        let mime = mime.into();
        if !offer.mimes.iter().any(|known| known == &mime) {
            offer.mimes.push(mime);
        }
    }

    /// Whether `id` is an offer we announced.
    ///
    /// This is the only way to tell an offer's events apart from anything
    /// else on the connection: opcodes are per-interface, so
    /// `data_control_offer.offer` is opcode 0 -- and so are `wl_callback.done`
    /// and `wl_seat.capabilities`. Matching on opcode alone would decode an
    /// unrelated numeric event as a string.
    #[must_use]
    pub fn contains(&self, id: u32) -> bool {
        self.offers.iter().any(|offer| offer.id == id)
    }

    /// Remove `id` and return its advertised MIME types.
    pub fn take(&mut self, id: u32) -> Option<Vec<String>> {
        let index = self.offers.iter().position(|offer| offer.id == id)?;
        Some(self.offers.remove(index).mimes)
    }

    /// Stop tracking `id` without reading it, e.g. a primary selection.
    pub fn discard(&mut self, id: u32) {
        self.offers.retain(|offer| offer.id != id);
    }

    /// How many offers are currently tracked.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.offers.len()
    }

    /// Whether nothing is tracked.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.offers.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::OfferRegistry;

    #[test]
    fn mimes_are_parked_until_the_offer_is_selected() {
        let mut registry = OfferRegistry::with_capacity(4);
        registry.announce(7);
        registry.advertise(7, "text/plain");
        registry.advertise(7, "image/png");
        assert_eq!(registry.take(7), Some(vec!["text/plain".into(), "image/png".into()]));
        // Taking consumes it: a second selection of the same id is not a
        // second copy of the same clipboard contents.
        assert_eq!(registry.take(7), None);
    }

    #[test]
    fn advertising_an_unknown_offer_is_ignored() {
        let mut registry = OfferRegistry::with_capacity(4);
        registry.advertise(9, "text/plain");
        assert!(registry.is_empty());
    }

    #[test]
    fn reannounced_id_does_not_inherit_stale_mimes() {
        let mut registry = OfferRegistry::with_capacity(4);
        registry.announce(3);
        registry.advertise(3, "image/png");
        registry.announce(3);
        registry.advertise(3, "text/plain");
        assert_eq!(registry.take(3), Some(vec!["text/plain".into()]));
    }

    #[test]
    fn tracking_is_bounded_so_a_long_lived_daemon_cannot_grow_forever() {
        let mut registry = OfferRegistry::with_capacity(2);
        registry.announce(1);
        registry.announce(2);
        registry.announce(3);
        assert_eq!(registry.len(), 2);
        assert_eq!(registry.take(1), None);
        assert_eq!(registry.take(3), Some(Vec::new()));
    }

    #[test]
    fn only_announced_ids_are_recognised_as_offers() {
        // The guard that keeps an unrelated opcode-0 event -- `wl_callback.done`,
        // `wl_seat.capabilities` -- from being decoded as an offer's MIME string.
        let mut registry = OfferRegistry::with_capacity(2);
        registry.announce(4);
        assert!(registry.contains(4));
        assert!(!registry.contains(5));
    }

    #[test]
    fn discarded_offers_are_forgotten() {
        let mut registry = OfferRegistry::with_capacity(2);
        registry.announce(5);
        registry.discard(5);
        assert!(registry.is_empty());
    }
}

//! Which submitted operations the kernel still owns.
//!
//! Every SQE this crate submits points the kernel at a buffer inside
//! [`Pty`](crate::Pty). The kernel may write into that buffer at any time
//! until the matching CQE appears. Freeing a buffer earlier than that hands
//! the kernel memory the allocator has taken back, which corrupts the heap.
//!
//! Closing the ring fd does **not** make that safe. It starts the kernel's
//! teardown, but the teardown is asynchronous, so an armed read can still
//! land afterwards. The only reliable proof that a buffer is idle is the
//! arrival of its completion.
//!
//! [`InFlight`] records which operations are outstanding, so teardown can
//! cancel exactly those and wait for exactly their completions.

/// One kind of operation this crate submits.
///
/// The discriminants double as `user_data` on the wire, so a CQE identifies
/// the buffer it wrote into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpTag {
    /// Read from the PTY master into `read_buf`.
    Read,
    /// Write to the PTY master from `write_buf`.
    Write,
    /// Read from the `signalfd` into `signal_buf`.
    Signal,
    /// Read from the wake eventfd into `wake_buf`.
    Wake,
}

impl OpTag {
    /// Every tag, for iterating the outstanding set.
    pub const ALL: [Self; 4] = [Self::Read, Self::Write, Self::Signal, Self::Wake];

    /// The `user_data` this tag travels as.
    ///
    /// Never zero: a zeroed CQE must not decode as a real operation.
    #[must_use]
    pub const fn as_user_data(self) -> u64 {
        self.slot() as u64
    }

    /// This tag's position in the outstanding-set bitmap.
    const fn slot(self) -> u8 {
        match self {
            Self::Read => 1,
            Self::Write => 2,
            Self::Signal => 3,
            Self::Wake => 4,
        }
    }

    /// Recover the tag a CQE was submitted with.
    #[must_use]
    pub const fn from_user_data(user_data: u64) -> Option<Self> {
        match user_data {
            1 => Some(Self::Read),
            2 => Some(Self::Write),
            3 => Some(Self::Signal),
            4 => Some(Self::Wake),
            _ => None,
        }
    }

    const fn bit(self) -> u8 {
        1u8 << self.slot()
    }
}

/// The set of operations the kernel has not yet completed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InFlight {
    bits: u8,
}

impl InFlight {
    #[must_use]
    pub const fn new() -> Self {
        Self { bits: 0 }
    }

    /// Record that an SQE for `tag` was submitted.
    pub const fn arm(&mut self, tag: OpTag) {
        self.bits |= tag.bit();
    }

    /// Record that `tag`'s completion arrived.
    pub const fn complete(&mut self, tag: OpTag) {
        self.bits &= !tag.bit();
    }

    #[must_use]
    pub const fn is_armed(self, tag: OpTag) -> bool {
        self.bits & tag.bit() != 0
    }

    /// True when the kernel owns none of our buffers, so they are safe to
    /// free.
    #[must_use]
    pub const fn is_quiet(self) -> bool {
        self.bits == 0
    }

    /// The outstanding tags, for cancelling each one.
    pub fn armed(self) -> impl Iterator<Item = OpTag> {
        OpTag::ALL
            .into_iter()
            .filter(move |tag| self.is_armed(*tag))
    }
}

#[cfg(test)]
mod tests {
    use super::{InFlight, OpTag};

    #[test]
    fn a_fresh_set_owns_nothing() {
        let set = InFlight::new();
        assert!(set.is_quiet());
        assert_eq!(set.armed().count(), 0);
    }

    #[test]
    fn an_armed_tag_reads_back_as_armed() {
        let mut set = InFlight::new();
        set.arm(OpTag::Read);
        assert!(set.is_armed(OpTag::Read));
        assert!(!set.is_quiet());
    }

    #[test]
    fn arming_one_tag_leaves_the_others_alone() {
        let mut set = InFlight::new();
        set.arm(OpTag::Read);
        assert!(!set.is_armed(OpTag::Write));
        assert!(!set.is_armed(OpTag::Signal));
        assert!(!set.is_armed(OpTag::Wake));
    }

    #[test]
    fn completing_the_last_tag_makes_the_set_quiet() {
        let mut set = InFlight::new();
        set.arm(OpTag::Signal);
        set.complete(OpTag::Signal);
        assert!(set.is_quiet());
    }

    /// Teardown waits for `is_quiet`, so an unrelated completion must not
    /// clear a tag that is still outstanding.
    #[test]
    fn completing_one_tag_does_not_clear_another() {
        let mut set = InFlight::new();
        set.arm(OpTag::Read);
        set.arm(OpTag::Wake);
        set.complete(OpTag::Wake);
        assert!(set.is_armed(OpTag::Read));
        assert!(!set.is_quiet());
    }

    #[test]
    fn arming_twice_is_cleared_by_one_completion() {
        let mut set = InFlight::new();
        set.arm(OpTag::Write);
        set.arm(OpTag::Write);
        set.complete(OpTag::Write);
        assert!(set.is_quiet());
    }

    #[test]
    fn completing_an_unarmed_tag_is_harmless() {
        let mut set = InFlight::new();
        set.complete(OpTag::Read);
        assert!(set.is_quiet());
    }

    #[test]
    fn armed_lists_every_outstanding_tag() {
        let mut set = InFlight::new();
        set.arm(OpTag::Read);
        set.arm(OpTag::Signal);
        let armed: Vec<OpTag> = set.armed().collect();
        assert_eq!(armed, vec![OpTag::Read, OpTag::Signal]);
    }

    #[test]
    fn every_tag_can_be_armed_at_once() {
        let mut set = InFlight::new();
        for tag in OpTag::ALL {
            set.arm(tag);
        }
        assert_eq!(set.armed().count(), OpTag::ALL.len());
    }

    #[test]
    fn each_tag_roundtrips_through_its_user_data() {
        for tag in OpTag::ALL {
            assert_eq!(OpTag::from_user_data(tag.as_user_data()), Some(tag));
        }
    }

    #[test]
    fn every_tag_has_a_distinct_user_data() {
        let mut seen = Vec::new();
        for tag in OpTag::ALL {
            assert!(!seen.contains(&tag.as_user_data()));
            seen.push(tag.as_user_data());
        }
    }

    /// The cancel SQEs submitted during teardown carry their own
    /// `user_data`, which must not be mistaken for an operation completing.
    #[test]
    fn an_unknown_user_data_decodes_to_no_tag() {
        assert_eq!(OpTag::from_user_data(0), None);
        assert_eq!(OpTag::from_user_data(u64::MAX), None);
    }
}

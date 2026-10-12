use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc::{Consumer, Producer, RingBuffer};

/// Held by whoever wants a background job to keep going. Dropping it is the
/// cancel signal: the job's [`KeepaliveWatch`] then reports abandonment.
pub struct Keepalive {
    _held: Producer<()>,
}

/// The job's side of a [`Keepalive`], checked without blocking.
pub struct KeepaliveWatch {
    line: Consumer<()>,
}

impl Keepalive {
    const SLOTS: usize = 4;

    #[must_use]
    pub fn pair() -> (Self, KeepaliveWatch) {
        let (held, line) = RingBuffer::new(Capacity::exact(Self::SLOTS)).split();
        (Self { _held: held }, KeepaliveWatch { line })
    }
}

impl KeepaliveWatch {
    #[must_use]
    pub fn is_abandoned(&self) -> bool {
        self.line.is_closed()
    }
}

#[cfg(test)]
mod tests {
    use super::Keepalive;

    #[test]
    fn a_held_keepalive_is_not_abandoned() {
        let (held, watch) = Keepalive::pair();
        assert!(!watch.is_abandoned());
        drop(held);
        assert!(watch.is_abandoned());
    }
}

use std::path::PathBuf;

use fsmodel::tally::SizeTally;
use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc::RingBuffer;

pub struct TallyRun {
    dirs: Vec<PathBuf>,
}

impl TallyRun {
    const WAKE_SLOTS: usize = 64;

    #[must_use]
    pub const fn new(dirs: Vec<PathBuf>) -> Self {
        Self { dirs }
    }

    #[must_use]
    pub fn total(&self) -> u64 {
        let (wake, mut wakes) = RingBuffer::new(Capacity::exact(Self::WAKE_SLOTS)).split();
        let mut tally = SizeTally::start(self.dirs.clone(), move || {
            let _ = wake.push_block(());
        })
        .expect("spawn size tally");
        let mut settled = 0;
        let mut bytes = 0_u64;
        while settled < self.dirs.len() {
            if wakes.pop_block().is_none() {
                break;
            }
            for tallied in tally.drain().into_iter().filter(|tallied| tallied.settled) {
                settled += 1;
                bytes += tallied.bytes;
            }
        }
        bytes
    }
}

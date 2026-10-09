//! [`SizeTally`] -- [`DirWalk`] totals for a batch of directories, measured
//! on a worker thread so a large tree never stalls the UI.
//!
//! Dropping the tally cancels the walk, so navigating away abandons a scan
//! of the directory being left instead of finishing it for nobody.

use crate::walk::DirWalk;
use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc::{Consumer, RingBuffer};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tallied {
    pub path: PathBuf,
    pub bytes: u64,
}

pub struct SizeTally {
    results: Consumer<Tallied>,
    cancel: Arc<AtomicBool>,
}

impl SizeTally {
    const RESULT_SLOTS: usize = 64;

    /// Walk `dirs` in order, calling `notify` after each total is queued so
    /// the owner can wake whatever loop will call [`Self::drain`].
    pub fn start<N>(dirs: Vec<PathBuf>, notify: N) -> std::io::Result<Self>
    where
        N: Fn() + Send + 'static,
    {
        let (sender, results) = RingBuffer::new(Capacity::exact(Self::RESULT_SLOTS)).split();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        std::thread::Builder::new()
            .name("size-tally".to_owned())
            .spawn(move || {
                let walk = DirWalk::new(&flag);
                for path in dirs {
                    let Some(bytes) = walk.total(&path) else {
                        return;
                    };
                    if sender.push_block(Tallied { path, bytes }).is_err() {
                        return;
                    }
                    notify();
                }
            })?;
        Ok(Self { results, cancel })
    }

    /// Every total that has arrived since the last drain. Never blocks.
    #[must_use]
    pub fn drain(&mut self) -> Vec<Tallied> {
        let mut arrived = Vec::new();
        self.results.drain(|tallied| arrived.push(tallied));
        arrived
    }
}

impl Drop for SizeTally {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::{SizeTally, Tallied};
    use crate::test_support::TempDir;
    use quetzalcoatl::capacity::Capacity;
    use quetzalcoatl::mpsc::{Consumer, Producer, RingBuffer};

    fn wake_ring() -> (Producer<()>, Consumer<()>) {
        RingBuffer::new(Capacity::exact(64)).split()
    }

    fn collect(tally: &mut SizeTally, wakes: &mut Consumer<()>, want: usize) -> Vec<Tallied> {
        let mut got = Vec::new();
        while got.len() < want {
            wakes
                .pop_block()
                .expect("the worker signals each total before it exits");
            got.extend(tally.drain());
        }
        got
    }

    #[test]
    fn each_directory_arrives_with_its_total() {
        let dir = TempDir::new("tally_each");
        let a = dir.create_dir("a");
        let b = dir.create_dir("b");
        dir.create_file("a/x", &[0; 3]);
        dir.create_file("b/y/z", &[0; 40]);

        let (wake, mut wakes) = wake_ring();
        let mut tally = SizeTally::start(vec![a.clone(), b.clone()], move || {
            let _ = wake.push_block(());
        })
        .expect("spawn");
        let got = collect(&mut tally, &mut wakes, 2);
        assert_eq!(
            got,
            [
                Tallied { path: a, bytes: 3 },
                Tallied { path: b, bytes: 40 }
            ]
        );
    }

    #[test]
    fn an_empty_batch_finishes_without_signalling() {
        let (wake, mut wakes) = wake_ring();
        let mut tally = SizeTally::start(Vec::new(), move || {
            let _ = wake.push_block(());
        })
        .expect("spawn");
        assert_eq!(
            wakes.pop_block(),
            None,
            "the worker exits without signalling"
        );
        assert!(tally.drain().is_empty());
    }

    #[test]
    fn dropping_the_tally_stops_the_worker() {
        let dir = TempDir::new("tally_drop");
        for i in 0..200 {
            dir.create_file(&format!("d/{i}"), b"x");
        }
        let (wake, mut wakes) = wake_ring();
        let batch = 10_000;
        let tally = SizeTally::start(vec![dir.path().join("d"); batch], move || {
            let _ = wake.push_block(());
        })
        .expect("spawn");
        drop(tally);
        let mut walked = 0;
        while wakes.pop_block().is_some() {
            walked += 1;
        }
        assert!(walked < batch, "the worker walked all {walked} directories");
    }
}

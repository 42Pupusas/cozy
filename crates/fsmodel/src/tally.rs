//! [`SizeTally`] -- [`DirWalk`] totals for a batch of directories, measured
//! on a worker thread so a large tree never stalls the UI.
//!
//! Dropping the tally cancels the walk, so navigating away abandons a scan
//! of the directory being left instead of finishing it for nobody.

use crate::walk::DirWalk;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tallied {
    pub path: PathBuf,
    pub bytes: u64,
}

#[derive(Debug)]
pub struct SizeTally {
    results: mpsc::Receiver<Tallied>,
    cancel: Arc<AtomicBool>,
}

impl SizeTally {
    /// Walk `dirs` in order, calling `notify` after each total is queued so
    /// the owner can wake whatever loop will call [`Self::drain`].
    pub fn start<N>(dirs: Vec<PathBuf>, notify: N) -> std::io::Result<Self>
    where
        N: Fn() + Send + 'static,
    {
        let (sender, results) = mpsc::channel();
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
                    if sender.send(Tallied { path, bytes }).is_err() {
                        return;
                    }
                    notify();
                }
            })?;
        Ok(Self { results, cancel })
    }

    /// Every total that has arrived since the last drain. Never blocks.
    #[must_use]
    pub fn drain(&self) -> Vec<Tallied> {
        self.results.try_iter().collect()
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
    use std::sync::mpsc;
    use std::time::Duration;

    fn collect(tally: &SizeTally, wakes: &mpsc::Receiver<()>, want: usize) -> Vec<Tallied> {
        let mut got = Vec::new();
        while got.len() < want {
            wakes
                .recv_timeout(Duration::from_secs(5))
                .expect("the worker signals each total");
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

        let (wake, wakes) = mpsc::channel();
        let tally = SizeTally::start(vec![a.clone(), b.clone()], move || {
            let _ = wake.send(());
        })
        .expect("spawn");
        let got = collect(&tally, &wakes, 2);
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
        let (wake, wakes) = mpsc::channel();
        let tally = SizeTally::start(Vec::new(), move || {
            let _ = wake.send(());
        })
        .expect("spawn");
        assert!(wakes.recv_timeout(Duration::from_secs(5)).is_err());
        assert!(tally.drain().is_empty());
    }

    #[test]
    fn dropping_the_tally_stops_the_worker() {
        let dir = TempDir::new("tally_drop");
        for i in 0..200 {
            dir.create_file(&format!("d/{i}"), b"x");
        }
        let (wake, wakes) = mpsc::channel();
        let batch = 10_000;
        let tally = SizeTally::start(vec![dir.path().join("d"); batch], move || {
            let _ = wake.send(());
        })
        .expect("spawn");
        drop(tally);
        let mut walked = 0;
        loop {
            match wakes.recv_timeout(Duration::from_secs(30)) {
                Ok(()) => walked += 1,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => panic!("worker never exited"),
            }
        }
        assert!(walked < batch, "the worker walked all {walked} directories");
    }
}

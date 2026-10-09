//! Run the real `TrayWatcher` against the live session bus and print every
//! snapshot it emits -- the end-to-end proof that registrations and later
//! item signals both reach the bar's channel.
//!
//! Run: `cargo run -p statusbar --example tray_watch`

use std::time::Instant;

use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc::RingBuffer;
use statusbar::{TraySnapshot, TrayWatcher};

struct TrayWatch;

impl TrayWatch {
    fn run() {
        let started = Instant::now();
        let (tx, mut rx) = RingBuffer::<TraySnapshot>::new(Capacity::exact(16)).split();
        let wake = waker::Waker::new().expect("waker");
        TrayWatcher::spawn(tx, wake);
        println!(
            "[{:7.3}s] real TrayWatcher spawned",
            started.elapsed().as_secs_f64()
        );
        loop {
            let Some(snapshot) = rx.pop_block() else {
                println!("watcher channel closed");
                return;
            };
            let ids: Vec<(&str, bool)> = snapshot
                .items
                .iter()
                .map(|item| (item.id.as_str(), item.attention))
                .collect();
            println!(
                "[{:7.3}s] snapshot: {} item(s) {ids:?}",
                started.elapsed().as_secs_f64(),
                snapshot.items.len(),
            );
        }
    }
}

fn main() {
    TrayWatch::run();
}

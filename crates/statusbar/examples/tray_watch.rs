//! Run the real `TrayWatcher` against the live session bus and print every
//! snapshot it emits -- the end-to-end proof that registrations and later
//! item signals both reach the bar's channel.
//!
//! Run: `cargo run -p statusbar --example tray_watch`

use std::sync::mpsc;
use std::time::Instant;

use statusbar::{TraySnapshot, TrayWatcher};

struct TrayWatch;

impl TrayWatch {
    fn run() {
        let started = Instant::now();
        let (tx, rx) = mpsc::channel::<TraySnapshot>();
        let wake = waker::Waker::new().expect("waker");
        TrayWatcher::spawn(tx, wake);
        println!("[{:7.3}s] real TrayWatcher spawned", started.elapsed().as_secs_f64());
        loop {
            let Ok(snapshot) = rx.recv() else {
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

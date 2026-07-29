//! Eventfd signal/drain costs, isolated from the waiting path.

use waker::Waker;

#[global_allocator]
static ALLOC: divan::AllocProfiler = divan::AllocProfiler::system();

#[divan::bench]
fn signal_and_drain(bencher: divan::Bencher) {
    let waker = Waker::new().expect("eventfd available");
    bencher.bench(|| {
        waker.signal();
        waker.drain();
        divan::black_box(waker.raw_fd())
    });
}

fn main() {
    divan::main();
}

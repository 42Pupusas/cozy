//! Tiny allocation-free utility paths used by every UI process.

use localtime::LocalTime;

#[global_allocator]
static ALLOC: divan::AllocProfiler = divan::AllocProfiler::system();

#[divan::bench(args = [0_i64, 1_700_000_000_i64, 2_000_000_000_i64])]
fn unix_to_local_timestamp(bencher: divan::Bencher, seconds: i64) {
    bencher.bench(|| divan::black_box(LocalTime::at_unix_seconds(seconds)));
}

#[divan::bench(args = ["#181818", "#d8d8d8", "#89b4fa", "invalid"])]
fn color_parse(bencher: divan::Bencher, value: &str) {
    bencher.bench(|| divan::black_box(config::Color::parse_hex(divan::black_box(value))));
}

fn main() {
    divan::main();
}

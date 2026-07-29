//! Small binary-protocol parsers: representative `DBus` message and address
//! inputs. Wire I/O is excluded so results are reproducible.

use dbus::{SessionAddress, message::Prelude};

#[global_allocator]
static ALLOC: divan::AllocProfiler = divan::AllocProfiler::system();

#[divan::bench]
fn session_address_parse(bencher: divan::Bencher) {
    let address = "unix:path=/run/user/1000/bus,guid=0123456789abcdef";
    bencher.bench(|| divan::black_box(SessionAddress::parse(divan::black_box(address))));
}

#[divan::bench]
fn dbus_invalid_message_rejection(bencher: divan::Bencher) {
    let bytes = [b'l', 1, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0];
    bencher.bench(|| divan::black_box(Prelude::parse(divan::black_box(&bytes))));
}

fn main() {
    divan::main();
}

//! `input::Encoder::encode_key`: VT/Kitty keyboard encoding.
//!
//! Zero-dep, no-heap-alloc by design (writes into a caller buffer) — like
//! `parser.rs`, the `AllocProfiler` here is a tripwire more than a
//! measurement: it should read zero allocations across every mode.
//!
//! Run:
//!   cargo bench -p tty-benches --bench input

use input::{Encoder, Key, KeyState, KeyboardMode, Modifiers};

#[global_allocator]
static ALLOC: divan::AllocProfiler = divan::AllocProfiler::system();

fn main() {
    divan::main();
}

const fn modes() -> &'static [KeyboardMode] {
    &[
        KeyboardMode::Legacy,
        KeyboardMode::XtermModifyOtherKeys,
        KeyboardMode::Kitty,
    ]
}

/// A representative spread of key presses: plain char, ctrl-char, arrow with
/// modifiers, function key, and an editing-cluster key (its own code path
/// split out in `encode.rs`).
const KEYS: &[(Key, Modifiers)] = &[
    (Key::Char('a'), Modifiers::NONE),
    (Key::Char('a'), Modifiers::CTRL),
    (Key::Up, Modifiers::SHIFT),
    (Key::F(5), Modifiers::NONE),
    (Key::Home, Modifiers::ALT),
];

#[divan::bench(args = modes())]
fn encode_key(bencher: divan::Bencher, mode: KeyboardMode) {
    let mut enc = Encoder::new();
    enc.keyboard_mode = mode;
    let mut buf = [0u8; 64];
    bencher.bench_local(|| {
        for (key, mods) in KEYS {
            divan::black_box(enc.encode_key(
                divan::black_box(*key),
                KeyState::Pressed,
                *mods,
                &mut buf,
            ));
        }
    });
}

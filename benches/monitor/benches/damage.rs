//! `monitor::damage::DamageList`: the per-frame set of touched rectangles the
//! painter hands to the platform layer. Its whole design claim is "no
//! per-frame allocation on the hot path" -- it is a fixed-capacity stack
//! array that coarsens to a bounding box instead of spilling to the heap.
//! This bench makes that claim measurable: under `AllocProfiler`, `push`,
//! `extend`, and `to_platform` in the below-capacity regime must report zero
//! allocations. Only `to_platform` (which builds a `Vec` for the platform
//! type) is expected to allocate, and this bench isolates exactly that so the
//! one unavoidable allocation is visible and bounded.
//!
//! Run:
//!   cargo bench -p monitor-benches --bench damage

use monitor::damage::DamageList;
use platform::draw::Rect;

#[global_allocator]
static ALLOC: divan::AllocProfiler = divan::AllocProfiler::system();

fn main() {
    divan::main();
}

/// Push a realistic dashboard frame's worth of regions (four chart edges plus
/// a dozen core-bar columns) into a fresh list. Stays under the coarsening
/// cap, so this must not allocate.
#[divan::bench]
fn push_below_cap(bencher: divan::Bencher) {
    bencher.bench_local(|| {
        let mut list = DamageList::new();
        for i in 0..16 {
            list.push(Rect::new(i * 20, 0, 2, 150));
        }
        divan::black_box(&list);
    });
}

/// Push far past the cap so the list coarsens to a single bounding rect. This
/// exercises the overflow fold; it must also stay allocation-free (the whole
/// point of coarsening instead of growing a heap vector).
#[divan::bench]
fn push_past_cap_coarsens(bencher: divan::Bencher) {
    bencher.bench_local(|| {
        let mut list = DamageList::new();
        for i in 0..256 {
            list.push(Rect::new(i * 4, 0, 2, 150));
        }
        divan::black_box(&list);
    });
}

/// Merge four child widgets' damage into a parent frame list, matching how
/// `Painter::paint` folds each chart/bar group's reported damage together.
#[divan::bench]
fn extend_children(bencher: divan::Bencher) {
    let mut children = Vec::new();
    for c in 0..4 {
        let mut child = DamageList::new();
        child.push(Rect::new(c * 90, 0, 2, 150));
        children.push(child);
    }
    bencher.bench_local(|| {
        let mut frame = DamageList::new();
        for child in &children {
            frame.extend(divan::black_box(child));
        }
        divan::black_box(&frame);
    });
}

/// The one path that legitimately allocates: converting to the platform's
/// `Vec`-backed damage type at the `App::run` seam. Isolated so its single
/// allocation is measured, not hidden inside a larger frame cost.
#[divan::bench]
fn to_platform_allocates(bencher: divan::Bencher) {
    let mut list = DamageList::new();
    for i in 0..16 {
        list.push(Rect::new(i * 20, 0, 2, 150));
    }
    bencher.bench_local(|| {
        let damage = divan::black_box(&list).to_platform();
        divan::black_box(damage);
    });
}

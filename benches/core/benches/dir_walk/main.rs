//! Cost of the file browser's directory walking: wall time, allocations on
//! the bench thread (divan), and allocations on every thread (`all threads`
//! lines printed before the table).

mod every_thread_alloc;
mod ledger_walk;
mod tally_run;
mod tree_fixture;
mod tree_shape;

use std::os::unix::fs::MetadataExt;
use std::sync::atomic::AtomicBool;

use every_thread_alloc::EveryThreadAlloc;
use fsmodel::reader::DirReader;
use fsmodel::walk::{DirContents, DirScan, DirWalk, ScanScratch};
use ledger_walk::LedgerWalk;
use tally_run::TallyRun;
use tree_fixture::TreeFixture;
use tree_shape::TreeShape;

#[global_allocator]
static ALLOC: EveryThreadAlloc = EveryThreadAlloc::new();

#[divan::bench(args = TreeShape::ALL)]
fn walk_total(bencher: divan::Bencher, shape: TreeShape) {
    let fixture = TreeFixture::build(shape);
    let cancel = AtomicBool::new(false);
    let walk = DirWalk::new(&cancel);
    assert_eq!(walk.total(fixture.root()), Some(fixture.expected_bytes()));
    bencher.bench_local(|| divan::black_box(walk.total(divan::black_box(fixture.root()))));
}

#[divan::bench(args = TreeShape::ALL)]
fn ledger_total(bencher: divan::Bencher, shape: TreeShape) {
    let fixture = TreeFixture::build(shape);
    let mut walk = LedgerWalk::new();
    assert_eq!(walk.total(fixture.root()), fixture.expected_bytes());
    bencher.bench_local(|| divan::black_box(walk.total(divan::black_box(fixture.root()))));
}

#[divan::bench(args = TreeShape::ALL)]
fn scan_root_level(bencher: divan::Bencher, shape: TreeShape) {
    let fixture = TreeFixture::build(shape);
    let scan = DirScan::new(std::fs::metadata(fixture.root()).expect("stat").dev());
    let mut scratch = ScanScratch::new();
    let mut contents = DirContents::new();
    bencher.bench_local(|| {
        divan::black_box(
            scan.scan(
                divan::black_box(fixture.root()),
                &mut scratch,
                &mut contents,
            )
            .bytes(),
        )
    });
}

#[divan::bench(args = TreeShape::ALL)]
fn read_listing(bencher: divan::Bencher, shape: TreeShape) {
    let fixture = TreeFixture::build(shape);
    let reader = DirReader::new();
    bencher.bench_local(|| divan::black_box(reader.read(divan::black_box(fixture.root()))));
}

#[divan::bench(args = TreeShape::ALL)]
fn tally_children(bencher: divan::Bencher, shape: TreeShape) {
    let fixture = TreeFixture::build(shape);
    let run = TallyRun::new(fixture.children_or_root());
    assert_eq!(run.total(), fixture.expected_child_bytes());
    bencher.bench_local(|| divan::black_box(run.total()));
}

struct EveryThreadReport;

impl EveryThreadReport {
    const ROUNDS: u64 = 20;

    fn print() {
        for shape in TreeShape::ALL {
            let fixture = TreeFixture::build(shape);
            let run = TallyRun::new(fixture.children_or_root());
            let before = ALLOC.snapshot();
            for _ in 0..Self::ROUNDS {
                divan::black_box(run.total());
            }
            let spent = ALLOC.snapshot().since(before);
            println!(
                "all threads  tally_children/{shape:<6} {:>8} allocs {:>10} B per run",
                spent.allocs / Self::ROUNDS,
                spent.bytes / Self::ROUNDS
            );
        }
    }
}

fn main() {
    EveryThreadReport::print();
    divan::main();
}

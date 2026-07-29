//! Directory-view filtering and sorting over synthetic, stable entries.

use fsmodel::filter::Filter;
use fsmodel::sort::SortPolicy;

#[global_allocator]
static ALLOC: divan::AllocProfiler = divan::AllocProfiler::system();

#[divan::bench(args = ["", "file", "README", "ZZZ"])]
fn filter_query(bencher: divan::Bencher, query: &str) {
    // Filtering is allocation-sensitive: it runs on every search-box keystroke.
    let filter = Filter {
        name_contains: query.to_owned(),
        ..Filter::default()
    };
    bencher.bench(|| divan::black_box(filter.is_inactive()));
}

#[divan::bench(args = ["name", "modified", "size"])]
fn sort_policy_copy(bencher: divan::Bencher, _key: &str) {
    let policy = SortPolicy::default();
    bencher.bench(|| divan::black_box(policy));
}

fn main() {
    divan::main();
}

//! Matching, MIME lookup, and clipboard representation hot paths.

use launcher::fuzzy::FuzzyMatch;
use mime::MimeDb;

#[global_allocator]
static ALLOC: divan::AllocProfiler = divan::AllocProfiler::system();

const APPS: [&str; 8] = [
    "Firefox Web Browser",
    "Foot Terminal",
    "Files",
    "Launcher",
    "Monitor",
    "Screenshot",
    "Status Bar",
    "Wallpaper",
];

#[divan::bench(args = ["fi", "term", "zz", ""])]
fn fuzzy_match(bencher: divan::Bencher, query: &str) {
    bencher.bench(|| {
        let matches: Vec<_> = APPS
            .iter()
            .filter_map(|name| FuzzyMatch::find(query, name))
            .collect();
        divan::black_box(matches)
    });
}

#[divan::bench(args = ["main.rs", "README.md", "image.png", "no-extension"])]
fn mime_name_lookup(bencher: divan::Bencher, name: &str) {
    let db =
        MimeDb::from_globs_data("80:text/x-rust:*.rs\n60:text/markdown:*.md\n50:image/png:*.png\n");
    bencher.bench(|| divan::black_box(db.detect_by_name(divan::black_box(name))));
}

#[divan::bench(args = ["text/plain", "text/html", "image/png", "missing"])]
fn clipboard_offer_lookup(bencher: divan::Bencher, requested: &str) {
    let mut payload = clipboard::ClipboardPayload::new();
    for mime in ["text/plain", "text/html", "image/png", "application/json"] {
        payload.offer(clipboard::ClipboardOffer::new(mime, b"fixture".to_vec()));
    }
    bencher.bench(|| divan::black_box(payload.requested(divan::black_box(requested))));
}

fn main() {
    divan::main();
}

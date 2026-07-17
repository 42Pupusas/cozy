//! `text_layout::Layouter::layout`: segmentation + `BiDi` + line-breaking.
//!
//! Separate from the VT pipeline corpora above (those are byte streams with
//! escape sequences; layout wants plain text paragraphs), so this file uses
//! its own small text fixtures rather than `tty_benches::CORPORA`.
//!
//! Run:
//!   cargo bench -p tty-benches --bench layout

use text_layout::{LayoutParams, Layouter};

#[global_allocator]
static ALLOC: divan::AllocProfiler = divan::AllocProfiler::system();

fn main() {
    divan::main();
}

const ASCII_PARAGRAPH: &str = "The quick brown fox jumps over the lazy dog. \
Pack my box with five dozen liquor jugs. How vexingly quick daft zebras jump. \
The five boxing wizards jump quickly, and then they rest before jumping again. ";

const CJK_PARAGRAPH: &str =
    "国际化测试字符宽度渲染国际化测试字符宽度渲染国际化测试字符宽度渲染国际化测试字符宽度渲染";

const BIDI_PARAGRAPH: &str =
    "Hello שלום world مرحبا text with mixed English, Hebrew, and Arabic runs 123 456.";

const fn fixtures() -> &'static [(&'static str, &'static str)] {
    &[
        ("ascii", ASCII_PARAGRAPH),
        ("cjk", CJK_PARAGRAPH),
        ("bidi", BIDI_PARAGRAPH),
    ]
}

#[divan::bench(args = fixtures())]
fn layout_wrapped(bencher: divan::Bencher, fixture: &(&str, &str)) {
    let (_, text) = *fixture;
    let text = text.repeat(20);
    let params = LayoutParams {
        max_width: 80,
        direction: None,
    };
    bencher.bench(|| Layouter::layout(divan::black_box(&text), params));
}

#[divan::bench(args = fixtures())]
fn layout_unwrapped(bencher: divan::Bencher, fixture: &(&str, &str)) {
    let (_, text) = *fixture;
    let text = text.repeat(20);
    let params = LayoutParams {
        max_width: 0,
        direction: None,
    };
    bencher.bench(|| Layouter::layout(divan::black_box(&text), params));
}

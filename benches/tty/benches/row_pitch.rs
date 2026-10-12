//! Cost of the content-scroll pixel copy under three row layouts:
//!
//! - `uniform`: every row is `cell_h` scanlines; the leftover strip sits
//!   unused at the bottom. One `copy_within` moves the whole band.
//! - `interleaved`: the leftover scanlines are spread evenly, so pitch
//!   alternates between `cell_h` and `cell_h + 1` down the screen.
//! - `block`: the first `leftover` rows get `cell_h + 1`, the rest `cell_h`.
//!
//! Padding sits below each row's glyph scanlines. The `merged` scroll copies
//! runs of rows whose padding matches their destination's with one memmove;
//! the `rowwise` scroll copies every row on its own.
//!
//! Run:
//!   `cargo bench -p tty-benches --bench row_pitch`

use divan::counter::BytesCount;

#[global_allocator]
static ALLOC: divan::AllocProfiler = divan::AllocProfiler::system();

fn main() {
    divan::main();
}

#[derive(Clone, Copy)]
struct Geometry {
    name: &'static str,
    width: usize,
    height: usize,
    cell_h: usize,
}

impl std::fmt::Display for Geometry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name)
    }
}

impl Geometry {
    const ALL: &'static [Self] = &[
        Self {
            name: "1x_1280x800_h18",
            width: 1280,
            height: 800,
            cell_h: 18,
        },
        Self {
            name: "2x_2560x1600_h37",
            width: 2560,
            height: 1600,
            cell_h: 37,
        },
        Self {
            name: "4k_3840x2160_h41",
            width: 3840,
            height: 2160,
            cell_h: 41,
        },
    ];

    const fn rows(self) -> usize {
        self.height / self.cell_h
    }

    const fn leftover(self) -> usize {
        self.height - self.rows() * self.cell_h
    }

    fn pixels(self) -> Vec<u32> {
        (0..self.width * self.height)
            .map(|i| u32::try_from(i % 0xff_ffff).unwrap_or(0))
            .collect()
    }
}

#[derive(Default)]
struct CopyStats {
    runs: usize,
    single_rows: usize,
    longest_run: usize,
}

impl CopyStats {
    const fn memmoves(&self) -> usize {
        self.runs + self.single_rows
    }
}

#[derive(Clone, Copy)]
enum Spread {
    None,
    Interleaved,
    Block,
}

impl Spread {
    fn tops(self, g: Geometry) -> Vec<usize> {
        let rows = g.rows();
        let left = g.leftover();
        (0..=rows)
            .map(|i| {
                i * g.cell_h
                    + match self {
                        Self::None => 0,
                        Self::Interleaved => i * left / rows.max(1),
                        Self::Block => i.min(left),
                    }
            })
            .collect()
    }
}

struct PitchLayout {
    width: usize,
    cell_h: usize,
    tops: Vec<usize>,
}

impl PitchLayout {
    fn new(g: Geometry, spread: Spread) -> Self {
        Self {
            width: g.width,
            cell_h: g.cell_h,
            tops: spread.tops(g),
        }
    }

    const fn rows(&self) -> usize {
        self.tops.len() - 1
    }

    fn pad(&self, row: usize) -> usize {
        self.tops[row + 1] - self.tops[row] - self.cell_h
    }

    fn moved_bytes(&self, delta: usize) -> usize {
        (self.tops[self.rows()] - self.tops[delta]) * self.width * 4
    }

    fn copy_row(&self, pixels: &mut [u32], src_row: usize, dst_row: usize) {
        let w = self.width;
        let glyph = self.cell_h * w;
        let src_off = self.tops[src_row] * w;
        let dst_off = self.tops[dst_row] * w;
        pixels.copy_within(src_off..src_off + glyph, dst_off);
        let pad_start = dst_off + glyph;
        let pad_end = self.tops[dst_row + 1] * w;
        let mut at = pad_start;
        while at < pad_end {
            pixels.copy_within(pad_start - w..pad_start, at);
            at += w;
        }
    }

    fn frame(&self, g: Geometry) -> Vec<u32> {
        let mut px = g.pixels();
        for row in 0..self.rows() {
            self.copy_row(&mut px, row, row);
        }
        px
    }

    fn scroll_up_rowwise(&self, pixels: &mut [u32], delta: usize) {
        for dst in 0..self.rows() - delta {
            self.copy_row(pixels, dst + delta, dst);
        }
    }

    fn scroll_up_merged(&self, pixels: &mut [u32], delta: usize) -> CopyStats {
        let w = self.width;
        let last = self.rows() - delta;
        let mut stats = CopyStats::default();
        let mut dst = 0;
        while dst < last {
            if self.pad(dst) != self.pad(dst + delta) {
                self.copy_row(pixels, dst + delta, dst);
                stats.single_rows += 1;
                dst += 1;
                continue;
            }
            let mut end = dst + 1;
            while end < last && self.pad(end) == self.pad(end + delta) {
                end += 1;
            }
            let src_off = self.tops[dst + delta] * w;
            let src_end = self.tops[end + delta] * w;
            pixels.copy_within(src_off..src_end, self.tops[dst] * w);
            stats.runs += 1;
            stats.longest_run = stats.longest_run.max(end - dst);
            dst = end;
        }
        stats
    }

    fn verify_merged(&self, g: Geometry, delta: usize) -> CopyStats {
        let mut expected = self.frame(g);
        self.scroll_up_rowwise(&mut expected, delta);
        let mut actual = self.frame(g);
        let stats = self.scroll_up_merged(&mut actual, delta);
        assert!(
            actual == expected,
            "merged scroll diverges from rowwise scroll"
        );
        stats
    }
}

struct Delta;

impl Delta {
    const ONE: usize = 0;
    const HALF: usize = 1;
    const HALF_MINUS_ONE: usize = 2;

    const fn rows(kind: usize, g: Geometry) -> usize {
        match kind {
            Self::HALF => g.rows() / 2,
            Self::HALF_MINUS_ONE => g.rows() / 2 - 1,
            _ => 1,
        }
    }
}

struct Case;

impl Case {
    fn rowwise(bencher: divan::Bencher, g: Geometry, spread: Spread, delta: usize) {
        let layout = PitchLayout::new(g, spread);
        let mut px = layout.frame(g);
        bencher
            .counter(BytesCount::new(layout.moved_bytes(delta)))
            .bench_local(|| layout.scroll_up_rowwise(divan::black_box(&mut px), delta));
    }

    fn merged(bencher: divan::Bencher, g: Geometry, spread: Spread, label: &str, delta: usize) {
        let layout = PitchLayout::new(g, spread);
        let stats = layout.verify_merged(g, delta);
        eprintln!(
            "{label:<11} {g} delta={delta:>2}: {:>2} rows -> {:>2} memmoves ({} runs, longest {}, {} single)",
            layout.rows() - delta,
            stats.memmoves(),
            stats.runs,
            stats.longest_run,
            stats.single_rows
        );
        let mut px = layout.frame(g);
        bencher
            .counter(BytesCount::new(layout.moved_bytes(delta)))
            .bench_local(|| layout.scroll_up_merged(divan::black_box(&mut px), delta));
    }
}

#[divan::bench(args = Geometry::ALL, consts = [Delta::ONE, Delta::HALF, Delta::HALF_MINUS_ONE])]
fn uniform<const DELTA: usize>(bencher: divan::Bencher, g: &Geometry) {
    Case::merged(bencher, *g, Spread::None, "uniform", Delta::rows(DELTA, *g));
}

#[divan::bench(args = Geometry::ALL, consts = [Delta::ONE, Delta::HALF, Delta::HALF_MINUS_ONE])]
fn uniform_rowwise<const DELTA: usize>(bencher: divan::Bencher, g: &Geometry) {
    Case::rowwise(bencher, *g, Spread::None, Delta::rows(DELTA, *g));
}

#[divan::bench(args = Geometry::ALL, consts = [Delta::ONE, Delta::HALF, Delta::HALF_MINUS_ONE])]
fn interleaved_rowwise<const DELTA: usize>(bencher: divan::Bencher, g: &Geometry) {
    Case::rowwise(bencher, *g, Spread::Interleaved, Delta::rows(DELTA, *g));
}

#[divan::bench(args = Geometry::ALL, consts = [Delta::ONE, Delta::HALF, Delta::HALF_MINUS_ONE])]
fn interleaved_merged<const DELTA: usize>(bencher: divan::Bencher, g: &Geometry) {
    Case::merged(
        bencher,
        *g,
        Spread::Interleaved,
        "interleaved",
        Delta::rows(DELTA, *g),
    );
}

#[divan::bench(args = Geometry::ALL, consts = [Delta::ONE, Delta::HALF, Delta::HALF_MINUS_ONE])]
fn block_merged<const DELTA: usize>(bencher: divan::Bencher, g: &Geometry) {
    Case::merged(bencher, *g, Spread::Block, "block", Delta::rows(DELTA, *g));
}

//! Headless widget geometry and wallpaper scaling.

use wallpaper::{ScaleMode, WallpaperImage};
use widget::{ListSelection, ListViewport};

#[global_allocator]
static ALLOC: divan::AllocProfiler = divan::AllocProfiler::system();

#[divan::bench(args = [100, 10_000, 100_000])]
fn viewport_visible_rows(bencher: divan::Bencher, len: usize) {
    let viewport = ListViewport::new(24, len, 720);
    bencher.bench(|| divan::black_box(viewport.visible_rows()));
}

#[divan::bench]
fn selection_move(bencher: divan::Bencher) {
    bencher
        .with_inputs(ListSelection::new)
        .bench_local_refs(|selection| divan::black_box(selection.cursor()));
}

#[divan::bench(args = [ScaleMode::Stretch, ScaleMode::Fit, ScaleMode::Fill, ScaleMode::Center, ScaleMode::Tile])]
fn wallpaper_blit(bencher: divan::Bencher, mode: ScaleMode) {
    let image = WallpaperImage {
        width: 320,
        height: 180,
        rgba: vec![0x7f; 320 * 180 * 4],
    };
    let mut output = vec![0u8; 1280 * 720 * 4];
    bencher.bench_local(|| {
        mode.blit_into(&image, 1280, 720, [0, 0, 0, 255], &mut output);
        divan::black_box(output[0])
    });
}

fn main() {
    divan::main();
}

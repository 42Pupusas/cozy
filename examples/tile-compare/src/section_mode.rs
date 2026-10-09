//! [`SectionMode`] -- the scene as solid subsurface sections in a
//! [`SceneWindow`]: no client pixel buffer, only state changes on the wire.

use std::time::{Duration, Instant};

use platform::scene::SceneWindow;
use platform::toplevel::LogicalSize;

use crate::run_report::RunReport;
use crate::tile_scene::TileScene;

pub struct SectionMode;

impl SectionMode {
    pub fn run(size: LogicalSize, tiles: usize, seconds: u64) {
        let wake = waker::Waker::new().expect("create waker");
        let mut window = SceneWindow::new("tile-compare: sections", "tile-compare", size, wake)
            .expect("open window");
        let mut scene = TileScene::new(tiles);
        let mut report = RunReport::begin("sections");
        while report.elapsed() < Duration::from_secs(seconds) && !window.is_closing() {
            window.request_redraw();
            let started = Instant::now();
            match window.tick(|size| scene.frame(size.width, size.height)) {
                Ok(Some(update)) => {
                    report.record_frame(started.elapsed(), update.requests);
                    scene.advance();
                }
                Ok(None) => {}
                Err(e) => {
                    eprintln!("tile-compare: {e}");
                    break;
                }
            }
            window.wait(16);
        }
        let extra = format!(
            "sections        {} surfaces, {} colour buffers",
            window.tree().sections(),
            window.tree().colours()
        );
        let _ = window.close();
        report.print(&extra);
    }
}

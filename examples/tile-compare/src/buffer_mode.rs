//! [`BufferMode`] -- the scene painted into a full-window shm buffer by a
//! [`Toplevel`], the way every window draws today.

use std::time::{Duration, Instant};

use platform::draw::{Canvas, Color};
use platform::toplevel::{LogicalSize, Painter, PhysicalSize, Toplevel};

use crate::run_report::RunReport;
use crate::tile_scene::TileScene;

pub struct BufferMode {
    scene: TileScene,
    paint_time: Duration,
    painted: bool,
}

impl Painter for BufferMode {
    fn paint(&mut self, canvas: &mut Canvas<'_>, size: PhysicalSize) {
        let started = Instant::now();
        for tile in self.scene.frame(size.width, size.height).rects {
            canvas.fill_rect(tile.rect, tile.color);
        }
        self.paint_time = started.elapsed();
        self.painted = true;
    }

    fn background(&self) -> Color {
        TileScene::background()
    }
}

impl BufferMode {
    pub fn run(size: LogicalSize, tiles: usize, seconds: u64) {
        let wake = waker::Waker::new().expect("create waker");
        let mut window = Toplevel::new(
            "tile-compare: buffer",
            "tile-compare",
            size,
            wake,
            TileScene::background(),
        )
        .expect("open window");
        let mut mode = Self {
            scene: TileScene::new(tiles),
            paint_time: Duration::ZERO,
            painted: false,
        };
        let mut report = RunReport::begin("buffer");
        while report.elapsed() < Duration::from_secs(seconds) && !window.is_closing() {
            window.request_redraw();
            let started = Instant::now();
            if window.tick(&mut mode).is_err() {
                break;
            }
            if std::mem::take(&mut mode.painted) {
                report.record_frame(started.elapsed(), 0);
                mode.scene.advance();
            }
            window.wait(16);
        }
        let size = window.size();
        let _ = window.close();
        report.print(&format!(
            "buffer          {}x{} px, 2 slots = {} kB of shm",
            size.width,
            size.height,
            size.byte_len() * 2 / 1024
        ));
    }
}

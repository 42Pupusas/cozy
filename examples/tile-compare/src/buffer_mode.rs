//! [`BufferMode`] -- the scene painted into a full-window shm buffer by a
//! [`Toplevel`], the way every window draws today. With `tracked` damage the
//! compositor is told only the tiles that changed; otherwise the whole
//! surface each frame.

use std::time::{Duration, Instant};

use platform::draw::{Canvas, Color, Damage};
use platform::scene::SolidFrame;
use platform::toplevel::{LogicalSize, Painter, PhysicalSize, Toplevel};

use crate::run_report::RunReport;
use crate::tile_diff::TileDiff;
use crate::tile_scene::TileScene;

pub struct BufferMode {
    scene: TileScene,
    tracked: bool,
    last: Option<SolidFrame>,
    damage: Damage,
    painted: bool,
}

impl Painter for BufferMode {
    fn paint(&mut self, canvas: &mut Canvas<'_>, size: PhysicalSize) {
        let frame = self.scene.frame(size.width, size.height);
        for tile in &frame.rects {
            canvas.fill_round_rect(tile.rect, tile.color, i32::from(tile.radius));
        }
        self.damage = match (&self.last, self.tracked) {
            (Some(last), true) => TileDiff::damage(last, &frame),
            _ => Damage::full(),
        };
        self.last = Some(frame);
        self.painted = true;
    }

    fn background(&self) -> Color {
        TileScene::background()
    }

    fn damage(&self) -> Damage {
        self.damage.clone()
    }
}

impl BufferMode {
    pub fn run(size: LogicalSize, scene: TileScene, seconds: u64, tracked: bool) {
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
            scene,
            tracked,
            last: None,
            damage: Damage::full(),
            painted: false,
        };
        let mut report = RunReport::begin(if tracked { "damaged" } else { "buffer" });
        while report.elapsed() < Duration::from_secs(seconds) && !window.is_closing() {
            window.request_redraw();
            let started = Instant::now();
            if window.tick(&mut mode).is_err() {
                break;
            }
            if std::mem::take(&mut mode.painted) {
                let requests = if mode.damage.is_full() {
                    1
                } else {
                    mode.damage.rects().len()
                };
                report.record_frame(started.elapsed(), requests + 3);
                mode.scene.advance();
            }
            window.wait(16);
            report.sample_gpu();
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

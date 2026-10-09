//! Standalone monitor: one Wayland window of tiles. The sampler thread reads
//! `/proc` and publishes snapshots; this thread only paints them.

use std::time::{Duration, Instant};

mod headless;
mod profile;
mod snapshot_cli;

use monitor::display_snapshot::DisplaySnapshot;
use monitor::history::History;
use monitor::monitor_font::MonitorFont;
use monitor::palette::MonitorPalette;
use monitor::redraw_gate::RedrawGate;
use monitor::repaint::Repaint;
use monitor::sample_interpolator::SampleInterpolator;
use monitor::snapshot::Snapshot;
use monitor::tile_scene::{SceneFrame, ScenePhases, TileScene};
use monitor::typography::Typography;
use platform::TermWindow;
use profile::{FrameProfile, PaintPhases, WorkerProfile};
use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc::{Consumer, RingBuffer};
use sysmon::MonitorReader;

const FRAME_INTERVAL: Duration = Duration::from_micros(16_667);
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
const GRAPH_INTERVAL: Duration = Duration::from_millis(250);
const SNAPSHOT_SLOTS: usize = 8;

struct App {
    window: TermWindow,
    typography: Typography,
    snapshot: Snapshot,
    history: History,
    rx: Consumer<Snapshot>,
    full_repaint: bool,
    has_received_snapshot: bool,
    profile: FrameProfile,
    display: SampleInterpolator,
    scene: TileScene,
    last_graph_sample: Instant,
    last_painted_display: DisplaySnapshot,
}

impl App {
    fn new() -> Self {
        let cfg = config::Config::load().unwrap_or_default();
        let wake = waker::Waker::new().expect("create monitor wakeup");
        let theme = platform::Theme::from_palette(&cfg.palette);
        let monitor_font = MonitorFont::from_config(&cfg);
        let window_font = monitor_font.font().clone();
        let background_opacity = cfg
            .monitor
            .background_opacity
            .unwrap_or_else(|| cfg.bar_opacity());
        let window = TermWindow::new(
            "monitor",
            120,
            40,
            wake.clone(),
            platform::CursorAnimConfig::default(),
            move |scale| {
                platform::Renderer::with_scale(theme, &window_font, scale)
                    .with_opacity(background_opacity)
            },
        )
        .expect("open monitor window");
        let scale_f32 = f32::from(i16::try_from(window.scale()).unwrap_or(i16::MAX));
        let typography = Typography::load(monitor_font.font(), monitor_font.sizes(scale_f32));
        Self {
            window,
            typography,
            snapshot: Snapshot::default(),
            history: History::new(),
            rx: Self::spawn_worker(wake),
            full_repaint: true,
            has_received_snapshot: false,
            profile: FrameProfile::new(),
            display: SampleInterpolator::new(),
            scene: TileScene::new(MonitorPalette::from_config(&cfg)),
            last_graph_sample: Instant::now()
                .checked_sub(GRAPH_INTERVAL)
                .unwrap_or_else(Instant::now),
            last_painted_display: DisplaySnapshot::default(),
        }
    }

    fn record_graph_sample(&mut self, now: Instant) -> bool {
        if now.duration_since(self.last_graph_sample) < GRAPH_INTERVAL {
            return false;
        }
        self.history.add(&self.snapshot.sample);
        self.last_graph_sample = now;
        true
    }

    fn spawn_worker(wake: waker::Waker) -> Consumer<Snapshot> {
        let (tx, rx) = RingBuffer::new(Capacity::exact(SNAPSHOT_SLOTS)).split();
        std::thread::spawn(move || {
            let mut reader = MonitorReader::new();
            let mut profile = WorkerProfile::new();
            let first = reader.prime_processes();
            let _ = tx.push_block(Snapshot {
                sample: reader.sample(),
                sweep: Some(first),
            });
            wake.signal();
            std::thread::sleep(SAMPLE_INTERVAL);
            let mut pending = Some(reader.prime_processes());
            loop {
                let started = Instant::now();
                if let Some(next) = reader.processes_tick() {
                    pending = Some(next);
                }
                profile.record_process_scan(started.elapsed());
                let started = Instant::now();
                let sample = reader.sample();
                profile.record_sample(started.elapsed());
                let _ = tx.push_block(Snapshot {
                    sample,
                    sweep: pending.take(),
                });
                wake.signal();
                profile.report();
                reader.refresh_network_ping();
                std::thread::sleep(SAMPLE_INTERVAL);
            }
        });
        rx
    }

    fn run(mut self) {
        let mut next_frame = Instant::now();
        loop {
            let now = Instant::now();
            if now < next_frame {
                #[allow(clippy::cast_possible_truncation)]
                let millis = next_frame
                    .duration_since(now)
                    .as_millis()
                    .max(1)
                    .min(i32::MAX as u128) as i32;
                self.window.wait(millis);
                continue;
            }
            self.drain_snapshots();
            self.drain_input();
            self.display.blend_into(&mut self.snapshot.sample);
            let new_graph_sample = self.record_graph_sample(now);
            let current_display = DisplaySnapshot::from_sample(&self.snapshot.sample);
            let display_changed = current_display != self.last_painted_display;
            let should_paint =
                RedrawGate::evaluate(self.full_repaint, new_graph_sample, display_changed);
            let alive = if should_paint {
                self.paint_and_present(current_display, display_changed)
            } else {
                self.pump_idle()
            };
            if !alive {
                break;
            }
            next_frame += FRAME_INTERVAL;
            if next_frame <= Instant::now() {
                next_frame = Instant::now() + FRAME_INTERVAL;
            }
        }
    }

    fn drain_snapshots(&mut self) {
        while let Some(mut snapshot) = self.rx.pop() {
            if !self.has_received_snapshot {
                self.full_repaint = true;
                self.has_received_snapshot = true;
            }
            if let Some(sweep) = snapshot.sweep.take() {
                self.full_repaint |= self.scene.accept_processes(&sweep);
            }
            self.display.accept(snapshot.sample.clone());
            self.snapshot.sample = snapshot.sample;
        }
    }

    fn drain_input(&mut self) {
        while let Some(event) = self.window.next_input() {
            if matches!(event, platform::InputEvent::Resize { .. }) {
                self.full_repaint = true;
            }
        }
    }

    fn paint_and_present(&mut self, current_display: DisplaySnapshot, text_changed: bool) -> bool {
        self.window.request_redraw();
        let scene = &mut self.scene;
        let typography = &mut self.typography;
        let sample = &self.snapshot.sample;
        let history = &self.history;
        let repaint = Repaint::new(self.full_repaint, text_changed);
        let mut scene_phases = ScenePhases::default();
        let mut paint_time = Duration::ZERO;
        let mut damage_rows = 0.0;
        let mut frame_presented = false;
        let frame_started = Instant::now();
        let result = self.window.tick_with_painter(|canvas| {
            let started = Instant::now();
            let damage = scene.paint(
                canvas,
                typography,
                &SceneFrame {
                    sample,
                    history,
                    repaint,
                },
                &mut scene_phases,
            );
            paint_time = started.elapsed();
            frame_presented = true;
            damage_rows = damage.row_coverage_fraction(canvas.height());
            if damage.regions() == [canvas.bounds()] {
                platform::draw::Damage::full()
            } else {
                damage.to_platform()
            }
        });
        match result {
            Ok(true) => {
                if frame_presented {
                    let phases = PaintPhases {
                        cpu_chart: scene_phases.charts,
                        static_content: scene_phases.chrome,
                        ..PaintPhases::default()
                    };
                    self.profile.record(
                        paint_time,
                        frame_started.elapsed(),
                        &phases,
                        repaint.full(),
                        damage_rows,
                    );
                    self.full_repaint = false;
                    self.last_painted_display = current_display;
                }
                true
            }
            Ok(false) | Err(_) => false,
        }
    }

    fn pump_idle(&mut self) -> bool {
        self.profile.record_skip();
        matches!(
            self.window
                .tick_with_painter(|_| platform::draw::Damage::none()),
            Ok(true)
        )
    }
}

fn main() {
    if let Ok(Some(options)) = headless_stress::StressOptions::parse(std::env::args()) {
        headless::MonitorStress::run(options);
        return;
    }
    let args: Vec<String> = std::env::args().collect();
    if snapshot_cli::SnapshotCli::requested(&args) {
        snapshot_cli::SnapshotCli::new().run();
        return;
    }
    App::new().run();
}

#[cfg(test)]
mod perf_harness {
    use monitor::history::History;
    use monitor::palette::MonitorPalette;
    use monitor::repaint::Repaint;
    use monitor::tile_scene::{SceneFrame, ScenePhases, TileScene};
    use monitor::typography::Typography;
    use platform::draw::Canvas;
    use std::time::{Duration, Instant};
    use sysmon::{MonitorSample, ProcessInfo};

    const WIDTH: i32 = 1400;
    const HEIGHT: i32 = 900;
    const ITERATIONS: usize = 4_000;

    struct Workload;

    #[allow(clippy::cast_precision_loss)]
    impl Workload {
        fn sample(tick: usize) -> MonitorSample {
            let phase = (tick % 100) as f32;
            MonitorSample {
                cpu: 30.0 + phase * 0.5,
                cores: vec![phase, 100.0 - phase, phase / 2.0, 50.0],
                memory: 40.0 + phase * 0.2,
                temperature: 55.0 + phase * 0.3,
                network_rx: phase / 10.0,
                network_tx: phase / 20.0,
                ..MonitorSample::default()
            }
        }

        fn processes() -> Vec<ProcessInfo> {
            (0..300)
                .map(|pid| ProcessInfo {
                    pid,
                    name: if pid % 3 == 0 {
                        format!("kworker/{pid}:1")
                    } else {
                        format!("proc-{}", pid % 40)
                    },
                    cpu: (pid % 17) as f32,
                    memory_kb: 1024 * (u64::from(pid) + 1),
                })
                .collect()
        }
    }

    #[test]
    #[ignore = "perf workload, not a correctness check"]
    fn headless_tile_paint_perf_workload() {
        let mut scene = TileScene::new(MonitorPalette::builtin());
        scene.accept_processes(&Workload::processes());
        let mut typography = Typography::embedded([32.0, 14.0, 13.0, 11.0]);
        let mut history = History::new();
        let mut buffer = vec![0_u32; (WIDTH * HEIGHT) as usize];
        let mut painting = Duration::ZERO;
        for tick in 0..ITERATIONS {
            let sample = Workload::sample(tick);
            if tick % 15 == 0 {
                history.add(&sample);
            }
            let mut canvas = Canvas::new(&mut buffer, WIDTH, HEIGHT);
            let started = Instant::now();
            scene.paint(
                &mut canvas,
                &mut typography,
                &SceneFrame {
                    sample: &sample,
                    history: &history,
                    repaint: Repaint::new(tick % 60 == 0, tick % 6 == 0),
                },
                &mut ScenePhases::default(),
            );
            painting += started.elapsed();
        }
        eprintln!("{ITERATIONS} frames, {painting:?} painting");
    }
}

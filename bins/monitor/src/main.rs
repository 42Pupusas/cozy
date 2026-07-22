//! Standalone htop-inspired monitor. It uses the normal xdg-shell toplevel
//! window shared with the terminal, while reusing the statusbar's font loader,
//! colors, and structured `/proc` readers. The monitor's worker never performs
//! system I/O on the Wayland thread.

use std::collections::VecDeque;
use std::sync::mpsc;
use std::time::Duration;

mod chart;

use chart::Chart;
use platform::TermWindow;
use platform::draw::{Canvas, Color, Rect};
use statusbar::{MonitorReader, MonitorSample, ProcessInfo, TextRenderer};

const HISTORY: usize = 120;
const GRAPH_H: i32 = 92;
const GRAPH_GAP: i32 = 12;
const FG: Color = Color::rgb(0xa6, 0xa6, 0x9c);
/// Opaque Kanagawa card surface. The monitor window may be translucent, but
/// charts need a stable surface so their fill and anti-aliased strokes do not
/// visually blend into whatever happens to be behind the window.
const CHART_PANEL: Color = Color::rgb(0x2a, 0x2a, 0x37);
const CYAN: Color = Color::rgb(0x8e, 0xa4, 0xa2);
const GREEN: Color = Color::rgb(0x8a, 0x9a, 0x7b);
const ORANGE: Color = Color::rgb(0xb6, 0x92, 0x7b);
const PURPLE: Color = Color::rgb(0xa2, 0x92, 0xa3);

#[derive(Clone, Default)]
struct Snapshot {
    sample: MonitorSample,
    processes: Vec<ProcessInfo>,
}

struct History {
    cpu: VecDeque<f32>,
    memory: VecDeque<f32>,
    temp: VecDeque<f32>,
    disk: VecDeque<f32>,
    rx: VecDeque<f32>,
    tx: VecDeque<f32>,
    load: VecDeque<f32>,
}

impl History {
    const fn new() -> Self {
        Self {
            cpu: VecDeque::new(),
            memory: VecDeque::new(),
            temp: VecDeque::new(),
            disk: VecDeque::new(),
            rx: VecDeque::new(),
            tx: VecDeque::new(),
            load: VecDeque::new(),
        }
    }
    fn push(queue: &mut VecDeque<f32>, value: f32) {
        queue.push_back(value);
        if queue.len() > HISTORY {
            queue.pop_front();
        }
    }
    fn add(&mut self, s: &MonitorSample) {
        Self::push(&mut self.cpu, s.cpu);
        Self::push(&mut self.memory, s.memory);
        Self::push(&mut self.temp, s.temperature);
        Self::push(&mut self.disk, s.disk);
        Self::push(&mut self.rx, s.network_rx);
        Self::push(&mut self.tx, s.network_tx);
        Self::push(&mut self.load, s.load[0]);
    }
}

struct Painter;
impl Painter {
    #[allow(
        clippy::too_many_arguments,
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::suboptimal_flops
    )]
    fn graph(
        canvas: &mut Canvas,
        text: &mut TextRenderer,
        title: &str,
        values: &VecDeque<f32>,
        x: i32,
        y: i32,
        w: i32,
        color: Color,
        maximum: f32,
    ) {
        // The chart deliberately has no grid or frame: at this small size they
        // compete with the data. Its subtle area makes the baseline legible
        // without reintroducing scale marks.
        Chart::new(Rect::new(x, y, w, GRAPH_H), color, CHART_PANEL)
            .draw(canvas, values, maximum);
        text.draw_text(canvas, title, x + 6, y + 6, FG, CHART_PANEL);
    }

    #[allow(clippy::too_many_lines)]
    fn paint(
        canvas: &mut Canvas,
        text: &mut TextRenderer,
        snap: &Snapshot,
        history: &History,
        background: Color,
    ) {
        // A painter-owned surface must start from a known background. Terminal
        // grid painting does this for terminal frames; monitor cards do not use
        // that grid, so leaving old backbuffer pixels here looks transparent.
        canvas.clear(background);
        let width = canvas.width();
        let margin = 20;
        let col_w = (width - margin * 2 - GRAPH_GAP * 2) / 3;
        let top = 18;
        Self::graph(
            canvas,
            text,
            &format!("CPU {:3.0}%", snap.sample.cpu),
            &history.cpu,
            margin,
            top,
            col_w,
            GREEN,
            100.0,
        );
        Self::graph(
            canvas,
            text,
            &format!("MEM {:3.0}%", snap.sample.memory),
            &history.memory,
            margin + col_w + GRAPH_GAP,
            top,
            col_w,
            CYAN,
            100.0,
        );
        Self::graph(
            canvas,
            text,
            &format!("TEMP {:3.0}C", snap.sample.temperature),
            &history.temp,
            margin + (col_w + GRAPH_GAP) * 2,
            top,
            col_w,
            ORANGE,
            100.0,
        );
        let second = top + GRAPH_H + 18;
        Self::graph(
            canvas,
            text,
            &format!("LOAD {:.2}", snap.sample.load[0]),
            &history.load,
            margin,
            second,
            col_w,
            PURPLE,
            8.0,
        );
        Self::graph(
            canvas,
            text,
            &format!("DISK {:3.0}%", snap.sample.disk),
            &history.disk,
            margin + col_w + GRAPH_GAP,
            second,
            col_w,
            ORANGE,
            100.0,
        );
        Self::graph(
            canvas,
            text,
            &format!(
                "NET ↓{:.1} ↑{:.1} MB/s",
                snap.sample.network_rx, snap.sample.network_tx
            ),
            &history.rx,
            margin + (col_w + GRAPH_GAP) * 2,
            second,
            col_w,
            CYAN,
            10.0,
        );
        let list_y = second + GRAPH_H + 22;
        text.draw_text(
            canvas,
            "PID       PROCESS                         MEM",
            margin,
            list_y,
            FG,
            Color::SLATE,
        );
        for (row, process) in snap.processes.iter().take(10).enumerate() {
            let y = list_y + 20 + i32::try_from(row).unwrap_or(0) * 18;
            text.draw_text(
                canvas,
                &format!(
                    "{:>6}   {:<28} {:>7}K",
                    process.pid, process.name, process.memory_kb
                ),
                margin,
                y,
                if row == 0 { ORANGE } else { FG },
                Color::SLATE,
            );
        }
    }
}

struct App {
    window: TermWindow,
    text: TextRenderer,
    snapshot: Snapshot,
    history: History,
    background: Color,
    rx: mpsc::Receiver<Snapshot>,
}
impl App {
    fn new() -> Self {
        let cfg = config::Config::load().unwrap_or_default();
        let wake = waker::Waker::new().expect("create monitor wakeup");
        let theme = platform::Theme::from_palette(&cfg.palette);
        let font = cfg.font.clone();
        let window = TermWindow::new(
            "monitor",
            120,
            40,
            wake.clone(),
            platform::CursorAnimConfig::default(),
            move |scale| platform::Renderer::with_scale(theme, &font, scale),
        )
        .expect("open monitor window");
        let scale = window.scale();
        let scale_f32 = f32::from(i16::try_from(scale).unwrap_or(i16::MAX));
        let size = cfg.font.px_size() * scale_f32;
        let text = TextRenderer::load_with_fallbacks(
            cfg.font.path.as_deref(),
            &[cfg.font.emoji_path.as_deref()],
            size,
        );
        let background = Color::rgb(
            cfg.palette.background.r,
            cfg.palette.background.g,
            cfg.palette.background.b,
        );
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = MonitorReader::new();
            loop {
                let _ = tx.send(Snapshot {
                    sample: reader.sample(),
                    processes: reader.processes(),
                });
                wake.signal();
                std::thread::sleep(Duration::from_secs(1));
            }
        });
        Self {
            window,
            text,
            snapshot: Snapshot::default(),
            history: History::new(),
            background,
            rx,
        }
    }
    fn run(mut self) {
        loop {
            while let Ok(snapshot) = self.rx.try_recv() {
                self.history.add(&snapshot.sample);
                self.snapshot = snapshot;
                self.window.request_redraw();
            }
            let snapshot = &self.snapshot;
            let history = &self.history;
            let text = &mut self.text;
            let background = self.background;
            match self.window.tick_with_painter(|canvas| {
                Painter::paint(canvas, text, snapshot, history, background);
            }) {
                Ok(true) => {}
                Ok(false) | Err(_) => break,
            }
            self.window.wait(250);
        }
    }
}
fn main() {
    App::new().run();
}

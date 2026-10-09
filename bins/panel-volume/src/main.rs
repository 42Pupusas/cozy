//! `panel-volume` entry point and popup event loop.

use std::thread::JoinHandle;

use panel_ui::{CommandChannel, CommandSender, PanelArgs, PanelKey, PopupWindow, Slider, Theme};
use panel_volume::model::{ControlKind, PANEL_HEIGHT, PANEL_WIDTH, VolumeCommand, VolumeModel};
use panel_volume::pactl::MixerSnapshot;
use panel_volume::view::{ControlHit, VolumeView};
use panel_volume::worker::VolumeWorker;
use platform::draw::Point;
use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc::{Consumer, RingBuffer};
use statusbar::TextRenderer;

struct VolumeApp {
    window: PopupWindow,
    text: TextRenderer,
    theme: Theme,
    model: VolumeModel,
    snapshot_rx: Consumer<MixerSnapshot>,
    command_tx: CommandSender<VolumeCommand>,
    worker: JoinHandle<()>,
    hits: Vec<ControlHit>,
    closing: bool,
}

impl VolumeApp {
    const SNAPSHOT_SLOTS: usize = 16;

    fn new(args: PanelArgs) -> Self {
        let cfg = config::Config::load().unwrap_or_else(|error| {
            eprintln!("panel-volume: config error: {error} — using defaults");
            config::Config::default()
        });
        let resolved = args.resolve(&cfg);
        let wake = waker::Waker::new().expect("create waker");
        let window = PopupWindow::new(
            PANEL_WIDTH,
            PANEL_HEIGHT,
            resolved.anchor,
            resolved.side,
            resolved.screen_w,
            resolved.gap,
            &resolved.theme,
            wake.clone(),
        )
        .unwrap_or_else(|error| {
            eprintln!("panel-volume: cannot open popup: {error}");
            std::process::exit(1);
        });
        let theme = resolved.theme;
        let text = TextRenderer::load_with_fallbacks(
            cfg.bar_font_path(),
            &[cfg.bar_emoji_path(), cfg.bar_icon_path()],
            cfg.bar_font_px_size(),
        );
        let (snapshot_tx, snapshot_rx) =
            RingBuffer::new(Capacity::exact(Self::SNAPSHOT_SLOTS)).split();
        let (command_tx, command_inbox) = CommandChannel::open();
        let worker = VolumeWorker::spawn(snapshot_tx, command_inbox, wake);
        Self {
            window,
            text,
            theme,
            model: VolumeModel::default(),
            snapshot_rx,
            command_tx,
            worker,
            hits: Vec::new(),
            closing: false,
        }
    }

    fn run(mut self) {
        self.serve();
        self.shut_down_worker();
    }

    /// Closing the command channel stops the worker, which reaps
    /// `pactl subscribe` on its way out. The snapshot ring is dropped first
    /// so a worker parked on a full ring is released rather than joined
    /// forever.
    fn shut_down_worker(self) {
        let Self {
            command_tx,
            worker,
            snapshot_rx,
            ..
        } = self;
        drop(snapshot_rx);
        drop(command_tx);
        let _ = worker.join();
    }

    fn serve(&mut self) {
        loop {
            while let Some(snapshot) = self.snapshot_rx.pop() {
                self.model.apply(snapshot);
                self.window.request_redraw();
            }
            if self.closing {
                break;
            }
            let Self {
                window,
                text,
                theme,
                model,
                hits,
                ..
            } = self;
            match window.tick(|canvas| {
                *hits = VolumeView::paint(canvas, text, theme, model);
            }) {
                Ok(true) => {}
                Ok(false) => break,
                Err(error) => {
                    eprintln!("panel-volume: {error}");
                    break;
                }
            }
            self.handle_keys();
            self.handle_pointer();
            // Checked before parking: `handle_keys` closes the panel on
            // Esc, and an indefinite wait would park before reaching the
            // test at the top of the loop.
            if self.closing {
                break;
            }
            self.window.wait_for_event();
        }
    }

    fn handle_keys(&mut self) {
        for key in self.window.take_keys() {
            let command = match key {
                PanelKey::Esc => {
                    self.closing = true;
                    None
                }
                PanelKey::Up => {
                    self.model.move_selection(-1);
                    None
                }
                PanelKey::Down | PanelKey::Tab => {
                    self.model.move_selection(1);
                    None
                }
                PanelKey::Left => self.model.adjust_selected(-5),
                PanelKey::Right => self.model.adjust_selected(5),
                PanelKey::Enter => self.model.selected_command(),
                _ => None,
            };
            if let Some(command) = command {
                self.command_tx.send(command);
            }
            self.window.request_redraw();
        }
    }

    fn handle_pointer(&mut self) {
        let point = self.window.pointer_pos();
        let scrolled: i32 = self.window.take_scroll().into_iter().sum();
        if scrolled != 0 {
            self.handle_scroll(point, scrolled);
        }
        if !self.window.take_click() {
            return;
        }
        let Some(hit) = self
            .hits
            .iter()
            .find(|hit| hit.rect().contains(Point::new(point.x, point.y)))
            .cloned()
        else {
            return;
        };
        self.model.selected = hit.index();
        let command = match hit {
            ControlHit::Slider { index, rect } => self.slider_command(index, rect, point.x),
            ControlHit::Row { .. } => self.model.selected_command(),
        };
        if let Some(command) = command {
            self.command_tx.send(command);
        }
        self.window.request_redraw();
    }

    fn handle_scroll(&mut self, point: Point, delta: i32) {
        let Some(hit) = self.hits.iter().find(|hit| hit.rect().contains(point)) else {
            return;
        };
        self.model.selected = hit.index();
        let step = if delta > 0 { 5 } else { -5 };
        if let Some(command) = self.model.adjust_selected(step) {
            self.command_tx.send(command);
        }
    }

    fn slider_command(
        &self,
        index: usize,
        rect: platform::draw::Rect,
        x: i32,
    ) -> Option<VolumeCommand> {
        let control = self.model.controls().get(index)?.clone();
        let ControlKind::Slider { target, .. } = control.kind else {
            return None;
        };
        Some(VolumeCommand::SetVolume {
            target,
            percent: Slider::pct_at(rect, x),
        })
    }
}

fn main() {
    let args = PanelArgs::parse(std::env::args().skip(1));
    VolumeApp::new(args).run();
}

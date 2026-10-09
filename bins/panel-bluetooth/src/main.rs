//! `panel-bluetooth` entry point: parse `--at <x>`/`--bar-height`, open
//! the popup, spawn the pairing agent, run the input loop.

use std::thread::JoinHandle;

use panel_bluetooth::agent::PairingAgent;
use panel_bluetooth::model::{BluetoothCommand, BluetoothModel, PANEL_HEIGHT, PANEL_WIDTH};
use panel_bluetooth::view::BluetoothView;
use panel_bluetooth::worker::{BluetoothState, BluetoothWorker};
use panel_ui::{CommandChannel, CommandSender, PanelArgs, PanelKey, PopupWindow, Theme};
use platform::draw::Point;
use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc::{Consumer, RingBuffer};
use statusbar::TextRenderer;

struct BluetoothApp {
    window: PopupWindow,
    text: TextRenderer,
    theme: Theme,
    model: BluetoothModel,
    state_rx: Consumer<BluetoothState>,
    command_tx: CommandSender<BluetoothCommand>,
    worker: JoinHandle<()>,
    row_rects: Vec<platform::draw::Rect>,
    toggle_rect: Option<platform::draw::Rect>,
    closing: bool,
}

impl BluetoothApp {
    const STATE_SLOTS: usize = 16;

    fn new(args: PanelArgs) -> Self {
        let cfg = config::Config::load().unwrap_or_else(|e| {
            eprintln!("panel-bluetooth: config error: {e} — using defaults");
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
        .unwrap_or_else(|e| {
            eprintln!("panel-bluetooth: cannot open popup: {e}");
            std::process::exit(1);
        });
        let theme = resolved.theme;
        let text = TextRenderer::load_with_fallbacks(
            cfg.bar_font_path(),
            &[cfg.bar_emoji_path(), cfg.bar_icon_path()],
            cfg.bar_font_px_size(),
        );
        let (state_tx, state_rx) = RingBuffer::new(Capacity::exact(Self::STATE_SLOTS)).split();
        let (command_tx, command_inbox) = CommandChannel::open();
        let worker = BluetoothWorker::spawn(state_tx, command_inbox, wake);
        Self {
            window,
            text,
            theme,
            model: BluetoothModel::default(),
            state_rx,
            command_tx,
            worker,
            row_rects: Vec::new(),
            toggle_rect: None,
            closing: false,
        }
    }

    fn run(mut self) {
        // The agent lives exactly as long as the panel: dropping it
        // unregisters from bluez.
        let _agent = PairingAgent::spawn();
        self.serve();
        self.shut_down_worker();
    }

    /// Closing the command channel tells the worker to stop discovery;
    /// joining it keeps the process alive until that call has been made.
    /// The state ring is dropped first so a worker parked on a full ring
    /// is released rather than joined forever.
    fn shut_down_worker(self) {
        let Self {
            command_tx,
            worker,
            state_rx,
            ..
        } = self;
        drop(state_rx);
        drop(command_tx);
        let _ = worker.join();
    }

    fn serve(&mut self) {
        loop {
            while let Some((adapter, devices)) = self.state_rx.pop() {
                self.model.apply(adapter, devices);
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
                row_rects,
                toggle_rect,
                ..
            } = self;
            match window.tick(|canvas| {
                let (rows, toggle) = BluetoothView::paint(canvas, text, theme, model);
                *row_rects = rows;
                *toggle_rect = toggle;
            }) {
                Ok(true) => {}
                Ok(false) => break,
                Err(e) => {
                    eprintln!("panel-bluetooth: {e}");
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
            if let Some(cmd) = self.apply_key(key) {
                self.command_tx.send(cmd);
            }
            self.window.request_redraw();
        }
    }

    fn apply_key(&mut self, key: PanelKey) -> Option<BluetoothCommand> {
        match key {
            PanelKey::Esc => {
                self.closing = true;
                None
            }
            PanelKey::Up => {
                self.model.move_selection(-1);
                None
            }
            PanelKey::Down => {
                self.model.move_selection(1);
                None
            }
            PanelKey::Enter => self.activate_selected(),
            PanelKey::Char('r') => Some(BluetoothCommand::SetScanning { on: true }),
            PanelKey::Char('f') => {
                self.model
                    .highlighted_device()
                    .map(|device| BluetoothCommand::Forget {
                        path: device.path.clone(),
                    })
            }
            _ => None,
        }
    }

    /// Activate the highlighted device: pair-and-connect when new,
    /// connect when paired but idle, disconnect when connected.
    fn activate_selected(&self) -> Option<BluetoothCommand> {
        let device = self.model.highlighted_device()?;
        let path = device.path.clone();
        if device.connected {
            Some(BluetoothCommand::Disconnect { path })
        } else if device.paired {
            Some(BluetoothCommand::Connect { path })
        } else {
            Some(BluetoothCommand::Pair { path })
        }
    }

    fn handle_pointer(&mut self) {
        if !self.window.take_click() {
            return;
        }
        let p = self.window.pointer_pos();
        if let Some(toggle) = self.toggle_rect
            && toggle.contains(p)
        {
            let on = self.model.adapter.as_ref().is_none_or(|a| !a.powered);
            self.command_tx.send(BluetoothCommand::SetPowered { on });
            return;
        }
        let Some(idx) = self
            .row_rects
            .iter()
            .position(|r| r.contains(Point::new(p.x, p.y)))
        else {
            return;
        };
        self.model.selected = idx + self.model.scroll;
        if let Some(cmd) = self.activate_selected() {
            self.command_tx.send(cmd);
        }
    }
}

fn main() {
    let args = PanelArgs::parse(std::env::args().skip(1));
    BluetoothApp::new(args).run();
}

//! `panel-wifi` entry point: parse placement arguments (`--at`,
//! `--at-right`, `--bar-height`, ...), open the popup, run the input
//! loop. See `lib.rs`'s module doc for the thread split.

use panel_ui::{CommandChannel, CommandSender, PanelArgs, PanelKey, PopupWindow, Theme};
use panel_wifi::{
    PANEL_HEIGHT, PANEL_WIDTH, WifiModel, model::WifiCommand, view::WifiView, worker::WifiWorker,
};
use platform::draw::Point;
use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc::{Consumer, RingBuffer};
use statusbar::TextRenderer;

struct WifiApp {
    window: PopupWindow,
    text: TextRenderer,
    theme: Theme,
    model: WifiModel,
    snapshot_rx: Consumer<dbus::nm_wifi::WifiPanelSnapshot>,
    command_tx: CommandSender<WifiCommand>,
    row_rects: Vec<platform::draw::Rect>,
    closing: bool,
}

impl WifiApp {
    const SNAPSHOT_SLOTS: usize = 16;

    fn new(args: PanelArgs) -> Self {
        let cfg = config::Config::load().unwrap_or_else(|e| {
            eprintln!("panel-wifi: config error: {e} — using defaults");
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
            eprintln!("panel-wifi: cannot open popup: {e}");
            std::process::exit(1);
        });
        let theme = resolved.theme;
        let px = cfg.bar_font_px_size();
        let text = TextRenderer::load_with_fallbacks(
            cfg.bar_font_path(),
            &[cfg.bar_emoji_path(), cfg.bar_icon_path()],
            px,
        );
        let (snapshot_tx, snapshot_rx) =
            RingBuffer::new(Capacity::exact(Self::SNAPSHOT_SLOTS)).split();
        let (command_tx, command_inbox) = CommandChannel::open();
        WifiWorker::spawn(snapshot_tx, command_inbox, wake);
        Self {
            window,
            text,
            theme,
            model: WifiModel::default(),
            snapshot_rx,
            command_tx,
            row_rects: Vec::new(),
            closing: false,
        }
    }

    fn run(mut self) {
        loop {
            while let Some(snapshot) = self.snapshot_rx.pop() {
                self.model.apply_snapshot(snapshot);
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
                ..
            } = &mut self;
            match window.tick(|canvas| {
                *row_rects = WifiView::paint(canvas, text, theme, model);
            }) {
                Ok(true) => {}
                Ok(false) => break,
                Err(e) => {
                    eprintln!("panel-wifi: {e}");
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
            let cmd = self.apply_key(key);
            if let Some(cmd) = cmd {
                self.command_tx.send(cmd);
                self.window.request_redraw();
            }
        }
    }

    /// Translate one key into a model mutation or a worker command;
    /// `None` closes the panel.
    fn apply_key(&mut self, key: PanelKey) -> Option<panel_wifi::model::WifiCommand> {
        use panel_wifi::model::WifiCommand;
        // Esc cancels password entry; otherwise it closes the panel.
        if self.model.pending_ap.is_some() {
            match key {
                PanelKey::Esc => {
                    let _ = self.model.take_password();
                    return None;
                }
                PanelKey::Char(c) => {
                    self.model.type_password_char(c);
                    return None;
                }
                PanelKey::Backspace => {
                    self.model.backspace_password();
                    return None;
                }
                PanelKey::Enter => {
                    let (ap, password) = self.model.take_password().unwrap_or((
                        dbus::nm_wifi::AccessPoint {
                            ssid: String::new(),
                            strength: 0,
                            secured: true,
                            path: String::new(),
                        },
                        String::new(),
                    ));
                    self.model.status = format!("connecting to {}…", ap.ssid);
                    return Some(WifiCommand::Connect {
                        ap,
                        password: Some(password),
                    });
                }
                _ => return None,
            }
        }
        match key {
            PanelKey::Esc => {
                self.window.request_redraw();
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
            PanelKey::Char('r') => Some(WifiCommand::Scan),
            PanelKey::Char('f') => {
                self.model
                    .highlighted_saved()
                    .map(|saved| WifiCommand::Forget {
                        path: saved.path.clone(),
                    })
            }
            _ => None,
        }
    }

    /// Activate the highlighted row: connect, or disconnect.
    fn activate_selected(&mut self) -> Option<panel_wifi::model::WifiCommand> {
        use panel_wifi::model::WifiCommand;
        let rows = self.model.visible_rows();
        let row = rows.get(self.model.selected)?;
        match row.kind {
            panel_wifi::model::RowKind::Disconnect => Some(WifiCommand::Disconnect),
            panel_wifi::model::RowKind::Network => {
                if self.model.begin_password() {
                    None
                } else if let Some(ap) = self.model.highlighted_ap() {
                    self.model.status = format!("connecting to {}…", ap.ssid);
                    Some(WifiCommand::Connect { ap, password: None })
                } else if let Some((ssid, path)) = self
                    .model
                    .highlighted_saved()
                    .map(|saved| (saved.ssid.clone(), saved.path.clone()))
                {
                    self.model.status = format!("connecting to {ssid}…");
                    Some(WifiCommand::ActivateSaved { path })
                } else {
                    None
                }
            }
        }
    }

    fn handle_pointer(&mut self) {
        if self.window.take_click() {
            let p = self.window.pointer_pos();
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
            self.window.request_redraw();
        }
    }
}

fn main() {
    let args = PanelArgs::parse(std::env::args().skip(1));
    WifiApp::new(args).run();
}

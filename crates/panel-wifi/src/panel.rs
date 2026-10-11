//! The Wi-Fi panel as the bar hosts it: model, worker link, and the row
//! rects the last paint produced.

use dbus::nm_wifi::WifiPanelSnapshot;
use panel_ui::{Panel, PanelFlow, PanelKey, Theme, WorkerLink};
use platform::draw::{Canvas, Point, Rect};
use statusbar::TextRenderer;

use crate::input::WifiInput;
use crate::model::{PANEL_HEIGHT, PANEL_WIDTH, WifiCommand, WifiModel};
use crate::view::WifiView;
use crate::worker::WifiWorker;

pub struct WifiPanel {
    model: WifiModel,
    row_rects: Vec<Rect>,
    worker: WorkerLink<WifiPanelSnapshot, WifiCommand>,
}

impl WifiPanel {
    /// Start the worker; `wake` is signalled on every snapshot.
    #[must_use]
    pub fn open(wake: waker::Waker) -> Self {
        Self {
            model: WifiModel::default(),
            row_rects: Vec::new(),
            worker: WorkerLink::start(|tx, inbox| Some(WifiWorker::spawn(tx, inbox, wake))),
        }
    }

    fn send(&self, command: Option<WifiCommand>) {
        if let Some(command) = command {
            self.worker.send(command);
        }
    }
}

impl Panel for WifiPanel {
    fn size(&self) -> (i32, i32) {
        (PANEL_WIDTH, PANEL_HEIGHT)
    }

    fn drain(&mut self) -> bool {
        let model = &mut self.model;
        self.worker.drain(|snapshot| model.apply_snapshot(snapshot))
    }

    fn paint(&mut self, canvas: &mut Canvas, text: &mut TextRenderer, theme: &Theme) {
        self.row_rects = WifiView::paint(canvas, text, theme, &self.model);
    }

    fn key(&mut self, key: PanelKey) -> PanelFlow {
        let input = WifiInput::key(&mut self.model, key);
        self.send(input.command);
        input.flow
    }

    fn click(&mut self, at: Point) {
        let Some(index) = self.row_rects.iter().position(|r| r.contains(at)) else {
            return;
        };
        self.model.selected = index + self.model.scroll;
        let command = WifiInput::activate(&mut self.model);
        self.send(command);
    }
}

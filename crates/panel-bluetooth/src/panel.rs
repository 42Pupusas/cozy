//! The Bluetooth panel as the bar hosts it. The pairing agent is
//! registered with bluez for exactly as long as the panel is open.

use panel_ui::{Panel, PanelFlow, PanelKey, Theme, WorkerLink};
use platform::draw::{Canvas, Point, Rect};
use statusbar::TextRenderer;

use crate::agent::PairingAgent;
use crate::input::BluetoothInput;
use crate::model::{BluetoothCommand, BluetoothModel, PANEL_HEIGHT, PANEL_WIDTH};
use crate::view::BluetoothView;
use crate::worker::{BluetoothState, BluetoothWorker};

pub struct BluetoothPopupPanel {
    model: BluetoothModel,
    row_rects: Vec<Rect>,
    toggle_rect: Option<Rect>,
    worker: WorkerLink<BluetoothState, BluetoothCommand>,
    _agent: PairingAgent,
}

impl BluetoothPopupPanel {
    #[must_use]
    pub fn open(wake: waker::Waker) -> Self {
        Self {
            model: BluetoothModel::default(),
            row_rects: Vec::new(),
            toggle_rect: None,
            worker: WorkerLink::start(|tx, inbox| Some(BluetoothWorker::spawn(tx, inbox, wake))),
            _agent: PairingAgent::spawn(),
        }
    }

    fn send(&self, command: Option<BluetoothCommand>) {
        if let Some(command) = command {
            self.worker.send(command);
        }
    }
}

impl Panel for BluetoothPopupPanel {
    fn size(&self) -> (i32, i32) {
        (PANEL_WIDTH, PANEL_HEIGHT)
    }

    fn drain(&mut self) -> bool {
        let model = &mut self.model;
        self.worker
            .drain(|(adapter, devices)| model.apply(adapter, devices))
    }

    fn paint(&mut self, canvas: &mut Canvas, text: &mut TextRenderer, theme: &Theme) {
        (self.row_rects, self.toggle_rect) = BluetoothView::paint(canvas, text, theme, &self.model);
    }

    fn key(&mut self, key: PanelKey) -> PanelFlow {
        let (command, flow) = BluetoothInput::key(&mut self.model, key);
        self.send(command);
        flow
    }

    fn click(&mut self, at: Point) {
        let command = BluetoothInput::click(&mut self.model, &self.row_rects, self.toggle_rect, at);
        self.send(command);
    }
}

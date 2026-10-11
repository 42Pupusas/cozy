//! The volume panel as the bar hosts it.

use panel_ui::{Panel, PanelFlow, PanelKey, Theme, WorkerLink};
use platform::draw::{Canvas, Point};
use statusbar::TextRenderer;

use crate::input::VolumeInput;
use crate::model::{PANEL_HEIGHT, PANEL_WIDTH, VolumeCommand, VolumeModel};
use crate::pactl::MixerSnapshot;
use crate::view::{ControlHit, VolumeView};
use crate::worker::VolumeWorker;

pub struct VolumePanel {
    model: VolumeModel,
    hits: Vec<ControlHit>,
    worker: WorkerLink<MixerSnapshot, VolumeCommand>,
}

impl VolumePanel {
    #[must_use]
    pub fn open(wake: waker::Waker) -> Self {
        Self {
            model: VolumeModel::default(),
            hits: Vec::new(),
            worker: WorkerLink::start(|tx, inbox| Some(VolumeWorker::spawn(tx, inbox, wake))),
        }
    }

    fn send(&self, command: Option<VolumeCommand>) {
        if let Some(command) = command {
            self.worker.send(command);
        }
    }
}

impl Panel for VolumePanel {
    fn size(&self) -> (i32, i32) {
        (PANEL_WIDTH, PANEL_HEIGHT)
    }

    fn drain(&mut self) -> bool {
        let model = &mut self.model;
        self.worker.drain(|snapshot| model.apply(snapshot))
    }

    fn paint(&mut self, canvas: &mut Canvas, text: &mut TextRenderer, theme: &Theme) {
        self.hits = VolumeView::paint(canvas, text, theme, &self.model);
    }

    fn key(&mut self, key: PanelKey) -> PanelFlow {
        let (command, flow) = VolumeInput::key(&mut self.model, key);
        self.send(command);
        flow
    }

    fn click(&mut self, at: Point) {
        let command = VolumeInput::click(&mut self.model, &self.hits, at);
        self.send(command);
    }

    fn scroll(&mut self, at: Point, delta: i32) {
        let command = VolumeInput::scroll(&mut self.model, &self.hits, at, delta);
        self.send(command);
    }
}

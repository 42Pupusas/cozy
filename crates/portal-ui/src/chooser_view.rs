use platform::draw::{Canvas, Color, Point};
use platform::protocol::PointerButton;
use platform::toplevel::{KeyPress, Painter, PhysicalSize, PointerEventKind, Position};
use portal_core::ChooserOutcome;
use tiles::hover::{Hover, Pane, Spot};
use tiles::theme::Theme;
use tiles::tile_assembly::{PlainTiles, TileAssembly};
use tiles::tile_painter::TilePainter;
use tiles::tile_scene::{Flow, MotionAim, Steering, TileScene};
use tiles::typography::Typography;
use widget::{DoubleClick, ListCommand, Mods, MonotonicClock};

use crate::chooser_footer::{ChooserFooter, FooterState};
use crate::chooser_layout::{ChooserLayout, FooterTarget};
use crate::chooser_model::ChooserModel;
use crate::chooser_step::ChooserStep;
use crate::footer_caption::FooterCaption;

pub struct ChooserView {
    model: ChooserModel,
    scene: TileScene,
    theme: Theme,
    type_scale: Typography,
    hover: Hover,
    footer_hover: Option<FooterTarget>,
    clicks: DoubleClick,
    size: PhysicalSize,
    focused: bool,
    outcome: Option<ChooserOutcome>,
}

impl ChooserView {
    #[must_use]
    pub fn new(
        model: ChooserModel,
        theme: Theme,
        type_scale: Typography,
        size: PhysicalSize,
        wake: waker::Waker,
    ) -> Self {
        Self {
            model,
            scene: TileScene::new(wake),
            theme,
            type_scale,
            hover: Hover::none(),
            footer_hover: None,
            clicks: DoubleClick::new(),
            size,
            focused: false,
            outcome: None,
        }
    }

    pub const fn resize(&mut self, size: PhysicalSize) {
        self.size = size;
    }

    pub const fn take_outcome(&mut self) -> Option<ChooserOutcome> {
        self.outcome.take()
    }

    #[must_use]
    pub const fn model(&self) -> &ChooserModel {
        &self.model
    }

    pub fn poll_listing(&mut self) -> bool {
        self.model.poll_listing().needs_redraw()
    }

    pub fn poll_tiles(&mut self) -> bool {
        self.scene.poll(self.model.navigator())
    }

    #[must_use]
    pub fn watch_fd(&self) -> Option<std::os::fd::RawFd> {
        self.model.navigator().watch_fd()
    }

    pub fn advance_motion(&mut self) -> bool {
        self.step_motion(MonotonicClock::now_ms())
    }

    pub fn step_motion(&mut self, now_ms: u64) -> bool {
        let flow = self.flow();
        let nav = self.model.navigator();
        let aim = MotionAim {
            spot: self.hover.spot(),
            cursor: self.focused.then(|| nav.pane().cursor()).flatten(),
            selected: nav.pane().selection().selected(),
            tray: None,
            actions: false,
            held: None,
        };
        self.scene.step(&flow, nav, &aim, now_ms)
    }

    #[must_use]
    pub fn motion_wait_ms(&self) -> Option<i32> {
        self.scene.frame_wait_ms()
    }

    fn layout(&mut self) -> ChooserLayout {
        let label = self.model.accept_label();
        let widths = ChooserFooter::new(&self.theme, &mut self.type_scale).widths(label);
        ChooserLayout::new(
            self.size.width,
            self.size.height,
            ChooserFooter::metrics(&self.theme),
            widths,
        )
    }

    fn flow(&mut self) -> Flow {
        Flow {
            area: self.layout().field(),
            floor: self.theme.spacing.tile_floor(),
            tail: None,
        }
    }

    fn apply(&mut self, step: ChooserStep) {
        match step {
            ChooserStep::Finished(selection) => {
                self.outcome = Some(ChooserOutcome::Picked(selection));
            }
            ChooserStep::Cancelled => self.outcome = Some(ChooserOutcome::Cancelled),
            ChooserStep::Moved | ChooserStep::Redraw | ChooserStep::Ignored => {}
        }
    }

    fn steer(&mut self, command: ListCommand) -> ChooserStep {
        let flow = self.flow();
        let cursor = self.model.navigator().pane().cursor();
        let extend = matches!(command, ListCommand::Extend(_));
        match self.scene.steer(&flow, cursor, command) {
            Steering::Land(row) => self.model.steer_to(row, extend),
            Steering::Scrolled(row) => {
                if let Some(row) = row {
                    let _ = self.model.steer_to(row, extend);
                }
                ChooserStep::Redraw
            }
            Steering::Stay => ChooserStep::Ignored,
        }
    }

    fn spot_at(&mut self, at: Position) -> Option<Spot> {
        let point = Point::new(at.x, at.y);
        let flow = self.flow();
        if let Some(placed) = self.scene.placed_at(&flow, point) {
            return Some(Spot::Item(Pane::Tiles, placed.row));
        }
        self.scene.more_at(&flow, point).then_some(Spot::More)
    }

    fn footer_at(&mut self, at: Position) -> Option<FooterTarget> {
        self.layout().hit(Point::new(at.x, at.y))
    }

    fn press_footer(&mut self, target: FooterTarget) {
        let step = match target {
            FooterTarget::Up => self.model.go_up(),
            FooterTarget::Cancel => ChooserStep::Cancelled,
            FooterTarget::Accept => self.model.accept(),
        };
        self.apply(step);
    }

    fn press_field(&mut self, at: Position, mods: Mods) {
        let point = Point::new(at.x, at.y);
        let flow = self.flow();
        if self.scene.more_at(&flow, point) {
            let shown = i32::try_from(self.scene.layout(&flow).len()).unwrap_or(i32::MAX);
            self.slide(shown.max(1), at);
            return;
        }
        let doubled = self.clicks.register(MonotonicClock::now_ms(), (at.x, at.y));
        let Some(row) = self.scene.hit(&flow, point) else {
            let step = self.model.clear_selection();
            self.apply(step);
            return;
        };
        let step = if doubled && mods == Mods::none() {
            self.model.activate_entry(row)
        } else {
            self.model.pick(row, mods)
        };
        self.apply(step);
    }

    fn slide(&mut self, steps: i32, at: Position) {
        let flow = self.flow();
        if self.scene.slide(&flow, self.model.navigator(), steps) {
            let spot = self.spot_at(at);
            let _ = self.hover.move_to(spot);
        }
    }

    fn press_left(&mut self, at: Position, mods: Mods) {
        if let Some(target) = self.footer_at(at) {
            self.press_footer(target);
            return;
        }
        if self.layout().field().contains(Point::new(at.x, at.y)) {
            self.press_field(at, mods);
        }
    }

    fn footer_state(&self) -> FooterState<'static> {
        let nav = self.model.navigator();
        FooterState {
            caption: FooterCaption::of(nav.path(), nav.filter().text()),
            accept_label: self.model.accept_label(),
            accept_enabled: self.model.can_accept(),
            up_enabled: nav.can_go_up(),
            hovered: self.footer_hover,
        }
    }
}

#[cfg(test)]
impl ChooserView {
    pub(crate) fn tile_centre(&mut self, name: &str) -> Position {
        let flow = self.flow();
        let nav = self.model.navigator();
        let placed = self
            .scene
            .layout(&flow)
            .into_iter()
            .find(|placed| nav.name_of(placed.row).as_deref() == Some(name))
            .expect("the entry has a tile");
        Position {
            x: placed.rect.x + placed.rect.w / 2,
            y: placed.rect.y + placed.rect.h / 2,
        }
    }

    pub(crate) fn button_centre(&mut self, target: FooterTarget) -> Position {
        let rect = self.layout().button(target);
        Position {
            x: rect.x + rect.w / 2,
            y: rect.y + rect.h / 2,
        }
    }

    pub(crate) fn caption_centre(&mut self) -> Position {
        let rect = self.layout().caption();
        Position {
            x: rect.x + rect.w / 2,
            y: rect.y + rect.h / 2,
        }
    }

    pub(crate) fn painted(&mut self) -> Vec<u32> {
        let size = self.size;
        let mut pixels = vec![0u32; (size.width * size.height).cast_unsigned() as usize];
        let mut canvas = Canvas::new(&mut pixels, size.width, size.height);
        self.paint(&mut canvas, size);
        pixels
    }
}

impl Painter for ChooserView {
    fn background(&self) -> Color {
        self.theme.background
    }

    fn paint(&mut self, canvas: &mut Canvas<'_>, size: PhysicalSize) {
        self.size = size;
        canvas.clear(self.theme.background);
        let flow = self.flow();
        self.scene.reaim_if_primed(&flow, self.model.navigator());
        self.scene.sync(self.model.navigator());
        let placed = self.scene.layout(&flow);
        let more = self.scene.more(&flow);
        let tiles = TileAssembly {
            scene: &self.scene,
            nav: self.model.navigator(),
            theme: &self.theme,
            overlay: &PlainTiles,
        }
        .tiles(&placed, more);
        let tiles = self.scene.stage(&flow, self.model.navigator(), tiles);
        TilePainter::new(&self.theme, &mut self.type_scale).paint(canvas, &tiles);
        let layout = self.layout();
        let state = self.footer_state();
        ChooserFooter::new(&self.theme, &mut self.type_scale).paint(canvas, &layout, &state);
    }

    fn on_key(&mut self, press: KeyPress) {
        self.scene.sync(self.model.navigator());
        let step = match self.model.translate(press) {
            Some(crate::chooser_action::ChooserAction::Browse(browse::BrowseAction::List(
                command,
            ))) if tiles::spiral_cursor::SpiralCursor::steers(command) => self.steer(command),
            Some(action) => self.model.act(action),
            None => ChooserStep::Ignored,
        };
        self.apply(step);
        self.scene.sync(self.model.navigator());
    }

    fn on_focus(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn on_pointer(&mut self, event: PointerEventKind) {
        self.scene.sync(self.model.navigator());
        match event {
            PointerEventKind::Press {
                button: PointerButton::Left,
                at,
                modifiers,
            } => self.press_left(
                at,
                Mods {
                    ctrl: modifiers.ctrl(),
                    shift: modifiers.shift(),
                },
            ),
            PointerEventKind::Motion(at) => {
                self.footer_hover = self.footer_at(at);
                let spot = if self.footer_hover.is_some() {
                    None
                } else {
                    self.spot_at(at)
                };
                let _ = self.hover.move_to(spot);
            }
            PointerEventKind::Scroll { lines, at, .. } => self.slide(lines, at),
            PointerEventKind::Leave => {
                let _ = self.hover.clear();
                self.footer_hover = None;
            }
            PointerEventKind::Enter(_)
            | PointerEventKind::Press { .. }
            | PointerEventKind::Release { .. } => {}
        }
    }
}

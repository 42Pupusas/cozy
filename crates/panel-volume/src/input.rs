//! Turns keys, clicks and wheel steps into model edits and worker
//! commands.

use panel_ui::{PanelFlow, PanelKey, Slider};
use platform::draw::{Point, Rect};

use crate::model::{ControlKind, VolumeCommand, VolumeModel};
use crate::view::ControlHit;

/// Percent per arrow key or wheel step.
const STEP_PCT: i32 = 5;

pub struct VolumeInput;

impl VolumeInput {
    pub fn key(model: &mut VolumeModel, key: PanelKey) -> (Option<VolumeCommand>, PanelFlow) {
        let command = match key {
            PanelKey::Esc => return (None, PanelFlow::Close),
            PanelKey::Up => {
                model.move_selection(-1);
                None
            }
            PanelKey::Down | PanelKey::Tab => {
                model.move_selection(1);
                None
            }
            PanelKey::Left => model.adjust_selected(-STEP_PCT),
            PanelKey::Right => model.adjust_selected(STEP_PCT),
            PanelKey::Enter => model.selected_command(),
            _ => None,
        };
        (command, PanelFlow::Stay)
    }

    pub fn click(model: &mut VolumeModel, hits: &[ControlHit], at: Point) -> Option<VolumeCommand> {
        let hit = hits.iter().find(|hit| hit.rect().contains(at))?.clone();
        model.selected = hit.index();
        match hit {
            ControlHit::Slider { index, rect } => Self::slider_command(model, index, rect, at.x),
            ControlHit::Row { .. } => model.selected_command(),
        }
    }

    pub fn scroll(
        model: &mut VolumeModel,
        hits: &[ControlHit],
        at: Point,
        delta: i32,
    ) -> Option<VolumeCommand> {
        let hit = hits.iter().find(|hit| hit.rect().contains(at))?;
        model.selected = hit.index();
        model.adjust_selected(if delta > 0 { STEP_PCT } else { -STEP_PCT })
    }

    fn slider_command(
        model: &VolumeModel,
        index: usize,
        rect: Rect,
        x: i32,
    ) -> Option<VolumeCommand> {
        let ControlKind::Slider { target, .. } = model.controls().get(index)?.kind.clone() else {
            return None;
        };
        Some(VolumeCommand::SetVolume {
            target,
            percent: Slider::pct_at(rect, x),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esc_closes_and_sends_nothing() {
        let mut model = VolumeModel::default();
        assert_eq!(
            VolumeInput::key(&mut model, PanelKey::Esc),
            (None, PanelFlow::Close)
        );
    }

    #[test]
    fn a_click_outside_every_control_does_nothing() {
        let mut model = VolumeModel::default();
        let hits = [ControlHit::Row {
            index: 0,
            rect: Rect::new(0, 0, 10, 10),
        }];
        assert_eq!(VolumeInput::click(&mut model, &hits, Point::new(50, 50)), None);
    }

    #[test]
    fn a_scroll_selects_the_control_under_the_pointer() {
        let mut model = VolumeModel::default();
        let hits = [
            ControlHit::Row {
                index: 0,
                rect: Rect::new(0, 0, 10, 10),
            },
            ControlHit::Row {
                index: 3,
                rect: Rect::new(0, 20, 10, 10),
            },
        ];
        VolumeInput::scroll(&mut model, &hits, Point::new(5, 25), 15);
        assert_eq!(model.selected, 3);
    }
}

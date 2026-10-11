//! Turns keys and clicks into model edits and worker commands.

use panel_ui::{PanelFlow, PanelKey};
use platform::draw::{Point, Rect};

use crate::model::{BluetoothCommand, BluetoothModel};

pub struct BluetoothInput;

impl BluetoothInput {
    pub fn key(model: &mut BluetoothModel, key: PanelKey) -> (Option<BluetoothCommand>, PanelFlow) {
        let command = match key {
            PanelKey::Esc => return (None, PanelFlow::Close),
            PanelKey::Up => {
                model.move_selection(-1);
                None
            }
            PanelKey::Down => {
                model.move_selection(1);
                None
            }
            PanelKey::Enter => Self::activate(model),
            PanelKey::Char('r') => Some(BluetoothCommand::SetScanning { on: true }),
            PanelKey::Char('f') => model
                .highlighted_device()
                .map(|device| BluetoothCommand::Forget {
                    path: device.path.clone(),
                }),
            _ => None,
        };
        (command, PanelFlow::Stay)
    }

    /// Pair a new device, connect a paired idle one, disconnect a
    /// connected one.
    #[must_use]
    pub fn activate(model: &BluetoothModel) -> Option<BluetoothCommand> {
        let device = model.highlighted_device()?;
        let path = device.path.clone();
        Some(if device.connected {
            BluetoothCommand::Disconnect { path }
        } else if device.paired {
            BluetoothCommand::Connect { path }
        } else {
            BluetoothCommand::Pair { path }
        })
    }

    pub fn click(
        model: &mut BluetoothModel,
        rows: &[Rect],
        toggle: Option<Rect>,
        at: Point,
    ) -> Option<BluetoothCommand> {
        if toggle.is_some_and(|t| t.contains(at)) {
            let on = model.adapter.as_ref().is_none_or(|a| !a.powered);
            return Some(BluetoothCommand::SetPowered { on });
        }
        let index = rows.iter().position(|r| r.contains(at))?;
        model.selected = index + model.scroll;
        Self::activate(model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esc_closes() {
        let mut model = BluetoothModel::default();
        assert_eq!(
            BluetoothInput::key(&mut model, PanelKey::Esc),
            (None, PanelFlow::Close)
        );
    }

    #[test]
    fn r_starts_discovery() {
        let mut model = BluetoothModel::default();
        assert_eq!(
            BluetoothInput::key(&mut model, PanelKey::Char('r')),
            (
                Some(BluetoothCommand::SetScanning { on: true }),
                PanelFlow::Stay
            )
        );
    }

    #[test]
    fn the_toggle_powers_on_a_missing_or_off_adapter() {
        let mut model = BluetoothModel::default();
        let toggle = Rect::new(0, 0, 20, 20);
        assert_eq!(
            BluetoothInput::click(&mut model, &[], Some(toggle), Point::new(5, 5)),
            Some(BluetoothCommand::SetPowered { on: true })
        );
    }

    #[test]
    fn a_click_off_every_row_does_nothing() {
        let mut model = BluetoothModel::default();
        let rows = [Rect::new(0, 40, 100, 30)];
        assert_eq!(
            BluetoothInput::click(&mut model, &rows, None, Point::new(5, 5)),
            None
        );
    }
}

//! Turns keys and row activations into model edits and worker commands.

use dbus::nm_wifi::AccessPoint;
use panel_ui::{PanelFlow, PanelKey};

use crate::model::{RowKind, WifiCommand, WifiModel};

/// The result of one input: an optional command, and whether the panel
/// stays open.
#[derive(Debug, PartialEq, Eq)]
pub struct WifiInput {
    pub command: Option<WifiCommand>,
    pub flow: PanelFlow,
}

impl WifiInput {
    const fn stay(command: Option<WifiCommand>) -> Self {
        Self {
            command,
            flow: PanelFlow::Stay,
        }
    }

    /// Esc cancels password entry when one is in progress and closes the
    /// panel otherwise.
    pub fn key(model: &mut WifiModel, key: PanelKey) -> Self {
        if model.pending_ap.is_some() {
            return Self::stay(Self::password_key(model, key));
        }
        let command = match key {
            PanelKey::Esc => {
                return Self {
                    command: None,
                    flow: PanelFlow::Close,
                };
            }
            PanelKey::Up => {
                model.move_selection(-1);
                None
            }
            PanelKey::Down => {
                model.move_selection(1);
                None
            }
            PanelKey::Enter => Self::activate(model),
            PanelKey::Char('r') => Some(WifiCommand::Scan),
            PanelKey::Char('f') => model.highlighted_saved().map(|saved| WifiCommand::Forget {
                path: saved.path.clone(),
            }),
            _ => None,
        };
        Self::stay(command)
    }

    fn password_key(model: &mut WifiModel, key: PanelKey) -> Option<WifiCommand> {
        match key {
            PanelKey::Esc => {
                let _ = model.take_password();
                None
            }
            PanelKey::Char(c) => {
                model.type_password_char(c);
                None
            }
            PanelKey::Backspace => {
                model.backspace_password();
                None
            }
            PanelKey::Enter => {
                let (ap, password) = model.take_password().unwrap_or_else(|| {
                    (
                        AccessPoint {
                            ssid: String::new(),
                            strength: 0,
                            secured: true,
                            path: String::new(),
                        },
                        String::new(),
                    )
                });
                model.status = format!("connecting to {}…", ap.ssid);
                Some(WifiCommand::Connect {
                    ap,
                    password: Some(password),
                })
            }
            _ => None,
        }
    }

    /// Activate the highlighted row: connect, prompt for a password, or
    /// disconnect.
    pub fn activate(model: &mut WifiModel) -> Option<WifiCommand> {
        let rows = model.visible_rows();
        let row = rows.get(model.selected)?;
        match row.kind {
            RowKind::Disconnect => Some(WifiCommand::Disconnect),
            RowKind::Network => {
                if model.begin_password() {
                    None
                } else if let Some(ap) = model.highlighted_ap() {
                    model.status = format!("connecting to {}…", ap.ssid);
                    Some(WifiCommand::Connect { ap, password: None })
                } else if let Some((ssid, path)) = model
                    .highlighted_saved()
                    .map(|saved| (saved.ssid.clone(), saved.path.clone()))
                {
                    model.status = format!("connecting to {ssid}…");
                    Some(WifiCommand::ActivateSaved { path })
                } else {
                    None
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esc_closes_the_panel_outside_password_entry() {
        let mut model = WifiModel::default();
        let input = WifiInput::key(&mut model, PanelKey::Esc);
        assert_eq!(input.flow, PanelFlow::Close);
        assert_eq!(input.command, None);
    }

    #[test]
    fn r_requests_a_scan() {
        let mut model = WifiModel::default();
        let input = WifiInput::key(&mut model, PanelKey::Char('r'));
        assert_eq!(input.command, Some(WifiCommand::Scan));
        assert_eq!(input.flow, PanelFlow::Stay);
    }

    #[test]
    fn password_entry_captures_text_and_esc_cancels_without_closing() {
        let mut model = WifiModel {
            pending_ap: Some(AccessPoint {
                ssid: "home".into(),
                strength: 80,
                secured: true,
                path: "/ap/1".into(),
            }),
            ..WifiModel::default()
        };
        WifiInput::key(&mut model, PanelKey::Char('x'));
        assert_eq!(model.password, "x");
        let input = WifiInput::key(&mut model, PanelKey::Esc);
        assert_eq!(input.flow, PanelFlow::Stay);
        assert!(model.pending_ap.is_none());
    }

    #[test]
    fn enter_in_password_entry_connects_with_the_typed_password() {
        let mut model = WifiModel {
            pending_ap: Some(AccessPoint {
                ssid: "home".into(),
                strength: 80,
                secured: true,
                path: "/ap/1".into(),
            }),
            ..WifiModel::default()
        };
        WifiInput::key(&mut model, PanelKey::Char('p'));
        let input = WifiInput::key(&mut model, PanelKey::Enter);
        let Some(WifiCommand::Connect { ap, password }) = input.command else {
            panic!("expected a connect command");
        };
        assert_eq!(ap.ssid, "home");
        assert_eq!(password.as_deref(), Some("p"));
    }
}

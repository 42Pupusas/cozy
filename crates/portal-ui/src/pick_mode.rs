use portal_core::ChooserKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickMode {
    Files { multiple: bool },
    Directories { multiple: bool },
}

impl PickMode {
    #[must_use]
    pub const fn from_kind(kind: &ChooserKind) -> Option<Self> {
        match *kind {
            ChooserKind::Open {
                multiple,
                directory: false,
            } => Some(Self::Files { multiple }),
            ChooserKind::Open {
                multiple,
                directory: true,
            } => Some(Self::Directories { multiple }),
            ChooserKind::Save { .. } | ChooserKind::SaveMany { .. } => None,
        }
    }

    #[must_use]
    pub const fn multiple(self) -> bool {
        match self {
            Self::Files { multiple } | Self::Directories { multiple } => multiple,
        }
    }

    #[must_use]
    pub const fn wants_directories(self) -> bool {
        matches!(self, Self::Directories { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_open_request_picks_files() {
        let kind = ChooserKind::Open {
            multiple: true,
            directory: false,
        };
        assert_eq!(
            PickMode::from_kind(&kind),
            Some(PickMode::Files { multiple: true })
        );
    }

    #[test]
    fn a_directory_request_picks_directories() {
        let kind = ChooserKind::Open {
            multiple: false,
            directory: true,
        };
        let mode = PickMode::from_kind(&kind).expect("mode");
        assert!(mode.wants_directories());
        assert!(!mode.multiple());
    }

    #[test]
    fn save_requests_have_no_pick_mode() {
        let save = ChooserKind::Save {
            name: None,
            file: None,
        };
        assert_eq!(PickMode::from_kind(&save), None);
        assert_eq!(
            PickMode::from_kind(&ChooserKind::SaveMany { names: vec![] }),
            None
        );
    }
}

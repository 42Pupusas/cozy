use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reaction {
    Ignored,
    Redraw,
    Moved,
    Open(PathBuf),
}

impl Reaction {
    #[must_use]
    pub const fn redraw_if(changed: bool) -> Self {
        if changed { Self::Redraw } else { Self::Ignored }
    }
}

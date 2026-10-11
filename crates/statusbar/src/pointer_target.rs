//! Which of the bar's surfaces a pointer event concerns.

use platform::id::SurfaceId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerTarget {
    Bar,
    Popup,
    Elsewhere,
}

impl PointerTarget {
    #[must_use]
    pub fn classify(focus: Option<SurfaceId>, bar: SurfaceId, popup: Option<SurfaceId>) -> Self {
        match focus {
            Some(s) if s == bar => Self::Bar,
            Some(s) if Some(s) == popup => Self::Popup,
            _ => Self::Elsewhere,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surfaces_classify_by_identity() {
        let bar = SurfaceId(3);
        let popup = Some(SurfaceId(9));
        assert_eq!(PointerTarget::classify(Some(bar), bar, popup), PointerTarget::Bar);
        assert_eq!(
            PointerTarget::classify(popup, bar, popup),
            PointerTarget::Popup
        );
        assert_eq!(
            PointerTarget::classify(Some(SurfaceId(9)), bar, None),
            PointerTarget::Elsewhere
        );
        assert_eq!(PointerTarget::classify(None, bar, popup), PointerTarget::Elsewhere);
    }
}

//! How the frozen buffer is made to cover the overlay surface exactly.

/// The surface state that makes a `buffer` sized capture cover a `surface`
/// sized overlay.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BackdropFit {
    /// `wp_viewport.set_destination` to the surface size; handles any scale.
    Viewport(i32, i32),
    /// `wl_surface.set_buffer_scale`, for integer scales without a viewporter.
    BufferScale(i32),
}

impl BackdropFit {
    /// Choose a fit, or `None` when the sizes cannot be reconciled.
    #[must_use]
    pub fn choose(buffer: (u32, u32), surface: (i32, i32), has_viewporter: bool) -> Option<Self> {
        let (sw, sh) = surface;
        if sw <= 0 || sh <= 0 {
            return None;
        }
        if has_viewporter {
            return Some(Self::Viewport(sw, sh));
        }
        let bw = i32::try_from(buffer.0).ok()?;
        let bh = i32::try_from(buffer.1).ok()?;
        let scale = bw / sw;
        (scale >= 1 && sw * scale == bw && sh * scale == bh).then_some(Self::BufferScale(scale))
    }
}

#[cfg(test)]
mod tests {
    use super::BackdropFit;

    #[test]
    fn a_viewporter_fits_any_scale() {
        assert_eq!(
            BackdropFit::choose((1920, 1080), (1536, 864), true),
            Some(BackdropFit::Viewport(1536, 864))
        );
    }

    #[test]
    fn without_a_viewporter_an_integer_scale_still_fits() {
        assert_eq!(
            BackdropFit::choose((3840, 2160), (1920, 1080), false),
            Some(BackdropFit::BufferScale(2))
        );
        assert_eq!(
            BackdropFit::choose((1920, 1080), (1920, 1080), false),
            Some(BackdropFit::BufferScale(1))
        );
    }

    #[test]
    fn without_a_viewporter_a_fractional_scale_does_not_fit() {
        assert_eq!(BackdropFit::choose((1920, 1080), (1536, 864), false), None);
    }

    #[test]
    fn an_unconfigured_surface_does_not_fit() {
        assert_eq!(BackdropFit::choose((1920, 1080), (0, 0), true), None);
    }
}

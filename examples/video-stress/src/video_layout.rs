//! Where a clip sits inside an output, in pixels.
//!
//! An output has square pixels, so the fit is a plain aspect comparison.
//! The clip keeps its shape and the unused pixels stay as padding, which
//! makes the letterbox bars part of the layout rather than a paint accident.

/// How a clip fills the surface it plays on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FitMode {
    /// Show the whole clip, padding the shorter axis.
    #[default]
    Fit,
    /// Cover the whole surface, cropping the longer axis.
    Fill,
    /// Ignore the clip aspect and use every pixel.
    Stretch,
}

impl FitMode {
    /// Parse the name used on the command line.
    pub fn parse(name: &str) -> Result<Self, String> {
        match name {
            "fit" => Ok(Self::Fit),
            "fill" => Ok(Self::Fill),
            "stretch" => Ok(Self::Stretch),
            other => Err(format!(
                "unknown fit mode: {other} (expected fit, fill, or stretch)"
            )),
        }
    }

    /// The name used on the command line.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Fit => "fit",
            Self::Fill => "fill",
            Self::Stretch => "stretch",
        }
    }
}

/// The destination rectangle of a clip on one surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoLayout {
    surface_width: u32,
    surface_height: u32,
    x: i64,
    y: i64,
    width: u32,
    height: u32,
}

impl VideoLayout {
    /// Place a `source_width` by `source_height` clip on a surface.
    #[must_use]
    pub fn place(
        surface_width: u32,
        surface_height: u32,
        source_width: u32,
        source_height: u32,
        mode: FitMode,
    ) -> Self {
        let (width, height) =
            Self::extent(surface_width, surface_height, source_width, source_height, mode);
        Self {
            surface_width,
            surface_height,
            x: (i64::from(surface_width) - i64::from(width)) / 2,
            y: (i64::from(surface_height) - i64::from(height)) / 2,
            width,
            height,
        }
    }

    /// The scaled size of the clip before centring.
    fn extent(
        surface_width: u32,
        surface_height: u32,
        source_width: u32,
        source_height: u32,
        mode: FitMode,
    ) -> (u32, u32) {
        if source_width == 0 || source_height == 0 || mode == FitMode::Stretch {
            return (surface_width, surface_height);
        }
        let wide = u64::from(surface_width) * u64::from(source_height);
        let tall = u64::from(surface_height) * u64::from(source_width);
        let width_limited = match mode {
            FitMode::Fill => wide < tall,
            _ => wide > tall,
        };
        if width_limited {
            let width = tall.max(1) / u64::from(source_height).max(1);
            (Self::clamp(width), surface_height)
        } else {
            let height = wide.max(1) / u64::from(source_width).max(1);
            (surface_width, Self::clamp(height))
        }
    }

    /// Narrow a computed extent, never returning zero pixels.
    fn clamp(extent: u64) -> u32 {
        u32::try_from(extent).unwrap_or(u32::MAX).max(1)
    }

    /// The left edge, which is negative when the clip is cropped.
    #[must_use]
    pub const fn x(&self) -> i64 {
        self.x
    }

    /// The top edge, which is negative when the clip is cropped.
    #[must_use]
    pub const fn y(&self) -> i64 {
        self.y
    }

    /// The drawn width in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// The drawn height in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// The surface width in pixels.
    #[must_use]
    pub const fn surface_width(&self) -> u32 {
        self.surface_width
    }

    /// The surface height in pixels.
    #[must_use]
    pub const fn surface_height(&self) -> u32 {
        self.surface_height
    }

    /// Whether any surface pixel falls outside the clip.
    #[must_use]
    pub const fn has_padding(&self) -> bool {
        self.width < self.surface_width || self.height < self.surface_height
    }

    /// The source column that feeds surface column `x`, if the clip covers it.
    #[must_use]
    pub fn source_column(&self, x: u32, source_width: u32) -> Option<u32> {
        Self::source_axis(i64::from(x) - self.x, self.width, source_width)
    }

    /// The source row that feeds surface row `y`, if the clip covers it.
    #[must_use]
    pub fn source_row(&self, y: u32, source_height: u32) -> Option<u32> {
        Self::source_axis(i64::from(y) - self.y, self.height, source_height)
    }

    /// Map one surface axis offset onto its source coordinate.
    fn source_axis(offset: i64, extent: u32, source_extent: u32) -> Option<u32> {
        if offset < 0 || offset >= i64::from(extent) || extent == 0 {
            return None;
        }
        let scaled = offset.unsigned_abs() * u64::from(source_extent);
        let source = scaled / u64::from(extent).max(1);
        u32::try_from(source).ok().filter(|s| *s < source_extent)
    }
}

#[cfg(test)]
mod tests {
    use super::{FitMode, VideoLayout};

    #[test]
    fn a_wide_surface_pads_the_sides() {
        let layout = VideoLayout::place(1920, 1080, 4, 3, FitMode::Fit);
        assert_eq!((layout.width(), layout.height()), (1440, 1080));
        assert_eq!(layout.x(), 240);
        assert_eq!(layout.y(), 0);
        assert!(layout.has_padding());
    }

    #[test]
    fn a_tall_surface_pads_the_top_and_bottom() {
        let layout = VideoLayout::place(1000, 1000, 4, 3, FitMode::Fit);
        assert_eq!((layout.width(), layout.height()), (1000, 750));
        assert_eq!(layout.y(), 125);
    }

    #[test]
    fn a_matching_aspect_leaves_no_padding() {
        let layout = VideoLayout::place(1600, 1200, 4, 3, FitMode::Fit);
        assert_eq!((layout.width(), layout.height()), (1600, 1200));
        assert!(!layout.has_padding());
    }

    #[test]
    fn filling_covers_the_surface_and_crops() {
        let layout = VideoLayout::place(1920, 1080, 4, 3, FitMode::Fill);
        assert!(layout.width() >= 1920);
        assert!(layout.height() >= 1080);
        assert!(layout.y() <= 0);
        assert!(!layout.has_padding());
    }

    #[test]
    fn stretching_uses_every_pixel() {
        let layout = VideoLayout::place(1920, 1080, 4, 3, FitMode::Stretch);
        assert_eq!((layout.width(), layout.height()), (1920, 1080));
        assert_eq!((layout.x(), layout.y()), (0, 0));
    }

    #[test]
    fn padded_columns_report_no_source() {
        let layout = VideoLayout::place(1920, 1080, 4, 3, FitMode::Fit);
        assert_eq!(layout.source_column(0, 240), None);
        assert_eq!(layout.source_column(1919, 240), None);
        assert!(layout.source_column(960, 240).is_some());
    }

    #[test]
    fn the_clip_edges_map_to_the_source_edges() {
        let layout = VideoLayout::place(800, 600, 4, 3, FitMode::Fit);
        assert_eq!(layout.source_column(0, 240), Some(0));
        assert_eq!(layout.source_row(0, 180), Some(0));
        assert_eq!(layout.source_column(799, 240), Some(239));
        assert_eq!(layout.source_row(599, 180), Some(179));
    }

    #[test]
    fn every_source_coordinate_stays_in_range() {
        let layout = VideoLayout::place(1000, 1000, 4, 3, FitMode::Fit);
        for x in 0..1000 {
            if let Some(column) = layout.source_column(x, 240) {
                assert!(column < 240, "column {column} at x {x}");
            }
        }
        for y in 0..1000 {
            if let Some(row) = layout.source_row(y, 180) {
                assert!(row < 180, "row {row} at y {y}");
            }
        }
    }

    #[test]
    fn a_degenerate_clip_uses_the_whole_surface() {
        let layout = VideoLayout::place(800, 600, 0, 0, FitMode::Fit);
        assert_eq!((layout.width(), layout.height()), (800, 600));
    }

    #[test]
    fn fit_mode_names_round_trip() {
        for mode in [FitMode::Fit, FitMode::Fill, FitMode::Stretch] {
            assert_eq!(FitMode::parse(mode.name()), Ok(mode));
        }
        assert!(FitMode::parse("cover").is_err());
    }
}

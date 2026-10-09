//! [`SectionFill`] -- what a section's buffer shows: one colour stretched
//! to the section, or a rounded corner of that colour.

use super::quarter::Quarter;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SectionFill {
    Solid(u32),
    /// A `side`×`side` corner mask, turned to `quarter`.
    Corner {
        argb: u32,
        side: u16,
        quarter: Quarter,
    },
}

impl SectionFill {
    /// The `wl_output.transform` the section's buffer needs.
    #[must_use]
    pub const fn transform(self) -> i32 {
        match self {
            Self::Solid(_) => 0,
            Self::Corner { quarter, .. } => quarter.transform(),
        }
    }
}

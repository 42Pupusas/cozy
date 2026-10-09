//! [`Quarter`] -- which corner of a rounded rect a corner section shows.
//! Every corner reuses the top-left mask, turned by a buffer transform.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Quarter {
    TopLeft,
    TopRight,
    BottomRight,
    BottomLeft,
}

impl Quarter {
    /// The `wl_output.transform` that shows the top-left mask as this
    /// corner: the buffer is the surface turned counter-clockwise by
    /// `transform` quarter turns, so the compositor turns it back clockwise.
    #[must_use]
    pub const fn transform(self) -> i32 {
        match self {
            Self::TopLeft => 0,
            Self::TopRight => 1,
            Self::BottomRight => 2,
            Self::BottomLeft => 3,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Quarter;

    #[test]
    fn each_corner_is_one_more_clockwise_turn() {
        let turns: Vec<i32> = [
            Quarter::TopLeft,
            Quarter::TopRight,
            Quarter::BottomRight,
            Quarter::BottomLeft,
        ]
        .iter()
        .map(|q| q.transform())
        .collect();
        assert_eq!(turns, [0, 1, 2, 3]);
    }
}

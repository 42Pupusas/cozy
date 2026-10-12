use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeShape {
    Wide,
    Deep,
    Bushy,
}

impl TreeShape {
    pub const ALL: [Self; 3] = [Self::Wide, Self::Deep, Self::Bushy];

    pub const FILE_BYTES: usize = 64;

    #[must_use]
    pub const fn fanout(self) -> u32 {
        match self {
            Self::Wide => 0,
            Self::Deep => 1,
            Self::Bushy => 5,
        }
    }

    #[must_use]
    pub const fn depth(self) -> u32 {
        match self {
            Self::Wide => 0,
            Self::Deep => 32,
            Self::Bushy => 4,
        }
    }

    #[must_use]
    pub const fn files_per_dir(self) -> u32 {
        match self {
            Self::Wide => 4096,
            Self::Deep | Self::Bushy => 4,
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Wide => "wide",
            Self::Deep => "deep",
            Self::Bushy => "bushy",
        }
    }
}

impl fmt::Display for TreeShape {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

//! [`SizeLabel`] -- memory sizes as the short strings tiles display.

pub struct SizeLabel;

impl SizeLabel {
    const MB: u64 = 1024;
    const GB: u64 = 1024 * 1024;

    #[must_use]
    pub fn kb(kb: u64) -> String {
        if kb >= Self::GB {
            format!("{}.{} GB", kb / Self::GB, kb % Self::GB * 10 / Self::GB)
        } else if kb >= Self::MB {
            format!("{} MB", kb / Self::MB)
        } else {
            format!("{kb} KB")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SizeLabel;

    #[test]
    fn sizes_use_the_largest_whole_unit() {
        assert_eq!(SizeLabel::kb(0), "0 KB");
        assert_eq!(SizeLabel::kb(1023), "1023 KB");
        assert_eq!(SizeLabel::kb(1024), "1 MB");
        assert_eq!(SizeLabel::kb(340 * 1024 + 500), "340 MB");
        assert_eq!(SizeLabel::kb(1024 * 1024), "1.0 GB");
        assert_eq!(SizeLabel::kb(1024 * 1024 * 3 / 2), "1.5 GB");
    }
}

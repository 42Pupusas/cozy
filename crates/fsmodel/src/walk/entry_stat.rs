use ququmatz::StatxMask;
use ququmatz::types::Statx;

/// The fields of one `statx` result a directory scan needs, decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryStat {
    pub is_dir: bool,
    pub device: u64,
    pub inode: u64,
    pub links: u32,
    pub bytes: u64,
}

impl EntryStat {
    pub const MASK: StatxMask = StatxMask::TYPE
        .union_const(StatxMask::NLINK)
        .union_const(StatxMask::INO)
        .union_const(StatxMask::SIZE);

    const S_IFMT: u16 = 0o170_000;
    const S_IFDIR: u16 = 0o040_000;

    /// Decode `raw`, or `None` if the kernel left a needed field unfilled.
    #[must_use]
    pub const fn of(raw: &Statx) -> Option<Self> {
        if !raw.has(Self::MASK) {
            return None;
        }
        Some(Self {
            is_dir: raw.stx_mode & Self::S_IFMT == Self::S_IFDIR,
            device: Self::device_number(raw.stx_dev_major, raw.stx_dev_minor),
            inode: raw.stx_ino,
            links: raw.stx_nlink,
            bytes: raw.stx_size,
        })
    }

    /// The `dev_t` encoding glibc's `makedev` produces, which is what
    /// `std::fs::Metadata::dev` reports.
    #[must_use]
    pub const fn device_number(major: u32, minor: u32) -> u64 {
        let major = major as u64;
        let minor = minor as u64;
        ((major & 0xffff_f000) << 32)
            | ((major & 0x0000_0fff) << 8)
            | ((minor & 0xffff_ff00) << 12)
            | (minor & 0x0000_00ff)
    }
}

#[cfg(test)]
mod tests {
    use super::EntryStat;

    #[test]
    fn small_numbers_pack_like_the_classic_encoding() {
        assert_eq!(EntryStat::device_number(8, 1), 0x0801);
        assert_eq!(EntryStat::device_number(0, 0), 0);
    }

    #[test]
    fn large_numbers_use_the_split_high_bits() {
        assert_eq!(
            EntryStat::device_number(0x1234_5678, 0x9abc_def0),
            0x1234_5000_0000_0000 | 0x0000_0000_0006_7800 | 0x0000_09ab_cde0_0000 | 0xf0
        );
    }
}

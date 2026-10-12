#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkedInode {
    pub device: u64,
    pub inode: u64,
    pub bytes: u64,
}

impl LinkedInode {
    #[must_use]
    pub const fn key(self) -> (u64, u64) {
        (self.device, self.inode)
    }
}

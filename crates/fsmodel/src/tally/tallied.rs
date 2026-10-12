use std::path::PathBuf;

/// A root's byte total so far. `settled` marks the final total; earlier
/// reports only ever grow toward it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tallied {
    pub path: PathBuf,
    pub bytes: u64,
    pub settled: bool,
}

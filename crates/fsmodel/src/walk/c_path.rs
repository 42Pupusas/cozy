use std::ffi::CStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// A reusable nul-terminated copy of a path, so handing paths to syscalls
/// does not allocate once the buffer has grown to fit.
#[derive(Debug, Default)]
pub struct CPath {
    bytes: Vec<u8>,
}

impl CPath {
    #[must_use]
    pub const fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    /// `path` as a C string, or `None` if it holds an interior nul.
    pub fn of(&mut self, path: &Path) -> Option<&CStr> {
        self.bytes.clear();
        self.bytes.extend_from_slice(path.as_os_str().as_bytes());
        self.bytes.push(0);
        CStr::from_bytes_with_nul(&self.bytes).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::CPath;
    use std::path::Path;

    #[test]
    fn a_path_gains_exactly_one_trailing_nul() {
        let mut c_path = CPath::new();
        let c = c_path.of(Path::new("/tmp/a b")).expect("no interior nul");
        assert_eq!(c.to_bytes_with_nul(), b"/tmp/a b\0");
    }

    #[test]
    fn a_shorter_path_does_not_keep_the_previous_tail() {
        let mut c_path = CPath::new();
        c_path.of(Path::new("/a/long/path"));
        let c = c_path.of(Path::new("/x")).expect("no interior nul");
        assert_eq!(c.to_bytes(), b"/x");
    }

    #[test]
    fn an_interior_nul_is_refused() {
        let mut c_path = CPath::new();
        assert!(c_path.of(Path::new("a\0b")).is_none());
    }
}

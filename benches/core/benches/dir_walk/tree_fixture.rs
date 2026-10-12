use std::path::{Path, PathBuf};

use crate::tree_shape::TreeShape;

pub struct TreeFixture {
    root: PathBuf,
    shape: TreeShape,
}

impl TreeFixture {
    #[must_use]
    pub fn build(shape: TreeShape) -> Self {
        let root = std::env::temp_dir().join(format!(
            "cozy-dir-walk-{}-{}",
            shape.label(),
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create fixture root");
        let fixture = Self { root, shape };
        fixture.fill(&fixture.root, shape.depth());
        fixture
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn children_or_root(&self) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(&self.root)
            .expect("read fixture root")
            .flatten()
            .filter(|item| item.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|item| item.path())
            .collect();
        if dirs.is_empty() {
            dirs.push(self.root.clone());
        }
        dirs.sort();
        dirs
    }

    #[must_use]
    pub fn expected_bytes(&self) -> u64 {
        let files = u64::from(self.shape.files_per_dir()) * self.dir_count();
        files * TreeShape::FILE_BYTES as u64
    }

    #[must_use]
    pub fn expected_child_bytes(&self) -> u64 {
        let root_files = u64::from(self.shape.files_per_dir()) * TreeShape::FILE_BYTES as u64;
        if self.shape.fanout() == 0 {
            self.expected_bytes()
        } else {
            self.expected_bytes() - root_files
        }
    }

    fn dir_count(&self) -> u64 {
        let fanout = u64::from(self.shape.fanout());
        (0..=self.shape.depth())
            .map(|level| fanout.pow(level))
            .sum()
    }

    fn fill(&self, dir: &Path, remaining: u32) {
        let contents = [0_u8; TreeShape::FILE_BYTES];
        for index in 0..self.shape.files_per_dir() {
            std::fs::write(dir.join(format!("f{index:05}")), contents).expect("write fixture file");
        }
        if remaining == 0 {
            return;
        }
        for index in 0..self.shape.fanout() {
            let child = dir.join(format!("d{index:02}"));
            std::fs::create_dir(&child).expect("create fixture dir");
            self.fill(&child, remaining - 1);
        }
    }
}

impl Drop for TreeFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

use std::ffi::CStr;
use std::path::Path;

use super::dir_contents::DirContents;
use super::name_buffers::NameBuffers;
use super::name_chunk::NameChunk;

/// Groups the names of one directory into [`NameChunk`]s of at most `limit`
/// names each, adding every full chunk to a [`DirContents`].
#[derive(Debug)]
pub struct ChunkPacker<'d> {
    dir: &'d Path,
    limit: usize,
    names: Vec<u8>,
    count: usize,
}

impl<'d> ChunkPacker<'d> {
    #[must_use]
    pub const fn new(dir: &'d Path, limit: usize) -> Self {
        Self {
            dir,
            limit,
            names: Vec::new(),
            count: 0,
        }
    }

    pub fn push(&mut self, name: &CStr, buffers: &mut NameBuffers, contents: &mut DirContents) {
        if self.count == 0 {
            self.names = buffers.take();
        }
        self.names.extend_from_slice(name.to_bytes_with_nul());
        self.count += 1;
        if self.count >= self.limit {
            self.flush(contents);
        }
    }

    pub fn finish(mut self, contents: &mut DirContents) {
        if self.count > 0 {
            self.flush(contents);
        }
    }

    fn flush(&mut self, contents: &mut DirContents) {
        let names = std::mem::take(&mut self.names);
        contents.push_chunk(NameChunk::new(self.dir.to_path_buf(), names));
        self.count = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::ChunkPacker;
    use crate::walk::DirContents;
    use crate::walk::name_buffers::NameBuffers;
    use std::path::Path;

    fn packed(limit: usize, names: &[&std::ffi::CStr]) -> DirContents {
        let mut buffers = NameBuffers::new();
        let mut contents = DirContents::new();
        let mut packer = ChunkPacker::new(Path::new("/d"), limit);
        for name in names {
            packer.push(name, &mut buffers, &mut contents);
        }
        packer.finish(&mut contents);
        contents
    }

    #[test]
    fn names_fill_chunks_up_to_the_limit_then_spill_into_a_new_one() {
        let contents = packed(2, &[c"a", c"b", c"c"]);
        let chunks: Vec<Vec<_>> = contents
            .chunks()
            .iter()
            .map(|chunk| chunk.names().collect())
            .collect();
        assert_eq!(chunks, [vec![c"a", c"b"], vec![c"c"]]);
        assert!(
            contents
                .chunks()
                .iter()
                .all(|chunk| chunk.dir() == Path::new("/d"))
        );
    }

    #[test]
    fn no_names_make_no_chunks() {
        assert!(packed(4, &[]).chunks().is_empty());
    }
}

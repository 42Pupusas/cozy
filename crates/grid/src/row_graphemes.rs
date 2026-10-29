//! Per-row grapheme cluster store.
//!
//! A cell whose glyph is more than one codepoint keeps the whole cluster
//! here and stores a tagged offset in the character plane instead of a
//! scalar value. Each row owns its store and clears it with the row, like
//! [`crate::row_styles::RowStyles`]. Replacing a cluster leaves the old one
//! unused; when the store reaches its limit it is rebuilt from the row's
//! live cluster cells: O(cols), local to the row.

/// Clusters packed one after another, each as a length followed by its
/// codepoints. One `Vec` keeps the per-row header small.
#[derive(Default)]
pub struct RowGraphemes {
    pool: Vec<char>,
}

impl RowGraphemes {
    /// Longest cluster kept. Bounds the memory a flood of stacked combining
    /// marks can take; codepoints past it are dropped.
    pub const MAX_LEN: usize = 32;

    /// Set on a character-plane value that is a cluster offset. No Unicode
    /// scalar value reaches this bit.
    const TAG: u32 = 1 << 31;

    /// Whether a character-plane value refers to a cluster.
    pub const fn is_cluster(raw: u32) -> bool {
        raw & Self::TAG != 0
    }

    /// The cluster a character-plane value refers to, if it is one.
    pub fn get(&self, raw: u32) -> Option<&[char]> {
        Self::is_cluster(raw).then(|| {
            let start = (raw & !Self::TAG) as usize;
            let len = self.pool[start] as usize;
            &self.pool[start + 1..=start + len]
        })
    }

    pub const fn is_empty(&self) -> bool {
        self.pool.is_empty()
    }

    pub fn clear(&mut self) {
        self.pool.clear();
    }

    /// Become a copy of `other`, reusing this store's allocation.
    pub fn copy_from(&mut self, other: &Self) {
        self.pool.clone_from(&other.pool);
    }

    /// Store `cluster` for column `col` of the row whose character plane is
    /// `chars`, writing the tagged offset there. `cluster` is at most
    /// [`Self::MAX_LEN`] long.
    pub fn insert(&mut self, cluster: &[char], chars: &mut [u32], col: usize) {
        chars[col] = 0;
        if self.pool.len() + cluster.len() >= (Self::MAX_LEN + 1) * chars.len() {
            self.rebuild(chars);
        }
        chars[col] = self.push(cluster);
    }

    /// Keep only the clusters `chars` still refers to, rewriting their
    /// offsets. Each cell owns its own cluster, so none are shared.
    fn rebuild(&mut self, chars: &mut [u32]) {
        let old = std::mem::take(self);
        for raw in chars.iter_mut() {
            if let Some(cluster) = old.get(*raw) {
                *raw = self.push(cluster);
            }
        }
    }

    fn push(&mut self, cluster: &[char]) -> u32 {
        let start =
            u32::try_from(self.pool.len()).expect("row grapheme pool stays under its limit");
        let len = u8::try_from(cluster.len()).expect("clusters are capped at MAX_LEN");
        self.pool.push(char::from(len));
        self.pool.extend_from_slice(cluster);
        start | Self::TAG
    }
}

#[cfg(test)]
mod tests {
    use super::RowGraphemes;

    #[test]
    fn stored_cluster_reads_back_and_scalars_are_not_clusters() {
        let mut store = RowGraphemes::default();
        let mut chars = [u32::from('a'); 4];
        store.insert(&['e', '\u{301}'], &mut chars, 2);
        assert_eq!(store.get(chars[2]), Some(&['e', '\u{301}'][..]));
        assert_eq!(store.get(chars[0]), None);
    }

    #[test]
    fn full_store_keeps_live_clusters_and_drops_the_rest() {
        let mut store = RowGraphemes::default();
        let mut chars = [0u32; 2];
        store.insert(&['a', '\u{301}'], &mut chars, 1);
        // Replace column 0's cluster over and over; column 1 keeps its own.
        let mark = |n: u32| char::from_u32(0x300 + n % 16).unwrap();
        for n in 0..1000 {
            store.insert(&['x', mark(n)], &mut chars, 0);
            assert!(store.pool.len() <= 2 * (RowGraphemes::MAX_LEN + 1));
        }
        assert_eq!(store.get(chars[0]), Some(&['x', mark(999)][..]));
        assert_eq!(store.get(chars[1]), Some(&['a', '\u{301}'][..]));
    }
}

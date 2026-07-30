//! SIMD scanner for printable-ASCII runs.
//!
//! The Ground-state fast path in [`crate::Parser::feed`] needs one primitive:
//! "how many leading bytes of this slice are printable ASCII (`0x20..=0x7E`)?"
//! Floods spend >95% of their bytes answering that question, so it is worth
//! answering 16 or 32 bytes at a time instead of one.
//!
//! # Vectorization of the predicate
//!
//! `b ∈ 0x20..=0x7E` has no single-instruction test, but shifting the range
//! down does: let `x = b.wrapping_sub(0x20)`; then `b` is printable iff
//! `x <= 0x5E` *unsigned*. SSE2/AVX2 lack unsigned byte compares, but have
//! unsigned byte `max`: `max_epu8(x, 0x5F) == x` is exactly `x >= 0x5F`,
//! i.e. "terminator here". `movemask` turns that into a bitmask whose
//! `trailing_zeros` is the run length within the block.
//!
//! # Dispatch
//!
//! SSE2 is part of the `x86_64` baseline, so the 16-byte path needs no runtime
//! detection. AVX2 is probed once per [`Scanner::new`] via
//! `is_x86_feature_detected!` (itself a cached atomic load); `Parser::feed`
//! constructs one `Scanner` per call, so detection cost is per-chunk, never
//! per-byte. Non-`x86_64` targets get a scalar twin with the same signature —
//! callers never see the difference.

/// Leading-run scanner for printable ASCII (`0x20..=0x7E`).
///
/// Construct once per `feed` call (construction is a cached CPU-feature
/// probe), then call [`Scanner::printable_run_len`] per run.
#[derive(Clone, Copy)]
pub struct Scanner {
    /// AVX2 available at runtime. Kept as a plain bool so the per-run branch
    /// predicts perfectly; a fn-pointer dispatch would defeat inlining.
    #[cfg(target_arch = "x86_64")]
    avx2: bool,
}

impl Scanner {
    /// First value outside the shifted printable range: `0x7E - 0x20 + 1`.
    /// After `x = b - 0x20`, any `x >= STOP_SHIFTED` marks a terminator.
    const STOP_SHIFTED: u8 = 0x5F;

    /// Probe CPU features (`x86_64`: AVX2; SSE2 is assumed as baseline).
    #[cfg(target_arch = "x86_64")]
    pub fn new() -> Self {
        Self {
            avx2: std::arch::is_x86_feature_detected!("avx2"),
        }
    }

    /// Non-`x86_64`: nothing to probe; scalar path only.
    #[cfg(not(target_arch = "x86_64"))]
    pub fn new() -> Self {
        Self {}
    }

    /// Length of the leading run of printable ASCII bytes (`0x20..=0x7E`)
    /// in `bytes`. Returns `bytes.len()` when the whole slice is printable.
    #[cfg(target_arch = "x86_64")]
    #[inline]
    pub fn printable_run_len(self, bytes: &[u8]) -> usize {
        if self.avx2 {
            // SAFETY: `avx2` was set by `is_x86_feature_detected!("avx2")`.
            unsafe { Self::run_len_avx2(bytes) }
        } else {
            Self::run_len_sse2(bytes)
        }
    }

    /// Scalar fallback for non-x86_64 targets.
    #[cfg(not(target_arch = "x86_64"))]
    #[inline]
    pub fn printable_run_len(self, bytes: &[u8]) -> usize {
        Self::run_len_scalar(bytes)
    }

    /// Scalar reference implementation: also the tail loop for the SIMD
    /// paths and the oracle the tests compare against.
    #[inline]
    fn run_len_scalar(bytes: &[u8]) -> usize {
        bytes
            .iter()
            .position(|&b| !matches!(b, 0x20..=0x7E))
            .unwrap_or(bytes.len())
    }

    /// SSE2 path: 16 bytes per iteration. SSE2 is unconditionally present on
    /// `x86_64`, so this is safe to call without detection; only the unaligned
    /// load itself needs `unsafe`.
    #[cfg(target_arch = "x86_64")]
    #[inline]
    // `loadu` explicitly permits unaligned pointers; the cast is the idiomatic
    // way to feed it (see the intrinsic's own docs), so the lint is a false
    // positive here.
    #[allow(clippy::cast_ptr_alignment)]
    fn run_len_sse2(bytes: &[u8]) -> usize {
        use std::arch::x86_64::{
            __m128i, _mm_cmpeq_epi8, _mm_loadu_si128, _mm_max_epu8, _mm_movemask_epi8,
            _mm_set1_epi8, _mm_sub_epi8,
        };
        let n = bytes.len();
        let mut i = 0;
        while i + 16 <= n {
            // SAFETY: `i + 16 <= n` guarantees the 16 bytes at
            // `bytes[i..]` are in-bounds; `loadu` has no alignment requirement.
            unsafe {
                let v = _mm_loadu_si128(bytes.as_ptr().add(i).cast::<__m128i>());
                let x = _mm_sub_epi8(v, _mm_set1_epi8(0x20));
                // stop = bytes where x >= STOP_SHIFTED (unsigned), i.e. the
                // original byte was outside 0x20..=0x7E.
                let stop = _mm_cmpeq_epi8(
                    _mm_max_epu8(x, _mm_set1_epi8(Self::STOP_SHIFTED.cast_signed())),
                    x,
                );
                let mask = _mm_movemask_epi8(stop).cast_unsigned();
                if mask != 0 {
                    return i + mask.trailing_zeros() as usize;
                }
            }
            i += 16;
        }
        i + Self::run_len_scalar(&bytes[i..])
    }

    /// AVX2 path: 32 bytes per iteration. Caller must have verified AVX2.
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2")]
    // Same `loadu` false positive as the SSE2 path.
    #[allow(clippy::cast_ptr_alignment)]
    unsafe fn run_len_avx2(bytes: &[u8]) -> usize {
        use std::arch::x86_64::{
            __m256i, _mm256_cmpeq_epi8, _mm256_loadu_si256, _mm256_max_epu8, _mm256_movemask_epi8,
            _mm256_set1_epi8, _mm256_sub_epi8,
        };
        let n = bytes.len();
        let mut i = 0;
        while i + 32 <= n {
            // SAFETY: `i + 32 <= n` keeps every load in-bounds;
            // `loadu` tolerates any alignment.
            unsafe {
                let v = _mm256_loadu_si256(bytes.as_ptr().add(i).cast::<__m256i>());
                let x = _mm256_sub_epi8(v, _mm256_set1_epi8(0x20));
                let stop = _mm256_cmpeq_epi8(
                    _mm256_max_epu8(x, _mm256_set1_epi8(Self::STOP_SHIFTED.cast_signed())),
                    x,
                );
                let mask = _mm256_movemask_epi8(stop).cast_unsigned();
                if mask != 0 {
                    return i + mask.trailing_zeros() as usize;
                }
            }
            i += 32;
        }
        // Tail < 32 bytes: finish with the SSE2 path (handles its own < 16
        // tail via scalar), keeping the terminator logic in one place.
        i + Self::run_len_sse2(&bytes[i..])
    }
}

#[cfg(test)]
mod tests {
    use super::Scanner;

    /// Independent oracle so a bug in `run_len_scalar` can't self-certify.
    fn oracle(bytes: &[u8]) -> usize {
        let mut i = 0;
        while i < bytes.len() && bytes[i] >= 0x20 && bytes[i] <= 0x7E {
            i += 1;
        }
        i
    }

    #[test]
    fn every_byte_value_classifies_like_the_oracle() {
        let s = Scanner::new();
        for b in 0u8..=255 {
            // Embed the probe byte mid-run so the SIMD path sees it inside
            // a full block, not just in the scalar tail.
            let mut buf = [b'a'; 64];
            buf[37] = b;
            assert_eq!(s.printable_run_len(&buf), oracle(&buf), "byte {b:#04x}");
        }
    }

    #[test]
    fn terminator_at_every_offset_up_to_three_blocks() {
        let s = Scanner::new();
        for len in 0..100 {
            for stop in 0..=len {
                let mut buf = vec![b'x'; len];
                if stop < len {
                    buf[stop] = 0x1B; // ESC
                }
                assert_eq!(
                    s.printable_run_len(&buf),
                    oracle(&buf),
                    "len {len} stop {stop}"
                );
            }
        }
    }

    #[test]
    fn wholly_printable_slices_return_full_length() {
        let s = Scanner::new();
        for len in [0, 1, 15, 16, 17, 31, 32, 33, 63, 64, 100, 4096] {
            let buf = vec![b' '; len];
            assert_eq!(s.printable_run_len(&buf), len);
        }
    }

    #[test]
    fn boundary_bytes_space_tilde_and_neighbours() {
        let s = Scanner::new();
        // 0x1F (below), 0x20 (space, in), 0x7E (tilde, in), 0x7F (DEL, out)
        assert_eq!(s.printable_run_len(&[0x1F; 40]), 0);
        assert_eq!(s.printable_run_len(&[0x20; 40]), 40);
        assert_eq!(s.printable_run_len(&[0x7E; 40]), 40);
        assert_eq!(s.printable_run_len(&[0x7F; 40]), 0);
    }

    #[test]
    fn misaligned_starts_scan_correctly() {
        let s = Scanner::new();
        let mut buf = [b'q'; 128];
        buf[100] = b'\n';
        for start in 0..40 {
            assert_eq!(
                s.printable_run_len(&buf[start..]),
                oracle(&buf[start..]),
                "start {start}"
            );
        }
    }
}

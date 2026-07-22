//! SIMD premultiply kernel for custom-painted (translucent) surfaces.
//!
//! [`crate::window::TermWindow::update_custom_argb`] rebuilds the whole
//! window's ARGB cache on every present for any custom painter (e.g.
//! `monitor`) running under a translucent renderer (`bg_alpha < 255`,
//! see [`crate::render::Renderer::needs_alpha`]). Profiling a live
//! translucent session found this loop costing 18-30ms/frame -- by far
//! the largest single cost in the present path, dwarfing the ~0.2ms actual
//! dashboard paint.
//!
//! The first version of this kernel batched the *stores* into one
//! 128/256-bit write per 4/8 pixels but still gathered each pixel's three
//! channels through a scalar 256-entry LUT per lane before packing --
//! i.e. it was SIMD-shaped but not actually vectorized arithmetic, and
//! measured basically the same as the scalar loop it replaced (the gather
//! was always the bottleneck, not the store). This version instead widens
//! all four bytes of each pixel to 16-bit lanes, multiplies by alpha, and
//! divides by 255 with a branch-free integer identity -- all in vector
//! registers, no scalar gather at all except in the sub-lane-width tail.
//!
//! The div-by-255 identity used throughout (`((x+1) + ((x+1)>>8)) >> 8 ==
//! x / 255` for `0 <= x <= 65025 = 255*255`) is a standard fixed-point
//! alpha-compositing trick; see the `div255_matches_floor_division_over_the_full_domain`
//! test for a proof-by-exhaustion over every reachable product.

/// `floor(x / 255)` via the branch-free fixed-point identity, shared by
/// every kernel (scalar included) so all paths agree on rounding exactly.
/// Valid for `x <= 255 * 255` -- the max product of two `u8`s -- which is
/// exactly the domain `channel * alpha` produces.
#[inline]
const fn div255(x: u32) -> u32 {
    let a = x + 1;
    (a + (a >> 8)) >> 8
}

/// Premultiplies a whole slice of packed `0x00RRGGBB` pixels into
/// `0xAARRGGBB`, using the fastest kernel this CPU supports.
///
/// Constructed once per [`crate::window::TermWindow`] present (detection is
/// a cached atomic load, but even that is once-per-frame, not once-per-pixel)
/// and reused across the frame's premultiply call.
#[derive(Clone, Copy)]
pub struct Premultiplier {
    #[cfg(target_arch = "x86_64")]
    avx2: bool,
}

impl Premultiplier {
    /// Probe CPU features once. `x86_64` always has SSE2; AVX2 needs a
    /// runtime check.
    #[cfg(target_arch = "x86_64")]
    #[must_use]
    pub fn new() -> Self {
        Self {
            avx2: std::arch::is_x86_feature_detected!("avx2"),
        }
    }

    #[cfg(not(target_arch = "x86_64"))]
    #[must_use]
    pub fn new() -> Self {
        Self {}
    }

    /// Premultiplies `src[i]` into `dst[i]` for every element. `src` and
    /// `dst` must be the same length; a length mismatch degrades to the
    /// shorter of the two rather than panicking, since this always runs on
    /// a matched row-slice pair from `update_custom_argb`.
    #[cfg(target_arch = "x86_64")]
    #[inline]
    pub fn run(self, alpha: u8, src: &[u32], dst: &mut [u32]) {
        let n = src.len().min(dst.len());
        let (src, dst) = (&src[..n], &mut dst[..n]);
        if self.avx2 {
            // SAFETY: `avx2` was set by `is_x86_feature_detected!("avx2")`.
            unsafe { Self::run_avx2(alpha, src, dst) };
        } else {
            Self::run_sse2(alpha, src, dst);
        }
    }

    #[cfg(not(target_arch = "x86_64"))]
    #[inline]
    pub fn run(self, alpha: u8, src: &[u32], dst: &mut [u32]) {
        Self::run_scalar(alpha, src, dst);
    }

    /// Scalar reference path: also the tail loop for the vector kernels and
    /// the oracle the tests compare against. Multiplies all four bytes
    /// uniformly (including the always-zero top byte) rather than
    /// unpacking to per-channel `u8`s, so it stays bit-for-bit identical to
    /// what the vector kernels compute per lane.
    #[inline]
    fn run_scalar(alpha: u8, src: &[u32], dst: &mut [u32]) {
        let alpha = u32::from(alpha);
        let alpha_shifted = alpha << 24;
        for (source, dest) in src.iter().zip(dst.iter_mut()) {
            let pixel = *source & 0x00ff_ffff;
            let red = div255(((pixel >> 16) & 0xff) * alpha) << 16;
            let green = div255(((pixel >> 8) & 0xff) * alpha) << 8;
            let blue = div255((pixel & 0xff) * alpha);
            *dest = alpha_shifted | red | green | blue;
        }
    }

    /// SSE2 path: 4 pixels (16 bytes) per iteration. Widens every byte of
    /// all 4 pixels to a 16-bit lane (`punpcklbw`/`punpckhbw` against an
    /// all-zero register), multiplies every lane by alpha in one
    /// `pmullw`, applies [`div255`] with vector shifts/adds, then narrows
    /// back to bytes with `packuswb` -- real SIMD arithmetic, not a
    /// vectorized store around a scalar gather.
    #[cfg(target_arch = "x86_64")]
    #[inline]
    #[allow(clippy::cast_ptr_alignment)] // `loadu`/`storeu` explicitly permit unaligned pointers.
    fn run_sse2(alpha: u8, src: &[u32], dst: &mut [u32]) {
        use std::arch::x86_64::{
            __m128i, _mm_loadu_si128, _mm_mullo_epi16, _mm_or_si128, _mm_packus_epi16,
            _mm_set1_epi16, _mm_set1_epi32, _mm_setzero_si128, _mm_storeu_si128,
            _mm_unpackhi_epi8, _mm_unpacklo_epi8,
        };

        let alpha_shifted = u32::from(alpha) << 24;
        let mut i = 0;
        // SAFETY: SSE2 is unconditionally available on `x86_64`; every load/
        // store below stays within `i + 4 <= src.len()`, checked by the loop
        // guard, and `loadu`/`storeu` tolerate any alignment.
        unsafe {
            let zero = _mm_setzero_si128();
            let alpha16 = _mm_set1_epi16(i16::from(alpha));
            let alpha_lane = _mm_set1_epi32(alpha_shifted.cast_signed());
            let one = _mm_set1_epi16(1);
            while i + 4 <= src.len() {
                let pixels: __m128i = _mm_loadu_si128(src.as_ptr().add(i).cast::<__m128i>());
                let lo = _mm_unpacklo_epi8(pixels, zero);
                let hi = _mm_unpackhi_epi8(pixels, zero);
                let lo = Self::div255_vec_sse2(_mm_mullo_epi16(lo, alpha16), one);
                let hi = Self::div255_vec_sse2(_mm_mullo_epi16(hi, alpha16), one);
                let packed = _mm_packus_epi16(lo, hi);
                let out = _mm_or_si128(packed, alpha_lane);
                _mm_storeu_si128(dst.as_mut_ptr().add(i).cast::<__m128i>(), out);
                i += 4;
            }
        }
        Self::run_scalar(alpha, &src[i..], &mut dst[i..]);
    }

    /// Vector [`div255`]: `((x+1) + ((x+1)>>8)) >> 8` applied to all eight
    /// 16-bit lanes at once.
    #[cfg(target_arch = "x86_64")]
    #[inline]
    unsafe fn div255_vec_sse2(
        x: std::arch::x86_64::__m128i,
        one: std::arch::x86_64::__m128i,
    ) -> std::arch::x86_64::__m128i {
        use std::arch::x86_64::{_mm_add_epi16, _mm_srli_epi16};
        // SAFETY: SSE2 is unconditionally available on `x86_64`; this is pure
        // lane-wise arithmetic with no memory access.
        unsafe {
            let a = _mm_add_epi16(x, one);
            let b = _mm_srli_epi16(a, 8);
            _mm_srli_epi16(_mm_add_epi16(a, b), 8)
        }
    }

    /// AVX2 path: 8 pixels (32 bytes) per iteration, same widen/multiply/
    /// div255/narrow strategy as [`Self::run_sse2`] but doubling the lane
    /// width. Caller must have verified AVX2.
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2")]
    #[allow(clippy::cast_ptr_alignment)] // `loadu`/`storeu` explicitly permit unaligned pointers.
    unsafe fn run_avx2(alpha: u8, src: &[u32], dst: &mut [u32]) {
        use std::arch::x86_64::{
            __m256i, _mm256_loadu_si256, _mm256_mullo_epi16, _mm256_or_si256, _mm256_packus_epi16,
            _mm256_set1_epi16, _mm256_set1_epi32, _mm256_setzero_si256, _mm256_storeu_si256,
            _mm256_unpackhi_epi8, _mm256_unpacklo_epi8,
        };

        let alpha_shifted = u32::from(alpha) << 24;
        let mut i = 0;
        // SAFETY: caller verified AVX2; every load/store stays within
        // `i + 8 <= src.len()`, checked by the loop guard, and `loadu`/
        // `storeu` tolerate any alignment.
        unsafe {
            let zero = _mm256_setzero_si256();
            let alpha16 = _mm256_set1_epi16(i16::from(alpha));
            let alpha_lane = _mm256_set1_epi32(alpha_shifted.cast_signed());
            let one = _mm256_set1_epi16(1);
            while i + 8 <= src.len() {
                let pixels: __m256i = _mm256_loadu_si256(src.as_ptr().add(i).cast::<__m256i>());
                let lo = _mm256_unpacklo_epi8(pixels, zero);
                let hi = _mm256_unpackhi_epi8(pixels, zero);
                let lo = Self::div255_vec_avx2(_mm256_mullo_epi16(lo, alpha16), one);
                let hi = Self::div255_vec_avx2(_mm256_mullo_epi16(hi, alpha16), one);
                let packed = _mm256_packus_epi16(lo, hi);
                let out = _mm256_or_si256(packed, alpha_lane);
                _mm256_storeu_si256(dst.as_mut_ptr().add(i).cast::<__m256i>(), out);
                i += 8;
            }
        }
        Self::run_sse2(alpha, &src[i..], &mut dst[i..]);
    }

    /// Vector [`div255`] over sixteen 16-bit lanes (AVX2 width).
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2")]
    unsafe fn div255_vec_avx2(
        x: std::arch::x86_64::__m256i,
        one: std::arch::x86_64::__m256i,
    ) -> std::arch::x86_64::__m256i {
        use std::arch::x86_64::{_mm256_add_epi16, _mm256_srli_epi16};
        // This fn is already `unsafe` via `#[target_feature(enable =
        // "avx2")]`; the intrinsics below are pure lane-wise arithmetic
        // with no memory access, so no nested `unsafe` block is needed.
        let a = _mm256_add_epi16(x, one);
        let b = _mm256_srli_epi16(a, 8);
        _mm256_srli_epi16(_mm256_add_epi16(a, b), 8)
    }
}

impl Default for Premultiplier {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{div255, Premultiplier};

    /// Independent oracle so a bug in the shared div255 path can't
    /// self-certify; matches `TermWindow::premultiply_pixel`'s original
    /// per-channel-division formula exactly (true integer division, not
    /// the fixed-point identity under test).
    fn oracle(pixel: u32, alpha: u8) -> u32 {
        let alpha = u32::from(alpha);
        let rgb = pixel & 0x00ff_ffff;
        let red = ((rgb >> 16) & 0xff) * alpha / 255;
        let green = ((rgb >> 8) & 0xff) * alpha / 255;
        let blue = (rgb & 0xff) * alpha / 255;
        (alpha << 24) | (red << 16) | (green << 8) | blue
    }

    #[test]
    fn div255_matches_floor_division_over_the_full_domain() {
        // Exhaustive: `channel * alpha` is always in `0..=255*255`, so this
        // covers every input the vector/scalar kernels can ever produce.
        for channel in 0u32..=255 {
            for alpha in 0u32..=255 {
                let product = channel * alpha;
                assert_eq!(
                    div255(product),
                    product / 255,
                    "channel {channel} alpha {alpha} product {product}"
                );
            }
        }
    }

    #[test]
    fn every_alpha_and_a_spread_of_pixels_matches_the_division_oracle() {
        let pre = Premultiplier::new();
        let pixels: Vec<u32> = (0..97)
            .map(|i| {
                let red = (i * 7) % 256;
                let green = (i * 13) % 256;
                let blue = (i * 29) % 256;
                (red << 16) | (green << 8) | blue
            })
            .collect();
        for alpha in [0u8, 1, 17, 128, 200, 254, 255] {
            let mut dst = vec![0u32; pixels.len()];
            pre.run(alpha, &pixels, &mut dst);
            for (i, (&px, &got)) in pixels.iter().zip(dst.iter()).enumerate() {
                assert_eq!(got, oracle(px, alpha), "pixel {i} alpha {alpha}");
            }
        }
    }

    #[test]
    fn lengths_that_are_not_multiples_of_the_vector_width_still_cover_every_pixel() {
        let pre = Premultiplier::new();
        // Exercise remainders against 4 and 8 (SSE2/AVX2 lane widths) plus
        // one arbitrary "medium" length.
        for len in [0u32, 1, 3, 4, 5, 7, 8, 9, 15, 16, 17, 100] {
            let pixels: Vec<u32> = (0..len)
                .map(|i| 0x00_010203u32.wrapping_mul(i + 1))
                .collect();
            let mut dst = vec![0u32; len as usize];
            pre.run(180, &pixels, &mut dst);
            for (i, (&px, &got)) in pixels.iter().zip(dst.iter()).enumerate() {
                assert_eq!(got, oracle(px, 180), "len {len} idx {i}");
            }
        }
    }

    #[test]
    fn mismatched_src_dst_lengths_do_not_panic_and_cover_the_shorter_length() {
        let pre = Premultiplier::new();
        let src = vec![0x00_112233u32; 20];
        let mut dst = vec![0u32; 5];
        pre.run(100, &src, &mut dst);
        for &got in &dst {
            assert_eq!(got, oracle(0x00_112233, 100));
        }
    }
}

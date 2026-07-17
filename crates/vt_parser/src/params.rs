#![allow(clippy::missing_const_for_fn)]
//! CSI / DCS parameter accumulator.
//!
//! Stores up to 16 numeric parameters (semicolon-separated), each up to
//! `u16::MAX`. Allocation-free — backed by a fixed-size array.

/// Maximum number of CSI parameters we track.
const MAX_PARAMS: usize = 16;

/// Accumulated CSI/DCS numeric parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Params {
    buf: [u16; MAX_PARAMS],
    /// Number of parameter slots seen so far (including empty ones created
    /// by a `;` separator). This is what callers see via `len()`/`get_or`.
    len: usize,
    /// Index of the slot currently receiving digits (or that would receive
    /// them next). Distinct from `len` so that a leading/embedded empty
    /// field (e.g. the row in `CSI ;5H`) doesn't get overwritten by the
    /// digits of the *next* field.
    cur: usize,
}

impl Params {
    pub(crate) const fn new() -> Self {
        Self {
            buf: [0; MAX_PARAMS],
            len: 0,
            cur: 0,
        }
    }

    /// Reset for a new sequence.
    pub(crate) fn clear(&mut self) {
        self.buf = [0; MAX_PARAMS];
        self.len = 0;
        self.cur = 0;
    }

    /// Feed a digit character into the current parameter.
    #[inline]
    pub(crate) fn push_digit(&mut self, digit: u8) {
        if self.len <= self.cur {
            self.len = self.cur + 1;
        }
        self.buf[self.cur] = self.buf[self.cur]
            .saturating_mul(10)
            .saturating_add(u16::from(digit - b'0'));
    }

    /// Fold a run of consecutive ASCII digits into the current parameter in
    /// one shot. Hoists the `len`/`cur` bookkeeping and the slot load/store
    /// out of the per-digit loop so the accumulator stays in a register —
    /// this is the batched fast path used by `Parser::feed` for the digit
    /// runs that dominate SGR-heavy streams (truecolor, 256-color).
    ///
    /// `digits` must contain only bytes in `b'0'..=b'9'`.
    #[inline]
    pub(crate) fn push_digits(&mut self, digits: &[u8]) {
        if digits.is_empty() {
            return;
        }
        if self.len <= self.cur {
            self.len = self.cur + 1;
        }
        let mut v = self.buf[self.cur];
        for &d in digits {
            v = v.saturating_mul(10).saturating_add(u16::from(d - b'0'));
        }
        self.buf[self.cur] = v;
    }

    /// Terminate the current parameter and start a new one.
    pub(crate) fn next_param(&mut self) {
        if self.cur + 1 < MAX_PARAMS {
            self.cur += 1;
        }
        if self.len <= self.cur {
            self.len = self.cur + 1;
        }
    }

    /// Number of parameters accumulated.
    #[must_use]
    pub fn len(&self) -> usize {
        // len == 0 means no parameters at all.
        // len >= 1 after at least one digit or separator.
        self.len
    }

    /// True when no parameters have been seen.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Get parameter `i`, returning `default` if missing or zero.
    #[must_use]
    pub fn get_or(&self, i: usize, default: u16) -> u16 {
        if i < self.len && self.buf[i] != 0 {
            self.buf[i]
        } else {
            default
        }
    }

    /// Get parameter `i` raw (0 if missing — use for params where 0 is valid).
    #[must_use]
    pub fn get_raw(&self, i: usize) -> u16 {
        if i < self.len { self.buf[i] } else { 0 }
    }

    /// Iterate over raw parameter values.
    pub fn iter(&self) -> impl Iterator<Item = u16> + '_ {
        self.buf[..self.len].iter().copied()
    }
}

impl Default for Params {
    fn default() -> Self {
        Self::new()
    }
}

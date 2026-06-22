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
    len: usize,
    /// True if the current parameter was explicitly set (vs. defaulted).
    has_value: bool,
}

impl Params {
    pub(crate) const fn new() -> Self {
        Self {
            buf: [0; MAX_PARAMS],
            len: 0,
            has_value: false,
        }
    }

    /// Reset for a new sequence.
    pub(crate) fn clear(&mut self) {
        self.buf = [0; MAX_PARAMS];
        self.len = 0;
        self.has_value = false;
    }

    /// Feed a digit character into the current parameter.
    pub(crate) fn push_digit(&mut self, digit: u8) {
        if self.len == 0 {
            self.len = 1;
        }
        let idx = self.len - 1;
        self.buf[idx] = self.buf[idx]
            .saturating_mul(10)
            .saturating_add(u16::from(digit - b'0'));
        self.has_value = true;
    }

    /// Terminate the current parameter and start a new one.
    pub(crate) fn next_param(&mut self) {
        if self.len < MAX_PARAMS {
            self.len += 1;
        }
        self.has_value = false;
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
        if i < self.len {
            self.buf[i]
        } else {
            0
        }
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

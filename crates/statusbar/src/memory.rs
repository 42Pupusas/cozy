//! Used/total RAM, from `/proc/meminfo`.
//!
//! A marker type (like [`crate::volume::Volume`]) rather than a value: every
//! call is a fresh `/proc/meminfo` read.

use std::fs;

/// Memory reads -- see the module doc for why this is a marker type.
pub struct Memory;

impl Memory {
    /// Used/total RAM in GiB, from `/proc/meminfo`.
    #[must_use]
    pub fn read() -> Option<String> {
        let text = fs::read_to_string("/proc/meminfo").ok()?;
        let mut total_kb = None;
        let mut avail_kb = None;
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("MemTotal:") {
                total_kb = Self::parse_kb(rest);
            } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
                avail_kb = Self::parse_kb(rest);
            }
        }
        let (total, avail) = (total_kb?, avail_kb?);
        let used_gib = (total - avail) as f64 / 1024.0 / 1024.0;
        let total_gib = total as f64 / 1024.0 / 1024.0;
        Some(format!("{used_gib:>4.1}/{total_gib:>4.1}GiB 󰅛"))
    }

    /// Parse a `/proc/meminfo` value field (`"   16384000 kB"`) to a raw
    /// kB count.
    fn parse_kb(s: &str) -> Option<u64> {
        s.trim()
            .strip_suffix(" kB")
            .unwrap_or(s.trim())
            .trim()
            .parse()
            .ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_kb_strips_suffix_and_whitespace() {
        assert_eq!(Memory::parse_kb("   16384000 kB"), Some(16_384_000));
        assert_eq!(Memory::parse_kb("1024 kB"), Some(1024));
    }

    #[test]
    fn parse_kb_without_suffix_still_parses() {
        assert_eq!(Memory::parse_kb("  2048  "), Some(2048));
    }

    #[test]
    fn parse_kb_garbage_is_none() {
        assert_eq!(Memory::parse_kb("not a number"), None);
        assert_eq!(Memory::parse_kb(""), None);
    }
}

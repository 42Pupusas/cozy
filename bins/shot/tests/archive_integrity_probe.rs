//! A probe over the *real* screenshot archive on this machine.
//!
//! Not a unit test of any one type: it decodes whatever is actually sitting in
//! the archive directory and reports which files are truncated. Corruption
//! that only shows up on real captures is invisible to the synthetic 2x2
//! images the writer's own tests use, so this exists to turn "some files are
//! corrupt" into a concrete list with byte counts.
//!
//! Skips silently when the directory is absent so it stays harmless in CI.

use ixiptla_core::Png;

/// Real screenshots are megabytes once decoded to RGBA.
const MAX_DECOMPRESSED: usize = 512 << 20;

/// Reports on the integrity of the archive directory.
struct ArchiveProbe {
    root: std::path::PathBuf,
}

impl ArchiveProbe {
    fn from_environment() -> Option<Self> {
        let root = match std::env::var_os("SHOT_DIR") {
            Some(dir) => std::path::PathBuf::from(dir),
            None => std::path::PathBuf::from(std::env::var_os("HOME")?).join("Images/Screenshots"),
        };
        root.is_dir().then_some(Self { root })
    }

    /// Every `.png` in the archive, sorted so the report reads chronologically.
    fn images(&self) -> Vec<std::path::PathBuf> {
        let mut found: Vec<_> = std::fs::read_dir(&self.root)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|e| e == "png"))
            .collect();
        found.sort();
        found
    }

    /// Describe one file: its size, whether it ends in IEND, and whether the
    /// pixels decode. Truncation shows up as a missing IEND long before the
    /// decoder is reached, which distinguishes "cut short" from "scrambled".
    fn describe(path: &std::path::Path) -> String {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let Ok(bytes) = std::fs::read(path) else {
            return format!("{name}: UNREADABLE");
        };
        let signature = bytes.starts_with(b"\x89PNG\r\n\x1a\n");
        // The last chunk of a complete PNG is always IEND; its 4-byte CRC
        // follows, so the marker sits 8 bytes from the end.
        let terminated = bytes.len() >= 12 && bytes[bytes.len() - 8..bytes.len() - 4] == *b"IEND";
        let decoded = Png::decode(&bytes, MAX_DECOMPRESSED);
        // Report structure and decodability *independently*. A file that ends
        // in IEND but fails to inflate was written completely and is corrupt
        // in its compressed payload -- which rules out a truncating writer and
        // points at the encoder instead.
        let verdict = match (&decoded, signature, terminated) {
            (Ok(_), true, true) => "ok".to_string(),
            (Err(e), _, true) => format!("COMPLETE FILE but DECODE FAILED: {e:?}"),
            (Err(e), _, false) => format!("TRUNCATED (no IEND) and DECODE FAILED: {e:?}"),
            (Ok(_), _, false) => "TRUNCATED (no IEND)".to_string(),
            (Ok(_), false, _) => "BAD SIGNATURE".to_string(),
        };
        format!(
            "{name}: {} bytes, sig={signature} iend={terminated}, {verdict}",
            bytes.len()
        )
    }
}

#[test]
fn every_archived_screenshot_is_a_complete_png() {
    let Some(probe) = ArchiveProbe::from_environment() else {
        eprintln!("no archive directory; nothing to probe");
        return;
    };
    let images = probe.images();
    eprintln!(
        "probing {} image(s) in {}",
        images.len(),
        probe.root.display()
    );
    let mut bad = Vec::new();
    for path in &images {
        let line = ArchiveProbe::describe(path);
        eprintln!("  {line}");
        if !line.ends_with("ok") {
            bad.push(line);
        }
    }
    assert!(
        bad.is_empty(),
        "corrupt archived screenshots:\n{}",
        bad.join("\n")
    );
}

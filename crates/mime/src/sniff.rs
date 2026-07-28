//! Content sniffing: identifying a file from its leading bytes.
//!
//! Needed because extensions lie and are often absent. `README`, `LICENSE`,
//! a downloaded `blob`, an executable in `~/.local/bin` -- none carry an
//! extension, and all are common things to find in a directory.
//!
//! # Why a built-in table, not `/usr/share/mime/magic`
//!
//! The system `magic` file is a binary format with nested offset rules,
//! indirect offsets, and byte masks -- a genuine parser, and one whose
//! failure modes are silent misidentification. The value curve is steep:
//! a few dozen well-known signatures cover essentially everything a user
//! actually double-clicks, and the honest fallback for the rest is the
//! text/binary heuristic below. So this table is deliberately small,
//! hand-audited, and offset-0 only (plus a couple of documented exceptions).

use crate::mime_type::MimeType;

/// One magic-byte signature.
struct Signature {
    /// Byte offset the pattern starts at. Almost always 0; RIFF/ISO-BMFF
    /// containers need a small non-zero offset for their subtype tag.
    offset: usize,
    /// The bytes that must be present at `offset`.
    magic: &'static [u8],
    /// The type implied by a match.
    mime: &'static str,
}

/// Identifies file types from content.
///
/// A struct rather than free functions so the signature table has an owner,
/// and so a future "load the system magic database too" can appear as a
/// field without touching call sites.
#[derive(Debug, Clone, Copy, Default)]
pub struct Sniffer;

/// How many bytes we read to make a decision.
///
/// 512 covers every signature in the table with room to spare, and gives
/// the text heuristic a solid sample. Deliberately small: sniffing happens
/// per-file when a directory has extensionless entries, so the read must be
/// one cheap block, not a whole file.
pub const SNIFF_LEN: usize = 512;

/// Signatures, checked in order. Longer/more specific patterns first so a
/// generic container can't shadow a precise match.
const SIGNATURES: &[Signature] = &[
    // --- Images ---
    Signature {
        offset: 0,
        magic: b"\x89PNG\r\n\x1a\n",
        mime: "image/png",
    },
    Signature {
        offset: 0,
        magic: b"\xff\xd8\xff",
        mime: "image/jpeg",
    },
    Signature {
        offset: 0,
        magic: b"GIF87a",
        mime: "image/gif",
    },
    Signature {
        offset: 0,
        magic: b"GIF89a",
        mime: "image/gif",
    },
    Signature {
        offset: 0,
        magic: b"BM",
        mime: "image/bmp",
    },
    Signature {
        offset: 0,
        magic: b"\x00\x00\x01\x00",
        mime: "image/vnd.microsoft.icon",
    },
    // WebP is RIFF with a "WEBP" tag at byte 8; the RIFF header alone is
    // ambiguous (WAV and AVI share it), hence the offset check.
    Signature {
        offset: 8,
        magic: b"WEBP",
        mime: "image/webp",
    },
    Signature {
        offset: 8,
        magic: b"WAVE",
        mime: "audio/x-wav",
    },
    Signature {
        offset: 8,
        magic: b"AVI ",
        mime: "video/x-msvideo",
    },
    // --- Documents / archives ---
    Signature {
        offset: 0,
        magic: b"%PDF-",
        mime: "application/pdf",
    },
    // ZIP: also the container for odt/docx/jar/apk. Distinguishing those
    // needs central-directory inspection, which is out of scope -- the
    // extension lookup handles them, and this is the honest fallback.
    Signature {
        offset: 0,
        magic: b"PK\x03\x04",
        mime: "application/zip",
    },
    Signature {
        offset: 0,
        magic: b"\x1f\x8b",
        mime: "application/gzip",
    },
    Signature {
        offset: 0,
        magic: b"BZh",
        mime: "application/x-bzip",
    },
    Signature {
        offset: 0,
        magic: b"\xfd7zXZ\x00",
        mime: "application/x-xz",
    },
    Signature {
        offset: 0,
        magic: b"\x28\xb5\x2f\xfd",
        mime: "application/zstd",
    },
    Signature {
        offset: 0,
        magic: b"7z\xbc\xaf\x27\x1c",
        mime: "application/x-7z-compressed",
    },
    Signature {
        offset: 0,
        magic: b"Rar!\x1a\x07",
        mime: "application/vnd.rar",
    },
    // `ustar` at offset 257 is the POSIX tar magic.
    Signature {
        offset: 257,
        magic: b"ustar",
        mime: "application/x-tar",
    },
    // --- Executables ---
    Signature {
        offset: 0,
        magic: b"\x7fELF",
        mime: "application/x-sharedlib",
    },
    Signature {
        offset: 0,
        magic: b"MZ",
        mime: "application/x-dosexec",
    },
    // --- Audio / video ---
    Signature {
        offset: 0,
        magic: b"OggS",
        mime: "audio/ogg",
    },
    Signature {
        offset: 0,
        magic: b"fLaC",
        mime: "audio/flac",
    },
    Signature {
        offset: 0,
        magic: b"ID3",
        mime: "audio/mpeg",
    },
    Signature {
        offset: 4,
        magic: b"ftyp",
        mime: "video/mp4",
    },
    Signature {
        offset: 0,
        magic: b"\x1a\x45\xdf\xa3",
        mime: "video/x-matroska",
    },
    // --- Text-ish, but with a definite signature ---
    Signature {
        offset: 0,
        magic: b"#!",
        mime: "application/x-shellscript",
    },
    Signature {
        offset: 0,
        magic: b"<?xml",
        mime: "application/xml",
    },
    Signature {
        offset: 0,
        magic: b"%!PS",
        mime: "application/postscript",
    },
    Signature {
        offset: 0,
        magic: b"SQLite format 3\x00",
        mime: "application/vnd.sqlite3",
    },
];

impl Sniffer {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Identify `data` (a file's leading bytes) by signature.
    ///
    /// Returns `None` when nothing matches -- ask [`Self::sniff`] instead if
    /// you want the text/binary fallback too.
    #[must_use]
    pub fn signature(self, data: &[u8]) -> Option<MimeType> {
        SIGNATURES
            .iter()
            .find(|sig| {
                data.len() >= sig.offset + sig.magic.len()
                    && &data[sig.offset..sig.offset + sig.magic.len()] == sig.magic
            })
            .and_then(|sig| MimeType::parse(sig.mime))
    }

    /// Identify `data`, falling back to a text/binary decision.
    ///
    /// Never returns `None`: an unidentifiable file is either `text/plain`
    /// or `application/octet-stream`, both of which are useful answers.
    #[must_use]
    pub fn sniff(self, data: &[u8]) -> MimeType {
        if let Some(mime) = self.signature(data) {
            return mime;
        }
        if Self::looks_like_text(data) {
            MimeType::parse("text/plain").expect("well-formed constant")
        } else {
            MimeType::octet_stream()
        }
    }

    /// Read the head of a file and sniff it.
    ///
    /// Returns `None` if the file can't be opened -- distinct from "opened
    /// but unidentifiable", which yields a type.
    #[must_use]
    pub fn sniff_path(self, path: &std::path::Path) -> Option<MimeType> {
        use std::io::Read;
        let mut file = std::fs::File::open(path).ok()?;
        let mut buf = vec![0u8; SNIFF_LEN];
        // A short read is normal (small files) and fine; only a hard error
        // means we couldn't sniff.
        let n = file.read(&mut buf).ok()?;
        buf.truncate(n);
        Some(self.sniff(&buf))
    }

    /// Heuristic: does this look like human-readable text?
    ///
    /// Two rules, in order:
    /// 1. **A NUL byte means binary.** This is the classic test and it is
    ///    near-perfect in practice -- text encodings in real use don't emit
    ///    embedded NULs (UTF-16 would, but it's vanishingly rare on Linux
    ///    and carries a BOM when it appears).
    /// 2. Otherwise, valid UTF-8 is text.
    ///
    /// An empty file counts as text: an empty `.txt` is far more plausible
    /// than empty binary data, and it makes "open in editor" the offer.
    #[must_use]
    pub fn looks_like_text(data: &[u8]) -> bool {
        if data.is_empty() {
            return true;
        }
        if data.contains(&0) {
            return false;
        }
        match std::str::from_utf8(data) {
            Ok(_) => true,
            // A truncated multi-byte sequence at the end of our 512-byte
            // window is an artefact of *where we cut*, not evidence of
            // binary data -- so accept it if everything before it was valid.
            Err(e) => e.error_len().is_none() && e.valid_up_to() > 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sniff(data: &[u8]) -> String {
        Sniffer::new().sniff(data).to_string()
    }

    #[test]
    fn detects_png() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR"), "image/png");
    }

    #[test]
    fn detects_jpeg_and_gif() {
        assert_eq!(sniff(b"\xff\xd8\xff\xe0\x00\x10JFIF"), "image/jpeg");
        assert_eq!(sniff(b"GIF89a\x01\x00"), "image/gif");
    }

    #[test]
    fn detects_pdf_and_archives() {
        assert_eq!(sniff(b"%PDF-1.7\n%..."), "application/pdf");
        assert_eq!(sniff(b"PK\x03\x04\x14\x00"), "application/zip");
        assert_eq!(sniff(b"\x1f\x8b\x08\x00"), "application/gzip");
    }

    #[test]
    fn detects_elf_binaries() {
        assert_eq!(sniff(b"\x7fELF\x02\x01\x01\x00"), "application/x-sharedlib");
    }

    #[test]
    fn detects_shebang_scripts() {
        assert_eq!(
            sniff(b"#!/bin/sh\necho hello\n"),
            "application/x-shellscript"
        );
    }

    /// RIFF alone is ambiguous, so the subtype tag at offset 8 is what
    /// separates WebP from WAV from AVI.
    #[test]
    fn distinguishes_riff_containers_by_offset_tag() {
        assert_eq!(sniff(b"RIFF\x00\x00\x00\x00WEBPVP8 "), "image/webp");
        assert_eq!(sniff(b"RIFF\x00\x00\x00\x00WAVEfmt "), "audio/x-wav");
    }

    #[test]
    fn detects_tar_by_offset_257_magic() {
        let mut data = vec![0u8; 300];
        data[257..262].copy_from_slice(b"ustar");
        // A tar header is mostly NULs, so this also proves the signature
        // check runs *before* the text heuristic.
        assert_eq!(sniff(&data), "application/x-tar");
    }

    #[test]
    fn plain_text_falls_back_to_text_plain() {
        assert_eq!(sniff(b"Hello, world!\nJust some notes.\n"), "text/plain");
    }

    #[test]
    fn nul_bytes_mean_binary() {
        assert_eq!(
            sniff(b"some text\x00\x01\x02more"),
            "application/octet-stream"
        );
    }

    #[test]
    fn empty_input_is_treated_as_text() {
        assert_eq!(sniff(b""), "text/plain");
        assert!(Sniffer::looks_like_text(b""));
    }

    #[test]
    fn utf8_text_is_recognised() {
        assert_eq!(sniff("héllo wörld — nya~ 🐾".as_bytes()), "text/plain");
    }

    /// The 512-byte window can cut a multi-byte character in half; that is
    /// an artefact of our own truncation and must not be read as binary.
    #[test]
    fn truncated_utf8_at_the_window_edge_is_still_text() {
        let mut data = "hello ".repeat(20).into_bytes();
        // Push a 4-byte emoji and lop off its tail.
        data.extend_from_slice("🐾".as_bytes());
        data.truncate(data.len() - 2);
        assert!(Sniffer::looks_like_text(&data));
    }

    /// But genuinely invalid UTF-8 in the *middle* is not a truncation
    /// artefact and should read as binary.
    #[test]
    fn mid_stream_invalid_utf8_is_binary() {
        let data = b"hello \xc3\x28 world";
        assert!(!Sniffer::looks_like_text(data));
    }

    #[test]
    fn signature_returns_none_for_unknown_content() {
        assert!(Sniffer::new().signature(b"just some plain text").is_none());
    }

    #[test]
    fn short_input_does_not_panic_on_offset_signatures() {
        // Shorter than the offsets several signatures require -- the bounds
        // check must hold rather than slicing out of range.
        for len in 0..12 {
            let data = vec![b'R'; len];
            let _ = Sniffer::new().sniff(&data);
        }
    }

    #[test]
    fn sniffs_a_real_file_from_disk() {
        let path = std::env::temp_dir().join(format!("mime-sniff-test-{}", std::process::id()));
        std::fs::write(&path, b"\x89PNG\r\n\x1a\nrest").expect("write");
        let got = Sniffer::new().sniff_path(&path).map(|m| m.to_string());
        let _ = std::fs::remove_file(&path);
        assert_eq!(got, Some("image/png".to_owned()));
    }

    #[test]
    fn missing_file_yields_none() {
        assert!(
            Sniffer::new()
                .sniff_path(std::path::Path::new("/nonexistent/definitely/not/here"))
                .is_none()
        );
    }
}

//! Turning a captured frame into a PNG file (or stdout).
//!
//! `CapturedFrame` already hands back exactly what `ixiptla-core` wants --
//! tightly packed RGBA8 scanlines, no filter bytes -- which is why this module
//! is thin: the two crates were built to meet here.

use ixiptla_core::{ColorType, Encoder, Ihdr, InterlaceMethod};
use screencopy::CapturedFrame;
use std::io::Write;
use std::path::Path;

/// Encodes captures and puts the bytes somewhere.
pub struct ShotWriter;

impl ShotWriter {
    /// Encode `frame` as a PNG.
    ///
    /// # Errors
    ///
    /// Fails if the frame's dimensions and buffer disagree, which would mean
    /// a bug in the capture path rather than anything the user did.
    pub fn encode(frame: &CapturedFrame) -> Result<Vec<u8>, String> {
        let ihdr = Ihdr {
            width: frame.width(),
            height: frame.height(),
            // 8-bit RGBA: the compositor's formats are all 8 bits per channel,
            // and keeping alpha means an `Argb8888` capture of a translucent
            // surface survives intact.
            bit_depth: 8,
            color_type: ColorType::TruecolorAlpha,
            // Interlacing would only help progressive display over a slow
            // link; for a local screenshot it costs size and time.
            interlace: InterlaceMethod::None,
        };
        Encoder::new(ihdr)
            .encode(frame.rgba())
            .map_err(|e| format!("encoding PNG failed: {e}"))
    }

    /// Write `png` to `path`, or to stdout when `path` is `None`.
    ///
    /// # Errors
    ///
    /// Reports the underlying I/O failure with the destination named, since
    /// "permission denied" alone doesn't say which file.
    pub fn write(png: &[u8], path: Option<&Path>) -> Result<(), String> {
        path.map_or_else(
            || Self::write_to_stdout(png),
            |path| Self::write_to_file(png, path),
        )
    }

    /// Write to a named file, reporting *which* file on failure.
    fn write_to_file(png: &[u8], path: &Path) -> Result<(), String> {
        std::fs::write(path, png).map_err(|e| format!("writing {}: {e}", path.display()))
    }

    /// Write to stdout, flushing so a piped consumer sees the whole file.
    fn write_to_stdout(png: &[u8]) -> Result<(), String> {
        let stdout = std::io::stdout();
        let mut handle = stdout.lock();
        handle
            .write_all(png)
            .and_then(|()| handle.flush())
            .map_err(|e| format!("writing to stdout: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::ShotWriter;
    use ixiptla_core::Png;
    use screencopy::{CapturedFrame, FrameParams};

    /// Decompression bound for decoding our own tiny test images back; these
    /// are a few dozen bytes, so anything non-trivial is generous.
    const MAX_DECOMPRESSED: usize = 1 << 20;

    /// A 2x2 capture with four distinct, easily identified colours.
    fn sample_frame() -> CapturedFrame {
        // Xrgb8888 little-endian bytes are B, G, R, X.
        let source = vec![
            0x00, 0x00, 0xFF, 0x00, // red
            0x00, 0xFF, 0x00, 0x00, // green
            0xFF, 0x00, 0x00, 0x00, // blue
            0xFF, 0xFF, 0xFF, 0x00, // white
        ];
        let params = FrameParams::new(platform::protocol::Format::Xrgb8888.code(), 2, 2, 8)
            .expect("valid params");
        CapturedFrame::from_shm(&params, 0, &source).expect("converts")
    }

    #[test]
    fn encodes_a_capture_that_decodes_back_to_the_same_pixels() {
        // The real proof that the whole chain agrees: compositor bytes ->
        // CapturedFrame -> PNG -> decoded pixels. A channel-order mistake
        // anywhere in that path shows up here as swapped colours.
        let frame = sample_frame();
        let png = ShotWriter::encode(&frame).expect("encodes");
        let decoded =
            Png::decode(&png, MAX_DECOMPRESSED).expect("our own encoder's output must decode");
        assert_eq!(decoded.ihdr.width, 2);
        assert_eq!(decoded.ihdr.height, 2);
        assert_eq!(decoded.pixels, frame.rgba());
    }

    #[test]
    fn output_is_a_real_png_by_signature() {
        let png = ShotWriter::encode(&sample_frame()).expect("encodes");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn red_stays_red_through_the_full_encode_path() {
        // Guards the byte-order swap specifically: if BGRA->RGBA regressed,
        // this pixel comes back blue and every screenshot looks subtly wrong.
        let frame = sample_frame();
        let png = ShotWriter::encode(&frame).expect("encodes");
        let decoded = Png::decode(&png, MAX_DECOMPRESSED).expect("decodes");
        assert_eq!(&decoded.pixels[..4], &[0xFF, 0x00, 0x00, 0xFF]);
    }

    #[test]
    fn writes_bytes_to_the_named_file() {
        let path = std::env::temp_dir().join("shot-writer-test.png");
        let png = ShotWriter::encode(&sample_frame()).expect("encodes");
        ShotWriter::write(&png, Some(&path)).expect("writes");
        let written = std::fs::read(&path).expect("reads back");
        assert_eq!(written, png);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_unwritable_path_reports_the_destination() {
        // "permission denied" with no filename is a common and infuriating
        // failure mode; the message must name what it tried to write.
        let path = std::path::Path::new("/nonexistent-dir-xyz/shot.png");
        let error = ShotWriter::write(b"x", Some(path)).expect_err("must fail");
        assert!(error.contains("shot.png"), "unhelpful error: {error}");
    }
}

//! Drives ffmpeg and writes the packed clip file.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use crate::luma_packer::LumaPacker;
use crate::pack_header::{HEADER_BYTES, PackHeader};
use crate::prepare_options::PrepareOptions;

/// Converts a source video into a packed clip on disk.
///
/// ffmpeg decodes and resamples; this owner only thresholds and packs. The
/// harness therefore never links a codec, and playback measures the renderer
/// rather than a decoder.
pub struct PackWriter {
    options: PrepareOptions,
}

impl PackWriter {
    /// Build a writer for `options`.
    #[must_use]
    pub const fn new(options: PrepareOptions) -> Self {
        Self { options }
    }

    /// Convert the source and write the pack file. Returns the frame count.
    pub fn write(&self) -> Result<u32, String> {
        let frames = self.pack_frames()?;
        let frame_bytes = self.frame_stride();
        let count = u32::try_from(frames.len() / frame_bytes.max(1))
            .map_err(|_| "clip holds more frames than the header can describe".to_owned())?;
        if count == 0 {
            return Err("ffmpeg produced no complete frames".to_owned());
        }
        let header = PackHeader::new(
            self.options.width,
            self.options.height,
            count,
            self.options.fps,
        )
        .map_err(|error| error.to_string())?;
        Self::write_file(&self.options.output, &header, &frames)?;
        Ok(count)
    }

    /// Packed byte length of one frame.
    const fn frame_stride(&self) -> usize {
        self.options.width.div_ceil(8) as usize * self.options.height as usize
    }

    /// Run ffmpeg and pack every complete luma frame it emits.
    fn pack_frames(&self) -> Result<Vec<u8>, String> {
        let header = PackHeader::new(self.options.width, self.options.height, 0, self.options.fps)
            .map_err(|error| error.to_string())?;
        let packer = LumaPacker::new(header, self.options.threshold);
        let mut child = Command::new("ffmpeg")
            .args(self.options.ffmpeg_arguments())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| format!("failed to start ffmpeg: {error}"))?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| "ffmpeg produced no output pipe".to_owned())?;
        let mut luma = vec![0u8; self.options.luma_frame_bytes()];
        let mut frames = Vec::new();
        loop {
            match Self::read_full(&mut stdout, &mut luma) {
                Ok(true) => packer.pack_frame(&luma, &mut frames),
                Ok(false) => break,
                Err(error) => {
                    let _ = child.kill();
                    return Err(format!("failed to read ffmpeg output: {error}"));
                }
            }
        }
        let status = child
            .wait()
            .map_err(|error| format!("failed to wait for ffmpeg: {error}"))?;
        if !status.success() {
            return Err(format!("ffmpeg exited with {status}"));
        }
        Ok(frames)
    }

    /// Fill `buffer` completely, reporting whether a whole frame arrived.
    fn read_full(source: &mut impl Read, buffer: &mut [u8]) -> std::io::Result<bool> {
        let mut filled = 0usize;
        while filled < buffer.len() {
            match source.read(&mut buffer[filled..])? {
                0 => return Ok(false),
                count => filled += count,
            }
        }
        Ok(true)
    }

    /// Write the header and frame region to `path`.
    fn write_file(path: &Path, header: &PackHeader, frames: &[u8]) -> Result<(), String> {
        let mut file = std::fs::File::create(path)
            .map_err(|error| format!("failed to create {}: {error}", path.display()))?;
        let mut bytes = Vec::with_capacity(HEADER_BYTES + frames.len());
        bytes.extend_from_slice(&header.to_bytes());
        bytes.extend_from_slice(frames);
        file.write_all(&bytes)
            .map_err(|error| format!("failed to write {}: {error}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::{PackWriter, PrepareOptions};

    fn options() -> PrepareOptions {
        PrepareOptions::parse(
            ["--input", "a.mp4", "--width", "13", "--height", "4"].map(str::to_owned),
        )
        .unwrap()
    }

    #[test]
    fn frame_stride_rounds_rows_up_to_bytes() {
        assert_eq!(PackWriter::new(options()).frame_stride(), 8);
    }

    #[test]
    fn read_full_reports_a_complete_frame() {
        let mut source: &[u8] = &[1, 2, 3, 4];
        let mut buffer = [0u8; 4];
        assert!(PackWriter::read_full(&mut source, &mut buffer).unwrap());
        assert_eq!(buffer, [1, 2, 3, 4]);
    }

    #[test]
    fn read_full_rejects_a_partial_trailing_frame() {
        let mut source: &[u8] = &[1, 2];
        let mut buffer = [0u8; 4];
        assert!(!PackWriter::read_full(&mut source, &mut buffer).unwrap());
    }
}

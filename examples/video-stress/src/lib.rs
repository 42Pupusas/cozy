//! Source-agnostic monochrome video playback stress harness.
//!
//! The renderer under test is measured, not the decoder. Every frame is
//! therefore decoded ahead of time into a packed 1-bit-per-pixel form that
//! this crate unpacks with a table lookup. A 480x360 frame occupies 21600
//! bytes packed, so a five-minute clip stays resident in memory and playback
//! never touches the disk or a video codec.
//!
//! Nothing here depends on Wayland, so the pack format, the presentation
//! clock, and the statistics are unit-testable without a compositor.

mod ascii_ramp;
mod coverage_sampler;
mod dither_matrix;
mod font_report;
mod frame_pack;
mod frame_painter;
mod glyph_coverage;
mod luma_packer;
mod mono_frame;
mod pack_header;
mod pack_writer;
mod playback_clock;
mod playback_stats;
mod player_options;
mod prepare_options;
mod subcell_grid;
mod terminal_player;
mod terminal_size;
mod unpack_table;
mod video_layout;
mod video_palette;

pub use ascii_ramp::AsciiRamp;
pub use coverage_sampler::CoverageSampler;
pub use dither_matrix::DitherMatrix;
pub use font_report::{FontReport, ModeCoverage};
pub use frame_pack::{FramePack, PackError};
pub use frame_painter::FramePainter;
pub use glyph_coverage::GlyphCoverage;
pub use luma_packer::LumaPacker;
pub use mono_frame::MonoFrame;
pub use pack_header::{HeaderError, PackHeader};
pub use pack_writer::PackWriter;
pub use playback_clock::{PlaybackClock, Tick};
pub use playback_stats::PlaybackStats;
pub use player_options::PlayerOptions;
pub use prepare_options::PrepareOptions;
pub use subcell_grid::SubcellGrid;
pub use terminal_player::TerminalPlayer;
pub use terminal_size::TerminalSize;
pub use unpack_table::UnpackTable;
pub use video_layout::{FitMode, VideoLayout};
pub use video_palette::VideoPalette;

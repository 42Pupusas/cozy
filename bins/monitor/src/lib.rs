//! Library face of the monitor dashboard.
//!
//! The `monitor` binary is a thin `main` that owns the Wayland window, the
//! sampler thread, and the run loop; everything it paints and every pure
//! decision it makes lives in these modules. Exposing them as a library has
//! one concrete purpose beyond tidiness: the headless paint and data paths
//! (chart surfaces, per-core bars, damage coalescing, the redraw gate) can be
//! driven directly from `divan` benches with an allocation profiler attached,
//! so "the four charts are 78% of paint" stops being a live-trace guess and
//! becomes a measured number with per-call allocation counts. A binary crate's
//! items are unreachable from a separate bench crate; a library's are not.

pub mod chart;
pub mod chart_cache;
pub mod core_bars;
pub mod damage;
pub mod dashboard_report;
pub mod display_snapshot;
pub mod redraw_gate;
pub mod sample_interpolator;

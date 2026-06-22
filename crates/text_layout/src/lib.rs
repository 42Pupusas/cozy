//! Text segmentation, BiDi metadata, line-breaking, and layout.
//!
//! Pure-Rust port of [`pretext`](https://github.com/chenglou/pretext) by
//! Cheng Lou, adapted for a native terminal emulator context.
//!
//! ## Module map (pretext → Rust)
//!
//! | pretext (`*.ts`)   | This crate (`*.rs`) | Notes                              |
//! |--------------------|---------------------|------------------------------------|
//! | `analysis.ts`      | [`analysis`]        | Segmentation; uses our UAX #29     |
//! | `measurement.ts`   | [`measurement`]     | Width via UAX #11 East Asian Width |
//! | `line_break.ts`    | [`line_break`]      | Pure arithmetic walk, 1:1 port     |
//! | `bidi.ts`          | [`bidi`]            | BiDi range tables + state machine  |
//! | `layout.ts`        | [`layout`]          | Public API tying it all together   |
//!
//! ## Terminal vs. browser measurement
//!
//! The original `pretext` delegates glyph-advance measurement to
//! `Canvas.measureText()`, which calls into the browser's full text shaper.
//! In a fixed-width terminal every glyph occupies exactly **1 or 2 cells**;
//! Unicode UAX #11 East Asian Width tables tell us which.  The `measurement`
//! module implements this shortcut — accurate for all terminal use-cases with
//! zero external dependencies.

#![warn(clippy::pedantic)]
#![warn(clippy::nursery)]
#![warn(clippy::perf)]
#![warn(clippy::style)]
#![allow(clippy::module_name_repetitions, clippy::doc_markdown)]

pub mod analysis;
pub mod bidi;
pub mod line_break;
pub mod measurement;
pub mod layout;

pub use layout::{layout, LayoutLine, LayoutParams};

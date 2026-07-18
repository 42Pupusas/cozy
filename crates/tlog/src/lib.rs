//! Compile-time-gated, runtime-selectable tracing for the terminal.
//!
//! This replaces the pattern of one bespoke `std::env::var("TERMINAL_DEBUG_X")`
//! check per subsystem (we had accumulated two: `TERMINAL_DEBUG_FPS` and an
//! ad-hoc `TERMINAL_DEBUG_PTY`) with a single facility every crate can use.
//!
//! Two independent knobs:
//!
//! - **Compile time** — the `trace` Cargo feature. Off (the default): every
//!   [`trace!`] call site compiles to nothing at all — not dead-code-eliminated,
//!   *absent*, because the emitting arm is behind `#[cfg(feature = "trace")]`.
//!   The only trace of a disabled call site is that its format arguments are
//!   still type-checked (via an `if false { format_args!(..) }` fallback), so
//!   a bad `{}`/arg-count mismatch is still caught in a normal build. A release
//!   binary built without the feature pays exactly zero runtime cost: no env
//!   read, no branch, no call.
//! - **Runtime** — once the feature is compiled in, the `TERMINAL_LOG` env var
//!   selects which [`Channel`]s actually emit, e.g.:
//!
//!   ```sh
//!   TERMINAL_LOG=pty,render cargo run --features trace --bin terminal
//!   TERMINAL_LOG=all        cargo run --features trace --bin terminal
//!   ```
//!
//!   An unselected channel costs one atomic load + a masked bit test; the
//!   `format_args!` call (and thus every argument expression) is inside the
//!   `is_on` check, so unselected channels never format anything.
//!
//! Output goes to stderr by default, or to a file named by `TERMINAL_LOG_FILE`
//! (handy for high-volume channels like raw PTY bytes, which would otherwise
//! interleave with the very terminal you're debugging).
//!
//! This crate intentionally leaves `TERMINAL_DEBUG_FPS` alone
//! (`crates/platform/src/window.rs`): it's a documented, already-zero-cost
//! (`Off` is one enum compare) facility with its own established workflow in
//! `scripts/stress/README.md`, and it drives an on-screen overlay mode that
//! doesn't fit the "named channel, formatted line" shape here. New debug
//! instrumentation should use [`trace!`] instead.

use std::io::Write as _;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

/// One bit per subsystem. Add new variants here as new call sites need them
/// -- remember to extend [`Channel::ALL`], [`Channel::bit`], [`Channel::name`],
/// and [`Channel::from_name`] together.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Channel {
    Pty,
    Parser,
    Grid,
    Render,
    Input,
    Platform,
    Config,
    Fps,
}

impl Channel {
    /// Every channel, for `TERMINAL_LOG=all` and for tests.
    pub const ALL: [Self; 8] = [
        Self::Pty,
        Self::Parser,
        Self::Grid,
        Self::Render,
        Self::Input,
        Self::Platform,
        Self::Config,
        Self::Fps,
    ];

    const fn bit(self) -> u32 {
        1 << (self as u32)
    }

    /// Lowercase name used both in `TERMINAL_LOG` and in the emitted prefix.
    const fn name(self) -> &'static str {
        match self {
            Self::Pty => "pty",
            Self::Parser => "parser",
            Self::Grid => "grid",
            Self::Render => "render",
            Self::Input => "input",
            Self::Platform => "platform",
            Self::Config => "config",
            Self::Fps => "fps",
        }
    }

    fn from_name(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.name().eq_ignore_ascii_case(s))
    }
}

static MASK: AtomicU32 = AtomicU32::new(0);
static MASK_INIT: OnceLock<()> = OnceLock::new();

/// Parse a `TERMINAL_LOG` value (`"pty,render"`, `"all"`, `""`) into a bitmask.
/// Unknown tokens are silently ignored -- a typo'd channel name should not
/// crash the terminal it's meant to be debugging.
fn parse_mask(spec: &str) -> u32 {
    let mut mask = 0u32;
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if part.eq_ignore_ascii_case("all") {
            return Channel::ALL.iter().fold(0, |m, c| m | c.bit());
        }
        if let Some(ch) = Channel::from_name(part) {
            mask |= ch.bit();
        }
    }
    mask
}

fn mask() -> u32 {
    MASK_INIT.get_or_init(|| {
        let spec = std::env::var("TERMINAL_LOG").unwrap_or_default();
        MASK.store(parse_mask(&spec), Ordering::Relaxed);
    });
    MASK.load(Ordering::Relaxed)
}

/// Whether `channel` is currently selected. Called by [`trace!`] before it
/// formats anything; cheap enough to also call directly to guard a block of
/// debug-only work larger than one line.
#[doc(hidden)]
#[must_use]
pub fn is_on(channel: Channel) -> bool {
    mask() & channel.bit() != 0
}

enum Sink {
    Stderr,
    File(Mutex<std::fs::File>),
}

static SINK: OnceLock<Sink> = OnceLock::new();

fn sink() -> &'static Sink {
    SINK.get_or_init(|| {
        std::env::var("TERMINAL_LOG_FILE").map_or(Sink::Stderr, |path| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .map_or(Sink::Stderr, |f| Sink::File(Mutex::new(f)))
        })
    })
}

/// Write one already-gated, already-formatted line. Not meant to be called
/// directly -- use [`trace!`].
#[doc(hidden)]
pub fn emit(channel: Channel, args: std::fmt::Arguments) {
    match sink() {
        Sink::Stderr => eprintln!("[{:>8}] {args}", channel.name()),
        Sink::File(f) => {
            if let Ok(mut f) = f.lock() {
                let _ = writeln!(f, "[{:>8}] {args}", channel.name());
            }
        }
    }
}

/// Trace to a named channel. `channel` is a bare identifier matching one of
/// the [`Channel`] variants (e.g. `Pty`, `Render` -- same spelling as the enum,
/// not the lowercase `TERMINAL_LOG` name).
///
/// ```ignore
/// tlog::trace!(Pty, "raw chunk: {}", tlog::Escaped(bytes));
/// ```
///
/// With the `trace` feature off (the default), this compiles to nothing but
/// a type-check of the format arguments. With it on, the line is emitted only
/// if `channel` is selected via `TERMINAL_LOG`.
#[macro_export]
macro_rules! trace {
    ($channel:ident, $($arg:tt)*) => {{
        #[cfg(feature = "trace")]
        {
            if $crate::is_on($crate::Channel::$channel) {
                $crate::emit($crate::Channel::$channel, format_args!($($arg)*));
            }
        }
        #[cfg(not(feature = "trace"))]
        {
            if false {
                // Type-check the format arguments without emitting any code
                // or reading any env var.
                let _ = format_args!($($arg)*);
            }
        }
    }};
}

/// `Display` wrapper that renders bytes with control characters made visible.
///
/// (`ESC` -> `\e`, `\n`/`\r`/`\t` -> their C escapes, other non-printables ->
/// `\xNN`), one `>>`-prefixed block. Meant for logging raw PTY/wire traffic,
/// where the ground truth is exactly which bytes were sent.
pub struct Escaped<'a>(pub &'a [u8]);

impl std::fmt::Display for Escaped<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, ">> ")?;
        for &b in self.0 {
            match b {
                0x1b => write!(f, "\\e")?,
                b'\n' => write!(f, "\\n")?,
                b'\r' => write!(f, "\\r")?,
                b'\t' => write!(f, "\\t")?,
                0x20..=0x7e => write!(f, "{}", b as char)?,
                other => write!(f, "\\x{other:02x}")?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Channel, Escaped, parse_mask};

    #[test]
    fn parse_mask_empty_is_zero() {
        assert_eq!(parse_mask(""), 0);
    }

    #[test]
    fn parse_mask_all_sets_every_bit() {
        let all = Channel::ALL.iter().fold(0, |m, c| m | c.bit());
        assert_eq!(parse_mask("all"), all);
        assert_eq!(parse_mask("ALL"), all, "channel names are case-insensitive");
    }

    #[test]
    fn parse_mask_selects_named_channels_only() {
        let mask = parse_mask("pty,render");
        assert_eq!(mask, Channel::Pty.bit() | Channel::Render.bit());
        assert_eq!(mask & Channel::Grid.bit(), 0, "grid was not requested");
    }

    #[test]
    fn parse_mask_tolerates_whitespace_and_unknown_tokens() {
        // A typo'd channel name must not panic -- it's silently dropped so a
        // debug session doesn't crash the very thing it's meant to observe.
        let mask = parse_mask(" pty , bogus, render ");
        assert_eq!(mask, Channel::Pty.bit() | Channel::Render.bit());
    }

    #[test]
    fn every_channel_name_round_trips() {
        for ch in Channel::ALL {
            assert_eq!(Channel::from_name(ch.name()), Some(ch));
        }
    }

    #[test]
    fn escaped_makes_control_bytes_visible() {
        let bytes = b"ca\x1b[90mrgo\x1b[0m\x08\x07";
        let out = format!("{}", Escaped(bytes));
        assert_eq!(out, ">> ca\\e[90mrgo\\e[0m\\x08\\x07");
    }

    #[test]
    fn escaped_leaves_plain_ascii_untouched() {
        assert_eq!(format!("{}", Escaped(b"cargo build")), ">> cargo build");
    }

    #[test]
    fn trace_macro_type_checks_but_is_silent_without_the_feature() {
        // With the `trace` feature off (the default for this crate's own
        // test build), this must compile and produce nothing on stderr --
        // it's exercised purely to prove the macro expands cleanly.
        let n = 42;
        trace!(Pty, "value {n} at {:?}", std::time::Instant::now());
    }
}

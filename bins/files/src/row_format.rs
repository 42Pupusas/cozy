//! [`RowFormat`] -- turning an [`Entry`] into the strings a row shows.
//!
//! Split from the painter because formatting is pure and the painter is not:
//! "how is 1536 bytes written" and "what colour is a broken symlink" are
//! decisions that deserve tests, and they cannot have any while they are
//! interleaved with `fill_rect` calls against a live framebuffer.

use fsmodel::entry::Entry;
use fsmodel::kind::EntryKind;
use platform::draw::Color;

use crate::theme::Theme;

/// Formats entries for display.
#[derive(Debug, Clone, Copy, Default)]
pub struct RowFormat;

impl RowFormat {
    /// The name as shown, with a trailing `/` on anything navigable.
    ///
    /// The slash is on *navigable* rather than on `kind == Dir`, so a symlink
    /// to a directory also gets it -- the marker answers "what happens if I
    /// press Enter", which is the question a user is actually asking.
    #[must_use]
    pub fn name(entry: &Entry) -> String {
        if entry.is_navigable() {
            format!("{}/", entry.name)
        } else {
            entry.name.clone()
        }
    }

    /// The size column.
    ///
    /// Directories get `--` rather than a number: the size of a directory
    /// inode is real but meaningless to a user, and showing `4.0K` next to a
    /// folder holding gigabytes is worse than showing nothing.
    #[must_use]
    pub fn size(entry: &Entry) -> String {
        if entry.is_navigable() {
            "--".to_owned()
        } else {
            Self::human_size(entry.size)
        }
    }

    /// Bytes in the shortest unambiguous form.
    ///
    /// Powers of 1024 with their proper IEC names, one decimal place below
    /// 10 so `1.5 KiB` is distinguishable from `1.9 KiB` while `340 KiB`
    /// does not waste three characters on `.0`.
    ///
    /// The unit is `KiB`, not `kB`. These are powers of 1024, and `kB` means
    /// 1000 bytes -- so writing `kB` here would state a figure we did not
    /// compute. `K` alone was worse still: it is not a unit of anything.
    /// A space precedes the unit because SI writes one between a number and
    /// its unit, and because `340KiB` reads as one token.
    #[must_use]
    pub fn human_size(bytes: u64) -> String {
        const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
        // Plain bytes get no decimal point at all: "512 B" not "512.0 B".
        if bytes < 1024 {
            return format!("{bytes} B");
        }
        // f64 has 53 bits of mantissa, so it represents every u64 up to 2^53
        // exactly; beyond that the rounding error is far below the one
        // decimal place shown, and no file is 8 petabytes anyway.
        #[allow(clippy::cast_precision_loss)]
        let mut value = bytes as f64;
        let mut unit = 0;
        while value >= 1024.0 && unit + 1 < UNITS.len() {
            value /= 1024.0;
            unit += 1;
        }
        let suffix = UNITS[unit];
        if value < 10.0 {
            format!("{value:.1} {suffix}")
        } else {
            format!("{value:.0} {suffix}")
        }
    }

    /// The colour a name is drawn in.
    ///
    /// Broken links are checked first and stat the target, which is why this
    /// is called once per *visible* row rather than for the whole listing:
    /// a syscall per row is fine for the thirty on screen and ruinous for
    /// a hundred thousand.
    #[must_use]
    pub fn color(entry: &Entry, theme: &Theme) -> Color {
        if entry.kind == EntryKind::Symlink {
            return if entry.is_broken_symlink() {
                theme.broken
            } else {
                theme.symlink
            };
        }
        if entry.is_navigable() {
            theme.dir
        } else {
            theme.text
        }
    }

    /// Truncate `text` to fit `columns` character cells, marking the cut with
    /// an ellipsis.
    ///
    /// Counts `char`s, not bytes: the font draws one cell per char, so a
    /// byte-based cut would both mis-measure non-ASCII names and risk
    /// slicing mid-character.
    #[must_use]
    pub fn truncate(text: &str, columns: usize) -> String {
        if columns == 0 {
            return String::new();
        }
        let len = text.chars().count();
        if len <= columns {
            return text.to_owned();
        }
        if columns == 1 {
            return "…".to_owned();
        }
        let keep = columns - 1;
        let mut out: String = text.chars().take(keep).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::RowFormat;

    #[test]
    fn plain_bytes_get_no_decimal_point() {
        assert_eq!(RowFormat::human_size(0), "0 B");
        assert_eq!(RowFormat::human_size(512), "512 B");
        assert_eq!(RowFormat::human_size(1023), "1023 B");
    }

    #[test]
    fn kilobytes_and_up_use_one_decimal_below_ten() {
        assert_eq!(RowFormat::human_size(1024), "1.0 KiB");
        assert_eq!(RowFormat::human_size(1536), "1.5 KiB");
        assert_eq!(RowFormat::human_size(1024 * 1024), "1.0 MiB");
    }

    #[test]
    fn larger_values_drop_the_decimal_to_save_a_column() {
        assert_eq!(RowFormat::human_size(1024 * 340), "340 KiB");
        assert_eq!(RowFormat::human_size(1024 * 1024 * 15), "15 MiB");
    }

    /// The unit table must run out gracefully rather than indexing past its
    /// end on an absurd size.
    #[test]
    fn an_enormous_size_saturates_at_the_last_unit() {
        let formatted = RowFormat::human_size(u64::MAX);
        assert!(formatted.ends_with(" PiB"), "got {formatted}");
    }

    /// The binary units must be spelled the binary way. `kB` is 1000 bytes;
    /// these are powers of 1024, so writing `kB` would state a figure that
    /// was never computed.
    #[test]
    fn the_units_are_the_iec_binary_names() {
        for (bytes, expected) in [
            (1024_u64, "KiB"),
            (1024 * 1024, "MiB"),
            (1024 * 1024 * 1024, "GiB"),
            (1024 * 1024 * 1024 * 1024, "TiB"),
        ] {
            let formatted = RowFormat::human_size(bytes);
            assert!(
                formatted.ends_with(expected),
                "{bytes} formatted as {formatted}, wanted {expected}"
            );
        }
    }

    /// A number and its unit are two tokens, so a space separates them.
    #[test]
    fn a_space_separates_the_figure_from_its_unit() {
        for bytes in [0_u64, 999, 1024, 4096, 1024 * 1024 * 7] {
            let formatted = RowFormat::human_size(bytes);
            let (figure, unit) = formatted
                .split_once(' ')
                .unwrap_or_else(|| panic!("{formatted} must separate figure and unit"));
            assert!(
                figure.chars().all(|c| c.is_ascii_digit() || c == '.'),
                "the figure must be numeric, got {figure}"
            );
            assert!(
                unit.chars().all(char::is_alphabetic),
                "the unit must be letters, got {unit}"
            );
        }
    }

    #[test]
    fn short_text_is_left_alone() {
        assert_eq!(RowFormat::truncate("readme.md", 20), "readme.md");
        assert_eq!(RowFormat::truncate("exact", 5), "exact");
    }

    #[test]
    fn long_text_is_cut_with_an_ellipsis_at_the_right_width() {
        let out = RowFormat::truncate("a-very-long-file-name.txt", 10);
        assert_eq!(out.chars().count(), 10, "must fit exactly");
        assert!(out.ends_with('…'));
        assert!(out.starts_with("a-very-lo"));
    }

    /// A byte-based truncation would slice mid-character here and panic, or
    /// mis-measure the width. Both are real: filenames are full of this.
    #[test]
    fn truncation_counts_characters_not_bytes() {
        let name = "日本語のファイル名です.txt";
        let out = RowFormat::truncate(name, 6);
        assert_eq!(out.chars().count(), 6);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn degenerate_widths_do_not_panic() {
        assert_eq!(RowFormat::truncate("anything", 0), "");
        assert_eq!(RowFormat::truncate("anything", 1), "…");
    }

    #[test]
    fn an_empty_string_survives_any_width() {
        assert_eq!(RowFormat::truncate("", 0), "");
        assert_eq!(RowFormat::truncate("", 10), "");
    }
}

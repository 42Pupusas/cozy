//! One-line summaries of history entries, for a picker to display.
//!
//! The output is deliberately `id<TAB>preview`, matching what `cliphist list`
//! emits, so an existing `fuzzel`/`dmenu` pipeline keeps working unchanged.
//! The id is what `restore` takes, and a picker only has to split on the tab.

use clipboard::HistoryEntry;

/// Longest preview text; beyond this a picker just truncates anyway.
const MAX_PREVIEW: usize = 100;

/// Renders a history entry as a single display line.
pub struct EntryPreview;

impl EntryPreview {
    /// `id<TAB>preview` for one entry.
    #[must_use]
    pub fn line(entry: &HistoryEntry) -> String {
        format!("{}\t{}", entry.id(), Self::describe(entry))
    }

    /// A human summary of what the entry holds.
    #[must_use]
    pub fn describe(entry: &HistoryEntry) -> String {
        let payload = entry.payload();
        // Binary representations get a description rather than mojibake: a
        // picker rendering raw PNG bytes would fill the menu with garbage and
        // could even break the terminal it runs in.
        if let Some(png) = payload.requested("image/png") {
            return format!("[image/png {}]", Self::human_size(png.len()));
        }
        for mime in ["text/plain;charset=utf-8", "text/plain", "text/uri-list"] {
            if let Some(bytes) = payload.requested(mime) {
                return Self::summarise(bytes);
            }
        }
        payload.mimes().next().map_or_else(
            || "[empty]".to_string(),
            |mime| format!("[{mime}]"),
        )
    }

    /// Collapse text to one printable line.
    fn summarise(bytes: &[u8]) -> String {
        let text = String::from_utf8_lossy(bytes);
        // Newlines and tabs would break the one-entry-per-line contract the
        // picker relies on, so they become spaces rather than being stripped
        // (which would run words together).
        let flattened: String = text
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        let trimmed = flattened.split_whitespace().collect::<Vec<_>>().join(" ");
        if trimmed.is_empty() {
            return "[whitespace]".to_string();
        }
        // Truncate on a char boundary: slicing bytes would panic on any
        // multi-byte character straddling the limit.
        if trimmed.chars().count() > MAX_PREVIEW {
            let cut: String = trimmed.chars().take(MAX_PREVIEW).collect();
            return format!("{cut}…");
        }
        trimmed
    }

    /// One decimal place, computed in integer arithmetic.
    ///
    /// A float would be the obvious way to write this, but converting a
    /// `usize` to `f64` is a documented precision loss the workspace lints
    /// against -- and there is no reason to introduce rounding into a string
    /// that is only ever read by a human.
    fn human_size(bytes: usize) -> String {
        const KIB: usize = 1024;
        const MIB: usize = 1024 * KIB;
        if bytes >= MIB {
            return Self::tenths(bytes, MIB, "MiB");
        }
        if bytes >= KIB {
            return Self::tenths(bytes, KIB, "KiB");
        }
        format!("{bytes} B")
    }

    /// `bytes / unit` to one decimal place, e.g. `2.0 KiB`.
    fn tenths(bytes: usize, unit: usize, suffix: &str) -> String {
        let whole = bytes / unit;
        // Scale before dividing so the first decimal digit survives integer
        // truncation.
        let fraction = (bytes % unit) * 10 / unit;
        format!("{whole}.{fraction} {suffix}")
    }
}

#[cfg(test)]
mod tests {
    use super::EntryPreview;
    use clipboard::{ClipboardOffer, ClipboardPayload, HistoryEntry};

    struct PreviewFixture;

    impl PreviewFixture {
        fn entry(mime: &str, bytes: Vec<u8>) -> HistoryEntry {
            let mut payload = ClipboardPayload::new();
            payload.offer(ClipboardOffer::new(mime, bytes));
            HistoryEntry::new(7, payload)
        }
    }

    #[test]
    fn a_line_is_id_tab_preview_so_a_picker_can_split_it() {
        let entry = PreviewFixture::entry("text/plain", b"hello".to_vec());
        assert_eq!(EntryPreview::line(&entry), "7\thello");
    }

    #[test]
    fn images_are_described_rather_than_dumped_as_bytes() {
        let entry = PreviewFixture::entry("image/png", vec![0u8; 2048]);
        assert_eq!(EntryPreview::describe(&entry), "[image/png 2.0 KiB]");
    }

    #[test]
    fn newlines_are_flattened_so_one_entry_stays_one_line() {
        let entry = PreviewFixture::entry("text/plain", b"a\nb\tc".to_vec());
        assert_eq!(EntryPreview::describe(&entry), "a b c");
    }

    #[test]
    fn long_text_is_truncated_on_a_char_boundary() {
        let entry = PreviewFixture::entry("text/plain", "é".repeat(200).into_bytes());
        let described = EntryPreview::describe(&entry);
        assert!(described.ends_with('…'));
        assert_eq!(described.chars().count(), 101);
    }

    #[test]
    fn whitespace_only_text_is_labelled_not_blank() {
        let entry = PreviewFixture::entry("text/plain", b"   \n ".to_vec());
        assert_eq!(EntryPreview::describe(&entry), "[whitespace]");
    }

    #[test]
    fn an_unknown_mime_is_named_rather_than_guessed_at() {
        let entry = PreviewFixture::entry("application/x-thing", vec![1, 2, 3]);
        assert_eq!(EntryPreview::describe(&entry), "[application/x-thing]");
    }
}

//! Plain-text URL auto-detection over a rendered row of cells.
//!
//! Not every program that prints a URL wraps it in an OSC 8 hyperlink escape
//! (`ESC ] 8 ; ; uri ST ... ESC ] 8 ; ; ST`) -- `trunk serve`'s "listening
//! at" banner is a concrete example: it prints a plain `http://127.0.0.1:...`
//! string through `tracing`, no escape sequence involved at all. Without this
//! scanner those URLs are inert text; real terminals (kitty, `WezTerm`, ...)
//! solve the same gap by pattern-matching visible text for URL-shaped runs
//! and treating them as clickable even though the application never asked
//! for it. This module is that pattern-match, split out on its own because
//! it's a pure "given this text, where are the URLs" concern with zero
//! dependency on window/pointer/rendering state -- [`crate::window`] is the
//! thing that decides what a hovered/clicked span *means*.
use grid::Cell;

use crate::logical_line::RowSpan;

/// One URL-shaped run found in a scanned line, as flat cell indices into
/// that line (inclusive on both ends).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlMatch {
    pub start: usize,
    pub end: usize,
    pub uri: String,
}

/// A detected URL placed on screen: its text plus the per-row column runs it
/// occupies (more than one when the URL soft-wraps across rows).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlSpan {
    pub segments: Vec<RowSpan>,
    pub uri: String,
}

/// What's under the pointer/click for hyperlink purposes.
///
/// Either a real OSC 8 link (resolved through the parser-owned link table,
/// so this side only carries its id) or a synthetically-detected plain-text
/// URL (which, having no entry in any table, carries its own URI and the
/// column span it covers directly). [`crate::window::TermWindow`] resolves
/// whichever a cell has into this common shape so the hover-underline and
/// Ctrl+click paths don't need two parallel implementations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    /// A real OSC 8 hyperlink, identified by `Grid`'s interned id. The URI
    /// lives in the UI thread's `link_id -> uri` mirror, not here.
    Real(u32),
    /// A plain-text URL this scanner detected with no application-side OSC 8
    /// escape backing it. Self-contained (span + URI) since there's no
    /// table to look either up in.
    Url(UrlSpan),
}

/// Stateless scanner for `http://`/`https://` runs in a row of cells.
///
/// A struct (not a free function) purely to give this behavior a name to
/// hang doc comments and future config off of (e.g. a day-two "also detect
/// `mailto:`/`ftp://`" tweak becomes a field here, not a new free function
/// with its own signature).
pub struct UrlScanner;

impl UrlScanner {
    /// Find every URL-shaped run in `line`, skipping any cell that already
    /// carries a real OSC 8 `link_id` -- those are already clickable via the
    /// normal path, and re-detecting the same text as a *second*, competing
    /// synthetic link would just be redundant work for an identical result.
    #[must_use]
    pub fn scan_line(line: &[Cell]) -> Vec<UrlMatch> {
        let chars: Vec<char> = line.iter().map(|c| c.ch).collect();
        let mut spans = Vec::new();
        let mut i = 0;
        while i < chars.len() {
            if line[i].link_id() != 0 || !Self::matches_scheme(&chars[i..]) {
                i += 1;
                continue;
            }
            let start = i;
            let mut end = i;
            while end < chars.len() && Self::is_url_char(chars[end]) {
                end += 1;
            }
            // Trim trailing punctuation a sentence would plausibly wrap the
            // URL in ("visit http://x.com." / "(http://x.com)") -- without
            // this the period/paren reads as part of the link.
            let mut trimmed_end = end;
            while trimmed_end > start && Self::is_trailing_punct(chars[trimmed_end - 1]) {
                trimmed_end -= 1;
            }
            if trimmed_end > start {
                let uri: String = chars[start..trimmed_end].iter().collect();
                spans.push(UrlMatch {
                    start,
                    end: trimmed_end - 1,
                    uri,
                });
            }
            // Resume scanning right after the run (not `trimmed_end` --
            // trailing punctuation we stripped still shouldn't be
            // re-examined as the start of a new scheme match).
            i = end.max(start + 1);
        }
        spans
    }

    /// The URL-shaped run covering flat index `idx` in `line`, if any.
    #[must_use]
    pub fn find_at(line: &[Cell], idx: usize) -> Option<UrlMatch> {
        Self::scan_line(line)
            .into_iter()
            .find(|span| span.start <= idx && idx <= span.end)
    }

    /// Whether `s` begins with `http://` or `https://`.
    fn matches_scheme(s: &[char]) -> bool {
        Self::starts_with_str(s, "https://") || Self::starts_with_str(s, "http://")
    }

    fn starts_with_str(s: &[char], pat: &str) -> bool {
        let pat_len = pat.chars().count();
        s.len() >= pat_len && s.iter().zip(pat.chars()).all(|(&a, b)| a == b)
    }

    /// Characters that can plausibly appear inside a URL's path/query/host.
    /// Deliberately permissive (this is detection over free-form terminal
    /// output, not validation) -- excludes only whitespace and the
    /// characters that unambiguously end a URL when text wraps it (quotes,
    /// angle brackets).
    const fn is_url_char(c: char) -> bool {
        !c.is_whitespace() && !matches!(c, '"' | '\'' | '<' | '>')
    }

    /// Trailing characters stripped off a detected run because they're far
    /// more likely to be surrounding prose punctuation than part of the URL.
    const fn is_trailing_punct(c: char) -> bool {
        matches!(
            c,
            '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '\'' | '"'
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grid::Attrs;

    fn line_from(s: &str) -> Vec<Cell> {
        s.chars()
            .map(|c| Cell::new(c, Attrs::default(), 0))
            .collect()
    }

    #[test]
    fn detects_a_bare_http_url() {
        let line = line_from("listening at http://127.0.0.1:8080/");
        let spans = UrlScanner::scan_line(&line);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].uri, "http://127.0.0.1:8080/");
        assert_eq!(spans[0].start, 13);
    }

    #[test]
    fn detects_https_and_trims_trailing_sentence_punctuation() {
        let line = line_from("see (https://example.com/path).");
        let spans = UrlScanner::scan_line(&line);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].uri, "https://example.com/path");
    }

    #[test]
    fn skips_cells_already_carrying_a_real_hyperlink() {
        let mut line = line_from("http://example.com");
        for cell in &mut line {
            *cell = Cell::new(cell.ch, Attrs::default(), 1);
        }
        assert!(UrlScanner::scan_line(&line).is_empty());
    }

    #[test]
    fn no_match_when_no_scheme_present() {
        let line = line_from("just some plain text, no links here");
        assert!(UrlScanner::scan_line(&line).is_empty());
    }

    #[test]
    fn find_at_locates_the_span_covering_a_given_column() {
        let line = line_from("go to http://example.com now");
        let hit = UrlScanner::find_at(&line, 10).expect("column sits inside the URL");
        assert_eq!(hit.uri, "http://example.com");
        assert!(UrlScanner::find_at(&line, 0).is_none());
    }

    #[test]
    fn a_url_soft_wrapped_across_rows_is_found_whole() {
        use crate::logical_line::LogicalLine;
        use crate::mirrored_row::MirroredRow;
        let rows: Vec<MirroredRow> = [
            ("see https://exa", Some(15)),
            ("mple.com/a/very", Some(15)),
            ("/long/path ok  ", None),
        ]
        .into_iter()
        .map(|(text, soft_wrap)| MirroredRow {
            content: grid::DirtyRowContent::Cells(line_from(text)),
            soft_wrap,
        })
        .collect();
        let line = LogicalLine::gather(&rows, 2).expect("row is mirrored");
        let at = line.index_of(3, 2).expect("cell is on the line");
        let hit = UrlScanner::find_at(line.cells(), at).expect("pointer is on the URL");
        assert_eq!(hit.uri, "https://example.com/a/very/long/path");
        let rows_hit: Vec<u16> = line
            .segments(hit.start, hit.end)
            .iter()
            .map(|s| s.row)
            .collect();
        assert_eq!(rows_hit, vec![0, 1, 2]);
    }

    #[test]
    fn finds_multiple_urls_on_one_line() {
        let line = line_from("http://a.com and https://b.com");
        let spans = UrlScanner::scan_line(&line);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].uri, "http://a.com");
        assert_eq!(spans[1].uri, "https://b.com");
    }
}

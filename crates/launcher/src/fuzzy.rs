//! Subsequence fuzzy matching + scoring for ranking apps against a query,
//! in the style of fzf/rofi's "type any subset of letters in order".
//!
//! Deliberately hand-rolled rather than pulling in a fuzzy-matching crate:
//! the scoring rules a launcher needs are a handful of simple bonuses
//! (contiguous run, word-start, whole-string prefix) that fit in one small
//! self-contained file, matching this workspace's preference for owning
//! its own small algorithms over a dependency.

/// The result of successfully matching `query` as a subsequence of a
/// candidate string, carrying enough to both rank and highlight it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyMatch {
    /// Higher is a better match. Comparable only against other matches
    /// produced by the same query.
    pub score: i32,
    /// Byte offsets into the candidate of each matched character, in
    /// order -- for highlighting matched letters in the UI.
    pub matched_indices: Vec<usize>,
}

impl FuzzyMatch {
    /// Try to match `query` as a case-insensitive subsequence of
    /// `candidate`. Returns `None` if `query` isn't a subsequence at all
    /// (a launcher shows only apps that match). An empty query always
    /// matches everything with a zero score, so an empty search box shows
    /// the full (unranked) app list.
    #[must_use]
    pub fn find(query: &str, candidate: &str) -> Option<Self> {
        if query.is_empty() {
            return Some(Self {
                score: 0,
                matched_indices: Vec::new(),
            });
        }

        let query_lower: Vec<char> = query.to_lowercase().chars().collect();
        let candidate_chars: Vec<(usize, char)> = candidate.char_indices().collect();
        let candidate_lower: Vec<char> = candidate.to_lowercase().chars().collect();
        // A lowercase candidate can have a different char count than the
        // original in pathological Unicode cases (rare, but `to_lowercase`
        // isn't guaranteed 1:1); guard rather than panic on index mismatch.
        if candidate_lower.len() != candidate_chars.len() {
            return Self::find_ascii_fallback(query, candidate);
        }

        let mut matched_indices = Vec::with_capacity(query_lower.len());
        let mut score = 0i32;
        let mut cursor = 0usize;
        let mut prev_matched_pos: Option<usize> = None;

        for &qc in &query_lower {
            let found = (cursor..candidate_lower.len()).find(|&i| candidate_lower[i] == qc)?;
            let (byte_offset, _) = candidate_chars[found];
            matched_indices.push(byte_offset);

            score += Self::position_bonus(found, &candidate_lower, prev_matched_pos);
            prev_matched_pos = Some(found);
            cursor = found + 1;
        }

        // Reward matches that are a prefix of the whole candidate (e.g.
        // query "fire" on "Firefox") on top of the per-character bonuses,
        // since that's the single strongest signal of intent.
        if matched_indices.first() == Some(&0) {
            score += 15;
        }

        Some(Self {
            score,
            matched_indices,
        })
    }

    /// Byte-oriented fallback for the rare case where lowercasing changes
    /// the candidate's character count (e.g. some ligature-like Unicode
    /// casing expansions). ASCII-only, case-insensitive subsequence check
    /// with no positional bonuses -- correctness over ranking nuance for
    /// this edge case.
    fn find_ascii_fallback(query: &str, candidate: &str) -> Option<Self> {
        let mut matched_indices = Vec::new();
        let mut cursor = 0usize;
        let cbytes: Vec<(usize, u8)> = candidate.bytes().enumerate().collect();

        for qc in query.bytes() {
            let qc_lower = qc.to_ascii_lowercase();
            let (idx, _) = cbytes
                .iter()
                .skip(cursor)
                .find(|(_, b)| b.to_ascii_lowercase() == qc_lower)?;
            matched_indices.push(*idx);
            cursor = idx + 1;
        }

        Some(Self {
            score: 0,
            matched_indices,
        })
    }

    /// Score contribution for matching at `pos`: a base point per matched
    /// character, plus bonuses for matching right after the previous match
    /// (a contiguous run reads as "the same word") and for matching at a
    /// word boundary (start of string, or just after `-`/`_`/` `/`.`) --
    /// the two strongest positional signals fzf-style matchers use.
    fn position_bonus(pos: usize, candidate_lower: &[char], prev_matched_pos: Option<usize>) -> i32 {
        let mut bonus = 1;

        if let Some(prev) = prev_matched_pos
            && pos == prev + 1
        {
            bonus += 5;
        }

        let is_word_boundary = pos == 0
            || matches!(
                candidate_lower.get(pos.wrapping_sub(1)),
                Some('-' | '_' | ' ' | '.')
            );
        if is_word_boundary {
            bonus += 8;
        }

        bonus
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_matches_with_zero_score() {
        let m = FuzzyMatch::find("", "Firefox").unwrap();
        assert_eq!(m.score, 0);
        assert!(m.matched_indices.is_empty());
    }

    #[test]
    fn exact_prefix_matches() {
        let m = FuzzyMatch::find("fire", "Firefox").unwrap();
        assert_eq!(m.matched_indices, vec![0, 1, 2, 3]);
    }

    #[test]
    fn case_insensitive() {
        assert!(FuzzyMatch::find("FIRE", "firefox").is_some());
        assert!(FuzzyMatch::find("fire", "FIREFOX").is_some());
    }

    #[test]
    fn subsequence_out_of_order_fails() {
        assert!(FuzzyMatch::find("efir", "firefox").is_none());
    }

    #[test]
    fn non_subsequence_fails() {
        assert!(FuzzyMatch::find("xyz", "firefox").is_none());
    }

    #[test]
    fn subsequence_with_gaps_matches() {
        let m = FuzzyMatch::find("ffx", "firefox").unwrap();
        // f-i-r-e-f-o-x: first 'f' at 0, second at 4 (after 'e'), 'x' at 6.
        assert_eq!(m.matched_indices, vec![0, 4, 6]);
    }

    #[test]
    fn prefix_match_scores_higher_than_scattered_match() {
        let prefix = FuzzyMatch::find("fire", "Firefox").unwrap();
        let scattered = FuzzyMatch::find("fx", "Firefox").unwrap();
        assert!(prefix.score > scattered.score);
    }

    #[test]
    fn contiguous_run_scores_higher_than_scattered_same_length() {
        let contiguous = FuzzyMatch::find("fir", "Firefox").unwrap();
        let scattered = FuzzyMatch::find("fox", "Firefox").unwrap();
        assert!(contiguous.score > scattered.score);
    }

    #[test]
    fn word_boundary_after_separator_scores_higher() {
        // "vc" as a scattered match: "Visual Studio Code" 'V' start-bonus
        // + 'C' word-boundary-after-space beats a fully non-boundary match.
        let boundary = FuzzyMatch::find("vc", "Visual Code").unwrap();
        let non_boundary = FuzzyMatch::find("is", "Visual Code").unwrap();
        assert!(boundary.score > non_boundary.score);
    }
}

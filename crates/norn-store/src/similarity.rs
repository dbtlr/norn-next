//! The one similarity module: the stem fold, the padded trigram set and the
//! Jaccard score that the stem trigram index, its retrieval read and the
//! repair planner's suggestions all compute by, so the index and the score
//! cannot disagree about what a trigram is.
//!
//! **The fold is a suggestion's, never a resolution rule.** NFC then
//! per-character lowercase decides which stems look alike; it decides nothing
//! about which document a link resolves to.

use std::collections::BTreeSet;
use unicode_normalization::UnicodeNormalization;

/// The Jaccard threshold the repair planner applies to stem suggestions.
pub const CUTOFF: f64 = 0.5;
/// The most postings a selected query trigram may hold before the read answers
/// too common. It bounds the postings a retrieval reads at
/// `(T - k + 1) * POSTINGS_CAP`; the realistic profile's measurement stands
/// behind the starting value.
pub const POSTINGS_CAP: usize = 256;

/// Normalize to NFC, then lowercase each Unicode scalar; lowercase may expand.
pub fn fold(text: &str) -> String {
    text.nfc().flat_map(char::to_lowercase).collect()
}

/// Distinct three-scalar windows of the folded stem, with two leading spaces
/// and one trailing space. Repeated windows occupy one entry, so an n-scalar
/// folded stem occupies at most n + 1 indexed trigrams.
pub fn trigrams(stem: &str) -> BTreeSet<String> {
    let padded: Vec<char> = format!("  {} ", fold(stem)).chars().collect();
    padded
        .windows(3)
        .map(|window| window.iter().collect())
        .collect()
}

/// The shared fraction of the union of two trigram sets.
pub fn jaccard(left: &BTreeSet<String>, right: &BTreeSet<String>) -> f64 {
    left.intersection(right).count() as f64 / left.union(right).count() as f64
}

/// The least shared count a query of t distinct trigrams needs at the cutoff.
pub fn required_shared(t: usize) -> usize {
    t.div_ceil(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padding_and_set_shape() {
        assert_eq!(
            trigrams("ab"),
            ["  a", " ab", "ab "].map(String::from).into()
        );
    }

    #[test]
    fn fold_is_nfc_then_character_lowercase() {
        assert_eq!(fold("E\u{301}COLE"), fold("École"));
        assert_eq!(fold("ABC"), "abc");
    }

    #[test]
    fn jaccard_of_a_known_pair() {
        let left = ["abc", "bcd", "cde"].map(String::from).into();
        let right = ["abc", "bcd", "def"].map(String::from).into();
        assert_eq!(jaccard(&left, &right), 0.5);
    }

    #[test]
    fn required_shared_is_positive_for_nonempty_sets() {
        for t in 1..=100 {
            assert!(required_shared(t) >= 1);
            assert_eq!(required_shared(t), t.div_ceil(2));
        }
    }
}

//! The one similarity module: the stem fold, the padded trigram set and the
//! Jaccard score that the stem trigram index, its retrieval read and the
//! repair planner's suggestions all compute by, so the index and the score
//! cannot disagree about what a trigram is; and the bounded optimal string
//! alignment distance that the planner's closest-member and declared-tag
//! suggestions and the `did_you_mean` hint count edits by.
//!
//! **The fold is a suggestion's, never a resolution rule.** NFC then
//! per-character lowercase decides which stems look alike; it decides nothing
//! about which document a link resolves to.

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::fmt;
use unicode_normalization::UnicodeNormalization;

/// The Jaccard threshold the repair planner applies to stem and heading-anchor
/// suggestions.
pub const CUTOFF: Score = Score {
    shared: 1,
    union: 2,
};
/// The most postings a selected query trigram may hold before the read answers
/// too common. It bounds the postings a retrieval reads at
/// `(T - k + 1) * POSTINGS_CAP`; the realistic profile's measurement stands
/// behind the starting value.
pub const POSTINGS_CAP: usize = 256;

/// An exact Jaccard ratio, ordered by cross-multiplication and rendered to two decimals.
#[derive(Clone, Copy, Debug)]
pub struct Score {
    shared: usize,
    union: usize,
}

impl Score {
    /// Whether this ratio reaches the one similarity cutoff.
    pub fn reaches_cutoff(self) -> bool {
        self >= CUTOFF
    }
}

impl Ord for Score {
    fn cmp(&self, other: &Self) -> Ordering {
        ((self.shared as u128) * (other.union as u128))
            .cmp(&((other.shared as u128) * (self.union as u128)))
    }
}

impl PartialOrd for Score {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Score {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Score {}

impl fmt::Display for Score {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:.2}", self.shared as f64 / self.union as f64)
    }
}

/// The Jaccard ratio from the shared count and the two trigram-set sizes.
pub const fn jaccard_counts(shared: usize, left: usize, right: usize) -> Score {
    Score {
        shared,
        union: left + right - shared,
    }
}

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
pub fn jaccard(left: &BTreeSet<String>, right: &BTreeSet<String>) -> Score {
    jaccard_counts(left.intersection(right).count(), left.len(), right.len())
}

/// The optimal string alignment distance over characters, or `None` past
/// `bound`. Insertion, removal, replacement and adjacent transposition each
/// cost one edit, and no substring may be edited twice.
///
/// Three rows retain the previous two prefixes a transposition reads.
pub fn osa_distance(left: &[char], right: &[char], bound: usize) -> Option<usize> {
    if left.len().abs_diff(right.len()) > bound {
        return None;
    }
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut earlier = previous.clone();
    let mut current = vec![0; right.len() + 1];
    for (row, left_char) in left.iter().enumerate() {
        current[0] = row + 1;
        for (column, right_char) in right.iter().enumerate() {
            current[column + 1] = (previous[column] + usize::from(left_char != right_char))
                .min(previous[column + 1] + 1)
                .min(current[column] + 1);
            if row > 0
                && column > 0
                && *left_char == right[column - 1]
                && left[row - 1] == *right_char
            {
                current[column + 1] = current[column + 1].min(earlier[column - 1] + 1);
            }
        }
        std::mem::swap(&mut earlier, &mut previous);
        std::mem::swap(&mut previous, &mut current);
    }
    let distance = previous[right.len()];
    (distance <= bound).then_some(distance)
}

/// The least shared count implied by the cutoff, since the union holds at least t.
pub fn required_shared(t: usize) -> usize {
    (t * CUTOFF.shared).div_ceil(CUTOFF.union)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An adjacent transposition costs one edit over Unicode characters.
    #[test]
    fn an_adjacent_transposition_is_one_edit() {
        assert_eq!(osa_distance(&['é', 'a'], &['a', 'é'], 1), Some(1));
    }

    /// A distance beyond the requested bound has no answer.
    #[test]
    fn the_distance_bound_answers_none_past_it() {
        assert_eq!(osa_distance(&['a'], &['b', 'c', 'd'], 1), None);
        assert_eq!(osa_distance(&['a', 'b'], &['c', 'd'], 1), None);
        assert_eq!(osa_distance(&['a', 'b'], &['c', 'd'], 2), Some(2));
    }

    /// OSA cannot edit a substring twice as unrestricted Damerau can.
    #[test]
    fn a_substring_cannot_be_edited_twice() {
        assert_eq!(osa_distance(&['c', 'a'], &['a', 'b', 'c'], 3), Some(3));
    }

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
        let score = jaccard(&left, &right);
        assert_eq!(score, CUTOFF);
        assert!(score.reaches_cutoff());
        assert_eq!(score.to_string(), "0.50");
        assert_eq!(score, jaccard_counts(1, 1, 2));
    }

    /// No leaf-stem length ceiling exists, so the premise, not the sampled range, carries
    /// the claim: CUTOFF.shared >= 1 makes t * shared >= 1 for t >= 1, so its ceiling quotient
    /// is at least 1.
    #[test]
    fn required_shared_is_positive_for_nonempty_sets() {
        const { assert!(CUTOFF.shared >= 1) };
        for t in 1..=100 {
            assert!(required_shared(t) >= 1);
        }
    }
}

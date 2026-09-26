//! A tag's identity: the one fold every comparison of two tag names reads.
//!
//! **Two tag names are one tag when their folds are equal.** The fold is
//! Unicode lowercase with no locale, taken one character at a time, so `#Work`
//! and `#work` are one tag and so are `#Über` and `#über`, while an accent is
//! part of the letter and is kept: `#café` is not `#cafe`. The fold covers the
//! whole nested name, `/` included, so `#Area/Work` and `#area/work` are one
//! tag.
//!
//! **Each character folds alone, to exactly one character.** A character folds
//! to its lowercase where [`char::to_lowercase`] gives exactly one character,
//! and stays as written where it gives several. No neighbour is read, so a
//! character folds the same inside a tag and inside a tag pattern, between
//! letters and beside a `*` or `?`, and a fold keeps a name's length in
//! characters. Two cases follow that a whole-string lowercase would decide
//! otherwise:
//!
//! - `Σ` always folds to `σ`, never to the final `ς`: `#ΟΔΟΣ` folds to `οδοσ`.
//! - `İ` lowercases to `i` and a combining dot, two characters, so it keeps
//!   its own identity: `#İ` is one tag, apart from `#i` and `#I`.
//!
//! **The fold is a tag's alone.** A path's fold is ASCII-only and exists only
//! where the vault root folds ([`crate::CaseFold`]); a tag names no file, so
//! its fold is the same on every root. The syntax layer reads a tag as
//! written and never folds it: the fold is what a comparison applies, and a
//! tag stored or reported keeps its written spelling beside it.

/// The fold `name` is compared under: every tag name equal to it under this
/// fold is the same tag. Each character folds alone — see the module doc.
pub fn fold_tag(name: &str) -> String {
    name.chars().map(fold_char).collect()
}

/// `c`'s lowercase where it is exactly one character, and `c` otherwise.
fn fold_char(c: char) -> char {
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(single), None) => single,
        _ => c,
    }
}

#[cfg(test)]
mod tests {
    use super::fold_tag;

    fn one_tag(left: &str, right: &str) -> bool {
        fold_tag(left) == fold_tag(right)
    }

    #[test]
    fn ascii_case_is_one_tag() {
        assert!(one_tag("Work", "work"));
        assert!(one_tag("WORK", "work"));
    }

    #[test]
    fn unicode_case_is_one_tag() {
        assert!(one_tag("Über", "über"));
        assert!(one_tag("ÉTÉ", "été"));
    }

    #[test]
    fn an_accent_is_part_of_the_letter() {
        assert!(!one_tag("café", "cafe"));
        assert!(!one_tag("Über", "uber"));
    }

    #[test]
    fn the_fold_covers_the_whole_nested_name() {
        assert_eq!(fold_tag("Area/Work"), "area/work");
        assert!(one_tag("Area/Work", "area/WORK"));
    }

    #[test]
    fn a_folded_name_folds_to_itself() {
        for name in ["Work", "Über", "Area/Work", "café", "ΟΔΟΣ"] {
            let folded = fold_tag(name);
            assert_eq!(fold_tag(&folded), folded, "{name}");
        }
    }

    /// Every character folds to one character that folds to itself, so a
    /// fold keeps a name's length in characters and folding twice is folding
    /// once.
    #[test]
    fn every_character_folds_to_one_character_that_is_its_own_fold() {
        for c in (0..=u32::from(char::MAX)).filter_map(char::from_u32) {
            let folded = fold_tag(&c.to_string());
            let mut chars = folded.chars();
            let (Some(single), None) = (chars.next(), chars.next()) else {
                panic!("{c:?} folds to {folded:?}, not one character");
            };
            assert_eq!(fold_tag(&single.to_string()), folded, "{c:?}");
        }
    }

    /// **A capital sigma folds to `σ` wherever it stands.** The fold reads no
    /// neighbour, so the sigma that ends a word folds as the one inside it.
    #[test]
    fn a_capital_sigma_always_folds_to_the_medial_sigma() {
        assert_eq!(fold_tag("ΟΔΟΣ"), "οδοσ");
        assert_eq!(fold_tag("ΑΣΒ"), "ασβ");
        assert_eq!(fold_tag("ΑΣ*"), "ασ*");
        assert!(one_tag("ΒΣ", "βσ"));
        assert!(!one_tag("ΒΣ", "βς"));
    }

    /// **A character whose lowercase is more than one character keeps its own
    /// identity.** `İ` lowercases to `i` and a combining dot, so it folds to
    /// itself and is neither `i` nor `I`.
    #[test]
    fn a_character_lowercasing_to_several_keeps_its_identity() {
        assert_eq!(fold_tag("aİb"), "aİb");
        assert!(!one_tag("İ", "i"));
        assert!(!one_tag("İ", "I"));
        assert!(!one_tag("İ", "i\u{307}"));
    }
}

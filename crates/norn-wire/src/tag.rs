//! A tag's identity: the one fold every comparison of two tag names reads.
//!
//! **Two tag names are one tag when their folds are equal.** The fold is
//! Unicode lowercase with no locale — Rust's `str::to_lowercase` — so `#Work`
//! and `#work` are one tag and so are `#Über` and `#über`, while an accent is
//! part of the letter and is kept: `#café` is not `#cafe`. The fold covers the
//! whole nested name, `/` included, so `#Area/Work` and `#area/work` are one
//! tag, and a `/`-separated containment test compares the folded names.
//!
//! **The fold is a tag's alone.** A path's fold is ASCII-only and exists only
//! where the vault root folds ([`crate::CaseFold`]); a tag names no file, so
//! its fold is the same on every root. The syntax layer reads a tag as
//! written and never folds it: the fold is what a comparison applies, and a
//! tag stored or reported keeps its written spelling beside it.

/// The fold `name` is compared under: every tag name equal to it under this
/// fold is the same tag.
pub fn fold_tag(name: &str) -> String {
    name.to_lowercase()
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
}

//! The one order every read answers paths in.
//!
//! **An answer orders paths with ASCII case folded, then bytewise**, on every
//! root: `(path COLLATE NOCASE, path)`. The bytewise path after the folded one
//! keeps the order total over two paths that fold together. A find's path
//! page, both sections of a field sort — the missing section's paths and the
//! valued section's tie-break — and each kind of a validate's findings stand in
//! it, so every verb answers one vault in one path order whatever the root's
//! case behaviour.
//!
//! **It is not the order a heal pages stored documents in.** A heal merges its
//! page against a walk of the vault, so it pages in the order the root proved
//! ([`crate::StoredPathOrder`]): bytewise where the root tells spellings apart,
//! which is a different order from this one there. Nothing here is read by a
//! heal, and nothing a heal pages by is read here.
//!
//! The `NOCASE` collation is the store's spelling of the fold, and
//! [`answer_place`] is the same order as this process compares it:
//! `NOCASE` folds `A`-`Z` onto `a`-`z` and compares bytes, which is
//! [`fold_ascii_case`] followed by a byte comparison.

use crate::path::fold_ascii_case;

/// `column` ordered in the answer order as an `ORDER BY` states it, each term
/// carrying `direction` — `""` ascending, `" DESC"` descending.
pub(crate) fn answer_ordering(column: &str, direction: &str) -> String {
    format!("{column} COLLATE NOCASE{direction}, {column}{direction}")
}

/// The rows whose `column` stands past `after` in the answer order, in the
/// direction `comparison` names — `>` ascending, `<` descending — as a seek of
/// an index holding that order reads it.
///
/// `after` is a placeholder that may bind `NULL`, which starts at the first
/// row; `beyond` is what an unset position coalesces to, the bound before
/// every row in the direction. The first term is the seek's bound on the
/// folded path, inclusive, with `seek` spelled before the column — `"+"` takes
/// the term out of an index's reach where another seek drives the statement.
/// The second states the exclusivity: a row folding equal to `after` stands
/// past it only where it sorts past it bytewise.
pub(crate) fn answer_after(
    seek: &str,
    column: &str,
    comparison: &str,
    after: &str,
    beyond: &str,
) -> String {
    format!(
        "{seek}{column} {comparison}= COALESCE({after}, {beyond}) COLLATE NOCASE
                       AND ({after} IS NULL OR {column} {comparison} {after} COLLATE NOCASE
                            OR {column} {comparison} {after})"
    )
}

/// Where `path` stands in the answer order, as this process compares it: the
/// path with ASCII case folded, then the path.
pub(crate) fn answer_place(path: &str) -> (String, String) {
    (fold_ascii_case(path), path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The place this process computes orders paths as `NOCASE` does, with
    /// the bytewise tie-break: `Z` sorts above `[`, `_` and `` ` `` folded,
    /// and `B` ahead of `b` where the two fold together.
    #[test]
    fn a_place_orders_paths_folded_then_bytewise() {
        let mut paths = ["b.md", "Z.md", "_.md", "B.md", "[.md", "`.md", "a.md"];
        paths.sort_by_key(|path| answer_place(path));
        assert_eq!(
            paths,
            ["[.md", "_.md", "`.md", "a.md", "B.md", "b.md", "Z.md"]
        );
    }
}

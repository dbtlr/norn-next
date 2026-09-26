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

use norn_db::rusqlite::types::Value;
use norn_wire::Pattern;

use super::glob::{literal_prefix, prefix_range};
use crate::path::fold_ascii_case;

/// `column` ordered in the answer order as an `ORDER BY` states it, each term
/// carrying `direction` — `""` ascending, `" DESC"` descending.
pub(crate) fn answer_ordering(column: &str, direction: &str) -> String {
    format!("{column} COLLATE NOCASE{direction}, {column}{direction}")
}

/// One column an answer page's index holds, and the bound a row passes it at.
pub(crate) struct Term<'a> {
    pub(crate) column: &'a str,
    pub(crate) bound: &'a str,
}

/// Where an answer page resumes, as a seek of an index holding the answer
/// order reads it: the index's columns from the first the page is not held
/// equal on, and the place the page stands past.
pub(crate) struct AnswerSeek<'a> {
    /// `""`, or `"+"` where another seek drives the statement: spelled before
    /// the first column, which takes the seek out of an index's reach.
    pub(crate) reach: &'a str,
    /// `>` ascending, `<` descending.
    pub(crate) comparison: &'a str,
    /// A column the index orders ahead of the path — a field sort's value —
    /// or `None`.
    pub(crate) lead: Option<Term<'a>>,
    /// The path column.
    pub(crate) path: &'a str,
    /// The bound on the path compared folded.
    pub(crate) folded: &'a str,
    /// The bound on the path compared bytewise.
    pub(crate) bytewise: &'a str,
    /// The row id, where rows share a path, or `None` where the path is unique.
    pub(crate) id: Option<Term<'a>>,
}

impl AnswerSeek<'_> {
    /// The rows standing past the place in the answer order, as one row-value
    /// comparison: `(lead, path, path, id) > (lead, folded, bytewise, id)`, the
    /// folded bound compared under `NOCASE`, each term optional as the seek
    /// states it.
    ///
    /// **SQLite seeks the whole row value**, so a continuation starts exactly
    /// past its place and rereads no row at its path. A row-value range runs as
    /// far into an index's columns as each term names the column there under
    /// that column's collation. The row id declares no collation, and SQLite
    /// takes a term into the seek only where its comparison has one, so the
    /// id's bound is spelled `COLLATE BINARY`; without it the seek stops at the
    /// bytewise path, and a page among many rows at one path rereads the ones
    /// before it.
    pub(crate) fn spelled(&self) -> String {
        let mut columns: Vec<&str> = Vec::new();
        let mut bounds: Vec<String> = Vec::new();
        if let Some(lead) = &self.lead {
            columns.push(lead.column);
            bounds.push(lead.bound.to_string());
        }
        columns.extend([self.path, self.path]);
        bounds.extend([
            format!("{} COLLATE NOCASE", self.folded),
            self.bytewise.to_string(),
        ]);
        if let Some(id) = &self.id {
            columns.push(id.column);
            bounds.push(format!("{} COLLATE BINARY", id.bound));
        }
        format!(
            "({}{}) {} ({})",
            self.reach,
            columns.join(", "),
            self.comparison,
            bounds.join(", ")
        )
    }
}

/// Where `path` stands in the answer order, as this process compares it: the
/// path with ASCII case folded, then the path.
pub(crate) fn answer_place(path: &str) -> (String, String) {
    (fold_ascii_case(path), path.to_string())
}

/// The range in the answer order that holds every path `pattern` matches,
/// under either fold: from the pattern's literal prefix with ASCII case
/// folded, inclusive, to the first text past it, exclusive, each compared
/// under `NOCASE`. A glob matching bytes admits only paths spelling its prefix
/// as written, and those fold to the folded prefix too, so the range holds
/// what the glob admits whatever the root's fold; the glob function decides
/// each path the range reaches.
pub(crate) fn answer_range(pattern: &Pattern) -> (String, Value) {
    prefix_range(fold_ascii_case(literal_prefix(pattern)))
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

    /// The answer's range is the folded prefix's, whatever case the glob
    /// spells it in: `notes/Z*` ranges from `notes/z` to `notes/{`, which
    /// `NOCASE` reaches `notes/Z.md` in.
    #[test]
    fn an_answer_range_folds_the_literal_prefix() {
        let range = |source: &str| answer_range(&Pattern::parse(source).expect("a pattern"));
        assert_eq!(
            range("notes/Z*"),
            ("notes/z".to_string(), Value::Text("notes/{".to_string()))
        );
        assert_eq!(
            range("Notes/**"),
            ("notes".to_string(), Value::Text("notet".to_string()))
        );
        assert_eq!(range("**/*.MD"), (String::new(), Value::Blob(Vec::new())));
    }

    /// A seek names every term it is given, the folded bound under `NOCASE`
    /// and the id's under `BINARY`, with the reach before the first column.
    #[test]
    fn a_seek_spells_one_row_value() {
        let seek = AnswerSeek {
            reach: "+",
            comparison: ">",
            lead: None,
            path: "f.path",
            folded: "?1",
            bytewise: "?2",
            id: Some(Term {
                column: "f.id",
                bound: "?3",
            }),
        };
        assert_eq!(
            seek.spelled(),
            "(+f.path, f.path, f.id) > (?1 COLLATE NOCASE, ?2, ?3 COLLATE BINARY)"
        );
        let led = AnswerSeek {
            reach: "",
            comparison: "<",
            lead: Some(Term {
                column: "f.raw",
                bound: "?1",
            }),
            path: "f.path",
            folded: "?2",
            bytewise: "?2",
            id: None,
        };
        assert_eq!(
            led.spelled(),
            "(f.raw, f.path, f.path) < (?1, ?2 COLLATE NOCASE, ?2)"
        );
    }
}

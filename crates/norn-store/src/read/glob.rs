//! A path glob as a statement runs it: a range the path index the root's order
//! selects seeks, and the match the range leaves to a function registered on
//! the read connection.
//!
//! The grammar is [`Pattern`]'s, and there is one reading of it. The range is a
//! narrowing the grammar implies — every path a pattern matches starts with the
//! pattern's literal prefix, under the case the root's order gives a glob —
//! and the function is [`Pattern::matches`] itself, called by SQLite on each
//! path the range reaches under that same case. So a statement filters by a
//! glob inside the page it reads, and the in-process matcher and the one a
//! statement runs cannot disagree about which paths a glob names.
//!
//! # Case is the root's
//!
//! A glob matches under the order the snapshot reads, the store's recorded
//! path order: bytewise where the root tells spellings apart, and with ASCII
//! case folded where it folds them ([`StoredPathOrder::glob_case`]). A find's
//! and a count's range follow ([`path_range`]): a bytewise range of the path
//! as written on `documents_path` where the root tells spellings apart, and
//! where it folds, a `NOCASE` range of the literal prefix with ASCII case
//! folded on `documents_path_nocase`. A validate's range is the answer order's
//! on every root ([`super::answer_order::answer_range`]), because its findings
//! answer in the folded order the findings indexes hold; the glob still
//! decides each path under the root's fold.
//!
//! A glob's work follows its literal prefix's range. A glob with no literal
//! prefix, such as `**/*.MD`, ranges over every path, and the glob function
//! reads each one on either root.

use norn_db::rusqlite::functions::FunctionFlags;
use norn_db::rusqlite::types::{Value, ValueRef};
use norn_db::rusqlite::{self, Connection};
use norn_wire::Pattern;

use crate::error::{self, StoreError};
use crate::facts::StoredPathOrder;
use crate::path::{fold_ascii_case, prefix_successor};

/// The name a statement calls the glob match by:
/// `norn_glob(pattern, path, path_order)`.
pub(crate) const GLOB_FUNCTION: &str = "norn_glob";

/// Register the functions a read builder's statements call on a read
/// connection.
///
/// The glob match is **deterministic** — its answer is a function of its three
/// arguments, the recorded spelling of the path order among them, so the case
/// a glob matches under is the statement's input rather than the connection's
/// state — and registered as such, so SQLite may factor a call over constant
/// arguments out of a loop. The pattern is parsed once per statement and kept
/// as the call's auxiliary data for the rows after the first.
pub(crate) fn register_functions(connection: &Connection) -> Result<(), StoreError> {
    connection
        .create_scalar_function(
            GLOB_FUNCTION,
            3,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
            |context| {
                let pattern = context.get_or_create_aux(0, |source: ValueRef<'_>| {
                    let source = source
                        .as_str()
                        .map_err(|problem| rusqlite::Error::UserFunctionError(Box::new(problem)))?;
                    Pattern::parse(source)
                        .map_err(|problem| rusqlite::Error::UserFunctionError(Box::new(problem)))
                })?;
                let path = context
                    .get_raw(1)
                    .as_str()
                    .map_err(|problem| rusqlite::Error::UserFunctionError(Box::new(problem)))?;
                let recorded = context
                    .get_raw(2)
                    .as_str()
                    .map_err(|problem| rusqlite::Error::UserFunctionError(Box::new(problem)))?;
                let order = StoredPathOrder::from_recorded(recorded).ok_or_else(|| {
                    rusqlite::Error::UserFunctionError(
                        format!("`{recorded}` is no path order").into(),
                    )
                })?;
                Ok(pattern.matches(path, order.glob_case()))
            },
        )
        .map_err(|problem| error::sql("registering the path-glob function", problem))
}

/// The range of paths every path `pattern` matches under `order` stands in:
/// from the pattern's literal prefix, inclusive, to the first text that does
/// not start with it, exclusive — compared bytewise where `order` tells
/// spellings apart, and under `NOCASE` where it folds ASCII case.
///
/// Where `order` folds, the prefix is folded before it is stepped: `NOCASE`
/// compares the folded spellings, so every path the folded glob matches folds
/// to a text starting with the folded prefix, and the successor of that folded
/// text bounds them all.
pub(crate) fn path_range(pattern: &Pattern, order: StoredPathOrder) -> (String, Value) {
    let prefix = literal_prefix(pattern);
    prefix_range(match order {
        StoredPathOrder::Sensitive => prefix.to_string(),
        StoredPathOrder::AsciiCaseInsensitive => fold_ascii_case(prefix),
    })
}

/// The text every path `pattern` matches starts with: the text before the
/// first wildcard. Where that wildcard opens a `**` segment, the separator
/// before it is left out too, because `**` matches the run of no segments:
/// `notes/**` matches `notes` itself, which does not start with `notes/`.
pub(crate) fn literal_prefix(pattern: &Pattern) -> &str {
    let source = pattern.as_str();
    let first_wildcard = source.find(['*', '?']).unwrap_or(source.len());
    let prefix = &source[..first_wildcard];
    let opens_any_depth = source[first_wildcard..].starts_with("**")
        && (prefix.is_empty() || prefix.ends_with('/'))
        && source[first_wildcard + 2..]
            .chars()
            .next()
            .is_none_or(|next| next == '/');
    if opens_any_depth {
        prefix.strip_suffix('/').unwrap_or(prefix)
    } else {
        prefix
    }
}

/// The range of texts starting with `prefix`: from `prefix`, inclusive, to its
/// [`prefix_successor`], exclusive. A prefix with no successor — empty, or
/// made only of the last character there is — is bounded above by an empty
/// blob, which SQLite orders after every text under either collation: the
/// range is then every path from the lower bound on.
///
/// Compared under `NOCASE`, a successor that steps onto an upper-case letter —
/// `@` onto `A` — folds back to its lower case, which widens the range and
/// never narrows it; the glob function is what decides a path.
pub(crate) fn prefix_range(prefix: String) -> (String, Value) {
    let upper = prefix_successor(&prefix).map_or(Value::Blob(Vec::new()), Value::Text);
    (prefix, upper)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(source: &str) -> (String, Value) {
        range_under(source, StoredPathOrder::Sensitive)
    }

    fn range_under(source: &str, order: StoredPathOrder) -> (String, Value) {
        path_range(&Pattern::parse(source).expect("a pattern"), order)
    }

    #[test]
    fn a_literal_prefix_bounds_the_range_it_opens() {
        assert_eq!(
            range("notes/*.md"),
            ("notes/".to_string(), Value::Text("notes0".to_string()))
        );
        assert_eq!(
            range("notes/a?.md"),
            ("notes/a".to_string(), Value::Text("notes/b".to_string()))
        );
        assert_eq!(
            range("notes/first.md"),
            (
                "notes/first.md".to_string(),
                Value::Text("notes/first.me".to_string())
            )
        );
    }

    /// `**` matches no segments at all, so the separator before it is not a
    /// character every match carries.
    #[test]
    fn an_any_depth_segment_leaves_its_separator_out_of_the_prefix() {
        assert_eq!(
            range("notes/**"),
            ("notes".to_string(), Value::Text("notet".to_string()))
        );
        assert_eq!(
            range("notes/**/a.md"),
            ("notes".to_string(), Value::Text("notet".to_string()))
        );
        // Two stars inside a segment are a `*`, not a segment quantifier.
        assert_eq!(
            range("notes/a**"),
            ("notes/a".to_string(), Value::Text("notes/b".to_string()))
        );
        assert_eq!(range("**/a.md"), (String::new(), Value::Blob(Vec::new())));
    }

    /// Where the root folds ASCII case, the range is the folded prefix's, so
    /// `NOCASE` reaches every spelling of it; where it does not, the prefix is
    /// the text as written.
    #[test]
    fn a_folding_order_ranges_over_the_folded_prefix() {
        let folded = StoredPathOrder::AsciiCaseInsensitive;
        assert_eq!(
            range_under("Notes/Z*.md", folded),
            ("notes/z".to_string(), Value::Text("notes/{".to_string()))
        );
        assert_eq!(
            range_under("Notes/**", folded),
            ("notes".to_string(), Value::Text("notet".to_string()))
        );
        assert_eq!(
            range_under("Notes/Z*.md", StoredPathOrder::Sensitive),
            ("Notes/Z".to_string(), Value::Text("Notes/[".to_string()))
        );
        // A letter outside ASCII keeps its case.
        assert_eq!(
            range_under("Été/*", folded),
            ("Été/".to_string(), Value::Text("Été0".to_string()))
        );
    }
}

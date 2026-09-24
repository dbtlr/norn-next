//! A path glob as a statement runs it: a range the path index seeks, and the
//! match the range leaves to a function registered on the read connection.
//!
//! The grammar is [`Pattern`]'s, and there is one reading of it. The range is a
//! narrowing the grammar implies — every path a pattern matches starts with the
//! pattern's literal prefix — and the function is [`Pattern::matches`] itself,
//! called by SQLite on each path the range reaches. So a statement filters by a
//! glob inside the page it reads, and the in-process matcher and the one a
//! statement runs cannot disagree about which paths a glob names.

use norn_db::rusqlite::functions::FunctionFlags;
use norn_db::rusqlite::types::{Value, ValueRef};
use norn_db::rusqlite::{self, Connection};
use norn_wire::{CaseFold, Pattern};

use crate::error::{self, StoreError};
use crate::path::prefix_successor;

/// The name a statement calls the glob match by: `norn_glob(pattern, path)`.
pub(crate) const GLOB_FUNCTION: &str = "norn_glob";

/// Register the functions a read builder's statements call on a read
/// connection.
///
/// The glob match is **deterministic** — its answer is a function of its two
/// arguments — and registered as such, so SQLite may factor a call over
/// constant arguments out of a loop. The pattern is parsed once per statement
/// and kept as the call's auxiliary data for the rows after the first.
pub(crate) fn register_functions(connection: &Connection) -> Result<(), StoreError> {
    connection
        .create_scalar_function(
            GLOB_FUNCTION,
            2,
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
                Ok(pattern.matches(path, CaseFold::Exact))
            },
        )
        .map_err(|problem| error::sql("registering the path-glob function", problem))
}

/// The range of paths every path `pattern` matches stands in: from the
/// pattern's literal prefix, inclusive, to the first text that does not start
/// with it, exclusive.
///
/// The prefix is the text before the first wildcard. Where that wildcard opens
/// a `**` segment, the separator before it is left out of the prefix too,
/// because `**` matches the run of no segments: `notes/**` matches `notes`
/// itself, which does not start with `notes/`.
///
/// The upper bound is the prefix's [`prefix_successor`]. A prefix with no
/// successor — empty, or made only of the last character there is — is bounded
/// above by an empty blob, which SQLite orders after every text: the range is
/// then every path from the lower bound on.
pub(crate) fn path_range(pattern: &Pattern) -> (String, Value) {
    let source = pattern.as_str();
    let first_wildcard = source.find(['*', '?']).unwrap_or(source.len());
    let mut prefix = &source[..first_wildcard];
    let opens_any_depth = source[first_wildcard..].starts_with("**")
        && (prefix.is_empty() || prefix.ends_with('/'))
        && source[first_wildcard + 2..]
            .chars()
            .next()
            .is_none_or(|next| next == '/');
    if opens_any_depth {
        prefix = prefix.strip_suffix('/').unwrap_or(prefix);
    }
    let upper = prefix_successor(prefix).map_or(Value::Blob(Vec::new()), Value::Text);
    (prefix.to_string(), upper)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(source: &str) -> (String, Value) {
        path_range(&Pattern::parse(source).expect("a pattern"))
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
}

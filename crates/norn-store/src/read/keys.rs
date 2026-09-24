//! The walk of the distinct keys documents carry, which every read that
//! enumerates those keys reads them through.

/// The walk of the distinct keys documents carry, in key order: a recursive
/// table `walked(key)` whose first row is the least key standing `from` —
/// a comparison and its bound, such as `> ?1` — and each row after it the
/// least key after the one before, ending in a `NULL` row once no key is left.
/// `rows`, where named, is the placeholder bounding how many rows it yields.
///
/// **Each step is one seek of the presence index**, `document_fields_presence`,
/// for the least key past a bound, so the walk reads one index entry per
/// distinct key rather than one per document that carries it. Every read that
/// enumerates the keys documents carry walks them through this one spelling.
pub(crate) fn key_walk(from: &str, rows: Option<&str>) -> String {
    let bounded = rows.map_or_else(String::new, |rows| format!("\n             LIMIT {rows}"));
    format!(
        "WITH RECURSIVE walked(key) AS (
             SELECT (SELECT MIN(o.key) FROM document_fields AS o
                      WHERE o.ordinal = 0 AND o.key {from})
             UNION ALL
             SELECT (SELECT MIN(o.key) FROM document_fields AS o
                      WHERE o.ordinal = 0 AND o.key > walked.key)
               FROM walked WHERE walked.key IS NOT NULL{bounded}
         )"
    )
}

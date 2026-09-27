//! The statements the link-health judgment runs, each spelled once and read by
//! both the judgment and the plan seam that bars it
//! ([`crate::ExplainedStatement`]).
//!
//! Every statement is driven by a list a `json_each` walks — the documents,
//! the distinct keys, the candidates' spellings, the links with one target —
//! or by one seek of the link index — a class's range, or a path key — and
//! `CROSS JOIN` keeps that driver the outer loop, so each entry costs its own
//! seeks and nothing is read end to end.

use norn_db::rusqlite::types::Value;

use crate::anchor::{anchor_held, carries_anchor};
use crate::error::StoreError;
use crate::facts::StoredPathOrder;
use crate::find::path_list;
use crate::health::LinkSelection;
use crate::json::{FrontmatterValue, canonical_json};
use crate::path::{ClassKey, PathKey, SuffixKey};
use crate::resolve::{self, AmbiguityIgnore};

/// Which arm of [`heads_sql`] and [`totals_sql`] a row came from: a suffix
/// key's class.
pub(crate) const CLASS_ARM: i64 = 0;

/// Which arm of [`heads_sql`] and [`totals_sql`] a row came from: the
/// documents at a path key.
pub(crate) const PATH_ARM: i64 = 1;

/// Which way [`links_sql`] reaches the links it reads: the statement's shape
/// for each [`LinkSelection`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Selected {
    /// By the documents holding them: an equality seek of `documents_path`
    /// per path, then of `links_document_ordinal` per document.
    Documents,
    /// By a class: a range seek of the link index over the class's keys.
    Class,
    /// By a path key: an equality seek of the link index at the key.
    Path,
}

impl Selected {
    /// The shape `selection` is read in.
    pub(crate) fn of(selection: LinkSelection<'_>) -> Self {
        match selection {
            LinkSelection::Documents(_) => Selected::Documents,
            LinkSelection::Class(_) => Selected::Class,
            LinkSelection::Path(_) => Selected::Path,
        }
    }
}

/// The links a judgment judges, reached the way `selected` names, one row per
/// key the link index holds each under in the key space `key` selects, and
/// one row with no key for a link held under none:
/// [`crate::ExplainedStatement::LinkHealthLinks`] over the documents at the
/// paths `?1` lists, [`crate::ExplainedStatement::LinkHealthClassLinks`] over
/// the links held under a key in the class `?1` opens and `?2` closes, and
/// [`crate::ExplainedStatement::LinkHealthPathLinks`] over the links held
/// under the path key `?1`.
///
/// The row's first thirteen columns are a link row's, in the order
/// [`crate::request::stored_link_row`] reads them; then the holding
/// document's path, the link's row id and ordinal, and the key and its
/// segment count. A link the selection reaches is read with every key it is
/// held under, whichever of them reached it.
pub(crate) fn links_sql(key: SuffixKey, selected: Selected) -> String {
    let link_key = resolve::link_key_column(key);
    let (from, reached) = match selected {
        Selected::Documents => (
            "json_each(?1) AS j CROSS JOIN documents AS d CROSS JOIN links AS l",
            "d.path = j.value AND l.document = d.id".to_string(),
        ),
        Selected::Class => (
            "link_keys AS s CROSS JOIN links AS l CROSS JOIN documents AS d",
            format!(
                "s.segments IS NOT NULL AND s.{link_key} >= ?1 AND s.{link_key} < ?2 \
                 AND l.id = s.link AND d.id = l.document"
            ),
        ),
        Selected::Path => (
            "link_keys AS s CROSS JOIN links AS l CROSS JOIN documents AS d",
            format!("s.{link_key} = ?1 AND l.id = s.link AND d.id = l.document"),
        ),
    };
    format!(
        "SELECT l.family, l.embed, l.protocol, l.target, l.title, l.anchor, l.block_ref,
                l.span_line, l.span_column, l.span_offset, l.anchor_text, l.anchor_marked,
                l.address, d.path, l.id, l.ordinal, k.{link_key}, k.segments
         FROM {from}
         LEFT JOIN link_keys AS k ON k.link = l.id
         WHERE {reached}"
    )
}

/// The values [`links_sql`] binds over documents: their paths.
pub(crate) fn documents_parameters(documents: &[&str]) -> Result<Vec<Value>, StoreError> {
    Ok(vec![path_list(documents)?])
}

/// The values [`links_sql`] binds over a class: its bounds.
pub(crate) fn class_parameters(class: &ClassKey) -> Vec<Value> {
    let (lower, upper) = class.bounds();
    vec![Value::Text(lower), Value::Text(upper)]
}

/// The values [`links_sql`] binds over a path key: the key.
pub(crate) fn path_parameters(path: &PathKey) -> Vec<Value> {
    vec![Value::Text(path.as_str().to_string())]
}

/// [`crate::ExplainedStatement::LinkHealthHeads`]: the head of what each
/// distinct key names, at most `?5` documents in the resolution ladder's
/// order, each row the arm, the key's index in its list, and the document's
/// id, path and rung.
///
/// Two arms, one per key space a link is addressed in. The class arm walks
/// the suffix keys `?1` lists, each a `[key, segments]` pair, and reads each
/// key's class through [`resolve::link_key_class`] — a range seek of the
/// suffix key the root probes, less the places the ignore set `?3` excludes
/// under the order `?4` — its rung the document's suffix key. The path arm
/// walks the path keys `?2` lists and reads the documents at each through
/// [`resolve::link_key_path`], its rung the key itself, so a path key's rows
/// rank by the path alone. Each head is cut in the statement, so a key costs
/// its class and hands back at most `?5` rows, however large the class.
///
/// The rows come back in no stated order: the judgment orders each head, and
/// merges a link's heads, by [`resolve::ladder_cmp`], the order the cut here
/// spells in SQL.
pub(crate) fn heads_sql(key: SuffixKey) -> String {
    let rung = key.column();
    let class = class_predicate(key);
    let path = resolve::link_key_path("dp", key, "j.value");
    let ladder = resolve::ladder(&format!("dl.{rung}"), "dl.path");
    format!(
        "SELECT {CLASS_ARM}, j.key, dc.id, dc.path, dc.{rung}
         FROM json_each(?1) AS j CROSS JOIN documents AS dc
         WHERE dc.id IN (SELECT dl.id FROM documents AS dl WHERE {class}
                         ORDER BY {ladder} LIMIT ?5)
         UNION ALL
         SELECT {PATH_ARM}, j.key, dc.id, dc.path, j.value
         FROM json_each(?2) AS j CROSS JOIN documents AS dc
         WHERE dc.id IN (SELECT dp.id FROM documents AS dp WHERE {path}
                         ORDER BY dp.path LIMIT ?5)"
    )
}

/// [`crate::ExplainedStatement::LinkHealthTotals`]: how many documents each
/// distinct key names, each row the arm, the key's index in its list, and the
/// count, over the two arms and the same predicates [`heads_sql`] reads.
pub(crate) fn totals_sql(key: SuffixKey) -> String {
    let class = class_predicate(key);
    let path = resolve::link_key_path("dp", key, "j.value");
    format!(
        "SELECT {CLASS_ARM}, j.key, (SELECT COUNT(*) FROM documents AS dl WHERE {class})
         FROM json_each(?1) AS j
         UNION ALL
         SELECT {PATH_ARM}, j.key, (SELECT COUNT(*) FROM documents AS dp WHERE {path})
         FROM json_each(?2) AS j"
    )
}

/// The class of the `[key, segments]` pair the row `j` walks, as the `documents`
/// rows a statement calls `dl`.
fn class_predicate(key: SuffixKey) -> String {
    resolve::link_key_class(
        "dl",
        key,
        "json_extract(j.value, '$[0]')",
        "json_extract(j.value, '$[1]')",
        "?3",
        "?4",
    )
}

/// The values [`heads_sql`] binds, and [`totals_sql`] less the head's bound:
/// the suffix keys beside their segment counts, the path keys, the ignore set
/// and the order it is matched under, and the head's bound where it is one.
pub(crate) fn keys_parameters(
    classes: &[(&str, u64)],
    paths: &[&str],
    ignore: &AmbiguityIgnore,
    order: StoredPathOrder,
    head: Option<usize>,
) -> Result<Vec<Value>, StoreError> {
    let classes = canonical_json(&FrontmatterValue::Sequence(
        classes
            .iter()
            .map(|(key, segments)| {
                FrontmatterValue::Sequence(vec![
                    FrontmatterValue::String((*key).to_string()),
                    FrontmatterValue::Int(
                        i64::try_from(*segments).expect("a segment count fits i64"),
                    ),
                ])
            })
            .collect(),
    ))?;
    let mut values = vec![
        Value::Text(classes),
        path_list(paths)?,
        Value::Text(ignore.encoded()),
        Value::Text(order.as_str().to_string()),
    ];
    values.extend(head.map(|head| Value::Integer(i64::try_from(head).expect("a head fits i64"))));
    Ok(values)
}

/// [`crate::ExplainedStatement::LinkHealthAnchors`]: for each `[link, target]`
/// pair `?1` lists whose link carries an anchor ([`carries_anchor`]), the
/// pair's index and whether the target document holds the place the anchor
/// names ([`anchor_held`]). A pair whose link carries no anchor answers no
/// row.
///
/// Each link is reached by its row id, and each anchor is a handful of
/// equality seeks into the one target document.
pub(crate) fn anchors_sql() -> String {
    format!(
        "SELECT j.key, {held} FROM json_each(?1) AS j CROSS JOIN links AS l
         WHERE l.id = json_extract(j.value, '$[0]') AND {carries}",
        held = anchor_held("l", "json_extract(j.value, '$[1]')"),
        carries = carries_anchor("l"),
    )
}

/// The values [`anchors_sql`] binds: the `[link, target]` pairs.
pub(crate) fn anchors_parameters(pairs: &[(i64, i64)]) -> Result<Vec<Value>, StoreError> {
    Ok(vec![Value::Text(canonical_json(
        &FrontmatterValue::Sequence(
            pairs
                .iter()
                .map(|(link, target)| {
                    FrontmatterValue::Sequence(vec![
                        FrontmatterValue::Int(*link),
                        FrontmatterValue::Int(*target),
                    ])
                })
                .collect(),
        ),
    )?)])
}

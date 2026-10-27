//! The statements the link-health judgment runs, each spelled once and read by
//! both the judgment and the plan seam that bars it
//! ([`crate::ExplainedStatement`]).
//!
//! Every statement is driven by a list a `json_each` walks — the documents,
//! the links or findings by row id, the distinct keys, the classes and paths
//! a changeset changed, the candidates' spellings, the links with one target —
//! or by one range or equality seek:
//! a class's range of the suffix keys, a path key, or a class's range of the
//! findings' class keys. `CROSS JOIN` keeps that driver the outer loop, so
//! each entry costs its own seeks and nothing is read end to end.

use norn_db::rusqlite::types::Value;

use crate::anchor::{anchor_held, carries_anchor};
use crate::error::StoreError;
use crate::find::path_list;
use crate::health::LinkSelection;
use crate::json::{FrontmatterValue, canonical_json};
use crate::path::{ClassKey, PathKey, SuffixKey};
use crate::resolve;

/// Which arm of [`heads_sql`] and [`totals_sql`] a row came from: a suffix
/// key's class.
pub(crate) const CLASS_ARM: i64 = 0;

/// Which arm of [`heads_sql`] and [`totals_sql`] a row came from: the
/// documents at a path key.
pub(crate) const PATH_ARM: i64 = 1;

/// Which way [`links_sql`] reaches the links it reads: the statement's shape
/// for each [`LinkSelection`], and for the links a changeset wrote.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Selected {
    /// By the documents holding them: an equality seek of `documents_path`
    /// per path, then of `links_document_ordinal` per document. Read whole.
    Documents,
    /// By the write that stamped their documents: a seek of
    /// `documents_change_feed` at one generation, in path order, then of
    /// `links_document_ordinal` per document. Read a page at a time.
    Written,
    /// Every stored link, paged by binary document path and link ordinal.
    Every,
    /// By a class: a range seek of the link index over the class's keys. Read
    /// a page at a time.
    Class,
    /// By a path key: an equality seek of the link index at the key. Read a
    /// page at a time.
    Path,
    /// By a directory range of path keys alone.
    Beneath,
    /// By their row ids: an equality seek of `links` per id. Read whole.
    Links,
}

impl Selected {
    /// The shape `selection` is read in.
    pub(crate) fn of(selection: LinkSelection<'_>) -> Self {
        match selection {
            LinkSelection::Documents(_) => Selected::Documents,
            LinkSelection::Class(_) => Selected::Class,
            LinkSelection::Path(_) => Selected::Path,
            LinkSelection::Beneath(_) => Selected::Beneath,
        }
    }
}

/// Where a paged selection resumes: the row of its driver it read last, which
/// [`links_sql`] hands back beside every link it reads.
///
/// The driver is the one index a paged shape seeks, and the page resumes past
/// this row of it: for the links a write stamped, the holding document's path
/// and the link's ordinal (`second` unused); for a class or a path key, the
/// link index row's key, its document, and its link, which the index holds in
/// that order.
#[derive(Clone, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct After {
    pub(crate) text: String,
    pub(crate) first: i64,
    pub(crate) second: i64,
}

impl After {
    /// Where a class's first page starts: before every key the class opens.
    pub(crate) fn class_start(class: &ClassKey) -> Self {
        After {
            text: class.bounds().0,
            first: -1,
            second: -1,
        }
    }

    /// Where a path key's first page starts: before every row at the key.
    pub(crate) fn path_start(path: &PathKey) -> Self {
        After {
            text: path.as_str().to_string(),
            first: -1,
            second: -1,
        }
    }

    /// Where the first page of a write's links starts: before every path.
    pub(crate) fn written_start() -> Self {
        After {
            text: String::new(),
            first: -1,
            second: 0,
        }
    }
}

/// The links a judgment judges, reached the way `selected` names, one row per
/// key the link index holds each under in the key space `key` selects, and
/// one row with no key for a link held under none:
/// [`crate::ExplainedStatement::LinkHealthLinks`] over the documents at the
/// paths `?1` lists, and [`crate::ExplainedStatement::LinkHealthFoundLinks`]
/// over the links whose row ids `?1` lists, each read whole; and, a page of at most the bound the last
/// parameter names at a time, each resuming past the driver row [`After`]
/// spells, [`crate::ExplainedStatement::LinkHealthWrittenLinks`] over the
/// links of the documents stamped with the generation `?1`,
/// [`crate::ExplainedStatement::LinkHealthClassLinks`] over the links held
/// under a key in the class `?1` closes, and
/// [`crate::ExplainedStatement::LinkHealthPathLinks`] over the links held
/// under the path key `?1`.
///
/// The row's first fourteen columns are a link row's, in the order
/// [`crate::request::stored_link_row`] reads them; then the holding
/// document's path, the link's row id and ordinal, the key and its segment
/// count, the holding document's generation, and the driver row a page
/// resumes past, which a whole read leaves `NULL`. A link the selection
/// reaches is read with every key it is held under, whichever of them reached
/// it.
///
/// A page is cut in its driver, a subquery the rest of the statement runs
/// once as its outer loop, so the bound counts links and never the keys each
/// is read beside, and a page costs its own links however far into its
/// driver it starts.
pub(crate) fn links_sql(key: SuffixKey, selected: Selected) -> String {
    let link_key = resolve::link_key_column(key);
    let columns = format!(
        "l.family, l.embed, l.protocol, l.target, l.title, l.anchor, l.block_ref,
                l.span_line, l.span_column, l.span_offset, l.anchor_text, l.anchor_marked,
                l.field, l.address, d.path, l.id, l.ordinal, k.{link_key}, k.segments, d.generation"
    );
    let driver = match selected {
        Selected::Documents => {
            return format!(
                "SELECT {columns}, NULL, NULL, NULL, EXISTS(SELECT 1 FROM directory_marks WHERE path_key >= '')
         FROM json_each(?1) AS j CROSS JOIN documents AS d CROSS JOIN links AS l
         LEFT JOIN link_keys AS k ON k.link = l.id
         WHERE d.path = j.value AND l.document = d.id"
            );
        }
        Selected::Links => {
            return format!(
                "SELECT {columns}, NULL, NULL, NULL, EXISTS(SELECT 1 FROM directory_marks WHERE path_key >= '')
         FROM json_each(?1) AS j CROSS JOIN links AS l CROSS JOIN documents AS d
         LEFT JOIN link_keys AS k ON k.link = l.id
         WHERE l.id = j.value AND d.id = l.document"
            );
        }
        Selected::Every => "SELECT lw.id AS link, dw.path AS after_text,
                        lw.ordinal AS after_first, 0 AS after_second
                 FROM documents AS dw CROSS JOIN links AS lw
                 WHERE dw.path >= ?2 AND lw.document = dw.id
                   AND lw.ordinal > CASE WHEN dw.path = ?2 THEN ?3 ELSE -1 END
                 ORDER BY dw.path, lw.ordinal LIMIT ?5"
            .to_string(),
        Selected::Written => "SELECT lw.id AS link, dw.path AS after_text,
                        lw.ordinal AS after_first, 0 AS after_second
                 FROM documents AS dw CROSS JOIN links AS lw
                 WHERE dw.generation = ?1 AND dw.path >= ?2 AND lw.document = dw.id
                   AND lw.ordinal > CASE WHEN dw.path = ?2 THEN ?3 ELSE -1 END
                 ORDER BY dw.path, lw.ordinal LIMIT ?5"
            .to_string(),
        Selected::Class => format!(
            "SELECT s.link AS link, s.{link_key} AS after_text, s.document AS after_first,
                        s.link AS after_second
                 FROM link_keys AS s
                 WHERE (s.{link_key}, s.document, s.link) > (?2, ?3, ?4) AND s.{link_key} < ?1
                   AND s.segments IS NOT NULL
                 ORDER BY s.{link_key}, s.document, s.link LIMIT ?5"
        ),
        Selected::Beneath => format!(
            "SELECT s.link AS link, s.{link_key} AS after_text, s.document AS after_first,
                        s.link AS after_second
                 FROM link_keys AS s INDEXED BY link_keys_path_key
                 WHERE (s.{link_key}, s.document, s.link) > (?2, ?3, ?4) AND s.{link_key} < ?1
                   AND s.segments IS NULL
                   AND NOT EXISTS (SELECT 1 FROM link_keys AS earlier
                       WHERE earlier.link = s.link AND earlier.segments IS NULL
                         AND earlier.key >= ?6 AND earlier.key < s.key)
                 ORDER BY s.{link_key}, s.document, s.link LIMIT ?5"
        ),
        Selected::Path => format!(
            "SELECT s.link AS link, s.{link_key} AS after_text, s.document AS after_first,
                        s.link AS after_second
                 FROM link_keys AS s
                 WHERE s.{link_key} = ?1 AND (s.document, s.link) > (?3, ?4)
                 ORDER BY s.document, s.link LIMIT ?5"
        ),
    };
    format!(
        "SELECT {columns}, p.after_text, p.after_first, p.after_second, EXISTS(SELECT 1 FROM directory_marks WHERE path_key >= '')
         FROM ({driver}) AS p CROSS JOIN links AS l CROSS JOIN documents AS d
         LEFT JOIN link_keys AS k ON k.link = l.id
         WHERE l.id = p.link AND d.id = l.document"
    )
}

/// The values for a page of every stored link; no generation filters it.
pub(crate) fn every_parameters(after: &After, limit: usize) -> Vec<Value> {
    paged(Value::Null, after, limit)
}

/// Discard only the link-health findings about a page's links, by the same
/// fingerprint, path and ordinal that make each link's finding unique.
pub(crate) fn judged_discard_sql() -> String {
    let kinds = crate::health::LINK_HEALTH_KINDS
        .iter()
        .map(|kind| format!("'{}'", kind.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "DELETE FROM findings WHERE id IN (
        SELECT f.id FROM json_each(?1) AS j CROSS JOIN findings AS f
        WHERE f.vault_schema_fingerprint = ?2
          AND f.path = json_extract(j.value, '$[0]')
          AND f.ordinal = json_extract(j.value, '$[1]')
          AND f.kind IN ({kinds}))"
    )
}

/// The links a page's discard replaces and the fingerprint they are filed under.
pub(crate) fn judged_discard_parameters(
    links: &[(&str, u64)],
    fingerprint: &str,
) -> Result<Vec<Value>, StoreError> {
    Ok(vec![
        Value::Text(canonical_json(&FrontmatterValue::Sequence(
            links
                .iter()
                .map(|(path, ordinal)| {
                    FrontmatterValue::Sequence(vec![
                        FrontmatterValue::String((*path).to_string()),
                        FrontmatterValue::Int(
                            i64::try_from(*ordinal).expect("a stored ordinal fits i64"),
                        ),
                    ])
                })
                .collect(),
        ))?),
        Value::Text(fingerprint.to_string()),
    ])
}

/// The values [`links_sql`] binds over one page of the links the write
/// stamped with `generation` wrote: the generation, where the page resumes,
/// and its bound.
pub(crate) fn written_parameters(generation: i64, after: &After, limit: usize) -> Vec<Value> {
    paged(Value::Integer(generation), after, limit)
}

/// The values [`links_sql`] binds over one page of a class: its upper bound,
/// where the page resumes, and its bound. The lower bound is the first
/// page's resume point ([`After::class_start`]).
pub(crate) fn class_parameters(class: &ClassKey, after: &After, limit: usize) -> Vec<Value> {
    paged(Value::Text(class.bounds().1), after, limit)
}

/// The values [`links_sql`] binds over one page of a path key: the key,
/// where the page resumes, and its bound.
pub(crate) fn path_parameters(path: &PathKey, after: &After, limit: usize) -> Vec<Value> {
    key_parameters(path.as_str(), after, limit)
}

/// The values [`links_sql`] binds over one page of the links held under
/// exactly `key`, a path key or a suffix key alike, read in
/// [`Selected::Path`]'s shape: the key, where the page resumes, and its
/// bound. The shape is an equality seek of the link index at the key, which
/// holds a suffix key as written beside a path, so the one statement reads
/// either.
pub(crate) fn key_parameters(key: &str, after: &After, limit: usize) -> Vec<Value> {
    paged(Value::Text(key.to_string()), after, limit)
}

/// A paged shape's values: what it selects by, the three columns of the
/// driver row it resumes past, and its bound.
fn paged(selected_by: Value, after: &After, limit: usize) -> Vec<Value> {
    vec![
        selected_by,
        Value::Text(after.text.clone()),
        Value::Integer(after.first),
        Value::Integer(after.second),
        Value::Integer(i64::try_from(limit).expect("a page bound fits i64")),
    ]
}

/// The values [`links_sql`] binds over documents: their paths.
pub(crate) fn documents_parameters(documents: &[&str]) -> Result<Vec<Value>, StoreError> {
    Ok(vec![path_list(documents)?])
}

/// The values [`links_sql`] binds over links, and [`discard_sql`] over
/// findings: their row ids.
pub(crate) fn ids_parameters(ids: &[i64]) -> Result<Vec<Value>, StoreError> {
    Ok(vec![Value::Text(canonical_json(
        &FrontmatterValue::Sequence(ids.iter().copied().map(FrontmatterValue::Int).collect()),
    )?)])
}

/// [`crate::ExplainedStatement::LinkHealthClassFindings`]: a page of the
/// findings standing under a class that no statement of the changeset
/// stamped with `?4` filed, at most `?5` rows, each the class key it is held
/// under, its row id, and the row id of the link it is about — `NULL` where
/// it is about no link. The page resumes past the `(class key, finding)` pair
/// `?2`, `?3`, and ends below the class's upper bound `?1`.
///
/// A range seek of `finding_classes_class_key`, which holds the finding
/// beside its key and so orders the pairs the page resumes by; each finding
/// is reached by its row id, its holder by its path, and its link by the
/// holder and the ordinal. A finding the changeset filed is stepped past, and
/// is one of the re-decided set's own.
pub(crate) fn class_findings_sql() -> String {
    "SELECT s.class_key, s.finding, l.id
     FROM finding_classes AS s CROSS JOIN findings AS f
     LEFT JOIN documents AS d ON d.path = f.path
     LEFT JOIN links AS l ON l.document = d.id AND l.ordinal = f.ordinal
     WHERE (s.class_key, s.finding) > (?2, ?3) AND s.class_key < ?1
       AND f.id = s.finding AND f.generation < ?4
     ORDER BY s.class_key, s.finding LIMIT ?5"
        .to_string()
}

/// The values [`class_findings_sql`] binds over one page of a class: its
/// upper bound, the pair the page resumes past, the generation the changeset
/// was stamped with, and the page's bound.
pub(crate) fn class_findings_parameters(
    class: &ClassKey,
    after: &After,
    generation: i64,
    limit: usize,
) -> Vec<Value> {
    vec![
        Value::Text(class.bounds().1),
        Value::Text(after.text.clone()),
        Value::Integer(after.first),
        Value::Integer(generation),
        Value::Integer(i64::try_from(limit).expect("a page bound fits i64")),
    ]
}

/// Which arm of [`occupied_sql`] a row came from: a class some finding is
/// held under.
pub(crate) const FINDINGS_ARM: i64 = 2;

/// [`crate::ExplainedStatement::LinkHealthOccupied`]: which of the classes
/// `?1` lists and the path keys `?2` lists a re-decision's passes could reach
/// anything under, each row the arm and the key's index in its list.
///
/// Three arms, one per pass: [`CLASS_ARM`] answers each class a suffix key the
/// comparison key names falls in, [`FINDINGS_ARM`] each class a finding is held under a
/// class key in, and [`PATH_ARM`] each path key a link is held under exactly.
/// These are the rows each pass reads first, so a key an arm answers no row
/// for is one that arm's pass would read nothing under. The class arms walk
/// the `[lower, upper]` bounds `?1` lists, each a range seek of the suffix
/// index or of `finding_classes_class_key`; the path arm an equality seek of
/// the link index per key. Each seek stops at its first row.
pub(crate) fn occupied_sql(key: SuffixKey) -> String {
    let link_key = resolve::link_key_column(key);
    format!(
        "SELECT {CLASS_ARM}, j.key FROM json_each(?1) AS j
         WHERE EXISTS (SELECT 1 FROM link_keys AS sc
                       WHERE sc.{link_key} >= json_extract(j.value, '$[0]')
                         AND sc.{link_key} < json_extract(j.value, '$[1]')
                         AND sc.segments IS NOT NULL)
         UNION ALL
         SELECT {FINDINGS_ARM}, j.key FROM json_each(?1) AS j
         WHERE EXISTS (SELECT 1 FROM finding_classes AS fc
                       WHERE fc.class_key >= json_extract(j.value, '$[0]')
                         AND fc.class_key < json_extract(j.value, '$[1]'))
         UNION ALL
         SELECT {PATH_ARM}, j.key FROM json_each(?2) AS j
         WHERE EXISTS (SELECT 1 FROM link_keys AS sp WHERE sp.{link_key} = j.value)"
    )
}

/// The values [`occupied_sql`] binds: each class's bounds, and the keys its
/// path arm seeks — path keys, or any key the link index holds as written.
pub(crate) fn occupied_parameters(
    classes: &[&ClassKey],
    paths: &[&str],
) -> Result<Vec<Value>, StoreError> {
    let bounds = canonical_json(&FrontmatterValue::Sequence(
        classes
            .iter()
            .map(|class| {
                let (lower, upper) = class.bounds();
                FrontmatterValue::Sequence(vec![
                    FrontmatterValue::String(lower),
                    FrontmatterValue::String(upper),
                ])
            })
            .collect(),
    ))?;
    Ok(vec![Value::Text(bounds), path_list(paths)?])
}

/// [`crate::ExplainedStatement::LinkHealthDiscard`]: discard the findings
/// whose row ids `?1` lists, each by its row id, their candidate, class and
/// path rows going with them.
pub(crate) fn discard_sql() -> String {
    "DELETE FROM findings WHERE id IN (SELECT value FROM json_each(?1))".to_string()
}

/// [`crate::ExplainedStatement::LinkHealthHeads`]: the head of what each
/// distinct key names, at most `?3` candidates per kind in the resolution ladder's
/// order, each row the arm, the key's index in its list, and the candidate's
/// id, path and rung.
///
/// Two arms, one per key space a link is addressed in. The class arm walks
/// the suffix keys `?1` lists, each a `[key, segments, files]` entry, and reads the
/// members each key's class admits through [`resolve::candidate_range_rows`],
/// its rung the candidate's suffix key. The path arm walks the path keys `?2`
/// lists and reads the candidates at each through [`resolve::link_key_path`],
/// its rung the key itself, so a path key's rows rank by the path alone. Each
/// kind's head is cut in the statement, so a key hands back at most twice `?3`
/// rows, however large its class. Consumers merge and cut to `?3` before
/// deciding whether the head fills or contains only exact members.
///
/// # A class's head is read past none of the members it keeps out
///
/// A document stays in the class of a key of `segments` segments where its
/// stored admitting count is at most `segments`
/// ([`crate::AmbiguityIgnore::admitting_segments`]). The members of one
/// count stand together in that count's run of the admitted-key index, in
/// ladder order, so the class arm enumerates each count from one to
/// `segments` through [`resolve::admitting_counts`]. It seeks each run cut at
/// `?3` rows, then cuts their union per kind at `?3` in ladder order. Each sort
/// holds at most `segments` heads, never the class. A key costs its segments'
/// worth of bounded seeks, and the members its class keeps out, however many
/// sort ahead of its head, are never read.
///
/// **The count is read as the row was derived.** A changeset computes it
/// under the declaration it is judged under, so where a pin moves the
/// ignore set, a row derived below that pin holds the count the old set gave
/// it until the heal that follows the pin derives it again, as it does
/// every row the old declaration judged. A schema reload closes the entry's
/// reader for its length, so no read answers in that window; a re-decision
/// inside the heal's own changesets may read an old count, and a class is
/// re-decided by every changeset that writes one of its members, the last of
/// them after every member is derived under the new set.
///
/// The rows come back in no stated order: the judgment merges and cuts each
/// key's head, then merges a link's heads, by [`resolve::ladder_cmp`], the order
/// the cuts here spell in SQL.
pub(crate) fn heads_sql(key: SuffixKey) -> String {
    let rung = key.column();
    let lower = "json_extract(j.value, '$[0]')";
    let upper = format!("{}({lower})", resolve::SUCCESSOR_FUNCTION);
    resolve::CANDIDATES
        .map(|candidates| {
            let class = resolve::candidate_range_rows(
                &candidates,
                key,
                (lower, &upper),
                "json_extract(j.value, '$[1]')",
                Some("?3"),
                "json_extract(j.value, '$[2]')",
            );
            let table = candidates.table;
            let dc = candidates.alias("c");
            let dp = candidates.alias("p");
            let path = resolve::link_key_path(&dp, key, "j.value");
            format!(
                "SELECT {CLASS_ARM}, j.key, {dc}.id, {dc}.path, {dc}.{rung}
         FROM json_each(?1) AS j CROSS JOIN {table} AS {dc}
         WHERE {dc}.id IN (SELECT id FROM ({class}))
         UNION ALL
         SELECT {PATH_ARM}, j.key, {dc}.id, {dc}.path, j.value
         FROM json_each(?2) AS j CROSS JOIN {table} AS {dc}
         WHERE {dc}.id IN (SELECT {dp}.id FROM {table} AS {dp} WHERE {path}
                         ORDER BY {dp}.path LIMIT ?3)"
            )
        })
        .join(" UNION ALL ")
}

/// The values [`heads_sql`] binds: the suffix keys beside their segment
/// counts, the path keys, and the head's bound.
pub(crate) fn heads_parameters(
    classes: &[(&str, u64, bool)],
    paths: &[&str],
    head: usize,
) -> Result<Vec<Value>, StoreError> {
    Ok(vec![
        class_list(classes)?,
        path_list(paths)?,
        Value::Integer(i64::try_from(head).expect("a head fits i64")),
    ])
}

/// [`crate::ExplainedStatement::LinkHealthTotals`]: how many candidates each
/// distinct key names, and how many of them stand at the key's exact path,
/// each row the arm, the key's index in its list, the count and the exact
/// count, over the two arms [`heads_sql`] reads. The class arm sums the
/// disjoint admitted-index run counts from one to the key's segment count,
/// over the same range and stored admitting counts as [`heads_sql`], so the
/// total reads every admitted member and none the class keeps out. Its exact
/// count is the admitted members whose whole suffix key is the key
/// ([`crate::resolve::exact_count`]), an equality seek of the suffix-key
/// index, which the exact-path-first step reads where a link's exact members
/// fill its head ([`crate::resolve::exact_members`]). A path key is exact by
/// its own form, so its arm counts none.
pub(crate) fn totals_sql(key: SuffixKey) -> String {
    let total = resolve::link_key_total(
        key,
        "json_extract(j.value, '$[0]')",
        "json_extract(j.value, '$[1]')",
        "json_extract(j.value, '$[2]')",
    );
    let exact = resolve::exact_count(
        key,
        "json_extract(j.value, '$[0]')",
        "json_extract(j.value, '$[1]')",
        "json_extract(j.value, '$[2]')",
    );
    let paths = resolve::CANDIDATES
        .map(|candidates| {
            let table = candidates.table;
            let dp = candidates.alias("p");
            let path = resolve::link_key_path(&dp, key, "j.value");
            format!("(SELECT COUNT(*) FROM {table} AS {dp} WHERE {path})")
        })
        .join(" + ");
    format!(
        "SELECT {CLASS_ARM}, j.key, ({total}), ({exact}) FROM json_each(?1) AS j
        UNION ALL
        SELECT {PATH_ARM}, j.key, ({paths}), 0
        FROM json_each(?2) AS j"
    )
}

/// The suffix keys beside their segment counts, and the path keys.
pub(crate) fn totals_parameters(
    classes: &[(&str, u64, bool)],
    paths: &[&str],
) -> Result<Vec<Value>, StoreError> {
    Ok(vec![class_list(classes)?, path_list(paths)?])
}

/// The suffix keys beside their segment counts, as the list of `[key,
/// segments, files]` entries the class arm of [`heads_sql`] and [`totals_sql`] walks.
fn class_list(classes: &[(&str, u64, bool)]) -> Result<Value, StoreError> {
    canonical_json(&FrontmatterValue::Sequence(
        classes
            .iter()
            .map(|(key, segments, files)| {
                FrontmatterValue::Sequence(vec![
                    FrontmatterValue::String((*key).to_string()),
                    FrontmatterValue::Int(
                        i64::try_from(*segments).expect("a segment count fits i64"),
                    ),
                    FrontmatterValue::Bool(*files),
                ])
            })
            .collect(),
    ))
    .map(Value::Text)
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

/// Exact stored spellings the overlay replaces, each reached by path equality.
pub(crate) const STANDING_TARGETS_SQL: &str = "SELECT d.path
    FROM json_each(?1) AS j CROSS JOIN documents AS d WHERE d.path = j.value";

/// Segment-aligned bounds in the comparison-key space used by path indexes.
pub(crate) fn directory_bounds(prefix: &crate::DirectoryPrefix) -> (String, String) {
    let key = crate::comparison_key(prefix.as_str());
    (format!("{key}/"), format!("{key}0"))
}

/// One equality seek per directory ancestor, stopping at the first mark.
pub(crate) fn directory_probe(path: &str) -> Result<(String, Vec<Value>), StoreError> {
    let ancestors: Vec<&str> = path.match_indices('/').map(|(at, _)| &path[..at]).collect();
    Ok((
        "SELECT EXISTS (SELECT 1 FROM json_each(?1) AS a
        WHERE EXISTS (SELECT 1 FROM directory_marks AS m WHERE m.path_key = a.value))"
            .to_string(),
        vec![path_list(&ancestors)?],
    ))
}

/// Resume a directory's path-only link range.
pub(crate) fn paged_directory_parameters(
    prefix: &crate::DirectoryPrefix,
    after: &After,
    limit: usize,
) -> Vec<Value> {
    let (lower, upper) = directory_bounds(prefix);
    let mut values = paged(Value::Text(upper), after, limit);
    values.push(Value::Text(lower));
    values
}

/// Probe the ancestors of a stored path link. Optional `.md` spellings have
/// the same directories, so one indexed key supplies the ancestor chain.
pub(crate) fn stored_directory_probe(link: i64) -> (String, Vec<Value>) {
    (
        "WITH RECURSIVE ancestors(prefix, tail) AS (
        SELECT '', key FROM (SELECT key FROM link_keys WHERE link = ?1
            AND segments IS NULL ORDER BY key LIMIT 1)
        UNION ALL
        SELECT prefix || CASE WHEN prefix = '' THEN '' ELSE '/' END ||
                   substr(tail, 1, instr(tail, '/') - 1),
               substr(tail, instr(tail, '/') + 1)
        FROM ancestors WHERE instr(tail, '/') > 0)
      SELECT EXISTS (SELECT 1 FROM ancestors AS a WHERE a.prefix <> ''
          AND EXISTS (SELECT 1 FROM directory_marks AS m WHERE m.path_key = a.prefix))"
            .to_string(),
        vec![Value::Integer(link)],
    )
}

//! The one resolver compiles written targets into indexed candidate classes.
//!
//! Every surface that asks which documents a target names asks here: a find's
//! `resolves` part, a read of a class's candidates — a get's target, a
//! links-to part's, and each candidate's minimal disambiguating suffix — and
//! every read of what a stored link names, which reads the class or the path
//! each of the link's keys opens: a row's links, and a links-to part's
//! confirmation that a link names one document alone. What a [`TargetClass`]
//! carries is what each of them needs and nothing more — the probe, over the
//! key the root selects, the target's segment count and the ignore set for
//! in-memory admission — and this module holds the one spelling of each class
//! predicate in SQL: admitted-count runs over the probed key's ranges, and a
//! link key's class or path, so no two surfaces can disagree
//! about a class.
//!
//! Every resolution surface reads the same normalized suffix or path keys.
//! [`crate::comparison_key`] applies NFC and Unicode default lowercasing on
//! every root; exact spelling takes no precedence among comparison twins.
//!
//! [`AmbiguityIgnore`] excludes a candidate unless the target's segments
//! reach the shallowest ignored place's last segment. A document at the
//! root is its whole place and is reached by its own name. Patterns and
//! candidate paths compare under the same key as resolution. Query path
//! globs retain their filesystem order independently.
//!
//! **Exact vault path first** (ADR 0040). A suffix target names its exact
//! vault path first, and every path it is a segment-aligned suffix of only
//! where no admitted document stands at an exact path. A document's whole
//! suffix key is the reversed key a target spelling its complete path opens,
//! so a document stands at a target's exact path exactly where its suffix
//! key equals one of the target's keys — the lower bounds of its ranges.
//! The globs drop a candidate before this step, so a copy they keep out
//! counts neither as an exact match nor as a deeper one; a document at an
//! exact path is one the target spells whole, so the globs never keep it
//! out. One reading of the rule serves every reader: [`exact_first`] spells
//! it over a statement's rows, [`exact_count`] counts what it keeps, and
//! [`exact_members`] applies it over a head read into memory, where a link's
//! keys are merged.
//!
//! The predicates here seek the stored key columns. Each document row stores
//! its admitting count under the pinned declaration, so every class read
//! seeks one run of the admitted-key index per count from one to the
//! target's segments, and never reads a member its class keeps out. Exact
//! members lead every run in ladder order, since a key is the least of the
//! rungs its range holds, so a head cut per run keeps every exact member it
//! can. The exact-path test is an equality seek of `documents_suffix_key`
//! per key, its stored count tested at each row it reaches, so it costs the
//! documents standing at the target's exact paths.

use std::cmp::Ordering;
use std::collections::BTreeSet;

use norn_db::rusqlite::functions::FunctionFlags;
use norn_db::rusqlite::types::Value;
use norn_db::rusqlite::{self, Connection};
use norn_wire::{CaseFold, LinkAddress, LinkFamily, Pattern};

use crate::error::{self, StoreError};
use crate::path::{
    ClassKey, SuffixKey, SuffixProbe, comparison_key, prefix_successor, suffix_probe,
};
use crate::request::range_predicate_from;

/// The name a statement calls the prefix step by: `norn_key_successor(key)`,
/// the least text after every text a separator-terminated suffix key opens,
/// which is [`crate::path::prefix_successor`] itself.
pub(crate) const SUCCESSOR_FUNCTION: &str = "norn_key_successor";

/// How many values [`TargetClass::parameters`] numbers after the ranges'
/// bounds: the target's segment count, and whether the target is rooted and
/// so names its exact paths alone.
pub(crate) const TARGET_PARAMETERS: usize = 2;

/// The disjoint stores every candidate read seeks before merging bounded heads.
pub(crate) const CANDIDATE_TABLES: [&str; 2] = ["documents", "vault_files"];

/// Each table has its own aliases even in nested union branches, so emitted
/// plans identify the index seeks of both candidate kinds without collision.
pub(crate) fn candidate_aliases(table: &str, sql: String, aliases: &[&str]) -> String {
    if table == "documents" {
        return sql;
    }
    aliases.iter().fold(sql, |sql, alias| {
        let file = alias.replacen('d', "f", 1);
        sql.replace(&format!("{alias}."), &format!("{file}."))
            .replace(&format!("AS {alias}"), &format!("AS {file}"))
    })
}

/// The separator between segments, in a target and in a path alike.
const SEPARATOR: char = '/';

/// The globs a vault's schema keeps out of ambiguity classes.
///
/// The store reads no schema: the host declares each glob on the
/// [`crate::ContentModel`] it derives from the schema it pinned, which holds
/// the set once, so the set a read applies is the set of the schema the
/// snapshot pins, and the set `describe` reports as path rules is that same
/// set.
///
/// A glob is held once, by its text, and the set is kept in the byte order of
/// that text: a glob written twice is the same text twice and ignores nothing
/// more.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AmbiguityIgnore {
    patterns: Vec<Pattern>,
    matching: Vec<Pattern>,
}

impl AmbiguityIgnore {
    /// The set that ignores nothing.
    pub fn none() -> Self {
        Self::default()
    }

    /// The set these globs name.
    pub fn new(patterns: impl IntoIterator<Item = Pattern>) -> Self {
        patterns
            .into_iter()
            .fold(Self::none(), AmbiguityIgnore::with)
    }

    /// The same set, also ignoring the places `pattern` names. A glob already
    /// held is the glob already held.
    pub fn with(mut self, pattern: Pattern) -> Self {
        if let Err(at) = self
            .patterns
            .binary_search_by(|held| held.as_str().cmp(pattern.as_str()))
        {
            let normalized = Pattern::parse(&comparison_key(pattern.as_str()))
                .expect("normalizing a valid glob preserves its grammar");
            self.patterns.insert(at, pattern);
            self.matching.insert(at, normalized);
        }
        self
    }

    /// The globs, each once, in the byte order of their text.
    pub fn patterns(&self) -> &[Pattern] {
        &self.patterns
    }

    /// The globs whose text is after `after`, or every glob where it is
    /// `None`, in the byte order of their text.
    pub(crate) fn after<'a>(&'a self, after: Option<&str>) -> impl Iterator<Item = &'a Pattern> {
        let from = after.map_or(0, |after| {
            self.patterns
                .partition_point(|pattern| pattern.as_str() <= after)
        });
        self.patterns[from..].iter()
    }

    /// Whether `path` stays in the class of a target of `target_segments`
    /// segments, under the comparison key.
    ///
    /// A path under no glob always stays. A path under one stays only where the
    /// target names its ignored place: the segments the target spells — the
    /// last `target_segments` of the path — run from the ignored place's last
    /// segment down to the leaf, so the target spells at least as many
    /// segments as there are from that place to the leaf. A one-segment target
    /// spells the stem alone, which names no place below the root; a document
    /// at the root is its whole place, so its own name names it.
    ///
    /// The globs and paths are normalized by [`crate::comparison_key`].
    pub fn admits(&self, path: &str, target_segments: usize) -> bool {
        target_segments >= self.admitting_segments(path)
    }

    /// The fewest segments a target spells that keep `path` in its class:
    /// [`AmbiguityIgnore::admits`] holds
    /// exactly for a target of at least this many segments.
    ///
    /// One for a path under no glob, and for a document at the root, whose
    /// own name names its whole place. Otherwise the segments from the ignored
    /// place down to the leaf, and never fewer than two, because a
    /// one-segment target spells a stem alone and names no place below the
    /// root.
    ///
    /// A store writes it beside each document it derives
    /// (`documents.admitting_segments`), so every class read seeks the
    /// members its class admits rather than stepping past the ones it keeps
    /// out.
    pub fn admitting_segments(&self, path: &str) -> usize {
        let Some(ignored) = self.ignored_place(path) else {
            return 1;
        };
        let depth = path.split(SEPARATOR).count();
        if depth == 1 {
            return 1;
        }
        let from_the_place_to_the_leaf = depth - ignored + 1;
        from_the_place_to_the_leaf.max(2)
    }

    /// How many segments the shallowest spelling of `path` or one of its
    /// ancestors an ignore glob matches has, or `None` where no glob matches
    /// any of them, compared under the resolution key.
    fn ignored_place(&self, path: &str) -> Option<usize> {
        if self.patterns.is_empty() {
            return None;
        }
        let path = comparison_key(path);
        let ends = path
            .match_indices(SEPARATOR)
            .map(|(at, _)| at)
            .chain(std::iter::once(path.len()));
        ends.enumerate()
            .find(|(_, end)| {
                let place = &path[..*end];
                self.matching
                    .iter()
                    .any(|pattern| pattern.matches(place, CaseFold::Exact))
            })
            .map(|(index, _)| index + 1)
    }
}

/// A target's comparison-key probe, segment count and ignored places, and
/// whether it is rooted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetClass {
    probe: SuffixProbe,
    target_segments: usize,
    rooted: bool,
    ignore: AmbiguityIgnore,
}

impl TargetClass {
    /// Compile a target no document holds, with its anchor already split
    /// off, excluding places named by `ignore`. The key is the same on every
    /// root.
    ///
    /// The target's form chooses where it is read from (ADR 0040). A target
    /// opening with `/` is a path from the vault root: its `.` and `..`
    /// steps folded, it names only the documents at the exact paths it
    /// spells, with or without `.md`, so its class is its probe's exact keys
    /// alone. A target whose first segment is `.` or `..` is a path from the
    /// folder of the document holding it, and a target given with no holder
    /// has no folder, so it names no class. Any other target is a suffix
    /// address, and a spelling [`suffix_probe`] refuses names no class.
    pub(crate) fn compile(target: &str, ignore: &AmbiguityIgnore) -> Result<Self, StoreError> {
        let refuse = |problem| StoreError::Path {
            path: target.to_string(),
            problem,
        };
        let (read, rooted) = match LinkAddress::of(LinkFamily::Wikilink, None, target) {
            LinkAddress::Suffix(read) => (read.into_owned(), false),
            LinkAddress::Rooted(path) => {
                let leaf = path.rsplit(SEPARATOR).next().unwrap_or(&path);
                let joined = crate::link::joined(Vec::new(), &path)
                    .filter(|_| !matches!(leaf, "" | "." | ".."))
                    .ok_or_else(|| refuse("it climbs above the vault root or names a folder"))?;
                (joined, true)
            }
            LinkAddress::Relative(_) => {
                return Err(refuse(
                    "it is read from the folder of a document holding it, and this target has \
                     none",
                ));
            }
            _ => return Err(refuse("it is empty")),
        };
        Ok(TargetClass {
            probe: suffix_probe(&read)?,
            target_segments: read.split(SEPARATOR).count(),
            rooted,
            ignore: ignore.clone(),
        })
    }

    /// The probe the class is read through.
    pub fn probe(&self) -> &SuffixProbe {
        &self.probe
    }

    /// The ambiguity classes a finding about this target belongs to, in the
    /// comparison key space.
    pub fn class_keys(&self) -> BTreeSet<ClassKey> {
        self.probe.class_keys()
    }

    /// Whether a document at `path` whose key this probe's ranges hold is in
    /// the class: the ignore set is the one test the ranges do not make.
    pub fn admits(&self, path: &str) -> bool {
        self.ignore.admits(path, self.target_segments)
    }

    /// Each range's bounds, then the target's segment count and whether it
    /// is rooted ([`TARGET_PARAMETERS`]).
    pub(crate) fn parameters(&self) -> Vec<Value> {
        self.probe
            .ranges()
            .flat_map(|(lower, upper)| {
                [
                    Value::Text(lower.to_string()),
                    Value::Text(upper.to_string()),
                ]
            })
            .chain([
                Value::Integer(self.target_segments as i64),
                Value::Integer(i64::from(self.rooted)),
            ])
            .collect()
    }
}

/// Splitting a separator-terminated suffix key yields one row per segment
/// plus one trailing empty element. Every probe lower bound and stored link
/// key spells exactly its target's segments, with `/` after each segment.
/// The driver's `n.key` is zero-based, so the admitting count is `n.key + 1`;
/// callers keep `n.key < segments`. JSON quoting keeps arbitrary key
/// characters intact.
pub(crate) fn admitting_counts(key: &str) -> String {
    format!(r#"json_each('[' || replace(json_quote({key}), '/', '","') || ']')"#)
}

/// A row from [`admitting_counts`] supplies its one-based admitting count.
pub(crate) fn admitting_count(alias: &str) -> String {
    format!("(CAST({alias}.key AS INTEGER) + 1)")
}

/// A driver's row is in range when its count is at most the target's segments.
pub(crate) fn admitting_count_within(alias: &str, segments: &str) -> String {
    format!("CAST({alias}.key AS INTEGER) < {segments}")
}

/// Each count's rows stand together in one run of the admitted index. The
/// count equality and key bounds seek that run in ladder order.
pub(crate) fn range_admitted(
    alias: &str,
    key: SuffixKey,
    (lower, upper): (&str, &str),
    admitting: &str,
) -> String {
    let column = format!("{alias}.{}", key.column());
    format!(
        "{alias}.admitting_segments = {admitting} AND {column} >= {lower} AND {column} < {upper}"
    )
}

/// The admitted rows of one range. A head cuts each run in ladder order before
/// merging, so neither the merge nor its sort ever reads a whole class.
/// `files` admits name-only candidates only where the address did not spell
/// optional `.md`, which belongs to documents alone.
pub(crate) fn range_rows(
    key: SuffixKey,
    bounds: (&str, &str),
    segments: &str,
    head: Option<&str>,
    files: &str,
) -> String {
    let rung = key.column();
    let rows = CANDIDATE_TABLES
        .map(|table| {
            let counts = admitting_counts(bounds.0);
            let admitted = range_admitted("dl", key, bounds, &admitting_count("n"));
            let within = admitting_count_within("n", segments);
            let eligible = if table == "vault_files" { files } else { "1" };
            let rows = if let Some(head) = head {
                let order = ladder(&format!("dl.{rung}"), "dl.path");
                format!(
                    "da.id IN (SELECT dl.id FROM {table} AS dl WHERE {admitted}
                             ORDER BY {order} LIMIT {head})"
                )
            } else {
                range_admitted("da", key, bounds, &admitting_count("n"))
            };
            let cut = head
                .map(|head| {
                    format!(
                        " ORDER BY {} LIMIT {head}",
                        ladder(&format!("da.{rung}"), "da.path")
                    )
                })
                .unwrap_or_default();
            candidate_aliases(
                table,
                format!(
                    "SELECT * FROM (SELECT da.id, da.path, da.{rung}
            FROM {counts} AS n CROSS JOIN {table} AS da WHERE {eligible} AND {within} AND {rows}{cut})"
                ),
                &["da", "dl"],
            )
        })
        .join(" UNION ALL ");
    let cut = head
        .map(|head| format!(" ORDER BY {rung}, path LIMIT {head}"))
        .unwrap_or_default();
    format!("SELECT * FROM ({rows}){cut}")
}

/// A class range counts each table's disjoint admitted runs by index seek.
pub(crate) fn range_total(
    key: SuffixKey,
    bounds: (&str, &str),
    segments: &str,
    files: &str,
) -> String {
    let counts = admitting_counts(bounds.0);
    let admitted = range_admitted("dl", key, bounds, &admitting_count("n"));
    let within = admitting_count_within("n", segments);
    let total = CANDIDATE_TABLES
        .map(|table| {
            let eligible = if table == "vault_files" { files } else { "1" };
            candidate_aliases(
                table,
                format!("(SELECT COUNT(*) FROM {table} AS dl WHERE {eligible} AND {admitted})"),
                &["dl"],
            )
        })
        .join(" + ");
    format!("SELECT COALESCE(SUM({total}), 0) FROM {counts} AS n WHERE {within}")
}

/// Membership tested at one document's row key: its stored count and the
/// target's ranges, numbered in [`TargetClass::parameters`]'s order, then the
/// exact-path step ([`target_exact_first`]).
pub(crate) fn predicate(alias: &str, key: SuffixKey, ranges: usize, first: usize) -> String {
    let column = format!("{alias}.{}", key.column());
    let ranges_sql = range_predicate_from(&column, ranges, first);
    let segments = first + ranges * 2;
    format!(
        "({ranges_sql}) AND {alias}.admitting_segments <= ?{segments} AND {}",
        target_exact_first(alias, key, ranges, first)
    )
}

/// The target's exact keys, the lower bounds of its ranges, as the SQL list
/// [`exact_first`] and [`exact_count`] read, numbered from `first` in
/// [`TargetClass::parameters`]'s order.
fn exact_keys(ranges: usize, first: usize) -> String {
    (0..ranges)
        .map(|range| format!("?{}", first + range * 2))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The exact-path step over a compiled target's class rows a statement calls
/// `alias`: [`exact_first`] over its exact keys, and, where the target is
/// rooted, its exact keys alone.
pub(crate) fn target_exact_first(
    alias: &str,
    key: SuffixKey,
    ranges: usize,
    first: usize,
) -> String {
    let keys = exact_keys(ranges, first);
    let segments = format!("?{}", first + ranges * 2);
    let rooted = format!("?{}", first + ranges * 2 + 1);
    format!(
        "({rooted} = 0 OR {alias}.{column} IN ({keys})) AND {exact}",
        column = key.column(),
        exact = exact_first(
            alias,
            key,
            &keys,
            &segments,
            if ranges == 1 { "1" } else { "0" }
        ),
    )
}

/// The exact-path-first step over the `documents` rows a statement calls
/// `alias`, all in one target's class: a row stays where its suffix key is
/// one of `exact_keys` — the target's own keys, an SQL list or subquery —
/// or where no document admitted to a target of `segments` segments stands
/// at one of them.
///
/// The test is an equality seek of `documents_suffix_key` per exact key, its
/// stored admitting count tested on each row it reaches, so it reads the
/// documents standing at the target's exact paths and no others.
/// [`exact_members`] is the same step over rows read into memory, and the two
/// must agree.
pub(crate) fn exact_first(
    alias: &str,
    key: SuffixKey,
    exact_keys: &str,
    segments: &str,
    files: &str,
) -> String {
    format!(
        "({alias}.{column} IN ({exact_keys}) OR NOT EXISTS ({exact}))",
        column = key.column(),
        exact = exact_rows(key, exact_keys, segments, files),
    )
}

/// How many documents admitted to a target of `segments` segments stand at
/// one of its exact keys, `exact_keys`: what [`exact_first`] keeps where any
/// stands there.
pub(crate) fn exact_count(key: SuffixKey, exact_keys: &str, segments: &str, files: &str) -> String {
    format!(
        "SELECT COUNT(*) FROM ({})",
        exact_rows(key, exact_keys, segments, files)
    )
}

/// The admitted candidates at one of `exact_keys`.
fn exact_rows(key: SuffixKey, exact_keys: &str, segments: &str, files: &str) -> String {
    let column = key.column();
    CANDIDATE_TABLES
        .map(|table| {
            let eligible = if table == "vault_files" { files } else { "1" };
            candidate_aliases(
                table,
                format!(
                    "SELECT 1 FROM {table} AS de WHERE {eligible} AND de.{column} IN ({exact_keys})
         AND de.admitting_segments <= {segments}"
                ),
                &["de"],
            )
        })
        .join(" UNION ALL ")
}

/// The exact-path-first step over the members of one link's class read into
/// memory, each its rung — its suffix key — and whatever the reader carries:
/// where some member's rung is one of `exact_keys`, the link's own suffix
/// keys, those members alone, and otherwise every member. It is
/// [`exact_first`] over rows a reader merged, and the members it is given
/// must already be the ones their class admits.
pub(crate) fn exact_members<'m, T>(
    exact_keys: &[&str],
    members: impl IntoIterator<Item = (&'m str, T)>,
) -> Vec<T> {
    let (exact, deeper): (Vec<_>, Vec<_>) = members
        .into_iter()
        .partition(|(rung, _)| exact_keys.contains(rung));
    if exact.is_empty() { deeper } else { exact }
        .into_iter()
        .map(|(_, member)| member)
        .collect()
}

/// Each range's admitted runs, streamed so a caller's ceiling can stop them.
pub(crate) fn target_rows(
    key: SuffixKey,
    ranges: usize,
    first: usize,
    head: Option<&str>,
) -> String {
    let segments = format!("?{}", first + ranges * 2);
    (0..ranges)
        .map(|range| {
            let lower = format!("?{}", first + range * 2);
            let upper = format!("?{}", first + range * 2 + 1);
            format!(
                "SELECT * FROM ({})",
                range_rows(
                    key,
                    (&lower, &upper),
                    &segments,
                    head,
                    if ranges == 1 { "1" } else { "0" }
                )
            )
        })
        .collect::<Vec<_>>()
        .join(" UNION ALL ")
}

/// The admitted rows of a link key, with the same per-run head bound as a
/// compiled target's ranges where `head` is given.
pub(crate) fn link_key_rows(
    key: SuffixKey,
    link_key: &str,
    segments: &str,
    head: Option<&str>,
    files: &str,
) -> String {
    range_rows(
        key,
        (link_key, &format!("{SUCCESSOR_FUNCTION}({link_key})")),
        segments,
        head,
        files,
    )
}

/// The sum of the disjoint admitted-index run counts of a link key.
pub(crate) fn link_key_total(
    key: SuffixKey,
    link_key: &str,
    segments: &str,
    files: &str,
) -> String {
    range_total(
        key,
        (link_key, &format!("{SUCCESSOR_FUNCTION}({link_key})")),
        segments,
        files,
    )
}

/// The indexed link column holding its normalized suffix or path key.
pub(crate) fn link_key_column(_key: SuffixKey) -> &'static str {
    "key"
}

/// The documents standing at a link's normalized complete path key.
/// `documents_path_key` seeks every comparison twin without a collation.
pub(crate) fn link_key_path(alias: &str, _key: SuffixKey, link_key: &str) -> String {
    format!("{alias}.path_key = {link_key}")
}

/// The rows of `documents` a statement calls `dr` that are in `class`: the
/// `FROM` and `WHERE` clauses every read of a class spells, its values numbered
/// from `?1` in [`TargetClass::parameters`]'s order.
pub(crate) fn class_rows(class: &TargetClass) -> String {
    let (key, ranges) = (class.probe().key(), class.probe().range_count());
    format!(
        "FROM ({}) AS dr WHERE {}",
        target_rows(key, ranges, 1, None),
        target_exact_first("dr", key, ranges, 1)
    )
}

/// A head bounds each range's count runs before the caller merges and cuts
/// them. Exact members lead each run, so the exact-path step after the cut
/// keeps every one a head can hold.
pub(crate) fn class_head_rows(class: &TargetClass, head: &str) -> String {
    let (key, ranges) = (class.probe().key(), class.probe().range_count());
    format!(
        "FROM ({}) AS dr WHERE {}",
        target_rows(key, ranges, 1, Some(head)),
        target_exact_first("dr", key, ranges, 1)
    )
}

/// How many documents a target names: where a rooted target, or any
/// admitted document at an exact path, keeps the exact paths alone, those
/// documents; otherwise the sum of the counts of the target's disjoint
/// ranges and admitting runs.
pub(crate) fn class_total(class: &TargetClass) -> String {
    let (key, ranges) = (class.probe().key(), class.probe().range_count());
    let segments = format!("?{}", ranges * 2 + 1);
    let rooted = format!("?{}", ranges * 2 + 2);
    let exact = exact_count(
        key,
        &exact_keys(ranges, 1),
        &segments,
        if ranges == 1 { "1" } else { "0" },
    );
    let totals = (0..class.probe().range_count())
        .map(|range| {
            let lower = format!("?{}", range * 2 + 1);
            let upper = format!("?{}", range * 2 + 2);
            format!(
                "({})",
                range_total(
                    class.probe().key(),
                    (&lower, &upper),
                    &segments,
                    if ranges == 1 { "1" } else { "0" }
                )
            )
        })
        .collect::<Vec<_>>()
        .join(" + ");
    format!("SELECT CASE WHEN {rooted} = 1 OR ({exact}) > 0 THEN ({exact}) ELSE {totals} END")
}

/// The resolution ladder's order over the rows [`class_rows`] reads: the
/// probed key, then the path. Total, because equal suffix keys are exactly
/// what an ambiguity class is made of, and a ladder whose ties fell out in
/// row-insertion order would reorder itself when a document is re-derived.
pub(crate) fn ladder_order(class: &TargetClass) -> String {
    format!(
        "ORDER BY {}",
        ladder(&format!("dr.{}", class.probe().key().column()), "dr.path")
    )
}

/// The resolution ladder's order over rows whose probed key is `key` and whose
/// path is `path`, each an expression: the key, then the path.
///
/// [`ladder_cmp`] is the same order over rows read into memory, and the two
/// must agree: a head a statement cuts in this order is merged and re-cut in
/// that one.
pub(crate) fn ladder(key: &str, path: &str) -> String {
    format!("{key}, {path}")
}

/// The resolution ladder's order over two rows read into memory, each its
/// probed key — its rung — and its path: the key, then the path, each
/// compared byte by byte.
///
/// It is the order [`ladder`] spells in SQL. The two agree because every
/// column a rung or a path is read from — `documents.suffix_key`,
/// `documents.path_key`, `documents.path` and `link_keys.key` — takes the `BINARY` collation, which compares
/// bytes as Rust compares `str`. A column given another collation, or a
/// ladder over another expression, changes both or neither.
pub(crate) fn ladder_cmp(left: (&str, &str), right: (&str, &str)) -> Ordering {
    left.cmp(&right)
}

/// Register the deterministic prefix step on the writer and every reader.
pub(crate) fn register_functions(connection: &Connection) -> Result<(), StoreError> {
    connection
        .create_scalar_function(
            SUCCESSOR_FUNCTION,
            1,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
            |context| {
                let key = context
                    .get_raw(0)
                    .as_str()
                    .map_err(|problem| rusqlite::Error::UserFunctionError(Box::new(problem)))?;
                Ok(prefix_successor(key))
            },
        )
        .map_err(|problem| error::sql("registering the key successor function", problem))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ignoring(globs: &[&str]) -> AmbiguityIgnore {
        AmbiguityIgnore::new(
            globs
                .iter()
                .map(|glob| Pattern::parse(glob).expect("a glob")),
        )
    }

    /// A target reaches an ignored place when the segments it spells run up to
    /// the place's last segment, and not one segment short of it; a
    /// one-segment target spells the stem alone and reaches no ignored place
    /// below the root.
    #[test]
    fn a_target_reaches_an_ignored_place_from_its_last_segment_down() {
        let nested = ignoring(&["**/drafts/**"]);
        assert!(nested.admits("notes/drafts/x.md", 2));
        assert!(!nested.admits("notes/drafts/x.md", 1));
        let deeper = ignoring(&["archive/**"]);
        assert!(deeper.admits("archive/norn/glossary.md", 3));
        assert!(!deeper.admits("archive/norn/glossary.md", 2));
        let leaf = ignoring(&["attachments/*"]);
        assert!(leaf.admits("attachments/image.md", 2));
        assert!(!leaf.admits("attachments/image.md", 1));
    }

    /// **A document at the root that a glob ignores is reached by its own
    /// name.** The place is the document, and a one-segment target naming it
    /// names the whole place, so it resolves.
    #[test]
    fn a_root_level_ignored_document_is_reached_by_its_own_name() {
        let root = ignoring(&["glossary.md"]);
        assert!(root.admits("glossary.md", 1));
        assert!(root.admits("docs/glossary.md", 1), "a path no glob matches");
    }

    /// **The admitting count is the shortest target that keeps a place in
    /// its class**: one for a place under no glob and for an ignored document
    /// at the root, the segments from the ignored place to the leaf below it,
    /// and two at least for an ignored place below the root.
    #[test]
    fn a_place_is_admitted_from_the_fewest_segments_that_name_it() {
        let set = ignoring(&["archive/**", "attachments/*", "glossary.md"]);
        for (path, segments) in [
            ("notes/glossary.md", 1),
            ("glossary.md", 1),
            ("archive/norn/glossary.md", 3),
            ("archive/glossary.md", 2),
            ("attachments/image.md", 2),
        ] {
            assert_eq!(set.admitting_segments(path), segments, "{path}");
            assert!(set.admits(path, segments), "{path}");
            assert!(!set.admits(path, segments - 1), "{path}");
        }
    }

    #[test]
    fn a_place_is_under_the_shallowest_spelling_a_glob_matches() {
        let set = ignoring(&["archive/**", "attachments/*", "**/drafts/**"]);
        assert_eq!(set.ignored_place("archive/deep/glossary.md"), Some(1));
        assert_eq!(set.ignored_place("attachments/image.md"), Some(2));
        assert_eq!(set.ignored_place("notes/drafts/x.md"), Some(2));
        assert_eq!(set.ignored_place("notes/glossary.md"), None);
        assert_eq!(set.ignored_place("Archive/glossary.md"), Some(1));
        assert_eq!(set.ignored_place("notes/Drafts/x.md"), Some(2));
    }
}

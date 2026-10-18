//! The statements the find builder emits, named, and the one composer that
//! spells each of them with its parameters.
//!
//! **One function writes a statement's text and binds its parameters.**
//! [`compose_page`] numbers each parameter as it writes the placeholder for
//! it, so the text and the values cannot drift apart, and the plan the
//! instrument takes and the page the builder reads are the same call over the
//! same inputs.

use norn_db::rusqlite::types::Value;

use crate::error::StoreError;
use crate::facts::StoredPathOrder;
use crate::json::{FrontmatterValue, canonical_json};
use crate::path::SuffixKey;
use crate::read::{
    AnswerSeek, Binder, Driver, FINDING_ROW_COLUMNS, FieldOrder, Filter, Reach, Term,
    answer_ordering, answer_place, answer_range, key_walk,
};
use crate::resolve::{self, TargetClass};

/// Every statement shape the find builder runs, named.
///
/// The same discipline as [`crate::ExplainedStatement`]: [`FindStatement::all`]
/// holds each shape once, [`FindStatement::slot`] is exhaustive over the enum,
/// and [`FIND_STATEMENTS`] is the count a census is checked against. A page's
/// filters are spelled inside the page statement they narrow, and are named by
/// [`crate::ReadFilter`]. Each page statement below is named by the index it seeks
/// with no filter; a filter that keeps what it seeks may drive the statement
/// instead — a path page from the filter's source where the filter keeps its
/// documents in the answer's path order, and otherwise from the filter's seek
/// where it keeps few enough documents ([`FindStatement::PartCeiling`]), the
/// statement sorting what that seek handed it. A page that walks its order
/// first reads where a window of its walk ends, one document past an offset —
/// a valued section's own statement narrowed by no filter, and otherwise a
/// [`FindStatement::PathPage`] narrowed by none — and then walks no further
/// than that document.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FindStatement {
    /// The pinned vault-schema fingerprint, read on the snapshot: what a
    /// finding filter is judged under.
    ActiveFingerprint,
    /// A page in the answer's path order — ASCII case folded, with the
    /// bytewise path as the tie-break — on `documents_path_nocase`. Read one
    /// document past an offset, narrowed by no filter, it is also where a
    /// find's walk in that order ends a window: a path page's and a missing
    /// section's.
    PathPage(PageDirection),
    /// The documents a field sort orders by value: one marker row per document,
    /// its least value under the order, in `(value, path COLLATE NOCASE, path)`
    /// order on that order's marker index, so documents tied at one value
    /// stand in the answer's path order.
    FieldValuePage(FieldOrder, PageDirection),
    /// The documents a field sort holds no value for under the order, in the
    /// answer's path order on `documents_path_nocase`: a walk of it that
    /// probes each document's marker row, passing every document that carries
    /// the key. They stand before every valued document ascending and after
    /// every one descending.
    FieldMissingPage(FieldOrder, PageDirection),
    /// Whether any document carries a key the declaration does not name: one
    /// existence seek of `document_fields_presence`.
    KnownKey,
    /// Every key a document carries, each once, in key order: the key walk
    /// every enumeration of the keys shares, a walk of
    /// `document_fields_presence` that seeks past each key to the next, so it
    /// costs the distinct keys rather than the rows that carry them. Run only
    /// where some key a request named is unknown.
    FieldUniverse,
    /// Whether a path part with no wildcard names a directory documents stand
    /// under and no document, under the root's path order: one seek of
    /// `documents_path` at the path, and one of the range beneath it, where
    /// the root tells spellings apart, and the same two of
    /// `documents_path_nocase` under `NOCASE` where it folds ASCII case.
    BareDirectory,
    /// Whether a date key holds a typed date stating no offset, and whether it
    /// holds one stating an offset: one seek of `document_fields_offset` at
    /// each spelling. Run once for each key with a dated order that a request
    /// sorts or compares by.
    OffsetSpellings,
    /// Whether the full-text engine parses a match part's query: one
    /// selection of `documents_fts` through its `MATCH` constraint, stepped
    /// until it answers whether any document matches, which is where the
    /// engine parses the query and reports a query it cannot parse.
    MatchProbe,
    /// The document rows a page found, by row id, carrying only the columns
    /// the projection names: the frontmatter projection, the head of the body
    /// and the body's length.
    HydrateDocuments,
    /// The head of one nested collection for each document a page found: the
    /// rows below the per-row ceiling, one range seek of the collection's
    /// `(document, ordinal)` index per document.
    NestedHead(Nested),
    /// How many rows one nested collection holds for each document whose head
    /// the ceiling cut: an index-only count over the same index.
    NestedTotal(Nested),
    /// The head of the findings standing at each path a page found, under the
    /// active fingerprint: the findings below the per-row ceiling, in
    /// `(position, kind, id)` order, one seek of `findings_path` per path, each
    /// finding then reached by its row id.
    FindingHead,
    /// How many findings stand at each path whose head the ceiling cut: an
    /// index-only count over `findings_path`.
    FindingTotal,
    /// The candidate heads of a set of findings: one seek of
    /// `finding_candidates`' primary key per finding. Run by every read that
    /// answers finding rows.
    FindingCandidates,
    /// The ambiguity classes of a set of findings: one seek of
    /// `finding_classes`' primary key per finding. Run by every read that
    /// answers finding rows.
    FindingClasses,
    /// The head of the class a target opens: each admitting-count run of
    /// each suffix range on `documents_admitted_suffix_key` where the root
    /// tells spellings apart and on `documents_admitted_folded_suffix_key`
    /// where it folds ASCII case, cut at [`crate::CANDIDATE_HEAD`] in ladder
    /// order, then merged and cut again. Run by every read that resolves one
    /// target: a get's and a links-to part's.
    ClassHead,
    /// How many documents the class holds, run only where the head filled
    /// [`crate::CANDIDATE_HEAD`]: a sum of the disjoint admitted-run counts.
    ClassTotal,
    /// Whether each suffix spelling of each candidate a read names names that
    /// candidate alone, for every candidate at once: for each range of each
    /// spelling's class, at most two of the documents in it — a range of the
    /// suffix key the root probes, less the places the schema ignores — each
    /// then reached by its row id. Run once by every read that names
    /// candidates, however many there are.
    CandidateSuffixes,
    /// What each of a page's links names, for every link at once: the keys of
    /// the links on `link_keys_link`, each distinct key read once — a suffix
    /// key's class a range of the suffix key the root probes, less the places
    /// the schema ignores, and a path key the documents at the path on
    /// `documents_path` or `documents_path_nocase` — and for each link, at
    /// most [`crate::CANDIDATE_HEAD`] of the documents its keys reached, in
    /// the resolution ladder's order, beside how many there were. Run once by
    /// every read that carries links, however many it carries.
    LinkTargets,
    /// Whether a part that keeps what it seeks keeps few enough documents to
    /// drive a page from what it matched: the distinct documents among the
    /// rows of its index seek alone, read until one past the cap it is asked
    /// at and counted, so it costs the rows of at most that many documents
    /// whatever the part keeps. Every condition the index does not answer is
    /// dropped, so a document the seek reaches and the part rejects counts
    /// too, and a part is never taken to keep fewer documents than it does.
    /// Run once per part per ask, in the request's order until one keeps few
    /// enough, by a find, validate or repair page no part drives from its
    /// documents in the page's order ([`crate::ReadFilter::is_path_ordered`]):
    /// first at the rows the page reads, then each time a window of its walk
    /// leaves it short, at the walk's budget, which doubles to a ceiling, as
    /// the read module's drive sets it.
    PartCeiling,
    /// The greatest document row id: one seek of the row ids' last entry,
    /// which bounds how many documents the vault holds — a deleted document's
    /// id is never reused below it — and so scales the ceiling a part is
    /// asked against. Read once by a page, the first time a round of its walk
    /// leaves it short.
    DocumentSpan,
}

/// How many statement shapes [`FindStatement::all`] holds.
pub const FIND_STATEMENTS: usize = 22;

impl FindStatement {
    /// Every statement shape, in slot order.
    ///
    /// The order and the direction a page carries, and the collection a nested
    /// read reads, are not part of its slot: a bar that cares about those axes
    /// ranges over them itself.
    pub fn all() -> [Self; FIND_STATEMENTS] {
        [
            Self::ActiveFingerprint,
            Self::PathPage(PageDirection::Ascending),
            Self::FieldValuePage(FieldOrder::Raw, PageDirection::Ascending),
            Self::FieldMissingPage(FieldOrder::Raw, PageDirection::Ascending),
            Self::KnownKey,
            Self::FieldUniverse,
            Self::BareDirectory,
            Self::MatchProbe,
            Self::HydrateDocuments,
            Self::NestedHead(Nested::Tags),
            Self::NestedTotal(Nested::Tags),
            Self::FindingHead,
            Self::FindingTotal,
            Self::FindingCandidates,
            Self::FindingClasses,
            Self::ClassHead,
            Self::ClassTotal,
            Self::CandidateSuffixes,
            Self::LinkTargets,
            Self::OffsetSpellings,
            Self::PartCeiling,
            Self::DocumentSpan,
        ]
    }

    /// Where this statement stands in [`Self::all`]. Exhaustive, so a shape
    /// added to the enum has to take a slot.
    pub fn slot(self) -> usize {
        let slot = match self {
            Self::ActiveFingerprint => 0,
            Self::PathPage(_) => 1,
            Self::FieldValuePage(..) => 2,
            Self::FieldMissingPage(..) => 3,
            Self::KnownKey => 4,
            Self::FieldUniverse => 5,
            Self::BareDirectory => 6,
            Self::MatchProbe => 7,
            Self::HydrateDocuments => 8,
            Self::NestedHead(_) => 9,
            Self::NestedTotal(_) => 10,
            Self::FindingHead => 11,
            Self::FindingTotal => 12,
            Self::FindingCandidates => 13,
            Self::FindingClasses => 14,
            Self::ClassHead => 15,
            Self::ClassTotal => 16,
            Self::CandidateSuffixes => 17,
            Self::LinkTargets => 18,
            Self::OffsetSpellings => 19,
            Self::PartCeiling => 20,
            Self::DocumentSpan => 21,
        };
        assert!(
            slot < FIND_STATEMENTS,
            "slot {slot} is outside the enumeration: grow `all` and `FIND_STATEMENTS` with the \
             statement that took it"
        );
        slot
    }
}

/// A nested collection a row can project.
///
/// Each is a table whose rows a document owns, keyed by the document and the
/// row's position in it, so the head of one is a range of that key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Nested {
    /// The tags the document carries, in `document_tags`.
    Tags,
    /// The headings the document carries, in `headings`.
    Headings,
    /// The block identifiers the document defines, in `blocks`.
    Blocks,
    /// The links the document carries, in `links`, each resolved at the read.
    Links,
}

impl Nested {
    /// Every collection, in the order a row's columns are hydrated in.
    pub const ALL: [Nested; 4] = [
        Nested::Tags,
        Nested::Headings,
        Nested::Blocks,
        Nested::Links,
    ];

    /// The table the collection's rows live in.
    pub const fn table(self) -> &'static str {
        match self {
            Nested::Tags => "document_tags",
            Nested::Headings => "headings",
            Nested::Blocks => "blocks",
            Nested::Links => "links",
        }
    }

    /// The columns an item is read from, in the order the stored-fact reader
    /// of the same table reads them, and a link's row id after its own, at
    /// [`LINK_ID_COLUMN`], which a read of what the link names seeks its keys
    /// by.
    pub(crate) const fn columns(self) -> &'static str {
        match self {
            Nested::Tags => "n.name, n.source, n.span_line, n.span_column, n.span_offset",
            Nested::Headings => {
                "n.text, n.slug, n.level, n.span_line, n.span_column, n.span_offset, \
                 n.body_offset, n.inside_container, n.reading"
            }
            Nested::Blocks => "n.block_id, n.span_line, n.span_column, n.span_offset",
            Nested::Links => {
                "n.family, n.embed, n.protocol, n.target, n.title, n.anchor, n.block_ref, \
                 n.span_line, n.span_column, n.span_offset, n.anchor_text, n.anchor_marked, n.field, \
                 n.id"
            }
        }
    }

    /// How many columns [`Nested::columns`] names, which is where the document
    /// id and the ordinal stand after them.
    pub(crate) const fn width(self) -> usize {
        match self {
            Nested::Tags => 5,
            Nested::Headings => 9,
            Nested::Blocks => 4,
            Nested::Links => LINK_ID_COLUMN + 1,
        }
    }
}

/// Where a link's row id stands among [`Nested::columns`] of the links.
pub(crate) const LINK_ID_COLUMN: usize = 13;

/// Which way a page runs.
///
/// The store's own reading of the wire's direction, so the statements it
/// spells are named by a closed set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PageDirection {
    /// Smallest first.
    Ascending,
    /// Largest first.
    Descending,
}

/// Where a page section resumes: the sort value and the path of the row it
/// stopped after. `None` in both starts the section at its first row.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SectionStart<'a> {
    pub(crate) sort: Option<&'a str>,
    pub(crate) path: Option<&'a str>,
}

/// What one page section reads: the statement, the key a field section is
/// ordered by, where it resumes, the filters it narrows by, and how many rows.
pub(crate) struct Section<'a> {
    pub(crate) statement: FindStatement,
    pub(crate) key: Option<&'a str>,
    pub(crate) start: SectionStart<'a>,
    pub(crate) filters: &'a [Filter],
    /// How the section reaches the documents its filters keep, as the page
    /// settles it ([`crate::store::Snapshot::drive_page`]).
    pub(crate) driver: Driver,
    pub(crate) rows: usize,
    /// The last place a walked section may read, inclusive: the end of the
    /// walk's window ([`crate::read::Paged::window`]), or `None` where the
    /// section reads to its end.
    pub(crate) until: Option<SectionStart<'a>>,
    /// The rows the section passes before the first it hands back: zero for
    /// a page, and the window's length less one, in documents, where the
    /// section names where a walk's window ends.
    pub(crate) offset: usize,
}

/// One page section's statement and its parameters, in the numbering the text
/// states.
///
/// Every section selects the document id, its path, its sort value — null
/// outside a field sort's valued section — and the content hash the document
/// was indexed under, in the order the page runs. A valued section reads the
/// hash off the document's row by its key, for the rows the page hands back.
///
/// **Where a section resumes is its lower bound**, not a test applied to the
/// rows it read: one row-value seek past its place in the answer order
/// ([`AnswerSeek`]), so a page among paths that fold together starts exactly
/// past its position. An unset position coalesces to a bound below every row
/// ascending — the empty text — and above every row descending — an empty
/// blob, which SQLite orders after every text. So the text does not branch on
/// whether the section resumes, and the plan is the same either way.
///
/// **How a section reaches its documents is the page's driver**, settled as
/// the page reads ([`Driver`]). A path page a filter keeping its documents in
/// the answer's path order narrows is driven from that filter's source
/// ([`compose_ordered_path_page`]). A section a filter keeping few enough
/// documents drives is driven from that filter's seek: the section's own order
/// index is spelled out of reach — the term that would seek it stands behind a
/// unary `+`, which SQLite reads as the same value and never as an index
/// constraint — so the section reaches each document the filter's seek handed
/// it by that document's key and sorts them: its cost is the filter's matches,
/// at most the walk's budget, not the order index's rows. Every other section
/// seeks its order index and tests each row it reads at that document's key
/// against every filter, the driving one's aside; where a window of the walk
/// bounds it, the window's last row is a second bound on the same row value
/// the section seeks past its place, so the seek stops there.
///
/// **A walk's window is a section narrowed by no filter**, reading one row
/// past an offset: the document a walk of that many documents past the place
/// stops at, as the order's index hands it back — a valued section's own, and
/// otherwise the path page's.
pub(crate) fn compose_page(section: &Section<'_>) -> (String, Vec<Value>) {
    let mut binder = Binder::default();
    let text = |value: Option<&str>| value.map_or(Value::Null, |value| Value::Text(value.into()));
    if let (FindStatement::PathPage(direction), Driver::Ordered(at)) =
        (section.statement, section.driver)
    {
        return compose_ordered_path_page(section, direction, at, binder);
    }
    let driving = match section.driver {
        Driver::Matched(at) => Some(at),
        Driver::Walked => None,
        Driver::Ordered(_) => unreachable!("only a path page is driven from a source"),
    };
    let order_seek = if driving.is_some() { "+" } else { "" };
    let (head, ordering, id) = match section.statement {
        FindStatement::ActiveFingerprint
        | FindStatement::KnownKey
        | FindStatement::FieldUniverse
        | FindStatement::BareDirectory
        | FindStatement::OffsetSpellings
        | FindStatement::MatchProbe
        | FindStatement::HydrateDocuments
        | FindStatement::NestedHead(_)
        | FindStatement::NestedTotal(_)
        | FindStatement::FindingHead
        | FindStatement::FindingTotal
        | FindStatement::FindingCandidates
        | FindStatement::FindingClasses
        | FindStatement::ClassHead
        | FindStatement::ClassTotal
        | FindStatement::CandidateSuffixes
        | FindStatement::LinkTargets
        | FindStatement::PartCeiling
        | FindStatement::DocumentSpan => {
            unreachable!("{:?} is not a page section", section.statement)
        }
        FindStatement::PathPage(direction) => {
            let after = binder.bind(text(section.start.path));
            let (comparison, beyond, descending) = spelled(direction);
            let place = format!("COALESCE({after}, {beyond})");
            let until = path_until(section, direction, &mut binder);
            (
                format!(
                    "SELECT d.id, d.path, NULL, d.content_hash FROM documents AS d
                     WHERE {}{until}",
                    path_seek(order_seek, comparison, &place)
                ),
                answer_ordering("d.path", descending),
                "d.id",
            )
        }
        FindStatement::FieldValuePage(order, direction) => {
            let key = binder.bind(text(section.key));
            let sort = binder.bind(text(section.start.sort));
            let after = binder.bind(text(section.start.path));
            let (comparison, beyond, descending) = spelled(direction);
            let (column, marker) = (order.column(), order.marker());
            let (valued, place, lead) = (
                format!("COALESCE({sort}, {beyond})"),
                format!("COALESCE({after}, {beyond})"),
                format!("f.{column}"),
            );
            let seek = AnswerSeek {
                reach: "",
                comparison,
                lead: Some(Term {
                    column: &lead,
                    bound: &valued,
                }),
                path: "f.path",
                folded: &place,
                bytewise: &place,
                trail: None,
                id: None,
            };
            let until = match section.until {
                Some(until) => {
                    let (sort, path) =
                        (binder.bind(text(until.sort)), binder.bind(text(until.path)));
                    let bound = AnswerSeek {
                        reach: "",
                        comparison: through(direction),
                        lead: Some(Term {
                            column: &lead,
                            bound: &sort,
                        }),
                        path: "f.path",
                        folded: &path,
                        bytewise: &path,
                        trail: None,
                        id: None,
                    };
                    format!("\n                       AND {}", bound.spelled())
                }
                None => String::new(),
            };
            (
                format!(
                    "SELECT f.document, f.path, f.{column},
                         (SELECT h.content_hash FROM documents AS h WHERE h.id = f.document)
                     FROM document_fields AS f
                     WHERE f.key = {key} AND {order_seek}f.{marker} = 1
                       AND {}{until}",
                    seek.spelled()
                ),
                format!(
                    "f.{column}{descending}, {}",
                    answer_ordering("f.path", descending)
                ),
                "f.document",
            )
        }
        FindStatement::FieldMissingPage(order, direction) => {
            let after = binder.bind(text(section.start.path));
            let key = binder.bind(text(section.key));
            let (comparison, beyond, descending) = spelled(direction);
            let marker = order.marker();
            let place = format!("COALESCE({after}, {beyond})");
            let until = path_until(section, direction, &mut binder);
            (
                format!(
                    "SELECT d.id, d.path, NULL, d.content_hash FROM documents AS d
                     WHERE {}{until}
                       AND NOT EXISTS (SELECT 1 FROM document_fields AS m
                           WHERE m.document = d.id AND m.key = {key} AND m.{marker} = 1)",
                    path_seek(order_seek, comparison, &place)
                ),
                answer_ordering("d.path", descending),
                "d.id",
            )
        }
    };
    // The driving filter's set drives the section, and every other filter is
    // tested at each document's key; where none drives, each row the order
    // index hands back is tested.
    let filters: String = section
        .filters
        .iter()
        .enumerate()
        .map(|(at, filter)| {
            let spelled = if driving == Some(at) {
                filter.spell(id, &mut binder)
            } else {
                filter.test(id, &mut binder)
            };
            format!("\n                       AND {spelled}")
        })
        .collect();
    let limit = limited(section, &mut binder);
    let sql = format!(
        "{head}{filters}
                     ORDER BY {ordering}
                     LIMIT {limit}"
    );
    (sql, binder.into_values())
}

/// A section's `LIMIT` operand: its rows, and the rows it passes before the
/// first it hands back where it names where a walk's window ends.
fn limited(section: &Section<'_>, binder: &mut Binder) -> String {
    let limit = binder.bind(Value::Integer(
        i64::try_from(section.rows).expect("a page's row count fits i64"),
    ));
    match section.offset {
        0 => limit,
        offset => format!(
            "{limit} OFFSET {}",
            binder.bind(Value::Integer(
                i64::try_from(offset).expect("a window's length fits i64")
            ))
        ),
    }
}

/// The comparison that holds a row at or before a place in the order a
/// `direction` runs: `<=` ascending, `>=` descending.
fn through(direction: PageDirection) -> &'static str {
    match direction {
        PageDirection::Ascending => "<=",
        PageDirection::Descending => ">=",
    }
}

/// The bound a section ordered by the path alone stops at, the window's end
/// inclusive, as a seek of `documents_path_nocase` reads it — a further term
/// on the same row value the section seeks past its place — or nothing where
/// the section reads to its end.
fn path_until(section: &Section<'_>, direction: PageDirection, binder: &mut Binder) -> String {
    match section.until {
        Some(until) => {
            let place = binder.bind(
                until
                    .path
                    .map_or(Value::Null, |path| Value::Text(path.into())),
            );
            format!(
                "\n                       AND {}",
                path_seek("", through(direction), &place)
            )
        }
        None => String::new(),
    }
}

/// A path page the filter at `at` drives from its source ([`Filter::source`]):
/// the documents it keeps, in the answer's path order the page's way, from
/// past the place the page resumes at, each reaching its row by its id and
/// tested against every other filter at its key. So a page reads the
/// documents the filter keeps from where it resumes until it is full, sorts
/// nothing, and costs the same however many the filter keeps before it or
/// after it.
fn compose_ordered_path_page(
    section: &Section<'_>,
    direction: PageDirection,
    at: usize,
    mut binder: Binder,
) -> (String, Vec<Value>) {
    let descending = direction == PageDirection::Descending;
    let reach = path_page_reach(section, descending);
    let source = section.filters[at]
        .source(&reach, &mut binder)
        .expect("a filter that keeps its documents in path order has a source");
    let tests: Vec<String> = section
        .filters
        .iter()
        .enumerate()
        .filter(|(other, _)| *other != at)
        .map(|(_, filter)| filter.test("d.id", &mut binder))
        .collect();
    let tested = if tests.is_empty() {
        String::new()
    } else {
        format!(
            "\n                     WHERE {}",
            tests.join("\n                       AND ")
        )
    };
    let (_, _, order) = spelled(direction);
    let limit = limited(section, &mut binder);
    let sql = format!(
        "SELECT d.id, d.path, NULL, d.content_hash FROM ({source}) AS dv
                     CROSS JOIN documents AS d ON d.id = dv.id{tested}
                     ORDER BY dv.folded{order}, dv.path{order}
                     LIMIT {limit}"
    );
    (sql, binder.into_values())
}

/// Where a path page's source reads from and stops: past the place the page
/// resumes at, and within every path part's folded range in the answer order
/// ([`answer_range`]) — from the range's near end where that stands past the
/// place, and to its far end — so a source driven beside a path part reads no
/// document the part's range leaves out, whichever part drives.
fn path_page_reach(section: &Section<'_>, descending: bool) -> Reach {
    let place = section.start.path.map(answer_place);
    let ranges: Vec<(String, Value)> = section
        .filters
        .iter()
        .filter_map(Filter::path_part)
        .map(|part| answer_range(&part.pattern))
        .collect();
    let text = |text: String| Value::Text(text);
    if descending {
        // A range's near end is its upper bound, exclusive: the paths folding
        // below it. The least of them, where it stands below the place, is
        // where the source starts; the greatest lower bound is where it stops.
        let near = ranges
            .iter()
            .filter_map(|(_, upper)| match upper {
                Value::Text(upper) => Some(upper.clone()),
                _ => None,
            })
            .min();
        let from = match (place, near) {
            (Some(place), Some(near)) if (near.clone(), String::new()) < place => {
                (text(near), text(String::new()))
            }
            (Some((folded, path)), _) => (text(folded), text(path)),
            (None, Some(near)) => (text(near), text(String::new())),
            (None, None) => (Value::Blob(Vec::new()), Value::Blob(Vec::new())),
        };
        let until = ranges.iter().map(|(lower, _)| lower.clone()).max();
        Reach {
            descending,
            from,
            inclusive: false,
            until: until.map(text),
        }
    } else {
        // A range's near end is its lower bound, inclusive: every path folding
        // to it or past it. The greatest of them, where it stands past the
        // place, is where the source starts; the least upper bound is where it
        // stops.
        let near = ranges.iter().map(|(lower, _)| lower.clone()).max();
        let (from, inclusive) = match (place, near) {
            (Some(place), Some(near)) if (near.clone(), String::new()) > place => {
                ((text(near), text(String::new())), true)
            }
            (Some((folded, path)), _) => ((text(folded), text(path)), false),
            (None, Some(near)) => ((text(near), text(String::new())), true),
            (None, None) => ((text(String::new()), text(String::new())), false),
        };
        let until = ranges
            .iter()
            .filter_map(|(_, upper)| match upper {
                Value::Text(upper) => Some(upper.clone()),
                _ => None,
            })
            .min();
        Reach {
            descending,
            from,
            inclusive,
            until: until.map(text),
        }
    }
}

/// The documents past `place` in the answer order, in the direction
/// `comparison` names, as a seek of `documents_path_nocase` reads them: a path
/// is unique, so the folded and the bytewise path name each document's place.
fn path_seek(reach: &str, comparison: &str, place: &str) -> String {
    AnswerSeek {
        reach,
        comparison,
        lead: None,
        path: "d.path",
        folded: place,
        bytewise: place,
        trail: None,
        id: None,
    }
    .spelled()
}

/// A direction's comparison, the bound an unset position coalesces to, and the
/// `ORDER BY` suffix.
fn spelled(direction: PageDirection) -> (&'static str, &'static str, &'static str) {
    match direction {
        PageDirection::Ascending => (">", "''", ""),
        PageDirection::Descending => ("<", "x''", " DESC"),
    }
}

/// The ids a hydration statement reads, as the JSON array `json_each` walks.
fn id_list(ids: &[i64]) -> Value {
    let listed: Vec<String> = ids.iter().map(i64::to_string).collect();
    Value::Text(format!("[{}]", listed.join(",")))
}

/// [`FindStatement::DocumentSpan`]: the greatest document row id, zero where
/// the vault holds none.
pub(crate) fn compose_document_span() -> (String, Vec<Value>) {
    (
        "SELECT coalesce(max(id), 0) FROM documents".to_string(),
        Vec::new(),
    )
}

/// [`FindStatement::KnownKey`]: whether any document carries `key`.
pub(crate) fn compose_known_key(key: &str) -> (String, Vec<Value>) {
    (
        "SELECT EXISTS (SELECT 1 FROM document_fields AS fk
                         WHERE fk.key = ?1 AND fk.ordinal = 0)"
            .to_string(),
        vec![Value::Text(key.to_string())],
    )
}

/// [`FindStatement::OffsetSpellings`]: whether `key` holds a typed date
/// stating no offset, and whether it holds one stating an offset.
pub(crate) fn compose_offset_spellings(key: &str) -> (String, Vec<Value>) {
    (
        "SELECT EXISTS (SELECT 1 FROM document_fields AS fu
                         WHERE fu.key = ?1 AND fu.offset_stated = 0),
                EXISTS (SELECT 1 FROM document_fields AS fs
                         WHERE fs.key = ?1 AND fs.offset_stated = 1)"
            .to_string(),
        vec![Value::Text(key.to_string())],
    )
}

/// [`FindStatement::FieldUniverse`]: every key a document carries, once each,
/// in key order: the whole [`key_walk`], from the first key.
///
/// Every key is text, and every text sorts at or after the empty one, so the
/// first step's bound excludes none.
pub(crate) fn compose_universe() -> (String, Vec<Value>) {
    (
        format!(
            "{} SELECT key FROM walked WHERE key IS NOT NULL",
            key_walk(">= ''", None)
        ),
        Vec::new(),
    )
}

/// [`FindStatement::BareDirectory`]: true where no document stands at `path`
/// and some document stands in `[lower, upper)`, the range of paths beneath
/// it, each compared under `order`'s collation. Where `order` folds ASCII
/// case that collation is `NOCASE`, which folds both sides of each
/// comparison, so the path and the bounds need not be folded first.
pub(crate) fn compose_bare_directory(
    path: &str,
    lower: &str,
    upper: &str,
    order: StoredPathOrder,
) -> (String, Vec<Value>) {
    let collation = order.collation();
    (
        format!(
            "SELECT NOT EXISTS (SELECT 1 FROM documents AS da WHERE da.path = ?1{collation})
            AND EXISTS (SELECT 1 FROM documents AS du
                WHERE du.path >= ?2{collation} AND du.path < ?3{collation})"
        ),
        vec![
            Value::Text(path.to_string()),
            Value::Text(lower.to_string()),
            Value::Text(upper.to_string()),
        ],
    )
}

/// [`FindStatement::MatchProbe`]: whether any document matches the full-text
/// `query`.
///
/// The answer is not what the probe is run for: the engine parses a query when
/// the selection is first stepped, so a statement that stepped no row — one
/// bounded by `LIMIT 0`, which SQLite answers without opening the selection —
/// would pass a query the page's own statement then fails on. `EXISTS` stops
/// at the first match, so the probe reads at most one.
pub(crate) fn compose_match_probe(query: &str) -> (String, Vec<Value>) {
    (
        "SELECT EXISTS (SELECT 1 FROM documents_fts WHERE documents_fts MATCH ?1)".to_string(),
        vec![Value::Text(query.to_string())],
    )
}

/// Which of a document row's own columns a hydration reads.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct DocumentColumns {
    /// The frontmatter projection, which a field is read off.
    pub(crate) frontmatter: bool,
    /// The head of the body, and the whole body's length in bytes.
    pub(crate) body: bool,
}

/// [`FindStatement::HydrateDocuments`]: the rows `ids` name, each as its id,
/// its frontmatter projection, at most `body_ceiling` bytes of its body, and
/// the whole body's length in bytes — a column the projection does not name
/// selected as `NULL`, so its value is never handed back.
///
/// The body's head is cut in bytes and may end inside a character; the reader
/// cuts it back to the last whole one.
pub(crate) fn compose_documents(
    ids: &[i64],
    columns: DocumentColumns,
    body_ceiling: usize,
) -> (String, Vec<Value>) {
    let mut binder = Binder::default();
    let listed = binder.bind(id_list(ids));
    let frontmatter = if columns.frontmatter {
        "d.frontmatter".to_string()
    } else {
        "NULL".to_string()
    };
    let (head, length) = if columns.body {
        let ceiling = binder.bind(Value::Integer(
            i64::try_from(body_ceiling).expect("a body ceiling fits i64"),
        ));
        (
            format!("substr(CAST(d.body AS BLOB), 1, {ceiling})"),
            "octet_length(d.body)".to_string(),
        )
    } else {
        ("NULL".to_string(), "NULL".to_string())
    };
    (
        format!(
            "SELECT d.id, {frontmatter}, {head}, {length} FROM documents AS d
             WHERE d.id IN (SELECT value FROM json_each({listed}))"
        ),
        binder.into_values(),
    )
}

/// [`FindStatement::NestedHead`]: the rows of `nested` below `ceiling` for
/// each document `ids` names, each row's item columns followed by its
/// document id and its ordinal.
pub(crate) fn compose_nested_head(
    nested: Nested,
    ids: &[i64],
    ceiling: usize,
) -> (String, Vec<Value>) {
    (
        format!(
            "SELECT {columns}, n.document, n.ordinal FROM {table} AS n
             WHERE n.document IN (SELECT value FROM json_each(?1)) AND n.ordinal < ?2",
            columns = nested.columns(),
            table = nested.table(),
        ),
        vec![
            id_list(ids),
            Value::Integer(i64::try_from(ceiling).expect("a row ceiling fits i64")),
        ],
    )
}

/// [`FindStatement::NestedTotal`]: how many rows of `nested` each document
/// `ids` names holds, as the id and the count.
pub(crate) fn compose_nested_total(nested: Nested, ids: &[i64]) -> (String, Vec<Value>) {
    (
        format!(
            "SELECT j.value, (SELECT COUNT(*) FROM {table} AS n WHERE n.document = j.value)
             FROM json_each(?1) AS j",
            table = nested.table(),
        ),
        vec![id_list(ids)],
    )
}

/// The paths a statement reads, as the JSON array `json_each` walks.
pub(crate) fn path_list(paths: &[&str]) -> Result<Value, StoreError> {
    canonical_json(&FrontmatterValue::Sequence(
        paths
            .iter()
            .map(|path| FrontmatterValue::String((*path).to_string()))
            .collect(),
    ))
    .map(Value::Text)
}

/// [`FindStatement::FindingHead`]: the findings standing at each of `paths`
/// under `fingerprint`, at most `ceiling` of them per path in `(position,
/// kind, id)` order, each as [`FINDING_ROW_COLUMNS`].
///
/// The paths drive the statement — `CROSS JOIN` keeps them the outer loop —
/// and each path's head is a seek of `findings_path` at `(path, fingerprint)`
/// that stops at the ceiling, so a document with more findings than the
/// ceiling costs its row the ceiling. A head is handed back by row id.
pub(crate) fn compose_finding_head(
    paths: &[&str],
    fingerprint: &str,
    ceiling: usize,
) -> Result<(String, Vec<Value>), StoreError> {
    Ok((
        format!(
            "SELECT {FINDING_ROW_COLUMNS} FROM json_each(?1) AS j
             CROSS JOIN findings AS f
             WHERE f.id IN (SELECT h.id FROM findings AS h
                 WHERE h.path = j.value AND h.vault_schema_fingerprint = ?2
                 ORDER BY h.position, h.kind, h.id
                 LIMIT ?3)"
        ),
        vec![
            path_list(paths)?,
            Value::Text(fingerprint.to_string()),
            Value::Integer(i64::try_from(ceiling).expect("a row ceiling fits i64")),
        ],
    ))
}

/// [`FindStatement::FindingTotal`]: how many findings stand at each of
/// `paths` under `fingerprint`, as the path and the count.
pub(crate) fn compose_finding_total(
    paths: &[&str],
    fingerprint: &str,
) -> Result<(String, Vec<Value>), StoreError> {
    Ok((
        "SELECT j.value, (SELECT COUNT(*) FROM findings AS h
                 WHERE h.path = j.value AND h.vault_schema_fingerprint = ?2)
             FROM json_each(?1) AS j"
            .to_string(),
        vec![path_list(paths)?, Value::Text(fingerprint.to_string())],
    ))
}

/// [`FindStatement::FindingCandidates`]: the candidate rows of each finding
/// `ids` names, as the finding, the rank, the path and the suffix.
pub(crate) fn compose_finding_candidates(ids: &[i64]) -> (String, Vec<Value>) {
    (
        "SELECT c.finding, c.rank, c.path, c.suffix FROM finding_candidates AS c
             WHERE c.finding IN (SELECT value FROM json_each(?1))"
            .to_string(),
        vec![id_list(ids)],
    )
}

/// [`FindStatement::FindingClasses`]: the class keys of each finding `ids`
/// names, as the finding and the key.
pub(crate) fn compose_finding_classes(ids: &[i64]) -> (String, Vec<Value>) {
    (
        "SELECT k.finding, k.class_key FROM finding_classes AS k
             WHERE k.finding IN (SELECT value FROM json_each(?1))"
            .to_string(),
        vec![id_list(ids)],
    )
}

/// [`FindStatement::ClassHead`]: at most `rows` of `class`, each as its id and
/// its path, in the resolution ladder's order.
///
/// **A class is read as every read of a class reads it**: its bounded runs are
/// [`resolve::class_head_rows`], and a head of it is in [`resolve::ladder_order`].
pub(crate) fn compose_class_head(class: &TargetClass, rows: usize) -> (String, Vec<Value>) {
    let mut values = class.parameters();
    let limit = values.len() + 1;
    values.push(Value::Integer(
        i64::try_from(rows).expect("a row count fits i64"),
    ));
    (
        format!(
            "SELECT dr.id, dr.path {} {} LIMIT ?{limit}",
            resolve::class_head_rows(class, &format!("?{limit}")),
            resolve::ladder_order(class)
        ),
        values,
    )
}

/// [`FindStatement::ClassTotal`]: how many documents `class` holds.
pub(crate) fn compose_class_total(class: &TargetClass) -> (String, Vec<Value>) {
    (resolve::class_total(class), class.parameters())
}

/// One range of one suffix spelling's class, as
/// [`FindStatement::CandidateSuffixes`] reads it: the range's bounds in the key
/// space the root probes, and how many segments the spelling spells.
pub(crate) struct SpellingRange<'a> {
    pub(crate) lower: &'a str,
    pub(crate) upper: &'a str,
    pub(crate) segments: usize,
}

/// [`FindStatement::CandidateSuffixes`]: for each of `ranges`, at most two
/// of the admitted documents in it under `order`, each as the range's index
/// in `ranges` and the document's id, in no order.
///
/// The ranges drive the statement — `CROSS JOIN` keeps them the outer loop.
/// Each admitting count opens one seek of the suffix key the root probes,
/// cut at two documents in ladder order. The bounded heads are merged and
/// cut at two before their documents are emitted, each reached by row id.
/// A range costs at most two members per admitting count, however large the
/// class. Every class range uses [`resolve::range_rows`].
pub(crate) fn compose_candidate_suffixes(
    ranges: &[SpellingRange<'_>],
    order: StoredPathOrder,
) -> Result<(String, Vec<Value>), StoreError> {
    let listed = canonical_json(&FrontmatterValue::Sequence(
        ranges
            .iter()
            .map(|range| {
                FrontmatterValue::Sequence(vec![
                    FrontmatterValue::String(range.lower.to_string()),
                    FrontmatterValue::String(range.upper.to_string()),
                    FrontmatterValue::Int(
                        i64::try_from(range.segments).expect("a segment count fits i64"),
                    ),
                ])
            })
            .collect(),
    ))?;
    let class = resolve::range_rows(
        SuffixKey::under(order),
        (
            "json_extract(j.value, '$[0]')",
            "json_extract(j.value, '$[1]')",
        ),
        "json_extract(j.value, '$[2]')",
        Some("2"),
    );
    Ok((
        format!(
            "SELECT j.key, dc.id FROM json_each(?1) AS j
        CROSS JOIN documents AS dc
        WHERE dc.id IN (SELECT id FROM ({class}))"
        ),
        vec![Value::Text(listed)],
    ))
}

/// [`FindStatement::LinkTargets`]: for each link `links` names that names a
/// document, at most `head` of the documents it names under `order`, in the
/// resolution ladder's order, each as the link's id, the document's id and
/// path, and how many documents the link names.
///
/// **Each distinct key is read once**, however many of the links carry it:
/// `keyed` is the page's keys, each once, and `reached` the documents each
/// reaches — a suffix key's bounded head through [`resolve::link_key_rows`],
/// a path key's documents through [`resolve::link_key_path`] — so links
/// sharing a key share its read. Each key's total sums its admitted runs
/// separately from its bounded head. Each link's head merges its keys' heads
/// in the ladder's order: the probed key, then the path. Its total sums
/// their disjoint counts. A path key's rows share one key, so they rank by
/// the path alone, which is its path order among documents a folding root
/// reads as one.
pub(crate) fn compose_link_targets(
    links: &[i64],
    order: StoredPathOrder,
    head: usize,
) -> (String, Vec<Value>) {
    let key = SuffixKey::under(order);
    let link_key = resolve::link_key_column(key);
    let class = resolve::link_key_rows(key, "keyed.key", "keyed.segments", Some("?2"));
    let total = resolve::link_key_total(key, "keyed.key", "keyed.segments");
    let path = resolve::link_key_path("dp", key, "keyed.key");
    let rung = format!("dc.{}", key.column());
    let ladder = resolve::ladder("reached.rung", "reached.path");
    (
        format!(
            "WITH page(link) AS (SELECT value FROM json_each(?1)),
             keyed(key, segments) AS MATERIALIZED (
                 SELECT DISTINCT lk.{link_key}, lk.segments
                 FROM page CROSS JOIN link_keys AS lk WHERE lk.link = page.link),
             totals(key, total) AS MATERIALIZED (
                 SELECT keyed.key, CASE WHEN keyed.segments IS NOT NULL THEN ({total})
                     ELSE (SELECT COUNT(*) FROM documents AS dp WHERE {path}) END FROM keyed),
             reached(key, id, path, rung) AS MATERIALIZED (
                 SELECT keyed.key, dc.id, dc.path, {rung}
                 FROM keyed CROSS JOIN documents AS dc
                 WHERE keyed.segments IS NOT NULL AND dc.id IN (SELECT id FROM ({class}))
                 UNION ALL
                 SELECT keyed.key, dc.id, dc.path, keyed.key
                 FROM keyed CROSS JOIN documents AS dc
                 WHERE keyed.segments IS NULL AND dc.id IN (
                     SELECT dp.id FROM documents AS dp WHERE {path} ORDER BY dp.path LIMIT ?2)),
             ranked(link, id, path, place, total) AS (
                 SELECT lr.link, reached.id, reached.path,
                        ROW_NUMBER() OVER (PARTITION BY lr.link ORDER BY {ladder}),
                        (SELECT SUM(t.total) FROM link_keys AS lt CROSS JOIN totals AS t
                         WHERE lt.link = lr.link AND t.key = lt.{link_key})
                 FROM page CROSS JOIN link_keys AS lr CROSS JOIN reached
                 WHERE lr.link = page.link AND reached.key = lr.{link_key})
             SELECT link, id, path, total FROM ranked WHERE place <= ?2
             ORDER BY link, place"
        ),
        vec![
            id_list(links),
            Value::Integer(i64::try_from(head).expect("a head fits i64")),
        ],
    )
}

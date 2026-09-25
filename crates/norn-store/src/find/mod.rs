//! The find builder: a request's conjunction, order and page bound compiled
//! into statements a read snapshot runs.
//!
//! The builder is inherent methods on [`Snapshot`], so every statement it runs
//! reads the one instant the snapshot was established at and is counted on the
//! snapshot's own statement counter. It answers a page of document keys — the
//! row id, the path and the value the page is ordered by — in the order the
//! request states, and where the next page starts.
//!
//! # A page is sections, each a seek or a stated walk
//!
//! **A page with no filter reads its order index in page order**, so nothing
//! sorts: the path page and a field sort's valued section seek it and stop at
//! the page's bound, and a field sort's missing section walks it, as stated
//! below. **A page with a filter
//! drives from the filter's seek and sorts the matched set**, so its cost is
//! bounded by the match count, which is what a narrowing part narrows.
//!
//! A path order is one section: `documents_path_nocase`, the path folded by
//! ASCII case with the bytewise path as the tie-break, so the order is total.
//!
//! A field order is two. The **valued** section reads the key's marker rows —
//! each document's least value under the order, one row per document — in
//! `(value, path)` order, on the order's marker index where no filter drives
//! the page, so a document whose field holds a set appears once, at its least
//! value. The **missing** section reads
//! the documents holding no value under the order — the key absent, every value
//! null, or, under the typed order, no value that reads as the declared type —
//! in bytewise path order. The missing section stands before the valued one
//! ascending and after it descending, which is where the wire says a document
//! missing the sort field goes. A page reads section after section until it
//! holds one row more than its bound, and the extra row is what says a next
//! page exists.
//!
//! **The missing section's walk is the price of ordering missing as `NULL`.**
//! The valued section is a seek of the marker index, and stops at the page's
//! bound. The missing section is a walk of the path index that probes each
//! document's marker row, so it passes every document carrying the key to
//! reach the next one that does not. An ascending first page reads the missing
//! section first, so where few or no documents miss the key it costs a walk
//! proportional to the documents that carry it. A drain pays that walk at most
//! twice: a page reads one row past its bound to learn a next page exists, and
//! the next page resumes from the row it kept, so the gap up to the look-ahead
//! row is walked again; a continuation past the missing section resumes in the
//! valued section and never walks it again.
//!
//! **Which order a field sort uses** is the typed one where the declaration
//! gives the key a typed order and the raw text's otherwise, on every page: the
//! order is the request's, and a continuation's cursor is judged against it.
//! **The declaration is the snapshot's.** It names the schema it was read from,
//! and a find whose declaration is not the one the snapshot pins is refused
//! ([`PageRefusal::DeclarationNotPinned`]), so a typed order is always the one
//! the typed column holds.
//!
//! **A part that compares values compares under the key's order.** Equality,
//! inequality, membership and a `before`/`after` bound on a key the declaration
//! gives a typed order compare the typed sort key of each value the request
//! names against the typed column, so `9` and `9.0` under a number are one
//! value to an equality part exactly as they are to the declared type's
//! comparison; a value that does not read as the type names no place in the
//! order and is refused. A key with no typed order compares raw text.
//!
//! # A filter is one seek, and a page drives from it
//!
//! Each part of the conjunction narrows every section by the rows one index
//! seek of its own reaches — [`ReadFilter`] names each and the index it
//! seeks — so a part costs the rows it matches, never the vault. A section a
//! part narrows reaches each document the part's seek handed it by the
//! document's key, and sorts them in the page's order: the order index is not
//! read at all. Inequality and absence are the exception
//! ([`ReadFilter::excludes`]): their seek reaches the documents a page must
//! not hold, so there is no seek of what they keep, and a section they alone
//! narrow seeks its order index as a section with no filter does and tests
//! each row against them.
//!
//! # A part that cannot be applied is reported, never dropped in silence
//!
//! **The report is a biconditional**: a part is reported in
//! [`Found::unsatisfied`] exactly where it cannot be applied as asked, and a
//! part that can be is never reported, even where it matches nothing.
//!
//! **A conjunction's part that cannot be applied empties the page.** A part
//! with no meaning narrows the answer to nothing, so a caller never receives
//! rows broader than it asked for, and the report is what keeps the empty page
//! from reading as a vault with nothing in it. **A key outside the field
//! universe is the one exception**: it is reported, and the page is answered
//! as the first entry below states, a predicate key's part filtering nothing.
//! Every other entry below empties the page.
//!
//! - **A key outside the field universe** — the keys the declaration names
//!   and the keys some document carries — is reported with the keys near it,
//!   by the one did-you-mean rule every read builder shares. An unknown sort
//!   key orders the page by path, ascending; an unknown projected key carries nothing under it; an
//!   unknown predicate key's part filters nothing. Whether a key is known is a
//!   declaration lookup or one existence seek of the presence rows, and the
//!   universe itself is walked only where some key is unknown.
//! - **A path part** is read by [`norn_wire::Pattern`]'s grammar, and matches
//!   nothing by construction three ways. A glob that does not parse is
//!   malformed. A glob naming no path the store could hold — read with each
//!   wildcard as a letter, it is not a document path — is impossible. And a
//!   glob with no wildcard, at which no document stands but under which some
//!   do, is a **bare directory**: the grammar matches globs, so the part
//!   matches the directory's own path alone, and a directory is no document.
//!   A path with no wildcard at which nothing stands and under which nothing
//!   stands is none of these: it is a part that can be applied and matches no
//!   document, and it is answered as that — an empty page, not a report.
//! - **A resolution target that is not a suffix address** names no document.
//! - **A links-to target that names several documents or none** names no one
//!   document a link could reach: it is reported, beside the head of an
//!   ambiguous one, and the page is empty.
//! - **A match part whose query the full-text engine cannot parse** is
//!   malformed. Whether it parses is asked before the page runs, by one probe
//!   of the full-text index; a malformed query's part is reported with the
//!   engine's words, and the page is empty, as a malformed glob's is.
//!
//! A `links_to` part resolves its own target to one document through the one
//! resolver, then matches the documents holding a link whose resolution is
//! exactly that document, sought on the link index at the keys the document is
//! named by ([`ReadFilter::LinksTo`]): a link naming two or more documents is a
//! backlink of none of them.
//!
//! A `resolves` part enumerates the target's ambiguity class through the one
//! resolver, [`crate::resolve::TargetClass`]: both reductions of a dotted leaf, over the raw
//! suffix key where the snapshot's root tells spellings apart and over the
//! folded key where it folds ASCII case, less the places the declaration's
//! ambiguity-ignore set keeps out of the class. The case behaviour is the
//! snapshot's ([`Snapshot::path_order`]): the order the rows it reads were
//! derived under, so no find detects it.
//!
//! # A find is keys, then rows
//!
//! [`Snapshot::find`] is the whole request: it judges the wire cursor the
//! request continues, pages the keys, hydrates the rows the page found —
//! exactly those, never the one past the bound that says a next page exists —
//! and mints the cursor the next page continues. The rows carry only the
//! columns the request projected ([`hydrate`] states what each costs), and
//! [`Found::work`] says what the find read.
//!
//! **A find's plans are of the statements it ran.** A find runs every
//! statement through one site, which records the statement's shape, its
//! filters, its text and its bound values in the order it ran them, and
//! prepares the text it recorded. [`Snapshot::find_plans`] runs the same find
//! and explains that record, so a plan is never of a second spelling of a
//! statement, and a section a page never reached is never explained.
//!
//! **A cursor names the order it was minted in, and is judged against the
//! request's.** A page ordered by a typed field is minted under the active
//! schema fingerprint, and one ordered any other way under none. A store with
//! no schema pinned mints every cursor under none: its declaration declares
//! nothing, so no key has a typed order there. A continuation whose cursor is
//! detectably not a position in the request's order is refused with the wire's
//! [`norn_wire::CursorOrderChanged`]: a typed order's cursor minted under
//! another fingerprint or under none, a raw or path order's minted under one,
//! and a path order's carrying a sort value. A raw continuation survives a
//! re-pin that leaves its key untyped, since the raw order has not moved. A
//! cursor names neither its sort key nor its direction, so the cursor's
//! encoding cannot say which order it was minted in, and two gaps follow. Two
//! raw field orders are not told apart, and a path order's cursor continued in
//! a raw field order reads as a position in the missing section. And a cursor
//! of any order, typed, raw or path, continued in the same order reversed
//! reads as a position in the reversed order, so an ascending cursor replayed
//! descending is answered rather than refused.

mod hydrate;
mod statement;

use std::collections::BTreeSet;

use norn_db::EmittedPlan;
use norn_wire::{
    Column, Cursor, CursorKey, Direction, DocumentRow, FindParams, FindReport, Moved, Page,
    SortKey, Unsatisfied,
};

use crate::error::{self, StoreError};
use crate::fields::ContentModel;
use crate::read::{
    FieldOrder, Filter, KeyPlace, Lookups, PageRefusal, Ran, ReadFilter, ReadStatement, Report,
    ResolvesPart, page_limit,
};
use crate::store::Snapshot;

pub use hydrate::{BODY_ROW_CEILING, FindWork, NESTED_ROW_CEILING, NestedRows};
pub(crate) use hydrate::{
    block_row, bounded_body, heading_row, tag_row, wire_block, wire_heading, wire_span,
};
pub use statement::{FIND_STATEMENTS, FindStatement, Nested, PageDirection};
use statement::{Section, SectionStart, compose_page};
pub(crate) use statement::{
    compose_bare_directory, compose_candidate_suffix, compose_class_head, compose_class_total,
    compose_finding_candidates, compose_finding_classes, compose_known_key, compose_match_probe,
    compose_path_head, compose_path_total, compose_universe,
};

/// Where a page stopped, or where a continuation resumes: the value the row
/// was ordered by, and its path. The order it stands in is the request's.
///
/// `sort` is `None` for a row of a path order, and for a row of a field order's
/// missing section.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FindPosition {
    pub(crate) sort: Option<String>,
    pub(crate) path: String,
}

/// One document a page found.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FoundKey {
    /// The document's row id, which is what a page's rows are hydrated by.
    document: i64,
    path: String,
    sort: Option<String>,
}

impl FoundKey {
    /// The document at row id `document` and `path`, standing in no sorted
    /// order: a key a read hydrates the rows of, whatever order it paged them
    /// in.
    pub(crate) fn unsorted(document: i64, path: String) -> Self {
        FoundKey {
            document,
            path,
            sort: None,
        }
    }

    /// The document's path.
    pub(crate) fn path(&self) -> &str {
        &self.path
    }

    /// Where a page that stopped at this document resumes.
    fn position(&self) -> FindPosition {
        FindPosition {
            sort: self.sort.clone(),
            path: self.path.clone(),
        }
    }

    /// The document's row id, which hydration reads the page's rows by.
    pub(crate) fn document(&self) -> i64 {
        self.document
    }
}

/// What [`Snapshot::find`] answers: a page of rows, where the next begins, and
/// what the request could not apply.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    /// The documents, at most the page bound of them, in the request's order.
    pub rows: Vec<DocumentRow>,
    /// Where the next page begins, and `None` where this page is the last.
    pub next: Option<Cursor>,
    /// What moved between the cursor this page continued and the snapshot it
    /// was answered from. Empty on a first page.
    pub moved: Vec<Moved>,
    /// The parts of the request that could not be applied as asked, in the
    /// order the request names them: the sort key, the conjunction's parts,
    /// then the projection's keys.
    pub unsatisfied: Vec<Unsatisfied>,
    /// The reading the page was answered from, as a cursor carries it: the
    /// schema fingerprint where the page ran in a typed order, and `None`
    /// where it ran in any other.
    pub snapshot: norn_wire::Snapshot,
    /// What the find read.
    pub work: FindWork,
}

impl Found {
    /// The unsatisfied parts and the report a handler wraps in a
    /// [`norn_wire::VaultAnswer`].
    pub fn into_report(self) -> (Vec<Unsatisfied>, FindReport) {
        (
            self.unsatisfied,
            Page::new(self.rows, self.next, self.moved),
        )
    }
}

/// A statement a find ran, with the plan SQLite reported for the text and the
/// values it ran with.
#[derive(Clone, Debug)]
pub struct FindPlan {
    pub statement: FindStatement,
    /// The filters the statement narrows by, in the request's order.
    pub filters: Vec<ReadFilter>,
    pub plan: EmittedPlan,
}

/// The order a page runs in.
#[derive(Clone, Copy, Debug)]
enum PageOrder<'a> {
    Path(PageDirection),
    Field {
        key: &'a str,
        order: FieldOrder,
        direction: PageDirection,
    },
}

impl PageOrder<'_> {
    /// The field order a field sort runs in, and `None` for a path order.
    fn field_order(self) -> Option<FieldOrder> {
        match self {
            PageOrder::Path(_) => None,
            PageOrder::Field { order, .. } => Some(order),
        }
    }
}

/// A request compiled: its order, its filters, and the parts it could not
/// apply as asked.
struct Compiled<'a> {
    order: PageOrder<'a>,
    filters: Vec<Filter>,
    reports: Vec<Report>,
    /// Whether some part of the conjunction matches no document, which
    /// empties every section.
    matches_nothing: bool,
}

impl Compiled<'_> {
    fn filter_shapes(&self) -> Vec<ReadFilter> {
        self.filters.iter().map(|filter| filter.shape).collect()
    }

    /// The field order a field sort runs in, and `None` for a path order.
    fn field_order(&self) -> Option<FieldOrder> {
        self.order.field_order()
    }
}

/// The columns a request projects, read once.
pub(crate) struct Projection<'a> {
    /// Every field the document carries: [`Column::Fields`].
    pub(crate) all_fields: bool,
    /// The keys [`Column::Field`] names, each once, in the order first named.
    pub(crate) keys: Vec<&'a str>,
    /// The body.
    pub(crate) body: bool,
    /// The nested collections named, each once, in [`Nested::ALL`]'s order.
    pub(crate) nested: Vec<Nested>,
    /// The findings standing over the document.
    pub(crate) findings: bool,
}

impl<'a> Projection<'a> {
    /// Every column a row can carry: every field, the body, each nested
    /// collection a find projects, and the findings.
    pub(crate) fn whole() -> Self {
        Projection {
            all_fields: true,
            keys: Vec::new(),
            body: true,
            nested: Nested::ALL.to_vec(),
            findings: true,
        }
    }

    /// What `columns` projects, or the refusal of a column this build of the
    /// store does not know.
    pub(crate) fn of(columns: &'a [Column]) -> Result<Self, PageRefusal> {
        let mut projection = Projection {
            all_fields: false,
            keys: Vec::new(),
            body: false,
            nested: Vec::new(),
            findings: false,
        };
        let mut nested = BTreeSet::new();
        for column in columns {
            match column {
                Column::Path {} => {}
                Column::Field { key, .. } => {
                    if !projection.keys.contains(&key.as_str()) {
                        projection.keys.push(key);
                    }
                }
                Column::Fields {} => projection.all_fields = true,
                Column::Body {} => projection.body = true,
                Column::Tags {} => {
                    nested.insert(0);
                }
                Column::Headings {} => {
                    nested.insert(1);
                }
                Column::Blocks {} => {
                    nested.insert(2);
                }
                Column::Links {} => {
                    nested.insert(3);
                }
                Column::Findings {} => projection.findings = true,
                _ => return Err(PageRefusal::UnknownPart { part: "a column" }),
            }
        }
        projection.nested = nested.into_iter().map(|slot| Nested::ALL[slot]).collect();
        Ok(projection)
    }

    /// Whether a row carries a field map at all.
    pub(crate) fn names_fields(&self) -> bool {
        self.all_fields || !self.keys.is_empty()
    }
}

impl Snapshot {
    /// One page of the documents `params` asks for, as rows carrying the
    /// columns it projects, in its order, continuing its cursor.
    ///
    /// `declared` is the vault's declaration, read from the schema the
    /// snapshot pins: it decides a field sort's order, how a bound is
    /// compared, and — beside the keys documents carry — which keys are known.
    /// The page holds `params.limit` rows, [`crate::DEFAULT_PAGE`] where it names
    /// none, and only those rows are hydrated.
    ///
    /// Refused: a page bound outside `1..=`[`crate::MAX_PAGE`]; a membership part
    /// naming no value or more than [`crate::IN_VALUES_CEILING`]; a declaration read
    /// from another schema than the snapshot pins; a cursor among rows that
    /// are not documents; a cursor that is not a position in the request's
    /// order, as the module states; a projected column or a part this build
    /// of the store does not know; and a bound that does not read as its key's
    /// declared type.
    pub fn find(&self, params: &FindParams, declared: &ContentModel) -> Result<Found, PageRefusal> {
        self.run_find(params, declared, &mut Lookups::default())
    }

    /// Every statement [`Snapshot::find`] runs for `params`, in the order it
    /// runs them, each with the plan SQLite reported for it.
    ///
    /// This is the find itself: the same request, judged, compiled, paged and
    /// hydrated on this snapshot, its cursor `params.after` judged as the find
    /// judges it, refused where the find is refused, and each statement
    /// counted as it runs. The plans are then taken, on this snapshot's
    /// read-only connection, of the very text and values each statement ran
    /// with. A statement the find did not run is not listed: a section a page
    /// filled before reaching, a hydration of a page with no rows, and a
    /// collection's total where no head filled its ceiling. A statement that
    /// ran twice is listed twice. An explain is a report about a statement
    /// rather than a run of it, so it is not counted.
    pub fn find_plans(
        &self,
        params: &FindParams,
        declared: &ContentModel,
    ) -> Result<Vec<FindPlan>, PageRefusal> {
        let mut lookups = Lookups::default();
        self.run_find(params, declared, &mut lookups)?;
        Ok(self.explained(lookups.ran, |statement, filters, plan| {
            let ReadStatement::Find(statement) = statement else {
                unreachable!("a find runs only the statements find names")
            };
            FindPlan {
                statement,
                filters,
                plan,
            }
        })?)
    }

    /// The find [`Snapshot::find`] answers and [`Snapshot::find_plans`]
    /// explains, recording every statement it runs in `lookups`.
    fn run_find(
        &self,
        params: &FindParams,
        declared: &ContentModel,
        lookups: &mut Lookups,
    ) -> Result<Found, PageRefusal> {
        let started = self.counters().statements_executed();
        let limit = page_limit(params.limit)?;
        let projection = Projection::of(&params.columns)?;
        let mut compiled = self.compile(params, declared, lookups)?;
        let (resume, moved) = match &params.after {
            None => (None, Vec::new()),
            Some(cursor) => self.judge(cursor, compiled.order, lookups)?,
        };
        let fields = self.projected_keys(&projection, declared, lookups, &mut compiled.reports)?;

        let mut work = FindWork::default();
        let (keys, next) = self.page_keys(&compiled, limit, resume.as_ref(), lookups, &mut work)?;
        let order = compiled.field_order();
        let snapshot = self.reading_facts(order, lookups)?;
        let next =
            next.map(|at| Cursor::new(snapshot.clone(), CursorKey::document(at.sort, at.path)));
        let unsatisfied = self.resolve(compiled.reports, declared, lookups)?;
        let rows = self.hydrate_rows(&keys, &projection, &fields, declared, lookups, &mut work)?;
        work.statements = self.counters().statements_executed() - started;
        Ok(Found {
            rows,
            next,
            moved,
            unsatisfied,
            snapshot,
            work,
        })
    }

    /// Judge the cursor a request continues against the request's `order` on
    /// this snapshot: where it resumes, and what moved since.
    ///
    /// **A cursor is refused wherever it is detectably not a position in
    /// `order`.** Its fingerprint is the order's: the active fingerprint for a
    /// typed order, and none for a raw or a path order — so a raw cursor
    /// continued in a typed order, a typed one continued in a raw or a path
    /// order, and a typed one minted under a fingerprint the snapshot no longer
    /// reads are refused. A path order's cursor carries no sort value, so one
    /// carrying a value is refused too. A field order's cursor carrying no sort
    /// value stands in its missing section.
    ///
    /// **A cursor names neither its sort key nor its direction**, so two gaps
    /// are answered from a position in an order the cursor was not minted in.
    /// The key: a cursor minted in one key's raw order and continued in
    /// another key's, or a path order's cursor continued in a raw field order,
    /// where it reads as a position in the missing section. The direction: a
    /// cursor of any order continued in the same order reversed, where an
    /// ascending cursor replayed descending resumes from its position read the
    /// other way. Both gaps are the wire's, and NORN-244 closes them by naming
    /// the order's key and direction in the cursor.
    fn judge(
        &self,
        cursor: &Cursor,
        order: PageOrder<'_>,
        lookups: &mut Lookups,
    ) -> Result<(Option<FindPosition>, Vec<Moved>), PageRefusal> {
        let CursorKey::Document { sort, path, .. } = cursor.key() else {
            return Err(PageRefusal::NotADocumentCursor);
        };
        let path_ordered = matches!(order, PageOrder::Path(_));
        let moved = self.judge_reading(
            cursor,
            order.field_order(),
            path_ordered && sort.is_some(),
            lookups,
        )?;
        Ok((
            Some(FindPosition {
                sort: sort.clone(),
                path: path.clone(),
            }),
            moved,
        ))
    }

    /// The keys `projection` names that are known, in its order; each unknown
    /// one is reported at the end of `reports`.
    pub(crate) fn projected_keys<'p>(
        &self,
        projection: &Projection<'p>,
        declared: &ContentModel,
        lookups: &mut Lookups,
        reports: &mut Vec<Report>,
    ) -> Result<Vec<&'p str>, StoreError> {
        let mut known = Vec::new();
        for key in projection.keys.iter().copied() {
            if self.is_known(key, declared, lookups)? {
                known.push(key);
            } else {
                reports.push(Report::Unknown(KeyPlace::Projection, key.to_string()));
            }
        }
        Ok(known)
    }

    /// The rows of the documents `keys` name, in their order, carrying the
    /// columns `projection` names, `fields` being the keys of it that are
    /// known.
    ///
    /// Every read that answers document rows hydrates them here, so a row
    /// carries the same columns at the same cost whichever verb paged it. The
    /// findings column reads under the active fingerprint, which is read only
    /// where the projection names that column, and the links column resolves
    /// under `declared`'s ambiguity-ignore set.
    pub(crate) fn hydrate_rows(
        &self,
        keys: &[FoundKey],
        projection: &Projection<'_>,
        fields: &[&str],
        declared: &ContentModel,
        lookups: &mut Lookups,
        work: &mut FindWork,
    ) -> Result<Vec<DocumentRow>, StoreError> {
        // A finding recorded under no schema is stamped with the empty
        // fingerprint.
        let findings_under = if projection.findings {
            Some(self.fingerprint(lookups)?.unwrap_or_default())
        } else {
            None
        };
        self.hydrate(
            keys,
            projection,
            fields,
            findings_under.as_deref(),
            declared.ambiguity_ignore(),
            work,
            &mut lookups.ran,
        )
    }

    /// The request's order and its conjunction, compiled under `declared`,
    /// which is refused where it was read from another schema than the
    /// snapshot pins.
    ///
    /// A field sort's order is the declaration's for its key: typed where the
    /// key is declared with a typed order, raw otherwise.
    fn compile<'a>(
        &self,
        params: &'a FindParams,
        declared: &ContentModel,
        lookups: &mut Lookups,
    ) -> Result<Compiled<'a>, PageRefusal> {
        self.declaration_pinned(declared, lookups)?;
        let mut reports = Vec::new();
        let order = match &params.sort {
            None => PageOrder::Path(PageDirection::Ascending),
            Some(sort) => {
                let direction = match sort.direction {
                    Direction::Ascending => PageDirection::Ascending,
                    Direction::Descending => PageDirection::Descending,
                    _ => {
                        return Err(PageRefusal::UnknownPart {
                            part: "a sort direction",
                        });
                    }
                };
                match &sort.key {
                    SortKey::Path {} => PageOrder::Path(direction),
                    SortKey::Field { key, .. } if !self.is_known(key, declared, lookups)? => {
                        reports.push(Report::Unknown(KeyPlace::Sort, key.clone()));
                        PageOrder::Path(PageDirection::Ascending)
                    }
                    SortKey::Field { key, .. } => PageOrder::Field {
                        key,
                        order: match declared.typed_order(key) {
                            Some(_) => FieldOrder::Typed,
                            None => FieldOrder::Raw,
                        },
                        direction,
                    },
                    _ => return Err(PageRefusal::UnknownPart { part: "a sort key" }),
                }
            }
        };
        let conjunction = self.compile_conjunction(
            &params.predicates,
            ResolvesPart::Answered,
            declared,
            lookups,
        )?;
        reports.extend(conjunction.reports);
        Ok(Compiled {
            order,
            filters: conjunction.filters,
            reports,
            matches_nothing: conjunction.matches_nothing,
        })
    }

    /// One page of keys: at most `limit`, and where the next page starts.
    ///
    /// `work` takes what the section statements cost: the keys they handed
    /// back — one past the bound where a next page exists — and what SQLite
    /// counted stepping them.
    fn page_keys(
        &self,
        compiled: &Compiled<'_>,
        limit: usize,
        at: Option<&FindPosition>,
        lookups: &mut Lookups,
        work: &mut FindWork,
    ) -> Result<(Vec<FoundKey>, Option<FindPosition>), StoreError> {
        let sections = if compiled.matches_nothing {
            Vec::new()
        } else {
            sections(compiled.order, at)
        };
        let page = self.read_page(
            sections,
            limit,
            &mut lookups.ran,
            |record, (statement, start), rows| {
                let composed = compose_page(&Section {
                    statement,
                    key: field_key(compiled.order),
                    start,
                    filters: &compiled.filters,
                    rows,
                });
                let section = Ran::new(statement, composed).narrowed_by(compiled.filter_shapes());
                self.read_keys(record, section)
            },
        )?;
        work.keys_read = page.read;
        work.page_stepped(page.stepped);
        Ok((page.rows, page.next.as_ref().map(FoundKey::position)))
    }

    /// Run one page section.
    fn read_keys(&self, record: &mut Vec<Ran>, section: Ran) -> Result<Vec<FoundKey>, StoreError> {
        self.run_statement(record, section, |row| {
            Ok(FoundKey {
                document: row.get(0)?,
                path: row.get(1)?,
                sort: row.get(2)?,
            })
        })
        .map_err(|problem| error::sql("reading a page of found documents", problem))
    }
}

/// The key a field order sorts by.
fn field_key<'a>(order: PageOrder<'a>) -> Option<&'a str> {
    match order {
        PageOrder::Path(_) => None,
        PageOrder::Field { key, .. } => Some(key),
    }
}

/// The sections a page reads from `at` on, in order, each with where it starts.
///
/// A position whose sort is `None` stands in a field order's missing section;
/// one carrying a sort stands in its valued section. A section after the one
/// the position stands in starts at its first row.
fn sections<'a>(
    order: PageOrder<'_>,
    at: Option<&'a FindPosition>,
) -> Vec<(FindStatement, SectionStart<'a>)> {
    let resumed = |at: &'a FindPosition| SectionStart {
        sort: at.sort.as_deref(),
        path: Some(at.path.as_str()),
    };
    match order {
        PageOrder::Path(direction) => vec![(
            FindStatement::PathPage(direction),
            at.map(resumed).unwrap_or_default(),
        )],
        PageOrder::Field {
            order, direction, ..
        } => {
            let valued = FindStatement::FieldValuePage(order, direction);
            let missing = FindStatement::FieldMissingPage(order, direction);
            let start = SectionStart::default();
            let in_missing = at.is_some_and(|at| at.sort.is_none());
            match (direction, at) {
                (PageDirection::Ascending, None) => vec![(missing, start), (valued, start)],
                (PageDirection::Ascending, Some(at)) if in_missing => {
                    vec![(missing, resumed(at)), (valued, start)]
                }
                (PageDirection::Ascending, Some(at)) => vec![(valued, resumed(at))],
                (PageDirection::Descending, None) => vec![(valued, start), (missing, start)],
                (PageDirection::Descending, Some(at)) if in_missing => {
                    vec![(missing, resumed(at))]
                }
                (PageDirection::Descending, Some(at)) => {
                    vec![(valued, resumed(at)), (missing, start)]
                }
            }
        }
    }
}

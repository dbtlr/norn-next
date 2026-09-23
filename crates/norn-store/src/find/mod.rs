//! The find builder: a request's conjunction, order and page bound compiled
//! into statements a read snapshot runs.
//!
//! The builder is inherent methods on [`Snapshot`], so every statement it runs
//! reads the one instant the snapshot was established at and is counted on the
//! snapshot's own statement counter. It answers a page of document keys — the
//! row id, the path and the value the page is ordered by — in the order the
//! request states, and where the next page starts.
//!
//! # A page is sections, each one index seek
//!
//! A path order is one section: `documents_path_nocase`, the path folded by
//! ASCII case with the bytewise path as the tie-break, so the order is total.
//!
//! A field order is two. The **valued** section reads the key's marker rows —
//! each document's least value under the order, one row per document — in
//! `(value, path)` order on the order's marker index, so a document whose field
//! holds a set appears once, at its least value. The **missing** section reads
//! the documents holding no value under the order — the key absent, every value
//! null, or, under the typed order, no value that reads as the declared type —
//! in bytewise path order. The missing section stands before the valued one
//! ascending and after it descending, which is where the wire says a document
//! missing the sort field goes. A page reads section after section until it
//! holds one row more than its bound, and the extra row is what says a next
//! page exists.
//!
//! **Which order a field sort uses** is the typed one where the declaration
//! gives the key a typed order and the raw text's otherwise, on a first page. A
//! continuation runs in the order it was started in, which it names in
//! [`Resume::order`]: a page begun in raw order goes on in raw order after a
//! re-pin that gave the key a type, because the raw order has not moved.
//!
//! **A part that compares values compares under the key's order.** Equality,
//! inequality, membership and a `before`/`after` bound on a key the declaration
//! gives a typed order compare the typed sort key of each value the request
//! names against the typed column, so `9` and `9.0` under a number are one
//! value to an equality part exactly as they are to the declared type's
//! comparison; a value that does not read as the type names no place in the
//! order and is refused. A key with no typed order compares raw text.
//!
//! # A filter is a membership test one seek answers
//!
//! Each part of the conjunction narrows every section by the rows one index
//! seek of its own reaches — [`FindFilter`] names each and the index it
//! seeks — so a part costs the rows it matches, never the vault. A part that
//! can match nothing by construction is reported in [`KeyPage::unsatisfied`]
//! and the page is empty: a conjunction holding it matches no document, and
//! the report is what keeps that from reading as a vault with nothing in it.
//!
//! # A part that cannot be applied is reported, never dropped in silence
//!
//! **The report is a biconditional**: a part is reported exactly where it
//! cannot be applied as asked, and a part that can be is never reported, even
//! where it matches nothing.
//!
//! - **A key outside the field universe** — the keys the declaration names
//!   and the keys some document carries — is reported with the keys near it
//!   ([`suggest`] states the one rule). An unknown sort key orders the page by
//!   path, ascending; an unknown projected key carries nothing under it; an
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
//!
//! A `resolves` part enumerates the target's ambiguity class through its suffix
//! probe, both reductions of a dotted leaf included, and compares suffix keys
//! bytewise. The folded suffix key and per-root case sensitivity land with
//! NORN-229, which is the task that consumes this filter's case behaviour.
//!
//! # A find is keys, then rows
//!
//! [`Snapshot::find`] is the whole request: it judges the wire cursor the
//! request continues, pages the keys, hydrates the rows the page found —
//! exactly those, never the one past the bound that says a next page exists —
//! and mints the cursor the next page continues. [`Snapshot::find_keys`] is
//! the paging half alone, over a [`Resume`] rather than a cursor. The rows
//! carry only the columns the request projected ([`hydrate`] states what each
//! costs), and [`Found::work`] says what the find read.
//!
//! **A cursor names the order it was minted in.** A page ordered by a typed
//! field is minted under the active schema fingerprint and one ordered any
//! other way under none, so a continuation runs in the typed order while that
//! fingerprint stands, is refused with the wire's
//! [`norn_wire::CursorOrderChanged`] once it does not, and a raw-order
//! continuation survives any re-pin. A store with no pinned schema reads the
//! empty fingerprint, the one a finding recorded under no schema is stamped
//! with.

mod glob;
mod hydrate;
mod statement;
mod suggest;

use std::collections::{BTreeMap, BTreeSet};

use norn_db::EmittedPlan;
use norn_db::rusqlite::params_from_iter;
use norn_db::rusqlite::types::Value;
use norn_wire::{
    Column, Cursor, CursorKey, CursorOrderChanged, Direction, DocumentRow, FindParams, FindReport,
    Moved, Page, Pattern, Predicate, SortKey, Unsatisfied,
};

use crate::ddl;
use crate::error::{self, StoreError};
use crate::fields::DeclaredFields;
use crate::json::{FrontmatterValue, canonical_json};
use crate::path::{DirectoryPrefix, DocumentPath, suffix_probe};
use crate::request::MAX_PAGE;
use crate::store::Snapshot;

pub(crate) use glob::register_functions;
pub use hydrate::{BODY_ROW_CEILING, FindWork, NESTED_ROW_CEILING, NestedRows};
pub use statement::{
    FIND_FILTERS, FIND_STATEMENTS, FindFilter, FindStatement, Nested, PageDirection,
};
use statement::{
    Filter, Section, SectionStart, compose_bare_directory, compose_documents, compose_known_key,
    compose_nested_head, compose_nested_total, compose_page, compose_universe,
};

/// How many rows a page holds when a request names no bound.
///
/// The store's default. A handler may narrow it by naming a bound of its own;
/// any bound is held to `1..=`[`MAX_PAGE`].
pub const DEFAULT_PAGE: usize = 100;

/// Which of a field's two orders a sort or a bound compares under.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FieldOrder {
    /// The canonical text's bytewise order.
    Raw,
    /// The declared type's order, over the typed sort key the schema pinned.
    Typed,
}

impl FieldOrder {
    /// The column this order compares.
    fn column(self) -> &'static str {
        match self {
            FieldOrder::Raw => "raw",
            FieldOrder::Typed => "typed",
        }
    }

    /// The column marking a document's least value under this order.
    fn marker(self) -> &'static str {
        match self {
            FieldOrder::Raw => "least_raw",
            FieldOrder::Typed => "least_typed",
        }
    }
}

/// Where a page stopped: the value the last row was ordered by, and its path.
///
/// `sort` is `None` for a row of a path order, and for a row of a field order's
/// missing section.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FindPosition {
    pub sort: Option<String>,
    pub path: String,
}

/// Where a continuation resumes, and the field order its first page ran in.
///
/// The order is read only by a field sort; a path order has one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Resume {
    pub order: FieldOrder,
    pub at: FindPosition,
}

/// One document a page found.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FoundKey {
    /// The document's row id, which is what a page's rows are hydrated by.
    document: i64,
    path: String,
    sort: Option<String>,
}

impl FoundKey {
    /// The document's path.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The value the page ordered the document by, where it was ordered by one.
    pub fn sort(&self) -> Option<&str> {
        self.sort.as_deref()
    }

    /// Where a page that stopped at this document resumes.
    pub fn position(&self) -> FindPosition {
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

/// One page of document keys, in the request's order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyPage {
    /// The documents, at most the page bound of them.
    pub keys: Vec<FoundKey>,
    /// Where the next page starts, or `None` where this page is the last.
    pub next: Option<FindPosition>,
    /// The field order a field sort ran in, and `None` for a path order.
    pub order: Option<FieldOrder>,
    /// The parts of the request that could not be applied as asked, in the
    /// order the request names them: the sort key, then the conjunction's
    /// parts. A page whose conjunction holds a part no document can satisfy is
    /// empty; an unknown key's part is reported and the page is answered
    /// without it.
    pub unsatisfied: Vec<Unsatisfied>,
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

/// A statement a request would run, with the plan SQLite reported for it.
#[derive(Clone, Debug)]
pub struct FindPlan {
    pub statement: FindStatement,
    /// The filters the statement narrows by, in the request's order.
    pub filters: Vec<FindFilter>,
    pub plan: EmittedPlan,
}

/// Why the builder answered no page.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum FindRefusal {
    /// The request filters by a fact the store keeps no index of.
    ///
    /// A **dormant carrier** for the link index NORN-229 builds: `links` stores
    /// a link's target raw and unindexed, so no seek answers a `links_to` part,
    /// and nothing reaches the part's filter until that index stands. Until
    /// then no request naming one can be answered, and saying so is the answer.
    NotIndexed { fact: &'static str },
    /// The request names a row column a find does not project yet.
    ///
    /// A **dormant carrier** for the resolved link and finding columns NORN-229
    /// builds: the store holds a document's link and finding rows, but a find's
    /// row carries neither column until the link index resolves what a link
    /// names, so no row composition reads them yet.
    NotProjected { column: &'static str },
    /// A value a comparing part names — an equality, an inequality, a
    /// membership or a `before`/`after` bound — on a key declared with a typed
    /// order does not read as that type, so it names no place in the key's
    /// order.
    UnreadableBound { key: String, value: String },
    /// The cursor was minted under a schema fingerprint the snapshot no longer
    /// reads, so the order its position is a position in no longer exists.
    OrderChanged(CursorOrderChanged),
    /// The cursor names a position among rows that are not documents.
    NotADocumentCursor,
    /// The request carries a part this build of the store does not know.
    UnknownPart { part: &'static str },
    /// The store refused a statement.
    Store(StoreError),
}

impl std::fmt::Display for FindRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FindRefusal::NotIndexed { fact } => {
                write!(formatter, "the store keeps no index of {fact}")
            }
            FindRefusal::NotProjected { column } => {
                write!(formatter, "{column} is not yet projected onto a find's row")
            }
            FindRefusal::UnreadableBound { key, value } => write!(
                formatter,
                "`{value}` does not read as the type `{key}` is declared with"
            ),
            FindRefusal::OrderChanged(changed) => write!(
                formatter,
                "the cursor was minted under the schema `{}`, and the order it continues no \
                 longer stands",
                changed.minted_under
            ),
            FindRefusal::NotADocumentCursor => {
                formatter.write_str("the cursor names a position among rows that are not documents")
            }
            FindRefusal::UnknownPart { part } => {
                write!(formatter, "this store does not know {part}")
            }
            FindRefusal::Store(problem) => problem.fmt(formatter),
        }
    }
}

impl std::error::Error for FindRefusal {}

impl From<StoreError> for FindRefusal {
    fn from(problem: StoreError) -> Self {
        FindRefusal::Store(problem)
    }
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

/// Where a request named a key.
#[derive(Clone, Copy, Debug)]
enum KeyPlace {
    Sort,
    Projection,
    Predicate,
}

/// One part of a request that could not be applied as asked, before the
/// field universe an unknown key's suggestions are drawn from is read.
enum Report {
    /// A part reported as it stands.
    Part(Unsatisfied),
    /// A key outside the field universe, named at `KeyPlace`.
    Unknown(KeyPlace, String),
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
    fn filter_shapes(&self) -> Vec<FindFilter> {
        self.filters.iter().map(|filter| filter.shape).collect()
    }

    /// The field order a field sort runs in, and `None` for a path order.
    fn field_order(&self) -> Option<FieldOrder> {
        match self.order {
            PageOrder::Path(_) => None,
            PageOrder::Field { order, .. } => Some(order),
        }
    }
}

/// A compiled part of the conjunction.
enum Part {
    Filter(Filter),
    MatchesNothing(Unsatisfied),
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
}

impl<'a> Projection<'a> {
    /// What `columns` projects, or the refusal of a column the store keeps no
    /// index of.
    fn of(columns: &'a [Column]) -> Result<Self, FindRefusal> {
        let mut projection = Projection {
            all_fields: false,
            keys: Vec::new(),
            body: false,
            nested: Vec::new(),
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
                    return Err(FindRefusal::NotProjected {
                        column: "the links column",
                    });
                }
                Column::Findings {} => {
                    return Err(FindRefusal::NotProjected {
                        column: "the findings column",
                    });
                }
                _ => return Err(FindRefusal::UnknownPart { part: "a column" }),
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

/// A statement one request's compile ran on the snapshot, kept so
/// [`Snapshot::find_plans`] explains the statement that ran rather than a
/// second spelling of it.
struct Probe {
    statement: FindStatement,
    sql: String,
    values: Vec<Value>,
}

/// What one request asked its snapshot while it was compiled, each question
/// asked once: the active fingerprint, which keys are known, and every probe
/// statement that ran.
#[derive(Default)]
struct Lookups {
    fingerprint: Option<String>,
    known: BTreeMap<String, bool>,
    probes: Vec<Probe>,
}

impl Snapshot {
    /// The pinned vault-schema fingerprint, or `None` where no schema is
    /// pinned. One point read on this snapshot, counted on it.
    pub fn active_fingerprint(&mut self) -> Result<Option<String>, StoreError> {
        self.count_statement();
        Ok(norn_db::meta::get_meta(
            self.connection(),
            ddl::meta::VAULT_SCHEMA_FINGERPRINT,
        )?)
    }

    /// One page of the documents `params` asks for, as rows carrying the
    /// columns it projects, in its order, continuing its cursor.
    ///
    /// `declared` is the vault's declaration: it decides a first page's field
    /// order, how a bound is compared, and — beside the keys documents carry —
    /// which keys are known. The page holds `params.limit` rows,
    /// [`DEFAULT_PAGE`] where it names none, held to `1..=`[`MAX_PAGE`], and
    /// only those rows are hydrated.
    ///
    /// Refused: a cursor among rows that are not documents; a cursor minted in
    /// a typed order under a fingerprint the snapshot no longer reads; a
    /// projected column or a part the store keeps no index of; and a bound
    /// that does not read as its key's declared type.
    pub fn find(
        &mut self,
        params: &FindParams,
        declared: &DeclaredFields,
    ) -> Result<Found, FindRefusal> {
        let started = self.counters().statements_executed();
        let projection = Projection::of(&params.columns)?;
        let mut lookups = Lookups::default();
        let (resume, moved) = match &params.after {
            None => (None, Vec::new()),
            Some(cursor) => self.judge(cursor, &mut lookups)?,
        };
        let mut compiled = self.compile(params, declared, resume.as_ref(), &mut lookups)?;
        let fields = self.projected_keys(&projection, declared, &mut lookups, &mut compiled)?;

        let mut work = FindWork::default();
        let (keys, next, read) =
            self.page_keys(&compiled, page_limit(params.limit), resume.as_ref())?;
        work.keys_read = read;
        let order = compiled.field_order();
        let snapshot = self.reading_facts(order, &mut lookups)?;
        let next =
            next.map(|at| Cursor::new(snapshot.clone(), CursorKey::document(at.sort, at.path)));
        let unsatisfied = self.resolve(compiled.reports, declared, &mut lookups)?;
        let rows = self.hydrate(&keys, &projection, &fields, &mut work)?;
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

    /// One page of the documents `params` asks for, as keys, in its order.
    ///
    /// `params.after` and `params.columns` are not read here: a caller judges
    /// the wire cursor and hands the position it names over as `resume`, and
    /// hydrates the rows itself. `declared` is the vault's declaration, which
    /// decides a first page's field order and how a bound is compared, and
    /// names known keys. The page holds `params.limit` rows, [`DEFAULT_PAGE`]
    /// where it names none, held to `1..=`[`MAX_PAGE`].
    pub fn find_keys(
        &mut self,
        params: &FindParams,
        declared: &DeclaredFields,
        resume: Option<&Resume>,
    ) -> Result<KeyPage, FindRefusal> {
        let mut lookups = Lookups::default();
        let compiled = self.compile(params, declared, resume, &mut lookups)?;
        let (keys, next, _) = self.page_keys(&compiled, page_limit(params.limit), resume)?;
        let order = compiled.field_order();
        let unsatisfied = self.resolve(compiled.reports, declared, &mut lookups)?;
        Ok(KeyPage {
            keys,
            next,
            order,
            unsatisfied,
        })
    }

    /// Every statement [`Snapshot::find`] would run for the same request,
    /// continuing from `resume`, each with the plan SQLite reported for it.
    ///
    /// The statements are composed by the same calls the find runs, over the
    /// same compiled request, and explained on this snapshot's read-only
    /// connection. The probes a compile asks — the fingerprint, whether a key
    /// is known, the field universe, whether a path is a bare directory — run
    /// here as they run there, and are counted; each is listed once per time
    /// it ran. A page statement is listed wherever the page could reach it from
    /// `resume`, whether or not the rows would have filled the page before it;
    /// a hydration statement is listed for each column the request projects,
    /// explained with no ids bound, since the ids are the page's and the plan
    /// does not read them. An explain is a report about a statement rather
    /// than a run of it, so it is not counted.
    pub fn find_plans(
        &mut self,
        params: &FindParams,
        declared: &DeclaredFields,
        resume: Option<&Resume>,
    ) -> Result<Vec<FindPlan>, FindRefusal> {
        let projection = Projection::of(&params.columns)?;
        let mut lookups = Lookups::default();
        if resume.is_some_and(|resume| resume.order == FieldOrder::Typed) {
            self.fingerprint(&mut lookups)?;
        }
        let mut compiled = self.compile(params, declared, resume, &mut lookups)?;
        let fields = self.projected_keys(&projection, declared, &mut lookups, &mut compiled)?;
        self.reading_facts(compiled.field_order(), &mut lookups)?;
        let limit = page_limit(params.limit);
        let matches_nothing = compiled.matches_nothing;
        let filters = compiled.filter_shapes();
        let mut composed: Vec<(FindStatement, Vec<FindFilter>, String, Vec<Value>)> = Vec::new();
        if !matches_nothing {
            for (statement, start) in sections(compiled.order, resume.map(|resume| &resume.at)) {
                let (sql, values) = compose_page(&Section {
                    statement,
                    key: field_key(compiled.order),
                    start,
                    filters: &compiled.filters,
                    rows: limit + 1,
                });
                composed.push((statement, filters.clone(), sql, values));
            }
        }
        self.resolve(compiled.reports, declared, &mut lookups)?;
        if !matches_nothing {
            let columns = statement::DocumentColumns {
                frontmatter: projection.all_fields || !fields.is_empty(),
                body: projection.body,
            };
            if columns != statement::DocumentColumns::default() {
                let (sql, values) = compose_documents(&[], columns, BODY_ROW_CEILING);
                composed.push((FindStatement::HydrateDocuments, Vec::new(), sql, values));
            }
            for nested in projection.nested.iter().copied() {
                let (sql, values) = compose_nested_head(nested, &[], NESTED_ROW_CEILING);
                composed.push((FindStatement::NestedHead(nested), Vec::new(), sql, values));
                let (sql, values) = compose_nested_total(nested, &[]);
                composed.push((FindStatement::NestedTotal(nested), Vec::new(), sql, values));
            }
        }

        let explain = |snapshot: &Snapshot, sql: &str, values: Vec<Value>| {
            norn_db::emitted_plan(snapshot.connection(), sql, params_from_iter(values))
                .map_err(StoreError::from)
        };
        let mut plans = Vec::new();
        for probe in lookups.probes {
            plans.push(FindPlan {
                statement: probe.statement,
                filters: Vec::new(),
                plan: explain(self, &probe.sql, probe.values)?,
            });
        }
        for (statement, filters, sql, values) in composed {
            plans.push(FindPlan {
                statement,
                filters,
                plan: explain(self, &sql, values)?,
            });
        }
        Ok(plans)
    }

    /// Judge the cursor a request continues against this snapshot: where it
    /// resumes, in the order it was minted in, and what moved since.
    ///
    /// The fingerprint is read only for a cursor minted in a typed order,
    /// which is the one kind of cursor a fingerprint decides anything about.
    fn judge(
        &mut self,
        cursor: &Cursor,
        lookups: &mut Lookups,
    ) -> Result<(Option<Resume>, Vec<Moved>), FindRefusal> {
        let CursorKey::Document { sort, path, .. } = cursor.key() else {
            return Err(FindRefusal::NotADocumentCursor);
        };
        let typed = cursor.snapshot().schema_fingerprint.is_some();
        let order = if typed {
            FieldOrder::Typed
        } else {
            FieldOrder::Raw
        };
        let now = self.reading_facts(typed.then_some(order), lookups)?;
        let moved = cursor
            .continuation(&now)
            .map_err(FindRefusal::OrderChanged)?;
        Ok((
            Some(Resume {
                order,
                at: FindPosition {
                    sort: sort.clone(),
                    path: path.clone(),
                },
            }),
            moved,
        ))
    }

    /// This snapshot's reading as a cursor carries it for a page in `order`:
    /// the epoch and the write generation, and the active fingerprint where
    /// the order is typed.
    fn reading_facts(
        &mut self,
        order: Option<FieldOrder>,
        lookups: &mut Lookups,
    ) -> Result<norn_wire::Snapshot, StoreError> {
        let fingerprint = match order {
            Some(FieldOrder::Typed) => Some(self.fingerprint(lookups)?),
            Some(FieldOrder::Raw) | None => None,
        };
        let generation =
            u64::try_from(self.reading().write_generation()).map_err(|_| StoreError::Damaged {
                what: "the store's write generation is below zero".to_string(),
            })?;
        Ok(norn_wire::Snapshot::new(
            self.epoch(),
            generation,
            fingerprint,
            None,
        ))
    }

    /// The active fingerprint, read once per request; the empty fingerprint
    /// where no schema is pinned.
    fn fingerprint(&mut self, lookups: &mut Lookups) -> Result<String, StoreError> {
        if let Some(fingerprint) = &lookups.fingerprint {
            return Ok(fingerprint.clone());
        }
        let fingerprint = self.active_fingerprint()?.unwrap_or_default();
        lookups.probes.push(Probe {
            statement: FindStatement::ActiveFingerprint,
            sql: norn_db::meta::META_READ_SQL.to_string(),
            values: vec![Value::Text(ddl::meta::VAULT_SCHEMA_FINGERPRINT.to_string())],
        });
        lookups.fingerprint = Some(fingerprint.clone());
        Ok(fingerprint)
    }

    /// Whether `key` is in the field universe: declared, or carried by some
    /// document. A declared key asks the snapshot nothing; any other is one
    /// existence seek, asked once per request.
    fn is_known(
        &mut self,
        key: &str,
        declared: &DeclaredFields,
        lookups: &mut Lookups,
    ) -> Result<bool, StoreError> {
        if declared.is_declared(key) {
            return Ok(true);
        }
        if let Some(known) = lookups.known.get(key) {
            return Ok(*known);
        }
        let (sql, values) = compose_known_key(key);
        let known = self.ask(&sql, &values, "asking whether a document carries a key")?;
        lookups.probes.push(Probe {
            statement: FindStatement::KnownKey,
            sql,
            values,
        });
        lookups.known.insert(key.to_string(), known);
        Ok(known)
    }

    /// Run one yes-or-no probe, counted on this snapshot.
    fn ask(
        &mut self,
        sql: &str,
        values: &[Value],
        operation: &'static str,
    ) -> Result<bool, StoreError> {
        self.count_statement();
        self.connection()
            .query_row(sql, params_from_iter(values.iter().cloned()), |row| {
                row.get(0)
            })
            .map_err(|problem| error::sql(operation, problem))
    }

    /// The field universe: every declared key, and every key a document
    /// carries.
    fn field_universe(
        &mut self,
        declared: &DeclaredFields,
        lookups: &mut Lookups,
    ) -> Result<BTreeSet<String>, StoreError> {
        const OPERATION: &str = "reading the keys the vault's documents carry";
        let (sql, values) = compose_universe();
        self.count_statement();
        let carried: Vec<String> = {
            let mut statement = self
                .connection()
                .prepare(&sql)
                .map_err(|problem| error::sql(OPERATION, problem))?;
            let rows = statement
                .query_map(params_from_iter(values.iter().cloned()), |row| row.get(0))
                .map_err(|problem| error::sql(OPERATION, problem))?;
            rows.collect::<Result<Vec<String>, _>>()
                .map_err(|problem| error::sql(OPERATION, problem))?
        };
        lookups.probes.push(Probe {
            statement: FindStatement::FieldUniverse,
            sql,
            values,
        });
        Ok(carried
            .into_iter()
            .chain(declared.keys().map(str::to_string))
            .collect())
    }

    /// The reports a request's compile made, with an unknown key's
    /// suggestions drawn from the field universe — which is read once, and
    /// only where some key is unknown.
    fn resolve(
        &mut self,
        reports: Vec<Report>,
        declared: &DeclaredFields,
        lookups: &mut Lookups,
    ) -> Result<Vec<Unsatisfied>, StoreError> {
        let universe = if reports
            .iter()
            .any(|report| matches!(report, Report::Unknown(..)))
        {
            self.field_universe(declared, lookups)?
        } else {
            BTreeSet::new()
        };
        Ok(reports
            .into_iter()
            .map(|report| match report {
                Report::Part(part) => part,
                Report::Unknown(place, key) => {
                    let near = suggest::did_you_mean(&key, universe.iter().map(String::as_str));
                    match place {
                        KeyPlace::Sort => Unsatisfied::unknown_sort_key(key, near),
                        KeyPlace::Projection => Unsatisfied::unknown_projection_key(key, near),
                        KeyPlace::Predicate => Unsatisfied::unknown_predicate_key(key, near),
                    }
                }
            })
            .collect())
    }

    /// The keys `projection` names that are known, in its order; each unknown
    /// one is reported on `compiled`.
    fn projected_keys<'p>(
        &mut self,
        projection: &Projection<'p>,
        declared: &DeclaredFields,
        lookups: &mut Lookups,
        compiled: &mut Compiled<'_>,
    ) -> Result<Vec<&'p str>, StoreError> {
        let mut known = Vec::new();
        for key in projection.keys.iter().copied() {
            if self.is_known(key, declared, lookups)? {
                known.push(key);
            } else {
                compiled
                    .reports
                    .push(Report::Unknown(KeyPlace::Projection, key.to_string()));
            }
        }
        Ok(known)
    }

    /// The request's order and its conjunction, compiled.
    fn compile<'a>(
        &mut self,
        params: &'a FindParams,
        declared: &DeclaredFields,
        resume: Option<&Resume>,
        lookups: &mut Lookups,
    ) -> Result<Compiled<'a>, FindRefusal> {
        let mut reports = Vec::new();
        let order = match &params.sort {
            None => PageOrder::Path(PageDirection::Ascending),
            Some(sort) => {
                let direction = match sort.direction {
                    Direction::Ascending => PageDirection::Ascending,
                    Direction::Descending => PageDirection::Descending,
                    _ => {
                        return Err(FindRefusal::UnknownPart {
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
                        order: resume.map_or_else(
                            || match declared.typed_order(key) {
                                Some(_) => FieldOrder::Typed,
                                None => FieldOrder::Raw,
                            },
                            |resume| resume.order,
                        ),
                        direction,
                    },
                    _ => return Err(FindRefusal::UnknownPart { part: "a sort key" }),
                }
            }
        };

        let mut filters = Vec::new();
        let mut matches_nothing = false;
        for predicate in &params.predicates {
            if let Some(key) = predicate_key(predicate)
                && !self.is_known(key, declared, lookups)?
            {
                reports.push(Report::Unknown(KeyPlace::Predicate, key.to_string()));
                continue;
            }
            match self.compile_predicate(predicate, declared, lookups)? {
                Part::Filter(filter) => filters.push(filter),
                Part::MatchesNothing(part) => {
                    matches_nothing = true;
                    reports.push(Report::Part(part));
                }
            }
        }
        Ok(Compiled {
            order,
            filters,
            reports,
            matches_nothing,
        })
    }

    /// One page of keys: at most `limit`, where the next page starts, and how
    /// many keys the section statements handed back — one past the bound where
    /// a next page exists.
    fn page_keys(
        &mut self,
        compiled: &Compiled<'_>,
        limit: usize,
        resume: Option<&Resume>,
    ) -> Result<(Vec<FoundKey>, Option<FindPosition>, u64), StoreError> {
        let mut keys: Vec<FoundKey> = Vec::new();
        if !compiled.matches_nothing {
            for (statement, start) in sections(compiled.order, resume.map(|resume| &resume.at)) {
                let rows = limit + 1 - keys.len();
                if rows == 0 {
                    break;
                }
                let (sql, values) = compose_page(&Section {
                    statement,
                    key: field_key(compiled.order),
                    start,
                    filters: &compiled.filters,
                    rows,
                });
                keys.extend(self.read_keys(&sql, values)?);
            }
        }
        let read = keys.len() as u64;
        let next = if keys.len() > limit {
            keys.truncate(limit);
            keys.last().map(FoundKey::position)
        } else {
            None
        };
        Ok((keys, next, read))
    }

    /// Run one page section, counted on this snapshot.
    fn read_keys(&mut self, sql: &str, values: Vec<Value>) -> Result<Vec<FoundKey>, StoreError> {
        const OPERATION: &str = "reading a page of found documents";
        self.count_statement();
        let mut statement = self
            .connection()
            .prepare(sql)
            .map_err(|problem| error::sql(OPERATION, problem))?;
        let rows = statement
            .query_map(params_from_iter(values), |row| {
                Ok(FoundKey {
                    document: row.get(0)?,
                    path: row.get(1)?,
                    sort: row.get(2)?,
                })
            })
            .map_err(|problem| error::sql(OPERATION, problem))?;
        rows.collect::<Result<Vec<FoundKey>, _>>()
            .map_err(|problem| error::sql(OPERATION, problem))
    }

    /// One part of the conjunction as the filter a statement spells, or the
    /// report that it matches nothing.
    ///
    /// Only a finding part reads the fingerprint, and only a path part with
    /// no wildcard probes whether it names a bare directory: the other parts
    /// bind nothing the snapshot has to be asked for.
    fn compile_predicate(
        &mut self,
        predicate: &Predicate,
        declared: &DeclaredFields,
        lookups: &mut Lookups,
    ) -> Result<Part, FindRefusal> {
        let text = |value: &str| Value::Text(value.to_string());
        let filter =
            |shape: FindFilter, values: Vec<Value>| Ok(Part::Filter(Filter { shape, values }));
        // The order a key's values compare under, and a request's value as a
        // place in it: its typed sort key where the key carries a typed order,
        // its text where it does not.
        let order = |key: &str| match declared.typed_order(key) {
            None => FieldOrder::Raw,
            Some(_) => FieldOrder::Typed,
        };
        let compared = |key: &String, value: &String| match declared.typed_order(key) {
            None => Ok(value.clone()),
            Some(typed) => typed
                .sort_key(value)
                .ok_or_else(|| FindRefusal::UnreadableBound {
                    key: key.clone(),
                    value: value.clone(),
                }),
        };
        match predicate {
            Predicate::Eq { key, value, .. } => filter(
                FindFilter::Equal(order(key)),
                vec![text(key), Value::Text(compared(key, value)?)],
            ),
            Predicate::NotEq { key, value, .. } => filter(
                FindFilter::NotEqual(order(key)),
                vec![text(key), Value::Text(compared(key, value)?)],
            ),
            Predicate::In { key, values, .. } => {
                let listed = canonical_json(&FrontmatterValue::Sequence(
                    values
                        .iter()
                        .map(|value| compared(key, value).map(FrontmatterValue::String))
                        .collect::<Result<Vec<FrontmatterValue>, FindRefusal>>()?,
                ))?;
                filter(
                    FindFilter::Member(order(key)),
                    vec![text(key), Value::Text(listed)],
                )
            }
            Predicate::Has { key, .. } => filter(FindFilter::Present, vec![text(key)]),
            Predicate::Missing { key, .. } => filter(FindFilter::Absent, vec![text(key)]),
            Predicate::Before { key, value, .. } | Predicate::After { key, value, .. } => {
                let (order, bound) = (order(key), compared(key, value)?);
                let shape = if matches!(predicate, Predicate::Before { .. }) {
                    FindFilter::Before(order)
                } else {
                    FindFilter::After(order)
                };
                filter(shape, vec![text(key), Value::Text(bound)])
            }
            Predicate::Matches { query, .. } => filter(FindFilter::FullText, vec![text(query)]),
            Predicate::Path { glob, .. } => match Pattern::parse(glob) {
                Err(problem) => Ok(Part::MatchesNothing(Unsatisfied::malformed_glob(
                    glob.clone(),
                    problem.to_string(),
                ))),
                Ok(pattern) => {
                    if let Some(part) = self.unmatchable_path(&pattern, lookups)? {
                        return Ok(Part::MatchesNothing(part));
                    }
                    let (lower, upper) = glob::path_range(&pattern);
                    filter(
                        FindFilter::PathGlob,
                        vec![Value::Text(lower), upper, text(glob)],
                    )
                }
            },
            Predicate::LinksTo { .. } => Err(FindRefusal::NotIndexed {
                fact: "a link's target",
            }),
            Predicate::Resolves { target, .. } => match suffix_probe(target.address()) {
                Err(_) => Ok(Part::MatchesNothing(Unsatisfied::impossible_path(
                    target.address(),
                ))),
                Ok(probe) => filter(
                    FindFilter::Resolves,
                    probe
                        .ranges()
                        .flat_map(|(lower, upper)| [text(lower), text(upper)])
                        .collect(),
                ),
            },
            Predicate::Tag { name, .. } => filter(FindFilter::Tag, vec![text(name)]),
            Predicate::HasFinding { kind, .. } => filter(
                FindFilter::Finding,
                vec![Value::Text(self.fingerprint(lookups)?), text(kind.as_str())],
            ),
            _ => Err(FindRefusal::UnknownPart {
                part: "a predicate",
            }),
        }
    }

    /// The report a parsed glob is answered with where it can match nothing by
    /// construction, or `None` where it can be applied.
    ///
    /// A glob with no wildcard is asked first whether it is a bare directory —
    /// no document at the path, and some beneath it — because that is the
    /// report that says what the request meant. Then any glob is impossible
    /// where, read with each wildcard as a letter, it is no document path the
    /// store accepts: every path the glob could match is spelled that way with
    /// other characters in the holes, and the refusals the grammar makes are
    /// about the separators and segments a hole does not change.
    fn unmatchable_path(
        &mut self,
        pattern: &Pattern,
        lookups: &mut Lookups,
    ) -> Result<Option<Unsatisfied>, StoreError> {
        let source = pattern.as_str();
        let literal = !source.contains(['*', '?']);
        if literal && let Ok(directory) = DirectoryPrefix::new(source) {
            let (lower, upper) = directory.descendant_bounds();
            let (sql, values) = compose_bare_directory(source, &lower, &upper);
            let bare = self.ask(
                &sql,
                &values,
                "asking whether a path names a bare directory",
            )?;
            lookups.probes.push(Probe {
                statement: FindStatement::BareDirectory,
                sql,
                values,
            });
            if bare {
                return Ok(Some(Unsatisfied::bare_directory(source)));
            }
        }
        if DocumentPath::new(&source.replace(['*', '?'], "a")).is_err() {
            return Ok(Some(Unsatisfied::impossible_path(source)));
        }
        Ok(None)
    }
}

/// The key a predicate names, where it names one.
fn predicate_key(predicate: &Predicate) -> Option<&str> {
    match predicate {
        Predicate::Eq { key, .. }
        | Predicate::NotEq { key, .. }
        | Predicate::In { key, .. }
        | Predicate::Has { key, .. }
        | Predicate::Missing { key, .. }
        | Predicate::Before { key, .. }
        | Predicate::After { key, .. } => Some(key),
        _ => None,
    }
}

/// The page bound a request names, or the default, held to `1..=MAX_PAGE`.
fn page_limit(limit: Option<u32>) -> usize {
    limit.map_or(DEFAULT_PAGE, |limit| {
        usize::try_from(limit)
            .unwrap_or(MAX_PAGE)
            .clamp(1, MAX_PAGE)
    })
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

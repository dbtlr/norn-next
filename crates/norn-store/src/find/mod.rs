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
//! gives the key a typed order and the raw text's otherwise, on every page: the
//! order is the request's, and a continuation's cursor is judged against it.
//! **The declaration is the snapshot's.** It names the schema it was read from,
//! and a find whose declaration is not the one the snapshot pins is refused
//! ([`FindRefusal::DeclarationNotPinned`]), so a typed order is always the one
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
//! # A filter is a membership test one seek answers
//!
//! Each part of the conjunction narrows every section by the rows one index
//! seek of its own reaches — [`FindFilter`] names each and the index it
//! seeks — so a part costs the rows it matches, never the vault. A part that
//! can match nothing by construction is reported in [`Found::unsatisfied`]
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
//! - **A match part whose query the full-text engine cannot parse** is
//!   malformed. Whether it parses is asked before the page runs, by one probe
//!   of the full-text index; a malformed query's part is reported with the
//!   engine's words and the page is answered without it, as an unknown
//!   predicate key's is.
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
//! cursor names no sort key, so two raw field orders are not told apart, and
//! a path order's cursor continued in a raw field order reads as a position
//! in the missing section: the cursor's encoding cannot say which order it was
//! minted in.

mod glob;
mod hydrate;
mod statement;
mod suggest;

use std::collections::{BTreeMap, BTreeSet};

use norn_db::EmittedPlan;
use norn_db::rusqlite::types::Value;
use norn_db::rusqlite::{self, Row, StatementStatus, params_from_iter};
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
    Filter, Section, SectionStart, compose_bare_directory, compose_known_key, compose_match_probe,
    compose_page, compose_universe,
};

/// How many rows a page holds when a request names no bound.
///
/// The store's default. A handler may narrow it by naming a bound of its own;
/// a bound outside `1..=`[`MAX_PAGE`] is refused
/// ([`FindRefusal::OutOfBound`]), never clamped.
pub const DEFAULT_PAGE: usize = 100;

/// The most values one membership part may name.
///
/// Every value is bound into the part's one statement, so the ceiling is what
/// bounds that statement's text and its parameters. A part naming more is
/// refused ([`FindRefusal::OutOfBound`]), and one naming none
/// ([`FindRefusal::EmptyMembership`]).
pub const IN_VALUES_CEILING: usize = 256;

/// A count a request names that the store holds to a range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FindBound {
    /// The rows a page holds: `1..=`[`MAX_PAGE`].
    PageRows,
    /// The values one membership part names: at most [`IN_VALUES_CEILING`].
    /// A part naming none is refused as [`FindRefusal::EmptyMembership`].
    MembershipValues,
}

impl FindBound {
    /// The most the count may be.
    pub const fn ceiling(self) -> usize {
        match self {
            FindBound::PageRows => MAX_PAGE,
            FindBound::MembershipValues => IN_VALUES_CEILING,
        }
    }
}

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
    /// The declaration the request was compiled under was read from a schema
    /// other than the one the snapshot pins, so its typed orders are not the
    /// ones the typed column holds. Each fingerprint is `None` for no schema.
    DeclarationNotPinned {
        declared_under: Option<String>,
        pinned: Option<String>,
    },
    /// The cursor is not a position in the order the request reads: it was
    /// minted under a schema fingerprint the snapshot no longer reads, or in
    /// another order than the request's.
    OrderChanged(CursorOrderChanged),
    /// The cursor names a position among rows that are not documents.
    NotADocumentCursor,
    /// A membership part on `key` names no value, so no document can satisfy
    /// it. The wire refuses one on read; this is the refusal of one built
    /// in-process.
    EmptyMembership { key: String },
    /// A count the request names is outside the range `bound` holds it to:
    /// `given` rows for a page, or `given` values for one membership part.
    OutOfBound { bound: FindBound, given: usize },
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
            FindRefusal::DeclarationNotPinned {
                declared_under,
                pinned,
            } => write!(
                formatter,
                "the declaration was read from {}, and the snapshot pins {}",
                schema_named(declared_under.as_deref()),
                schema_named(pinned.as_deref())
            ),
            FindRefusal::OrderChanged(changed) => write!(
                formatter,
                "the cursor was minted in an order under {}, and the request reads one under {}",
                schema_named(changed.minted_under.as_deref()),
                schema_named(changed.current.as_deref())
            ),
            FindRefusal::NotADocumentCursor => {
                formatter.write_str("the cursor names a position among rows that are not documents")
            }
            FindRefusal::EmptyMembership { key } => {
                write!(formatter, "the membership part on `{key}` names no value")
            }
            FindRefusal::OutOfBound { bound, given } => match bound {
                FindBound::PageRows => write!(
                    formatter,
                    "a page holds 1 to {} rows, and {given} were asked for",
                    bound.ceiling()
                ),
                FindBound::MembershipValues => write!(
                    formatter,
                    "a membership part names at most {} values, and {given} were named",
                    bound.ceiling()
                ),
            },
            FindRefusal::UnknownPart { part } => {
                write!(formatter, "this store does not know {part}")
            }
            FindRefusal::Store(problem) => problem.fmt(formatter),
        }
    }
}

impl std::error::Error for FindRefusal {}

/// A schema fingerprint as a refusal names it: quoted, or "no schema".
fn schema_named(fingerprint: Option<&str>) -> String {
    fingerprint.map_or_else(
        || "no schema".to_string(),
        |named| format!("the schema `{named}`"),
    )
}

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

impl PageOrder<'_> {
    /// The field order a field sort runs in, and `None` for a path order.
    fn field_order(self) -> Option<FieldOrder> {
        match self {
            PageOrder::Path(_) => None,
            PageOrder::Field { order, .. } => Some(order),
        }
    }
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
        self.order.field_order()
    }
}

/// A compiled part of the conjunction.
enum Part {
    Filter(Filter),
    /// A part no document can satisfy: reported, and every section is empty.
    MatchesNothing(Unsatisfied),
    /// A part that cannot be applied as asked: reported, and the page is
    /// answered without it.
    Unapplied(Unsatisfied),
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

/// One statement a find ran on its snapshot, as it ran: its shape, the
/// filters it narrows by, its text and the values bound to it.
///
/// [`Snapshot::run_statement`] records one before it prepares the text the
/// record holds, which is how [`Snapshot::find_plans`] explains the statement
/// that ran rather than a second spelling of it.
pub(super) struct Ran {
    statement: FindStatement,
    filters: Vec<FindFilter>,
    sql: String,
    values: Vec<Value>,
    /// What SQLite counted while the statement was stepped, read once every
    /// row it answers has been read.
    stepped: Stepped,
}

/// What SQLite counts while one statement is stepped, read off the
/// statement's own status before it is dropped.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Stepped {
    /// Steps forward through a loop no constraint bounds: a table read end to
    /// end, or an index read end to end.
    full_scan_steps: u64,
    /// Sorts the statement ran: a temporary B-tree an `ORDER BY` filled
    /// because no index hands its rows back in order.
    sorts: u64,
    /// Virtual-machine operations the statement ran, whatever they did.
    vm_steps: u64,
}

impl Stepped {
    /// The counts `statement` holds, having been stepped to its end.
    fn of(statement: &rusqlite::Statement<'_>) -> Self {
        // SQLite keeps each count unsigned and hands it back as a C `int`, so
        // the bits read back as the unsigned count they are.
        let count = |status| u64::from(statement.get_status(status) as u32);
        Stepped {
            full_scan_steps: count(StatementStatus::FullscanStep),
            sorts: count(StatementStatus::Sort),
            vm_steps: count(StatementStatus::VmStep),
        }
    }
}

impl Ran {
    /// `statement`, composed as `(sql, values)`, narrowing by no filter.
    pub(super) fn new(statement: FindStatement, (sql, values): (String, Vec<Value>)) -> Self {
        Ran {
            statement,
            filters: Vec::new(),
            sql,
            values,
            stepped: Stepped::default(),
        }
    }

    /// The same statement, narrowing by `filters`.
    fn narrowed_by(mut self, filters: Vec<FindFilter>) -> Self {
        self.filters = filters;
        self
    }
}

/// What one request asked its snapshot, each question asked once — the active
/// fingerprint and which keys are known — and every statement the find ran,
/// in the order it ran them.
#[derive(Default)]
struct Lookups {
    /// The active fingerprint once read, `None` inside where no schema is
    /// pinned.
    fingerprint: Option<Option<String>>,
    known: BTreeMap<String, bool>,
    ran: Vec<Ran>,
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
    /// `declared` is the vault's declaration, read from the schema the
    /// snapshot pins: it decides a field sort's order, how a bound is
    /// compared, and — beside the keys documents carry — which keys are known.
    /// The page holds `params.limit` rows, [`DEFAULT_PAGE`] where it names
    /// none, and only those rows are hydrated.
    ///
    /// Refused: a page bound outside `1..=`[`MAX_PAGE`]; a membership part
    /// naming no value or more than [`IN_VALUES_CEILING`]; a declaration read
    /// from another schema than the snapshot pins; a cursor among rows that
    /// are not documents; a cursor that is not a position in the request's
    /// order, as the module states; a projected column or a part the store
    /// keeps no index of; and a bound that does not read as its key's declared
    /// type.
    pub fn find(
        &mut self,
        params: &FindParams,
        declared: &DeclaredFields,
    ) -> Result<Found, FindRefusal> {
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
        &mut self,
        params: &FindParams,
        declared: &DeclaredFields,
    ) -> Result<Vec<FindPlan>, FindRefusal> {
        let mut lookups = Lookups::default();
        self.run_find(params, declared, &mut lookups)?;
        let mut plans = Vec::with_capacity(lookups.ran.len());
        for ran in lookups.ran {
            let plan =
                norn_db::emitted_plan(self.connection(), &ran.sql, params_from_iter(ran.values))
                    .map_err(StoreError::from)?;
            plans.push(FindPlan {
                statement: ran.statement,
                filters: ran.filters,
                plan,
            });
        }
        Ok(plans)
    }

    /// The find [`Snapshot::find`] answers and [`Snapshot::find_plans`]
    /// explains, recording every statement it runs in `lookups`.
    fn run_find(
        &mut self,
        params: &FindParams,
        declared: &DeclaredFields,
        lookups: &mut Lookups,
    ) -> Result<Found, FindRefusal> {
        let started = self.counters().statements_executed();
        let limit = page_limit(params.limit)?;
        let projection = Projection::of(&params.columns)?;
        let mut compiled = self.compile(params, declared, lookups)?;
        let (resume, moved) = match &params.after {
            None => (None, Vec::new()),
            Some(cursor) => self.judge(cursor, compiled.order, lookups)?,
        };
        let fields = self.projected_keys(&projection, declared, lookups, &mut compiled)?;

        let mut work = FindWork::default();
        let (keys, next) = self.page_keys(&compiled, limit, resume.as_ref(), lookups, &mut work)?;
        let order = compiled.field_order();
        let snapshot = self.reading_facts(order, lookups)?;
        let next =
            next.map(|at| Cursor::new(snapshot.clone(), CursorKey::document(at.sort, at.path)));
        let unsatisfied = self.resolve(compiled.reports, declared, lookups)?;
        let rows = self.hydrate(&keys, &projection, &fields, &mut work, &mut lookups.ran)?;
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

    /// Run `ran`, counted on this snapshot and recorded at the end of `record`
    /// before it is prepared, and read each row it answers through `read`.
    ///
    /// Every statement a find runs is run here, and what is prepared is the
    /// text the record holds, bound to the values it holds:
    /// [`Snapshot::find_plans`] explains the record, so the plan of a
    /// statement is the plan of what ran. Once every row is read, the record
    /// takes what SQLite counted stepping it ([`Stepped`]).
    pub(super) fn run_statement<T>(
        &mut self,
        record: &mut Vec<Ran>,
        ran: Ran,
        read: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
    ) -> rusqlite::Result<Vec<T>> {
        self.count_statement();
        record.push(ran);
        let ran = record.last_mut().expect("the statement was just recorded");
        let mut statement = self.connection().prepare(&ran.sql)?;
        let rows = statement
            .query_map(params_from_iter(ran.values.iter()), read)?
            .collect::<rusqlite::Result<Vec<T>>>()?;
        ran.stepped = Stepped::of(&statement);
        Ok(rows)
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
    /// **Two raw orders share one spelling.** A cursor names no sort key, so a
    /// cursor minted in one key's raw order and continued in another key's, or
    /// a path order's cursor continued in a raw field order — where it reads as
    /// a position in the missing section — is answered from a position in an
    /// order it was not minted in. That gap is the wire's, and NORN-244 closes
    /// it by naming the order in the cursor.
    fn judge(
        &mut self,
        cursor: &Cursor,
        order: PageOrder<'_>,
        lookups: &mut Lookups,
    ) -> Result<(Option<FindPosition>, Vec<Moved>), FindRefusal> {
        let CursorKey::Document { sort, path, .. } = cursor.key() else {
            return Err(FindRefusal::NotADocumentCursor);
        };
        let now = self.reading_facts(order.field_order(), lookups)?;
        let minted_under = cursor.snapshot().schema_fingerprint.as_deref();
        let path_ordered = matches!(order, PageOrder::Path(_));
        if minted_under != now.schema_fingerprint.as_deref() || (path_ordered && sort.is_some()) {
            let current = now.schema_fingerprint.clone();
            return Err(FindRefusal::OrderChanged(match minted_under {
                Some(minted_under) => CursorOrderChanged::new(minted_under, current),
                None => CursorOrderChanged::minted_raw(current),
            }));
        }
        let moved = cursor
            .continuation(&now)
            .map_err(FindRefusal::OrderChanged)?;
        Ok((
            Some(FindPosition {
                sort: sort.clone(),
                path: path.clone(),
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
            Some(FieldOrder::Typed) => self.fingerprint(lookups)?,
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

    /// The active fingerprint, read once per request; `None` where no schema
    /// is pinned.
    fn fingerprint(&mut self, lookups: &mut Lookups) -> Result<Option<String>, StoreError> {
        if let Some(fingerprint) = &lookups.fingerprint {
            return Ok(fingerprint.clone());
        }
        let read = Ran::new(
            FindStatement::ActiveFingerprint,
            (
                norn_db::meta::META_READ_SQL.to_string(),
                vec![Value::Text(ddl::meta::VAULT_SCHEMA_FINGERPRINT.to_string())],
            ),
        );
        let fingerprint = self
            .run_statement(&mut lookups.ran, read, |row| row.get::<_, String>(0))
            .map_err(|problem| error::sql("reading the pinned schema fingerprint", problem))?
            .into_iter()
            .next();
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
        let known = self.ask(
            &mut lookups.ran,
            Ran::new(FindStatement::KnownKey, compose_known_key(key)),
            "asking whether a document carries a key",
        )?;
        lookups.known.insert(key.to_string(), known);
        Ok(known)
    }

    /// Run one yes-or-no probe, which answers exactly one row.
    fn ask(
        &mut self,
        record: &mut Vec<Ran>,
        probe: Ran,
        operation: &'static str,
    ) -> Result<bool, StoreError> {
        self.run_statement(record, probe, |row| row.get::<_, bool>(0))
            .and_then(|answers| {
                answers
                    .into_iter()
                    .next()
                    .ok_or(rusqlite::Error::QueryReturnedNoRows)
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
        let carried: Vec<String> = self
            .run_statement(
                &mut lookups.ran,
                Ran::new(FindStatement::FieldUniverse, compose_universe()),
                |row| row.get(0),
            )
            .map_err(|problem| error::sql(OPERATION, problem))?;
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

    /// The request's order and its conjunction, compiled under `declared`,
    /// which is refused where it was read from another schema than the
    /// snapshot pins.
    ///
    /// A field sort's order is the declaration's for its key: typed where the
    /// key is declared with a typed order, raw otherwise.
    fn compile<'a>(
        &mut self,
        params: &'a FindParams,
        declared: &DeclaredFields,
        lookups: &mut Lookups,
    ) -> Result<Compiled<'a>, FindRefusal> {
        let pinned = self.fingerprint(lookups)?;
        if declared.schema() != pinned.as_deref() {
            return Err(FindRefusal::DeclarationNotPinned {
                declared_under: declared.schema().map(str::to_string),
                pinned,
            });
        }
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
                        order: match declared.typed_order(key) {
                            Some(_) => FieldOrder::Typed,
                            None => FieldOrder::Raw,
                        },
                        direction,
                    },
                    _ => return Err(FindRefusal::UnknownPart { part: "a sort key" }),
                }
            }
        };

        let mut filters = Vec::new();
        let mut matches_nothing = false;
        for predicate in &params.predicates {
            membership_bound(predicate)?;
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
                Part::Unapplied(part) => reports.push(Report::Part(part)),
            }
        }
        Ok(Compiled {
            order,
            filters,
            reports,
            matches_nothing,
        })
    }

    /// One page of keys: at most `limit`, and where the next page starts.
    ///
    /// `work` takes what the section statements cost: the keys they handed
    /// back — one past the bound where a next page exists — and what SQLite
    /// counted stepping them.
    fn page_keys(
        &mut self,
        compiled: &Compiled<'_>,
        limit: usize,
        at: Option<&FindPosition>,
        lookups: &mut Lookups,
        work: &mut FindWork,
    ) -> Result<(Vec<FoundKey>, Option<FindPosition>), StoreError> {
        let mut keys: Vec<FoundKey> = Vec::new();
        if !compiled.matches_nothing {
            for (statement, start) in sections(compiled.order, at) {
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
                let section =
                    Ran::new(statement, (sql, values)).narrowed_by(compiled.filter_shapes());
                keys.extend(self.read_keys(&mut lookups.ran, section)?);
                let ran = lookups.ran.last().expect("the section was just recorded");
                work.page_stepped(ran.stepped);
            }
        }
        work.keys_read = keys.len() as u64;
        let next = if keys.len() > limit {
            keys.truncate(limit);
            keys.last().map(FoundKey::position)
        } else {
            None
        };
        Ok((keys, next))
    }

    /// Run one page section.
    fn read_keys(
        &mut self,
        record: &mut Vec<Ran>,
        section: Ran,
    ) -> Result<Vec<FoundKey>, StoreError> {
        self.run_statement(record, section, |row| {
            Ok(FoundKey {
                document: row.get(0)?,
                path: row.get(1)?,
                sort: row.get(2)?,
            })
        })
        .map_err(|problem| error::sql("reading a page of found documents", problem))
    }

    /// One part of the conjunction as the filter a statement spells, or the
    /// report that it matches nothing.
    ///
    /// Only a finding part reads the fingerprint, only a path part with no
    /// wildcard probes whether it names a bare directory, and only a match
    /// part probes whether the full-text engine parses its query: the other
    /// parts bind nothing the snapshot has to be asked for.
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
            Predicate::Matches { query, .. } => match self.match_problem(query, lookups)? {
                Some(problem) => Ok(Part::Unapplied(Unsatisfied::malformed_query(
                    query.clone(),
                    problem,
                ))),
                None => filter(FindFilter::FullText, vec![text(query)]),
            },
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
                // A finding recorded under no schema is stamped with the
                // empty fingerprint.
                vec![
                    Value::Text(self.fingerprint(lookups)?.unwrap_or_default()),
                    text(kind.as_str()),
                ],
            ),
            _ => Err(FindRefusal::UnknownPart {
                part: "a predicate",
            }),
        }
    }

    /// What the full-text engine says is wrong with `query`, or `None` where it
    /// parses. One [`FindStatement::MatchProbe`].
    ///
    /// A statement that does not prepare is the store's problem and refused as
    /// one; a query the engine cannot parse is the request's, and is read as
    /// that exactly where [`query_problem`] says so.
    fn match_problem(
        &mut self,
        query: &str,
        lookups: &mut Lookups,
    ) -> Result<Option<String>, StoreError> {
        const OPERATION: &str = "asking whether a full-text query parses";
        let answered = self.run_statement(
            &mut lookups.ran,
            Ran::new(FindStatement::MatchProbe, compose_match_probe(query)),
            |row| row.get::<_, bool>(0),
        );
        match answered {
            Ok(_) => Ok(None),
            Err(problem) => match query_problem(&problem) {
                Some(said) => Ok(Some(said)),
                None => Err(error::sql(OPERATION, problem)),
            },
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
            let bare = self.ask(
                &mut lookups.ran,
                Ran::new(
                    FindStatement::BareDirectory,
                    compose_bare_directory(source, &lower, &upper),
                ),
                "asking whether a path names a bare directory",
            )?;
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

/// What `problem`, met stepping a prepared [`FindStatement::MatchProbe`], says
/// is wrong with the query it bound, or `None` where it is a problem of the
/// store's.
///
/// The full-text engine parses a query when the probe is stepped, and reports
/// every query it cannot read — `fts5: syntax error near …`, an unterminated
/// string, a column filter naming no column — as the plain `SQLITE_ERROR`, with
/// its words as the message. A damaged index, a failed read, a busy or an
/// interrupted connection each report a code of their own, and stay the
/// store's.
fn query_problem(problem: &norn_db::rusqlite::Error) -> Option<String> {
    match problem {
        norn_db::rusqlite::Error::SqliteFailure(failure, message)
            if failure.extended_code == norn_db::rusqlite::ffi::SQLITE_ERROR =>
        {
            Some(message.clone().unwrap_or_else(|| failure.to_string()))
        }
        _ => None,
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

/// The page bound a request names, or [`DEFAULT_PAGE`] where it names none;
/// a bound outside `1..=`[`MAX_PAGE`] is refused.
fn page_limit(limit: Option<u32>) -> Result<usize, FindRefusal> {
    let Some(limit) = limit else {
        return Ok(DEFAULT_PAGE);
    };
    let given = usize::try_from(limit).unwrap_or(usize::MAX);
    if given == 0 || given > FindBound::PageRows.ceiling() {
        return Err(FindRefusal::OutOfBound {
            bound: FindBound::PageRows,
            given,
        });
    }
    Ok(given)
}

/// A membership part's values held to `1..=`[`IN_VALUES_CEILING`]; every other
/// part passes.
fn membership_bound(predicate: &Predicate) -> Result<(), FindRefusal> {
    let Predicate::In { key, values, .. } = predicate else {
        return Ok(());
    };
    if values.is_empty() {
        return Err(FindRefusal::EmptyMembership { key: key.clone() });
    }
    if values.len() > FindBound::MembershipValues.ceiling() {
        return Err(FindRefusal::OutOfBound {
            bound: FindBound::MembershipValues,
            given: values.len(),
        });
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use norn_db::rusqlite::{Error, ffi};

    use super::query_problem;

    fn failure(extended_code: i32, message: &str) -> Error {
        Error::SqliteFailure(ffi::Error::new(extended_code), Some(message.to_string()))
    }

    /// The engine's words for a query it cannot read are the request's
    /// problem; a damaged index, a failed read, a busy or an interrupted
    /// connection is the store's, whatever its message says.
    #[test]
    fn only_a_query_the_engine_cannot_read_is_the_requests_problem() {
        for said in [
            "fts5: syntax error near \"\"",
            "unterminated string",
            "no such column: title",
        ] {
            assert_eq!(
                query_problem(&failure(ffi::SQLITE_ERROR, said)).as_deref(),
                Some(said)
            );
        }
        for code in [
            ffi::SQLITE_CORRUPT_VTAB,
            ffi::SQLITE_CORRUPT,
            ffi::SQLITE_IOERR_READ,
            ffi::SQLITE_BUSY,
            ffi::SQLITE_INTERRUPT,
            ffi::SQLITE_NOMEM,
        ] {
            assert_eq!(
                query_problem(&failure(code, "fts5: syntax error near \"\"")),
                None,
                "code {code} was read as the request's"
            );
        }
        assert_eq!(query_problem(&Error::QueryReturnedNoRows), None);
    }
}

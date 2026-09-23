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
//! # A filter is a membership test one seek answers
//!
//! Each part of the conjunction narrows every section by the rows one index
//! seek of its own reaches — [`FindFilter`] names each and the index it
//! seeks — so a part costs the rows it matches, never the vault. A part that
//! can match nothing, such as a glob that does not parse or a resolution
//! target that is not a suffix address, is reported in
//! [`KeyPage::unsatisfied`] and the page is empty: a conjunction holding it
//! matches no document, and the report is what keeps that from reading as a
//! vault with nothing in it.
//!
//! A `resolves` part enumerates the target's ambiguity class through its suffix
//! probe, both reductions of a dotted leaf included, and compares suffix keys
//! bytewise. The folded suffix key and per-root case sensitivity land with
//! NORN-229, which is the task that consumes this filter's case behaviour.
//!
//! # What a caller composes around it
//!
//! Judging a wire cursor, minting the next one, hydrating the rows a page
//! found, and reporting the request's unknown keys are the caller's around
//! [`Snapshot::find_keys`]: it takes a [`Resume`] rather than a cursor, and
//! answers keys rather than rows.

mod glob;
mod statement;

use norn_db::EmittedPlan;
use norn_db::rusqlite::params_from_iter;
use norn_db::rusqlite::types::Value;
use norn_wire::{Direction, FindParams, Predicate, SortKey, Unsatisfied};

use crate::ddl;
use crate::error::{self, StoreError};
use crate::fields::DeclaredFields;
use crate::json::{FrontmatterValue, canonical_json};
use crate::path::suffix_probe;
use crate::request::MAX_PAGE;
use crate::store::Snapshot;

pub(crate) use glob::register_functions;
pub use statement::{FIND_FILTERS, FIND_STATEMENTS, FindFilter, FindStatement, PageDirection};
use statement::{Filter, Section, SectionStart, compose_page};

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

    /// The document's row id.
    #[allow(dead_code)] // Hydration reads it; the keys page is its only producer today.
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
    /// The parts of the conjunction no document can satisfy. A page carrying
    /// one is empty.
    pub unsatisfied: Vec<Unsatisfied>,
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
    /// The request names a fact the store keeps no index of.
    ///
    /// A **dormant carrier**: the link index a `links_to` part filters by lands
    /// with `consumer`, which is the task that builds it; until then no request
    /// can be answered by it, and saying so is the answer.
    NotIndexed {
        fact: &'static str,
        consumer: &'static str,
    },
    /// A `before` or `after` bound on a key declared with a type does not read
    /// as that type, so it names no place in the key's order.
    UnreadableBound { key: String, value: String },
    /// The request carries a part this build of the store does not know.
    UnknownPart { part: &'static str },
    /// The store refused a statement.
    Store(StoreError),
}

impl std::fmt::Display for FindRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FindRefusal::NotIndexed { fact, consumer } => {
                write!(
                    formatter,
                    "the store keeps no index of {fact} (lands with {consumer})"
                )
            }
            FindRefusal::UnreadableBound { key, value } => write!(
                formatter,
                "`{value}` does not read as the type `{key}` is declared with"
            ),
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

/// The task that builds the link index a `links_to` part and the link columns
/// wait for.
const LINK_INDEX_CONSUMER: &str = "NORN-229";

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

/// A request compiled: its order, its filters, and the parts no document can
/// satisfy.
struct Compiled<'a> {
    order: PageOrder<'a>,
    filters: Vec<Filter>,
    unsatisfied: Vec<Unsatisfied>,
    /// Whether the fingerprint read ran to bind a finding filter.
    read_fingerprint: bool,
}

impl Compiled<'_> {
    fn filter_shapes(&self) -> Vec<FindFilter> {
        self.filters.iter().map(|filter| filter.shape).collect()
    }
}

/// A compiled part of the conjunction.
enum Part {
    Filter(Filter),
    MatchesNothing(Unsatisfied),
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

    /// One page of the documents `params` asks for, as keys, in its order.
    ///
    /// `params.after` is not read here: a caller judges the wire cursor and
    /// hands the position it names over as `resume`. `declared` is the vault's
    /// declaration, which decides a first page's field order and how a bound is
    /// compared. The page holds `params.limit` rows, [`DEFAULT_PAGE`] where it
    /// names none, held to `1..=`[`MAX_PAGE`].
    pub fn find_keys(
        &mut self,
        params: &FindParams,
        declared: &DeclaredFields,
        resume: Option<&Resume>,
    ) -> Result<KeyPage, FindRefusal> {
        let compiled = self.compile(params, declared, resume)?;
        let limit = page_limit(params.limit);
        let mut keys: Vec<FoundKey> = Vec::new();
        if compiled.unsatisfied.is_empty() {
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
        let next = if keys.len() > limit {
            keys.truncate(limit);
            keys.last().map(FoundKey::position)
        } else {
            None
        };
        Ok(KeyPage {
            keys,
            next,
            order: match compiled.order {
                PageOrder::Path(_) => None,
                PageOrder::Field { order, .. } => Some(order),
            },
            unsatisfied: compiled.unsatisfied,
        })
    }

    /// Every statement [`Snapshot::find_keys`] would run for the same request,
    /// each with the plan SQLite reported for it.
    ///
    /// The statements are composed by the same call the page runs, over the
    /// same compiled request, and explained on this snapshot's read-only
    /// connection. A page statement is listed wherever the page could reach
    /// it from `resume`, whether or not the rows would have filled the page
    /// before it. An explain is a report about a statement rather than a run
    /// of it, so it is not counted; the fingerprint read a finding filter
    /// binds is run, and is.
    pub fn find_plans(
        &mut self,
        params: &FindParams,
        declared: &DeclaredFields,
        resume: Option<&Resume>,
    ) -> Result<Vec<FindPlan>, FindRefusal> {
        let compiled = self.compile(params, declared, resume)?;
        let limit = page_limit(params.limit);
        let mut plans = Vec::new();
        if compiled.read_fingerprint {
            plans.push(FindPlan {
                statement: FindStatement::ActiveFingerprint,
                filters: Vec::new(),
                plan: norn_db::emitted_plan(
                    self.connection(),
                    norn_db::meta::META_READ_SQL,
                    [ddl::meta::VAULT_SCHEMA_FINGERPRINT],
                )
                .map_err(StoreError::from)?,
            });
        }
        if !compiled.unsatisfied.is_empty() {
            return Ok(plans);
        }
        for (statement, start) in sections(compiled.order, resume.map(|resume| &resume.at)) {
            let (sql, values) = compose_page(&Section {
                statement,
                key: field_key(compiled.order),
                start,
                filters: &compiled.filters,
                rows: limit + 1,
            });
            plans.push(FindPlan {
                statement,
                filters: compiled.filter_shapes(),
                plan: norn_db::emitted_plan(self.connection(), &sql, params_from_iter(values))
                    .map_err(StoreError::from)?,
            });
        }
        Ok(plans)
    }

    /// The request's order and its conjunction, compiled.
    fn compile<'a>(
        &mut self,
        params: &'a FindParams,
        declared: &DeclaredFields,
        resume: Option<&Resume>,
    ) -> Result<Compiled<'a>, FindRefusal> {
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

        let mut fingerprint: Option<String> = None;
        let mut filters = Vec::new();
        let mut unsatisfied = Vec::new();
        for predicate in &params.predicates {
            let part = compile_predicate(predicate, declared, &mut || {
                if fingerprint.is_none() {
                    fingerprint = Some(self.active_fingerprint()?.unwrap_or_default());
                }
                Ok(fingerprint.clone().unwrap_or_default())
            })?;
            match part {
                Part::Filter(filter) => filters.push(filter),
                Part::MatchesNothing(part) => unsatisfied.push(part),
            }
        }
        Ok(Compiled {
            order,
            filters,
            unsatisfied,
            read_fingerprint: fingerprint.is_some(),
        })
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

/// One part of the conjunction as the filter a statement spells.
///
/// `fingerprint` reads the active fingerprint, and is called only by a finding
/// part: the other parts bind nothing the snapshot has to be asked for.
fn compile_predicate(
    predicate: &Predicate,
    declared: &DeclaredFields,
    fingerprint: &mut dyn FnMut() -> Result<String, StoreError>,
) -> Result<Part, FindRefusal> {
    let text = |value: &str| Value::Text(value.to_string());
    let filter = |shape: FindFilter, values: Vec<Value>| Ok(Part::Filter(Filter { shape, values }));
    match predicate {
        Predicate::Eq { key, value, .. } => filter(FindFilter::Equal, vec![text(key), text(value)]),
        Predicate::NotEq { key, value, .. } => {
            filter(FindFilter::NotEqual, vec![text(key), text(value)])
        }
        Predicate::In { key, values, .. } => {
            let listed = canonical_json(&FrontmatterValue::Sequence(
                values
                    .iter()
                    .cloned()
                    .map(FrontmatterValue::String)
                    .collect(),
            ))?;
            filter(FindFilter::Member, vec![text(key), Value::Text(listed)])
        }
        Predicate::Has { key, .. } => filter(FindFilter::Present, vec![text(key)]),
        Predicate::Missing { key, .. } => filter(FindFilter::Absent, vec![text(key)]),
        Predicate::Before { key, value, .. } | Predicate::After { key, value, .. } => {
            let (order, bound) = match declared.typed_order(key) {
                None => (FieldOrder::Raw, value.clone()),
                Some(typed) => (
                    FieldOrder::Typed,
                    typed
                        .sort_key(value)
                        .ok_or_else(|| FindRefusal::UnreadableBound {
                            key: key.clone(),
                            value: value.clone(),
                        })?,
                ),
            };
            let shape = if matches!(predicate, Predicate::Before { .. }) {
                FindFilter::Before(order)
            } else {
                FindFilter::After(order)
            };
            filter(shape, vec![text(key), Value::Text(bound)])
        }
        Predicate::Matches { query, .. } => filter(FindFilter::FullText, vec![text(query)]),
        Predicate::Path { glob, .. } => match norn_wire::Pattern::parse(glob) {
            Err(problem) => Ok(Part::MatchesNothing(Unsatisfied::malformed_glob(
                glob.clone(),
                problem.to_string(),
            ))),
            Ok(pattern) => {
                let (lower, upper) = glob::path_range(&pattern);
                filter(
                    FindFilter::PathGlob,
                    vec![Value::Text(lower), upper, text(glob)],
                )
            }
        },
        Predicate::LinksTo { .. } => Err(FindRefusal::NotIndexed {
            fact: "a link's target",
            consumer: LINK_INDEX_CONSUMER,
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
            vec![Value::Text(fingerprint()?), text(kind.as_str())],
        ),
        _ => Err(FindRefusal::UnknownPart {
            part: "a predicate",
        }),
    }
}

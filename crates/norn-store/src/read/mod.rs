//! What every read builder shares: the refusal a builder answers with instead
//! of a page, the statements a read runs and records, the reading a cursor is
//! judged against, the one compilation of a conjunction, the filters it
//! spells, the one walk of the keys documents carry, and the one keyset page a
//! builder reads section after section.
//!
//! A read builder is inherent methods on [`Snapshot`], so every statement it
//! runs reads the one instant the snapshot was established at and is counted
//! on the snapshot's own statement counter. **A builder states only what is
//! its own**: the statements it names, the sections a page of it reads and in
//! which order, and what a row of it is. Everything else is here, so two
//! builders never grow two answers to one question — a part of a conjunction
//! means one thing to every verb, a cursor's reading is judged one way, and a
//! page stops, and says a next page exists, one way.
//!
//! **A builder's plans are of the statements it ran.** Every statement a read
//! runs goes through [`Snapshot::run_statement`], which records the
//! statement's name, its filters, its text and its bound values in the order
//! it ran them, and prepares the text it recorded; [`Snapshot::explained`]
//! explains that record, so a plan is never of a second spelling of a
//! statement, and a section a page never reached is never explained.

mod conjunction;
mod filter;
mod finding;
mod glob;
mod keys;
mod page;
mod reading;
mod run;
mod suggest;

use norn_wire::{CandidateHead, CursorOrderChanged, Hint, ResolutionTarget};

use crate::count::CountStatement;
use crate::describe::DescribeStatement;
use crate::error::StoreError;
use crate::find::FindStatement;
use crate::get::GetStatement;
use crate::request::MAX_PAGE;
use crate::search::SearchStatement;
#[cfg(doc)]
use crate::store::Snapshot;
use crate::validate::ValidateStatement;

pub(crate) use conjunction::{Conjunction, KeyPlace, Report, ResolvesPart};
pub(crate) use filter::glob_test;
pub(crate) use filter::{Binder, Filter};
pub use filter::{READ_FILTERS, ReadFilter};
pub(crate) use finding::{FINDING_ROW_COLUMNS, FindingBase, finding_base};
pub(crate) use glob::register_functions;
pub(crate) use keys::key_walk;
pub(crate) use run::{Lookups, Ran, Stepped};

/// How many rows a page holds when a request names no bound.
///
/// The store's default. A handler may narrow it by naming a bound of its own;
/// a bound outside `1..=`[`MAX_PAGE`] is refused
/// ([`PageRefusal::OutOfBound`]), never clamped.
pub const DEFAULT_PAGE: usize = 100;

/// The most values one membership part may name.
///
/// Every value is bound into the part's one statement, so the ceiling is what
/// bounds that statement's text and its parameters. A part naming more is
/// refused ([`PageRefusal::OutOfBound`]), and one naming none
/// ([`PageRefusal::EmptyMembership`]).
pub const IN_VALUES_CEILING: usize = 256;

/// A count a request names that the store holds to a range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadBound {
    /// The rows a page holds: `1..=`[`MAX_PAGE`].
    PageRows,
    /// The values one membership part names: at most [`IN_VALUES_CEILING`].
    /// A part naming none is refused as [`PageRefusal::EmptyMembership`].
    MembershipValues,
}

impl ReadBound {
    /// The most the count may be.
    pub const fn ceiling(self) -> usize {
        match self {
            ReadBound::PageRows => MAX_PAGE,
            ReadBound::MembershipValues => IN_VALUES_CEILING,
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
    pub(crate) fn column(self) -> &'static str {
        match self {
            FieldOrder::Raw => "raw",
            FieldOrder::Typed => "typed",
        }
    }

    /// The column marking a document's least value under this order.
    pub(crate) fn marker(self) -> &'static str {
        match self {
            FieldOrder::Raw => "least_raw",
            FieldOrder::Typed => "least_typed",
        }
    }
}

/// A statement a read builder ran, named by the builder that names it.
///
/// A read compiles its conjunction through probes the find builder names, and
/// reads finding rows, document rows and the active fingerprint through
/// statements the find builder names, so a count, a validate, a describe, a
/// search and a get run find's statements beside their own; the record of what
/// a read ran holds any of them, and each builder's enumeration stays its own.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadStatement {
    /// A statement [`FindStatement`] names.
    Find(FindStatement),
    /// A statement [`CountStatement`] names.
    Count(CountStatement),
    /// A statement [`ValidateStatement`] names.
    Validate(ValidateStatement),
    /// A statement [`DescribeStatement`] names.
    Describe(DescribeStatement),
    /// A statement [`SearchStatement`] names.
    Search(SearchStatement),
    /// A statement [`GetStatement`] names.
    Get(GetStatement),
}

impl From<SearchStatement> for ReadStatement {
    fn from(statement: SearchStatement) -> Self {
        ReadStatement::Search(statement)
    }
}

impl From<DescribeStatement> for ReadStatement {
    fn from(statement: DescribeStatement) -> Self {
        ReadStatement::Describe(statement)
    }
}

impl From<GetStatement> for ReadStatement {
    fn from(statement: GetStatement) -> Self {
        ReadStatement::Get(statement)
    }
}

impl From<ValidateStatement> for ReadStatement {
    fn from(statement: ValidateStatement) -> Self {
        ReadStatement::Validate(statement)
    }
}

impl From<FindStatement> for ReadStatement {
    fn from(statement: FindStatement) -> Self {
        ReadStatement::Find(statement)
    }
}

impl From<CountStatement> for ReadStatement {
    fn from(statement: CountStatement) -> Self {
        ReadStatement::Count(statement)
    }
}

/// A target that names more than one document, as a refusal carries it.
///
/// The same bounded head and the same hint a finding over the class carries,
/// so a refusal and a finding say one thing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetAmbiguity {
    /// The target as the request named it, anchor included.
    pub target: ResolutionTarget,
    /// The first of the documents it names in the resolution ladder's order,
    /// at most [`crate::CANDIDATE_HEAD`], each named by its minimal
    /// disambiguating suffix, with how many there were.
    pub head: CandidateHead,
    /// The target whose `find` resolves every one of them: the target's
    /// address, anchor left off.
    pub hint: Hint,
}

/// Why a read builder answered no page.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum PageRefusal {
    /// The request filters by a fact the store keeps no index of.
    ///
    /// A **dormant carrier** for the link index NORN-229 builds: `links` stores
    /// a link's target raw and unindexed, so no seek answers a `links_to` part,
    /// and nothing reaches the part's filter until that index stands. Until
    /// then no request naming one can be answered, and saying so is the answer.
    NotIndexed { fact: &'static str },
    /// The request names a row column a find does not project yet.
    ///
    /// A **dormant carrier** for the resolved link column NORN-229 builds: the
    /// store holds a document's link rows, but a find's row carries no link
    /// column until the link index resolves what a link names, so no row
    /// composition reads them yet.
    NotProjected { column: &'static str },
    /// A value a comparing part names — an equality, an inequality, a
    /// membership or a `before`/`after` bound — on a key declared with a typed
    /// order does not read as that type, so it names no place in the key's
    /// order.
    UnreadableBound { key: String, value: String },
    /// The declaration the request was compiled under was read from a schema
    /// other than the one the snapshot pins, so its typed orders are not the
    /// ones the typed column holds, and the facets it declares are not the
    /// pinned schema's. Each fingerprint is `None` for no schema.
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
    /// The cursor names no position among a count's tallies: it is not a
    /// tally's, its grouping tuple is another width than the request's, or a
    /// member names no place in its key's order.
    NotATallyCursor,
    /// The cursor names no position among a validate's findings: it is not a
    /// finding's.
    NotAFindingCursor,
    /// The cursor names no position among a describe's facets: it is not a
    /// facet's.
    NotAFacetCursor,
    /// The cursor names no position among a search's hits: it is not a hit's.
    NotAHitCursor,
    /// The request answers a summary and carries a cursor. A summary answers
    /// every tally at once and is not paged, so no cursor names a position it
    /// continues from.
    SummaryNotPaged,
    /// A membership part on `key` names no value, so no document can satisfy
    /// it. The wire refuses one on read; this is the refusal of one built
    /// in-process.
    EmptyMembership { key: String },
    /// A count the request names is outside the range `bound` holds it to:
    /// `given` rows for a page, or `given` values for one membership part.
    OutOfBound { bound: ReadBound, given: usize },
    /// The request carries a part this build of the store does not know.
    UnknownPart { part: &'static str },
    /// The target names more than one document.
    AmbiguousTarget(Box<TargetAmbiguity>),
    /// The target names no document.
    UnknownTarget { target: ResolutionTarget },
    /// The cursor names no position in the collection a get pages: it is not
    /// an ordinal, or not a finding's at the document's path.
    NotACollectionCursor,
    /// The request carries `part`, which the answer it asks for — `answer` —
    /// does not take: an anchor or a column on a collection page, a column on
    /// a section or a block, or a cursor on anything but a collection page.
    PartNotTaken {
        part: &'static str,
        answer: &'static str,
    },
    /// The store refused a statement.
    Store(StoreError),
}

impl std::fmt::Display for PageRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PageRefusal::NotIndexed { fact } => {
                write!(formatter, "the store keeps no index of {fact}")
            }
            PageRefusal::NotProjected { column } => {
                write!(formatter, "{column} is not yet projected onto a find's row")
            }
            PageRefusal::UnreadableBound { key, value } => write!(
                formatter,
                "`{value}` does not read as the type `{key}` is declared with"
            ),
            PageRefusal::DeclarationNotPinned {
                declared_under,
                pinned,
            } => write!(
                formatter,
                "the declaration was read from {}, and the snapshot pins {}",
                schema_named(declared_under.as_deref()),
                schema_named(pinned.as_deref())
            ),
            PageRefusal::OrderChanged(changed) => write!(
                formatter,
                "the cursor was minted in an order under {}, and the request reads one under {}",
                schema_named(changed.minted_under.as_deref()),
                schema_named(changed.current.as_deref())
            ),
            PageRefusal::NotADocumentCursor => {
                formatter.write_str("the cursor names a position among rows that are not documents")
            }
            PageRefusal::NotATallyCursor => {
                formatter.write_str("the cursor names no position among this count's tallies")
            }
            PageRefusal::NotAFindingCursor => {
                formatter.write_str("the cursor names no position among this validate's findings")
            }
            PageRefusal::NotAFacetCursor => {
                formatter.write_str("the cursor names no position among a describe's facets")
            }
            PageRefusal::NotAHitCursor => {
                formatter.write_str("the cursor names no position among a search's hits")
            }
            PageRefusal::SummaryNotPaged => {
                formatter.write_str("a summary is not paged, so it continues no cursor")
            }
            PageRefusal::EmptyMembership { key } => {
                write!(formatter, "the membership part on `{key}` names no value")
            }
            PageRefusal::OutOfBound { bound, given } => match bound {
                ReadBound::PageRows => write!(
                    formatter,
                    "a page holds 1 to {} rows, and {given} were asked for",
                    bound.ceiling()
                ),
                ReadBound::MembershipValues => write!(
                    formatter,
                    "a membership part names at most {} values, and {given} were named",
                    bound.ceiling()
                ),
            },
            PageRefusal::UnknownPart { part } => {
                write!(formatter, "this store does not know {part}")
            }
            PageRefusal::AmbiguousTarget(ambiguity) => write!(
                formatter,
                "`{}` names {} documents, and a get answers about one",
                ambiguity.target,
                ambiguity.head.total()
            ),
            PageRefusal::UnknownTarget { target } => {
                write!(formatter, "`{target}` names no document")
            }
            PageRefusal::NotACollectionCursor => {
                formatter.write_str("the cursor names no position in the collection paged")
            }
            PageRefusal::PartNotTaken { part, answer } => {
                write!(
                    formatter,
                    "{answer} takes no {}",
                    part.trim_start_matches("a ").trim_start_matches("an ")
                )
            }
            PageRefusal::Store(problem) => problem.fmt(formatter),
        }
    }
}

impl std::error::Error for PageRefusal {}

/// A schema fingerprint as a refusal names it: quoted, or "no schema".
fn schema_named(fingerprint: Option<&str>) -> String {
    fingerprint.map_or_else(
        || "no schema".to_string(),
        |named| format!("the schema `{named}`"),
    )
}

impl From<StoreError> for PageRefusal {
    fn from(problem: StoreError) -> Self {
        PageRefusal::Store(problem)
    }
}

/// The page bound a request names, or [`DEFAULT_PAGE`] where it names none;
/// a bound outside `1..=`[`MAX_PAGE`] is refused.
pub(crate) fn page_limit(limit: Option<u32>) -> Result<usize, PageRefusal> {
    let Some(limit) = limit else {
        return Ok(DEFAULT_PAGE);
    };
    let given = usize::try_from(limit).unwrap_or(usize::MAX);
    if given == 0 || given > ReadBound::PageRows.ceiling() {
        return Err(PageRefusal::OutOfBound {
            bound: ReadBound::PageRows,
            given,
        });
    }
    Ok(given)
}

//! What every read builder shares: the refusal a builder answers with instead
//! of a page, the statements a read runs and records, the reading a cursor is
//! judged against, the one compilation of a conjunction, the filters it
//! spells, the advisories its applied parts earn, the one walk of the keys
//! documents carry, and the one keyset page a builder reads section after
//! section.
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

mod advisory;
mod conjunction;
mod filter;
mod finding;
mod glob;
mod keys;
mod naming;
mod page;
mod reading;
mod run;
mod suggest;

use norn_wire::{
    AnswerShape, CandidateHead, CollectionSelector, Cursor, CursorOrderChanged, Direction, Hint,
    OrderPair, PagedRows, RequestPart, ResolutionTarget, Rung, RungSet, Sort, SortKey,
};

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

pub(crate) use advisory::{Compared, DateComparison};
pub(crate) use conjunction::{Conjunction, KeyPlace, Report, ResolvesPart};
pub(crate) use filter::glob_test;
pub(crate) use filter::{Binder, Filter};
pub use filter::{READ_FILTERS, ReadFilter};
pub(crate) use finding::{FINDING_ROW_COLUMNS, FindingBase, finding_base};
pub(crate) use glob::register_functions;
pub(crate) use keys::key_walk;
pub(crate) use naming::{Naming, wire_path};
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReadBound {
    /// The rows a page holds: `1..=`[`MAX_PAGE`].
    PageRows,
    /// The values the membership part on `key` names: at most
    /// [`IN_VALUES_CEILING`]. A part naming none is refused as
    /// [`PageRefusal::EmptyMembership`].
    MembershipValues { key: String },
}

impl ReadBound {
    /// The most the count may be.
    pub const fn ceiling(&self) -> usize {
        match self {
            ReadBound::PageRows => MAX_PAGE,
            ReadBound::MembershipValues { .. } => IN_VALUES_CEILING,
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
/// reads finding rows and the active fingerprint, and hydrates document rows,
/// through statements the find builder names, so a count, a validate, a
/// describe, a search and a get run find's statements beside their own; the
/// record of what a read ran holds any of them, and each builder's enumeration
/// stays its own.
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
/// so a refusal and a finding say one thing. It maps one-to-one, field for
/// field, onto the wire's `vault/ambiguous-target` detail
/// ([`norn_wire::ErrorDetail::AmbiguousTarget`]); the store keeps its own
/// type because a refusal is typed by what it refuses and the wire detail is
/// one variant of every detail an error carries.
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
pub enum PageRefusal {
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
    /// The cursor names no position among the rows the request pages.
    /// `cursor` is the rows its key names a position among, and `paged` the
    /// rows the request pages. The two differ where the cursor is another
    /// kind of row's, or another collection of a document's; they are equal
    /// where the cursor's key is the paged rows' kind and names no position
    /// among them: a tally's of another grouping width than the request's or
    /// with a member naming no place in its key's order, a finding's at
    /// another path than the document a get pages, a path order's carrying a
    /// sort value, a hit's, a facet's, a finding's or an ordinal's carrying a
    /// schema fingerprint, which no page of those rows mints, or one whose
    /// position is past what the store counts.
    CursorNotTaken { cursor: PagedRows, paged: PagedRows },
    /// The request answers a summary and carries a cursor. A summary answers
    /// every tally at once and is not paged, so no cursor names a position it
    /// continues from.
    SummaryNotPaged,
    /// A membership part on `key` names no value, so no document can satisfy
    /// it. The wire refuses one on read; this is the refusal of one built
    /// in-process.
    EmptyMembership { key: String },
    /// A count the request names is outside the range `bound` holds it to:
    /// `given` rows for a page, or `given` values for the membership part on
    /// the key the bound names.
    OutOfBound { bound: ReadBound, given: usize },
    /// The request carries a part this build of the store does not know:
    /// `part` is [`RequestPart::Unknown`], naming it in words.
    UnknownPart { part: RequestPart },
    /// The target names more than one document.
    AmbiguousTarget(Box<TargetAmbiguity>),
    /// The target names no document.
    UnknownTarget { target: ResolutionTarget },
    /// The request carries `part`, which the answer it asks for — `answer` —
    /// does not take: an anchor or a column on a collection page, a column on
    /// a section or a block, or a cursor or a limit on anything but a
    /// collection page.
    PartNotTaken {
        part: RequestPart,
        answer: AnswerShape,
    },
    /// The store refused a statement: its file, its driver or its environment
    /// refused, and the error is the store's own account. A
    /// [`StoreError::Damaged`] is the store's derived data being damaged, which
    /// the host answers as an entry untrusted while it discards and rebuilds
    /// that data; every other store error is filed as a failed read.
    Store(StoreError),
}

impl std::fmt::Display for PageRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
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
            PageRefusal::OrderChanged(changed) => order_change_told(changed, formatter),
            PageRefusal::CursorNotTaken { cursor, paged } if cursor == paged => write!(
                formatter,
                "the cursor names no position among {} the request pages",
                rows_named(*paged)
            ),
            PageRefusal::CursorNotTaken { cursor, paged } => write!(
                formatter,
                "the cursor names a position among {}, and the request pages {}",
                rows_named(*cursor),
                rows_named(*paged)
            ),
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
                ReadBound::MembershipValues { key } => write!(
                    formatter,
                    "a membership part names at most {} values, and the part on `{key}` named {given}",
                    bound.ceiling()
                ),
            },
            PageRefusal::UnknownPart { part } => {
                write!(formatter, "this store does not know {}", part_named(part))
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
            PageRefusal::PartNotTaken { part, answer } => write!(
                formatter,
                "{} takes no {}",
                answer_named(*answer),
                part_noun(part)
            ),
            PageRefusal::Store(problem) => problem.fmt(formatter),
        }
    }
}

impl std::error::Error for PageRefusal {}

impl PageRefusal {
    /// The refusal of `cursor` on a request paging `paged`: the rows its key
    /// names a position among, and the rows the request pages.
    pub(crate) const fn cursor_not_taken(cursor: &Cursor, paged: PagedRows) -> Self {
        PageRefusal::CursorNotTaken {
            cursor: cursor.key().rows(),
            paged,
        }
    }
}

/// Rows as a refusal names them, in the plural.
fn rows_named(rows: PagedRows) -> String {
    match rows {
        PagedRows::Document => "documents".to_string(),
        PagedRows::Hit => "hits".to_string(),
        PagedRows::Tally => "tallies".to_string(),
        PagedRows::Finding => "findings".to_string(),
        PagedRows::Facet => "facets".to_string(),
        PagedRows::Collection { of } => format!("a document's {}", collection_named(of)),
        _ => "rows".to_string(),
    }
}

/// A part of a request as a refusal names it on its own: with its article,
/// `an anchor`, or in the words a part this build does not know is named in.
fn part_named(part: &RequestPart) -> &str {
    match part {
        RequestPart::Anchor => "an anchor",
        RequestPart::Column => "a column",
        RequestPart::Cursor => "a cursor",
        RequestPart::Limit => "a limit",
        RequestPart::Unknown { name, .. } => name,
        _ => "a part",
    }
}

/// A part of a request as a refusal that negates it names it: the noun
/// alone, `a section takes no anchor`.
fn part_noun(part: &RequestPart) -> &str {
    match part {
        RequestPart::Anchor => "anchor",
        RequestPart::Column => "column",
        RequestPart::Cursor => "cursor",
        RequestPart::Limit => "limit",
        RequestPart::Unknown { name, .. } => name,
        _ => "part",
    }
}

/// An answer shape as a refusal names it, with its article.
fn answer_named(answer: AnswerShape) -> &'static str {
    match answer {
        AnswerShape::CollectionPage => "a collection page",
        AnswerShape::Record => "a record",
        AnswerShape::Section => "a section",
        AnswerShape::Block => "a block",
        AnswerShape::Summary => "a summary",
        _ => "this answer",
    }
}

/// A collection as a refusal names it.
fn collection_named(selector: CollectionSelector) -> &'static str {
    match selector {
        CollectionSelector::Links => "links",
        CollectionSelector::Headings => "headings",
        CollectionSelector::Blocks => "block identifiers",
        CollectionSelector::Tags => "tags",
        CollectionSelector::Findings => "findings",
        _ => "collection",
    }
}

/// An order change as a refusal tells it: the two document orders or the two
/// ladders where they differ, else the two fingerprints, which differ wherever
/// the orders do not.
fn order_change_told(
    changed: &CursorOrderChanged,
    formatter: &mut std::fmt::Formatter<'_>,
) -> std::fmt::Result {
    match changed.orders.as_deref() {
        Some(OrderPair::Document {
            cursor, request, ..
        }) if cursor != request => write!(
            formatter,
            "the cursor was minted in {}, and the request reads {}",
            order_named(cursor),
            order_named(request)
        ),
        Some(OrderPair::Hit {
            cursor, request, ..
        }) if cursor != request => write!(
            formatter,
            "the cursor was ranked by {}, and the request ranks by {}",
            ladder_named(cursor),
            ladder_named(request)
        ),
        _ => write!(
            formatter,
            "the cursor was minted in an order under {}, and the request reads one under {}",
            schema_named(changed.minted_under.as_deref()),
            schema_named(changed.current.as_deref())
        ),
    }
}

/// A document order as a refusal names it: the quoted key or "the path", then
/// the direction.
fn order_named(order: &Sort) -> String {
    let key = match &order.key {
        SortKey::Field { key, .. } => format!("`{key}`"),
        SortKey::Path { .. } => "the path".to_string(),
        _ => "an order".to_string(),
    };
    let direction = match order.direction {
        Direction::Ascending => "ascending",
        Direction::Descending => "descending",
        _ => "in another direction",
    };
    format!("{key} {direction}")
}

/// A ladder as a refusal names it: its rungs in ladder order, as the wire
/// spells each.
fn ladder_named(ladder: &RungSet) -> String {
    let rungs: Vec<&str> = ladder
        .rungs()
        .iter()
        .map(|rung| match rung {
            Rung::Lexical => "lexical",
            Rung::Vector => "vector",
            Rung::Expansion => "expansion",
            Rung::Rerank => "rerank",
            _ => "another rung",
        })
        .collect();
    format!("the ladder [{}]", rungs.join(", "))
}

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
pub fn page_limit(limit: Option<u32>) -> Result<usize, PageRefusal> {
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

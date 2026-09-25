//! The describe builder: the vault's content model, declared and observed, as
//! one page of facets on a read snapshot.
//!
//! The builder is inherent methods on [`Snapshot`], as the find, count and
//! validate builders are, so every statement it runs reads the one instant the
//! snapshot was established at and is counted on the snapshot's own statement
//! counter. **A describe answers keys and declarations only**: what values a
//! key holds and how many documents hold them is a count's to answer.
//!
//! # Where each facet comes from
//!
//! **The declared facets are the pinned declaration's**: the declared fields
//! with their type, whether they are required and their closed set, the
//! declared tags, the tag patterns, the declared folders, the path rules and
//! the stance on an undeclared tag are read off the [`ContentModel`] the
//! host hands over, which is refused unless it was read from the schema the
//! snapshot pins. They are a read of memory and run no statement, drawn from
//! the page's position on, so a page builds no declared facet past the one
//! that says a next page exists. A store with no schema pinned declares
//! nothing, so it answers no declared facet.
//!
//! **The observed fields are the keys documents carry**, one facet per key,
//! each listing every container some document holds it in. They are read from
//! the field pillar's presence rows by the one statement
//! [`DescribeStatement`] names, off the presence index alone: it walks the
//! distinct keys from the page's position, one seek per key, and seeks each
//! key's containers, so **what an observed-field page costs is linear in the
//! keys it pages** — not in the documents carrying them, nor their bodies, nor
//! the keys past the page.
//!
//! # A page is the facets in `(kind, key)` order
//!
//! A page reads one kind after another, in the byte order of the kind's code
//! ([`FacetKind::in_code_order`]) as a validate reads its finding kinds, each
//! kind a section in the byte order of the text that keys its facets — the
//! key a facet's cursor names. The kinds are the request's,
//! or every kind where it names none. A page reads at most one facet past its
//! bound to learn a next page exists, and the cursor it mints names the last
//! facet's kind and key.
//!
//! **The order is no schema's.** A key orders by its bytes whatever the
//! declaration says of it, so a cursor names a position in one order that
//! every schema shares: it is judged positionally, its reading names no
//! fingerprint, and one minted under another schema continues from the same
//! place among whatever the snapshot now answers. A cursor minted under a
//! request naming other kinds continues too, since its position is one in the
//! whole order.

mod statement;

use norn_db::EmittedPlan;
use norn_wire::{
    ContainerKind, Cursor, CursorKey, DescribeParams, DescribeReport, Facet, FacetKind, Moved,
    Page, PagedRows,
};

use crate::error::{self, StoreError};
use crate::fields::{ContentModel, FieldContainer};
use crate::read::{Lookups, PageRefusal, Ran, ReadStatement, Stepped, page_limit};
use crate::store::Snapshot;

use statement::compose_observed;
pub use statement::{DESCRIBE_STATEMENTS, DescribeStatement};

/// What [`Snapshot::describe`] answers: a page of facets and where the next
/// begins.
#[derive(Clone, Debug, PartialEq)]
pub struct Described {
    /// The facets, at most the page bound of them, in `(kind, key)` order.
    pub facets: Vec<Facet>,
    /// Where the next page begins, and `None` where this page is the last.
    pub next: Option<Cursor>,
    /// What moved between the cursor this page continued and the snapshot it
    /// was answered from. Empty on a first page.
    pub moved: Vec<Moved>,
    /// The reading the page was answered from, as a cursor carries it. A
    /// describe's order is no schema's, so it names no fingerprint.
    pub snapshot: norn_wire::Snapshot,
    /// What the describe read.
    pub work: DescribeWork,
}

impl Described {
    /// The report a handler wraps in a [`norn_wire::VaultAnswer`].
    pub fn into_report(self) -> DescribeReport {
        Page::new(self.facets, self.next, self.moved)
    }
}

/// What one describe read, by kind.
///
/// **The step counters are the observed-field statement's cost as SQLite ran
/// it**, read off its own status once its rows are read — never the
/// fingerprint read. A declared facet runs no statement, so it adds nothing
/// to them. A pair of describes over two vaults reads whether a page's work
/// follows the vault by comparing them.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DescribeWork {
    /// The statements the describe ran, each counted on the snapshot as it
    /// ran.
    pub statements: u64,
    /// The facets the page's sections handed back, declared and observed: one
    /// past the bound where a next page exists.
    pub facets_read: u64,
    /// Steps the observed-field statement took through a loop no constraint
    /// bounds — a table or an index read end to end.
    pub full_scan_steps: u64,
    /// Sorts the observed-field statement ran.
    pub sorts: u64,
    /// Virtual-machine operations the observed-field statement ran.
    pub vm_steps: u64,
}

impl DescribeWork {
    fn stepped(&mut self, stepped: Stepped) {
        self.full_scan_steps += stepped.full_scan_steps;
        self.sorts += stepped.sorts;
        self.vm_steps += stepped.vm_steps;
    }
}

/// A statement a describe ran, with the plan SQLite reported for the text and
/// the values it ran with.
#[derive(Clone, Debug)]
pub struct DescribePlan {
    /// The statement, named by the builder that names it: the observed-field
    /// page, or the fingerprint read the find builder names.
    pub statement: ReadStatement,
    pub plan: EmittedPlan,
}

impl Snapshot {
    /// One page of the facets `params` asks for, in `(kind, key)` order,
    /// continuing its cursor.
    ///
    /// `declared` is the vault's declaration, read from the schema the
    /// snapshot pins: every declared facet is read off it. The page holds
    /// `params.limit` facets, [`crate::DEFAULT_PAGE`] where it names none.
    ///
    /// Refused as every read is refused: a page bound outside
    /// `1..=`[`crate::MAX_PAGE`], and a declaration read from another schema
    /// than the snapshot pins. And refused as a cursor that names no position
    /// among facets ([`PageRefusal::CursorNotTaken`]).
    pub fn describe(
        &self,
        params: &DescribeParams,
        declared: &ContentModel,
    ) -> Result<Described, PageRefusal> {
        self.run_describe(params, declared, &mut Lookups::default())
    }

    /// Every statement [`Snapshot::describe`] runs for `params`, in the order
    /// it runs them, each with the plan SQLite reported for it.
    ///
    /// This is the describe itself, run on this snapshot, and each plan is
    /// taken of the very text and values its statement ran with, as
    /// [`Snapshot::find_plans`] takes a find's. A statement the describe did
    /// not run is not listed, so a page of declared facets lists no describe
    /// statement.
    pub fn describe_plans(
        &self,
        params: &DescribeParams,
        declared: &ContentModel,
    ) -> Result<Vec<DescribePlan>, PageRefusal> {
        let mut lookups = Lookups::default();
        self.run_describe(params, declared, &mut lookups)?;
        Ok(
            self.explained(lookups.ran, |statement, _, plan| DescribePlan {
                statement,
                plan,
            })?,
        )
    }

    /// The describe [`Snapshot::describe`] answers and
    /// [`Snapshot::describe_plans`] explains, recording every statement it
    /// runs in `lookups`.
    fn run_describe(
        &self,
        params: &DescribeParams,
        declared: &ContentModel,
        lookups: &mut Lookups,
    ) -> Result<Described, PageRefusal> {
        let started = self.counters().statements_executed();
        let limit = page_limit(params.limit)?;
        self.declaration_pinned(declared, lookups)?;
        let (at, moved) = match &params.after {
            None => (None, Vec::new()),
            Some(cursor) => {
                let CursorKey::Facet { kind, key, .. } = cursor.key() else {
                    return Err(PageRefusal::cursor_not_taken(cursor, PagedRows::Facet));
                };
                let moved = self.judge_reading(cursor, None, false, lookups)?;
                (Some((*kind, key.as_str())), moved)
            }
        };
        let snapshot = self.reading_facts(None, lookups)?;

        let mut work = DescribeWork::default();
        let page = self.read_page(
            sections(&params.facets, at),
            limit,
            &mut lookups.ran,
            |record, (kind, after), rows| match kind {
                FacetKind::ObservedField => self.read_observed(record, after, rows),
                declared_kind => Ok(declared
                    .facets_of(declared_kind, after)
                    .take(rows)
                    .collect()),
            },
        )?;
        work.facets_read = page.read;
        work.stepped(page.stepped);
        let next = page
            .next
            .map(|last| Cursor::new(snapshot.clone(), last.cursor_key()));
        work.statements = self.counters().statements_executed() - started;
        Ok(Described {
            facets: page.rows,
            next,
            moved,
            snapshot,
            work,
        })
    }

    /// At most `rows` observed fields, each after `after`.
    fn read_observed(
        &self,
        record: &mut Vec<Ran>,
        after: Option<&str>,
        rows: usize,
    ) -> Result<Vec<Facet>, StoreError> {
        let section = Ran::new(
            DescribeStatement::ObservedFields,
            compose_observed(after, rows),
        );
        self.run_statement(record, section, |row| {
            let key: String = row.get(0)?;
            let mut containers = Vec::new();
            for (column, container) in FieldContainer::ALL.into_iter().enumerate() {
                if row.get::<_, bool>(column + 1)? {
                    containers.push(container_kind(container));
                }
            }
            Ok(Facet::observed_field(key, containers))
        })
        .map_err(|problem| error::sql("reading a page of observed fields", problem))
    }
}

/// The wire's spelling of a container the presence rows record.
fn container_kind(container: FieldContainer) -> ContainerKind {
    match container {
        FieldContainer::Scalar => ContainerKind::Scalar,
        FieldContainer::Sequence => ContainerKind::Sequence,
        FieldContainer::Map => ContainerKind::Map,
    }
}

/// Where `kind` stands in the order a page reads the kinds in.
fn position(kind: FacetKind) -> usize {
    FacetKind::in_code_order()
        .iter()
        .position(|listed| *listed == kind)
        .expect("the vocabulary lists every kind")
}

/// The sections a page reads from `at` on, in order: one per kind requested —
/// every kind where `requested` names none — each with the key it resumes
/// after.
///
/// A position stands in its kind's section and resumes there; every kind after
/// it starts at its first facet, and every kind before it has been read. The
/// position's kind need not be one the request names.
fn sections<'a>(
    requested: &[FacetKind],
    at: Option<(FacetKind, &'a str)>,
) -> Vec<(FacetKind, Option<&'a str>)> {
    FacetKind::in_code_order()
        .into_iter()
        .filter(|kind| requested.is_empty() || requested.contains(kind))
        .filter_map(|kind| match at {
            None => Some((kind, None)),
            Some((at_kind, key)) if kind == at_kind => Some((kind, Some(key))),
            Some((at_kind, _)) if position(kind) > position(at_kind) => Some((kind, None)),
            Some(_) => None,
        })
        .collect()
}

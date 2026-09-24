//! The validate builder: the findings standing over a vault, read back from
//! the findings pillar on a read snapshot, as a page of finding rows or as one
//! tally per kind.
//!
//! The builder is inherent methods on [`Snapshot`], as the find and count
//! builders are, so every statement it runs reads the one instant the snapshot
//! was established at and is counted on the snapshot's own statement counter.
//! **A validate runs no rule and emits no plan**: findings are derived where
//! documents are derived, and a validate reads what stands. What stands is
//! every finding recorded under the active fingerprint — the fingerprint of
//! the schema the snapshot pins, and the empty text where none is pinned.
//!
//! # A page is the findings in `(kind, path, id)` order
//!
//! A page reads one kind after another, in the kinds' order, and each kind is
//! a section in `(path, id)` order: a seek of the kind's findings from the
//! page's position that stops at the page's bound, so nothing sorts. A page
//! reads at most one finding past its bound to learn a next page exists, and
//! the cursor it mints names the last finding's kind, path and id, where the
//! next page resumes. The kinds are the request's, or every kind where it
//! names none; a severity floor admits the severities at it or above, and
//! where that is one severity the section seeks the findings of that severity
//! alone. [`ValidateStatement`] names what each statement reads.
//!
//! Each finding is answered as the row the finding row accessor reads, which
//! is the one a find's findings column reads: its bounded candidate head, the
//! total it heads, and the hint that names the `find` enumerating its class,
//! all read off what the pillar stores.
//!
//! # A path part judges the finding's path; every other part its document
//!
//! The conjunction is compiled by the one compilation every read builder
//! shares, so a part that cannot be applied is reported as a find reports it,
//! and **a `resolves` part is not applicable**: it answers which documents a
//! target names, which is a find, so a validate reports it
//! ([`Unsatisfied::ResolvesNotApplicable`]) and filters nothing by it.
//!
//! **A path part judges the path the finding stands at**, so a finding where
//! no document row stands — a file that could not be read — is found by a
//! path part naming it. **Every other part judges the document row at the
//! finding's path**: a field, a tag, a full-text match, a finding, each is a
//! fact of a document, and a finding with no document row beside it satisfies
//! none of them, an inequality and an absence included. A part on a predicate
//! key outside the field universe is one of them: it is reported with the keys
//! near it and filters nothing among documents, so it admits every finding
//! standing on a document row and none standing where no row does.
//!
//! # A summary is an aggregate, and is not paged
//!
//! A summary answers how many findings the request admits, one tally per kind
//! and severity, in that order, from one aggregate statement over the index
//! that holds the findings in that order and covers every column a tally
//! reads, so it reads no finding row. Its cells are each one seek of that
//! index, grouped in the index's order; where a document part that keeps what
//! it seeks drives it, the documents matched each take one seek per cell, and
//! the groups are sorted. It answers every tally at once, so it takes no page
//! bound and continues no cursor.

mod statement;

use norn_db::EmittedPlan;
use norn_wire::{
    Cursor, CursorKey, FindingKind, FindingRow, KindTally, Moved, Page, Severity, Unsatisfied,
    ValidateParams, ValidateReport,
};

use crate::error::{self, StoreError};
use crate::fields::DeclaredFields;
use crate::read::{
    Conjunction, FindingBase, Lookups, PageRefusal, Ran, ReadFilter, ReadStatement, Resolution,
    Stepped, finding_base, page_limit,
};
use crate::request::unreadable;
use crate::store::Snapshot;

use statement::{Findings, compose_findings};
pub use statement::{VALIDATE_STATEMENTS, ValidateStatement};

/// What [`Snapshot::validate`] answers: the findings or their tally, and what
/// the request could not apply.
#[derive(Clone, Debug, PartialEq)]
pub struct Validated {
    /// The page of findings, or the tally of them.
    pub answer: Validation,
    /// The parts of the request that could not be applied as asked, in the
    /// order the request names them.
    pub unsatisfied: Vec<Unsatisfied>,
    /// The reading the answer was established under, as a cursor carries it.
    /// A validate's order is no field's, so it names no fingerprint.
    pub snapshot: norn_wire::Snapshot,
    /// What the validate read.
    pub work: ValidateWork,
}

/// What a validate answers with.
#[derive(Clone, Debug, PartialEq)]
pub enum Validation {
    /// A page of findings, in `(kind, path, id)` order.
    Findings {
        /// The findings, at most the page bound of them.
        rows: Vec<FindingRow>,
        /// Where the next page begins, and `None` where this page is the last.
        next: Option<Cursor>,
        /// What moved between the cursor this page continued and the snapshot
        /// it was answered from. Empty on a first page.
        moved: Vec<Moved>,
    },
    /// How many findings stand, one tally per kind and severity, in that
    /// order.
    Summary {
        /// The tallies, one per kind and severity some finding stands under.
        by_kind: Vec<KindTally>,
    },
}

impl Validated {
    /// The unsatisfied parts and the report a handler wraps in a
    /// [`norn_wire::VaultAnswer`].
    pub fn into_report(self) -> (Vec<Unsatisfied>, ValidateReport) {
        let report = match self.answer {
            Validation::Findings { rows, next, moved } => {
                ValidateReport::findings(Page::new(rows, next, moved))
            }
            Validation::Summary { by_kind } => ValidateReport::summary(by_kind),
        };
        (self.unsatisfied, report)
    }
}

/// What one validate read, by kind.
///
/// **The finding counters are the page or summary statements' cost as SQLite
/// ran them**, read off each statement's own status once its rows are read
/// and summed over those statements — never the probes the conjunction's
/// compilation ran, nor the statements that read the page's candidate heads.
/// A pair of validates over two vault sizes reads whether a validate's work
/// grows with the vault by comparing them.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ValidateWork {
    /// The statements the validate ran, each counted on the snapshot as it
    /// ran.
    pub statements: u64,
    /// The rows the page or summary statements handed back: findings, one
    /// past the bound where a next page exists, or tallies.
    pub rows_read: u64,
    /// Steps those statements took through a loop no constraint bounds — a
    /// table or an index read end to end.
    pub full_scan_steps: u64,
    /// Sorts those statements ran: a temporary B-tree an order or a grouping
    /// filled because no index handed its rows back in that order.
    pub sorts: u64,
    /// Virtual-machine operations those statements ran.
    pub vm_steps: u64,
}

impl ValidateWork {
    fn stepped(&mut self, stepped: Stepped) {
        self.full_scan_steps += stepped.full_scan_steps;
        self.sorts += stepped.sorts;
        self.vm_steps += stepped.vm_steps;
    }
}

/// A statement a validate ran, with the plan SQLite reported for the text and
/// the values it ran with.
#[derive(Clone, Debug)]
pub struct ValidatePlan {
    /// The statement, named by the builder that names it: a page or summary
    /// statement, or a statement the conjunction's compilation or the finding
    /// row accessor ran.
    pub statement: ReadStatement,
    /// The filters the statement narrows by, in the request's order.
    pub filters: Vec<ReadFilter>,
    pub plan: EmittedPlan,
}

/// The request compiled: what it narrows the findings by.
struct Narrowing {
    fingerprint: String,
    kinds: Vec<&'static str>,
    severities: Option<Vec<&'static str>>,
    conjunction: Conjunction,
}

impl Snapshot {
    /// The findings `params` asks for, as a page in `(kind, path, id)` order
    /// continuing its cursor, or as one tally per kind and severity.
    ///
    /// `declared` is the vault's declaration, read from the schema the
    /// snapshot pins: it decides how a conjunction's part compares and —
    /// beside the keys documents carry — which predicate keys are known. A
    /// page holds `params.limit` findings, [`crate::DEFAULT_PAGE`] where it
    /// names none; a summary is not paged and ignores the bound.
    ///
    /// Refused as a find is refused, through the same compilation: a page
    /// bound outside `1..=`[`crate::MAX_PAGE`], a membership part naming no
    /// value or more than [`crate::IN_VALUES_CEILING`], a declaration read
    /// from another schema than the snapshot pins, a part the store keeps no
    /// index of, and a bound that does not read as its key's declared type.
    /// And refused as a cursor that names no position among a validate's
    /// findings ([`PageRefusal::NotAFindingCursor`]), and as any cursor on a
    /// summary, which is not paged ([`PageRefusal::SummaryNotPaged`]).
    pub fn validate(
        &self,
        params: &ValidateParams,
        declared: &DeclaredFields,
    ) -> Result<Validated, PageRefusal> {
        self.run_validate(params, declared, &mut Lookups::default())
    }

    /// Every statement [`Snapshot::validate`] runs for `params`, in the order
    /// it runs them, each with the plan SQLite reported for it.
    ///
    /// This is the validate itself, run on this snapshot, and each plan is
    /// taken of the very text and values its statement ran with, as
    /// [`Snapshot::find_plans`] takes a find's. A statement the validate did
    /// not run is not listed.
    pub fn validate_plans(
        &self,
        params: &ValidateParams,
        declared: &DeclaredFields,
    ) -> Result<Vec<ValidatePlan>, PageRefusal> {
        let mut lookups = Lookups::default();
        self.run_validate(params, declared, &mut lookups)?;
        Ok(
            self.explained(lookups.ran, |statement, filters, plan| ValidatePlan {
                statement,
                filters,
                plan,
            })?,
        )
    }

    /// The validate [`Snapshot::validate`] answers and
    /// [`Snapshot::validate_plans`] explains, recording every statement it
    /// runs in `lookups`.
    fn run_validate(
        &self,
        params: &ValidateParams,
        declared: &DeclaredFields,
        lookups: &mut Lookups,
    ) -> Result<Validated, PageRefusal> {
        let started = self.counters().statements_executed();
        let limit = if params.summary {
            if params.after.is_some() {
                return Err(PageRefusal::SummaryNotPaged);
            }
            0
        } else {
            page_limit(params.limit)?
        };
        self.declaration_pinned(declared, lookups)?;
        let conjunction = self.compile_conjunction(
            &params.predicates,
            Resolution::NotApplicable,
            declared,
            lookups,
        )?;
        let narrowing = Narrowing {
            fingerprint: self.fingerprint(lookups)?.unwrap_or_default(),
            kinds: kinds_read(&params.kinds),
            severities: params
                .severity
                .map(|floor| {
                    Severity::ALL
                        .into_iter()
                        .filter(|severity| severity.is_at_least(floor))
                        .map(|severity| severity.as_str())
                        .collect()
                })
                .filter(|admitted: &Vec<&'static str>| admitted.len() < Severity::ALL.len()),
            conjunction,
        };

        let snapshot = self.reading_facts(None, lookups)?;
        let mut work = ValidateWork::default();
        let answer = if params.summary {
            Validation::Summary {
                by_kind: self.summarize(&narrowing, lookups, &mut work)?,
            }
        } else {
            let (resume, moved) = match &params.after {
                None => (None, Vec::new()),
                Some(cursor) => {
                    let CursorKey::Finding { kind, path, id, .. } = cursor.key() else {
                        return Err(PageRefusal::NotAFindingCursor);
                    };
                    let moved = self.judge_reading(cursor, None, false, lookups)?;
                    let id = i64::try_from(*id).map_err(|_| PageRefusal::NotAFindingCursor)?;
                    (Some((kind.as_str(), path.as_str(), id)), moved)
                }
            };
            let (bases, next) =
                self.page_findings(&narrowing, limit, resume, lookups, &mut work)?;
            let next = next
                .map(|last| -> Result<Cursor, StoreError> {
                    let kind = FindingKind::try_from(last.kind.as_str())
                        .map_err(|_| unreadable("findings.kind", &last.kind))?;
                    let id = u64::try_from(last.id)
                        .map_err(|_| unreadable("findings.id", &last.id.to_string()))?;
                    Ok(Cursor::new(
                        snapshot.clone(),
                        CursorKey::finding(kind, last.path, id),
                    ))
                })
                .transpose()?;
            let rows = self.finding_rows(&mut lookups.ran, bases)?;
            Validation::Findings { rows, next, moved }
        };
        let unsatisfied = self.resolve(narrowing.conjunction.reports, declared, lookups)?;
        work.statements = self.counters().statements_executed() - started;
        Ok(Validated {
            answer,
            unsatisfied,
            snapshot,
            work,
        })
    }

    /// One page of findings: at most `limit`, and the finding the next page
    /// continues after.
    fn page_findings(
        &self,
        narrowing: &Narrowing,
        limit: usize,
        at: Option<(&str, &str, i64)>,
        lookups: &mut Lookups,
        work: &mut ValidateWork,
    ) -> Result<(Vec<FindingBase>, Option<FindingBase>), StoreError> {
        let sections: Vec<(&'static str, Option<(&str, i64)>)> =
            if narrowing.conjunction.matches_nothing {
                Vec::new()
            } else {
                sections(&narrowing.kinds, at)
            };
        let shapes: Vec<ReadFilter> = narrowing
            .conjunction
            .filters
            .iter()
            .map(|filter| filter.shape)
            .collect();
        let page = self.read_page(
            sections,
            limit,
            &mut lookups.ran,
            |(kind, after), rows| {
                let composed = compose_findings(&Findings {
                    statement: ValidateStatement::KindPage,
                    fingerprint: &narrowing.fingerprint,
                    kinds: &[kind],
                    severities: narrowing.severities.as_deref(),
                    after,
                    filters: &narrowing.conjunction.filters,
                    on_a_document: narrowing.conjunction.names_unknown_key,
                    rows,
                });
                Ran::new(ValidateStatement::KindPage, composed).narrowed_by(shapes.clone())
            },
            |record, section| {
                self.run_statement(record, section, finding_base)
                    .map_err(|problem| error::sql("reading a page of findings", problem))
            },
        )?;
        work.rows_read = page.read;
        work.stepped(page.stepped);
        Ok((page.rows, page.next))
    }

    /// The tallies `narrowing` admits, one per kind and severity.
    fn summarize(
        &self,
        narrowing: &Narrowing,
        lookups: &mut Lookups,
        work: &mut ValidateWork,
    ) -> Result<Vec<KindTally>, StoreError> {
        if narrowing.conjunction.matches_nothing {
            return Ok(Vec::new());
        }
        let composed = compose_findings(&Findings {
            statement: ValidateStatement::Summary,
            fingerprint: &narrowing.fingerprint,
            kinds: &narrowing.kinds,
            severities: narrowing.severities.as_deref(),
            after: None,
            filters: &narrowing.conjunction.filters,
            on_a_document: narrowing.conjunction.names_unknown_key,
            rows: 0,
        });
        let summary = Ran::new(ValidateStatement::Summary, composed).narrowed_by(
            narrowing
                .conjunction
                .filters
                .iter()
                .map(|filter| filter.shape)
                .collect(),
        );
        let read = self
            .run_statement(&mut lookups.ran, summary, |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, u64>(2)?,
                ))
            })
            .map_err(|problem| error::sql("reading the tally of findings", problem))?;
        work.rows_read = read.len() as u64;
        work.stepped(
            lookups
                .ran
                .last()
                .expect("the summary was just recorded")
                .stepped,
        );
        read.into_iter()
            .map(|(kind, severity, count)| {
                let kind = FindingKind::try_from(kind.as_str())
                    .map_err(|_| unreadable("findings.kind", &kind))?;
                let severity = Severity::try_from(severity.as_str())
                    .map_err(|_| unreadable("findings.severity", &severity))?;
                Ok(KindTally::new(kind, severity, count))
            })
            .collect()
    }
}

/// The kinds a validate reads, as stored, each once in the order a page reads
/// them: the request's, or every kind the registry holds where it names none.
fn kinds_read(requested: &[FindingKind]) -> Vec<&'static str> {
    let named: &[FindingKind] = if requested.is_empty() {
        &FindingKind::ALL
    } else {
        requested
    };
    let mut kinds: Vec<&'static str> = named.iter().map(FindingKind::as_str).collect();
    kinds.sort_unstable();
    kinds.dedup();
    kinds
}

/// The sections a page reads from `at` on, in order: one per kind, each with
/// the `(path, id)` it resumes after.
///
/// A position stands in its kind's section and resumes there; every kind after
/// it starts at its first finding, and every kind before it has been read.
fn sections<'a>(
    kinds: &[&'static str],
    at: Option<(&str, &'a str, i64)>,
) -> Vec<(&'static str, Option<(&'a str, i64)>)> {
    kinds
        .iter()
        .filter_map(|kind| match at {
            None => Some((*kind, None)),
            Some((at_kind, path, id)) if *kind == at_kind => Some((*kind, Some((path, id)))),
            Some((at_kind, ..)) if *kind > at_kind => Some((*kind, None)),
            Some(_) => None,
        })
        .collect()
}

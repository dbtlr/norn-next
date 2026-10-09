//! The repair batch read: the selected findings merged into one path order and
//! paged by document.
//!
//! A validate pages kind-first, one kind's section after another, and a repair
//! plans by document, so it reads the same findings the other way round. A
//! batch merges the kinds' seeks into one read in `(path COLLATE NOCASE, path,
//! position, id)` order ([`ValidateStatement::MergedPage`]), resumes strictly
//! after a path, and never splits one document's selected findings across two
//! batches.
//!
//! # A batch is cut between documents
//!
//! The limit is a soft target in selected findings. A batch reads the first
//! `limit + 1` findings from where it resumes. If they are no more than `limit`
//! they are the batch and nothing remains. Otherwise the finding at the limit
//! decides: when it stands at another path than the last finding inside the
//! limit, the batch is the `limit` findings and more remain; when it stands at
//! the same path, the document is cut, and the rest of its findings are read
//! ([`ValidateStatement::DocumentTail`]) and joined on, so a batch runs past its
//! limit only by its boundary document's remaining findings. Paths are compared
//! bytewise: on a root that tells spellings apart `A.md` and `a.md` fold
//! together and stand adjacent in this order, but they are two documents. When
//! a document was cut, one more finding read past it tells whether more
//! remain.
//!
//! # The cursor names the last document, and only when more remain
//!
//! The cursor a batch mints names the path of the last document it covered,
//! and a batch continues strictly after that path: after `(fold(path), path)`
//! at the greatest position and id, bytewise as well as folded. A batch that
//! reads to the end of the selection mints none, so a cursor's presence says
//! more remain. A repair cursor is minted under no fingerprint, since a raw
//! order of the paths is no schema's, and a validate's cursor names a place in
//! another order, so each is refused as naming no position among the other's
//! rows.

use norn_wire::{
    AnswerAdvisory, Cursor, CursorKey, FindingKind, FindingRow, Moved, PagedRows, Predicate,
    RepairParams, RuleSet, Severity, Unsatisfied,
};

use super::statement::{Findings, compose_findings};
use super::{Narrowing, Selected, ValidatePlan, ValidateStatement, ValidateWork};
use crate::error::{self, StoreError};
use crate::fields::ContentModel;
use crate::read::{FindingBase, Lookups, PageRefusal, Ran, finding_base, page_limit};
use crate::store::Snapshot;

/// What a repair batch selects findings by, and where it resumes.
///
/// It is the selection of a [`RepairParams`] and nothing of its mode or
/// threshold, which decide what a repair writes and not which findings it
/// reads. The selection is compiled as a validate's is.
#[derive(Clone, Copy, Debug)]
pub struct RepairSelection<'a> {
    /// The conjunction a finding's document must satisfy.
    pub predicates: &'a [Predicate],
    /// The kinds selected; empty selects every kind.
    pub kinds: &'a [FindingKind],
    /// The severity selected at or above; `None` selects every severity.
    pub severity: Option<Severity>,
    /// The one rule whose findings are selected; `None` selects every finding.
    pub rule: Option<&'a str>,
    /// The soft target of selected findings a batch holds; `None` leaves it to
    /// [`crate::DEFAULT_PAGE`].
    pub limit: Option<u32>,
    /// The cursor of the batch this one continues; `None` starts at the first
    /// document.
    pub after: Option<&'a Cursor>,
}

impl<'a> From<&'a RepairParams> for RepairSelection<'a> {
    fn from(params: &'a RepairParams) -> Self {
        RepairSelection {
            predicates: &params.predicates,
            kinds: &params.kinds,
            severity: params.severity,
            rule: params.rule.as_deref(),
            limit: params.limit,
            after: params.after.as_ref(),
        }
    }
}

/// What [`Snapshot::repair_batch`] answers: the findings of whole documents in
/// path order, and where the next batch begins.
#[derive(Clone, Debug, PartialEq)]
pub struct RepairBatch {
    /// The selected findings of the documents the batch covers, in `(path
    /// COLLATE NOCASE, path, position, id)` order.
    pub rows: Vec<FindingRow>,
    /// Every rule set the rows cite, each once, in the order of its identity;
    /// empty where no row cites one.
    pub rule_sets: Vec<RuleSet>,
    /// Where the next batch begins: the last document this batch covered. It
    /// is present only where more remain, and minted under no fingerprint.
    pub next: Option<Cursor>,
    /// What moved between the cursor this batch continued and the snapshot it
    /// was answered from. Empty on a first batch.
    pub moved: Vec<Moved>,
    /// The parts of the selection that could not be applied as asked, in the
    /// order the request names them.
    pub unsatisfied: Vec<Unsatisfied>,
    /// What the parts that were applied assumed: a mixed-offset comparison,
    /// once per key.
    pub advisories: Vec<AnswerAdvisory>,
    /// The reading the batch was established under, as a cursor carries it.
    pub snapshot: norn_wire::Snapshot,
    /// What the batch read, counted as a validate counts its page.
    pub work: ValidateWork,
}

impl Snapshot {
    // A dormant carrier: Layer 5B repair (NORN-373) is the consuming layer.
    // The host's repair handler pages a repair through this read; nothing
    // calls it outside this crate's tests until that handler lands.
    /// The selected findings of the next documents in path order, a batch of
    /// about `selection.limit` findings that never splits a document.
    ///
    /// `declared` is the vault's declaration, read from the schema the
    /// snapshot pins, as [`Snapshot::validate`] takes it. The batch is refused
    /// as a validate is, through the same compilation: a limit outside
    /// `1..=`[`crate::MAX_PAGE`], a declaration read from another schema than
    /// the snapshot pins, a rule it does not declare
    /// ([`PageRefusal::UnknownRule`]), and a bound that does not read as its
    /// key's declared type. A cursor that is no repair's, or that carries a
    /// fingerprint, names no position among a repair's documents
    /// ([`PageRefusal::CursorNotTaken`]).
    ///
    pub fn repair_batch(
        &self,
        selection: &RepairSelection<'_>,
        declared: &ContentModel,
    ) -> Result<RepairBatch, PageRefusal> {
        self.run_repair_batch(selection, declared, &mut Lookups::default())
    }

    /// Every statement [`Snapshot::repair_batch`] runs for `selection`, in the
    /// order it runs them, each with the plan SQLite reported for it, as
    /// [`Snapshot::validate_plans`] reports a validate's.
    pub fn repair_batch_plans(
        &self,
        selection: &RepairSelection<'_>,
        declared: &ContentModel,
    ) -> Result<Vec<ValidatePlan>, PageRefusal> {
        let mut lookups = Lookups::default();
        self.run_repair_batch(selection, declared, &mut lookups)?;
        Ok(
            self.explained(lookups.ran, |statement, filters, plan| ValidatePlan {
                statement,
                filters,
                plan,
            })?,
        )
    }

    /// The batch [`Snapshot::repair_batch`] answers and
    /// [`Snapshot::repair_batch_plans`] explains, recording every statement it
    /// runs in `lookups`.
    fn run_repair_batch(
        &self,
        selection: &RepairSelection<'_>,
        declared: &ContentModel,
        lookups: &mut Lookups,
    ) -> Result<RepairBatch, PageRefusal> {
        let started = self.counters().statements_executed();
        let limit = page_limit(selection.limit)?;
        let narrowing = self.narrow(
            &Selected {
                predicates: selection.predicates,
                kinds: selection.kinds,
                severity: selection.severity,
                rule: selection.rule,
            },
            declared,
            lookups,
        )?;
        let snapshot = self.reading_facts(None, lookups)?;
        let (after, moved) = match selection.after {
            None => (None, Vec::new()),
            Some(cursor) => {
                let CursorKey::RepairDocument { path, .. } = cursor.key() else {
                    return Err(PageRefusal::cursor_not_taken(
                        cursor,
                        PagedRows::RepairDocument,
                    ));
                };
                let moved =
                    self.judge_unordered_reading(cursor, PagedRows::RepairDocument, lookups)?;
                (Some(path.as_str()), moved)
            }
        };

        let mut work = ValidateWork::default();
        let (bases, remains) = self.batch_findings(&narrowing, limit, after, lookups, &mut work)?;
        let next = match bases.last() {
            Some(last) if remains => Some(Cursor::new(
                snapshot.clone(),
                CursorKey::repair_document(last.path.clone()),
            )),
            _ => None,
        };
        let rows = self.finding_rows(&mut lookups.ran, bases)?;
        let rule_sets = self.rule_sets(&mut lookups.ran, &rows)?;
        let advisories =
            self.offset_advisories(&narrowing.conjunction.date_comparisons([]), lookups)?;
        let unsatisfied = self.resolve(narrowing.conjunction.reports, declared, lookups)?;
        work.statements = self.counters().statements_executed() - started;
        Ok(RepairBatch {
            rows,
            rule_sets,
            next,
            moved,
            unsatisfied,
            advisories,
            snapshot,
            work,
        })
    }

    /// The findings of one batch, and whether more remain after them: the
    /// first `limit` findings from where the batch resumes, through the last
    /// finding of the document the limit falls inside.
    fn batch_findings(
        &self,
        narrowing: &Narrowing,
        limit: usize,
        after: Option<&str>,
        lookups: &mut Lookups,
        work: &mut ValidateWork,
    ) -> Result<(Vec<FindingBase>, bool), StoreError> {
        if narrowing.conjunction.matches_nothing {
            return Ok((Vec::new(), false));
        }
        let mut found = self.read_merged(
            narrowing,
            ValidateStatement::MergedPage,
            after.map(past_document),
            limit + 1,
            lookups,
            work,
        )?;
        if found.len() <= limit {
            return Ok((found, false));
        }
        let boundary = &found[limit - 1];
        let (path, position, id) = (boundary.path.clone(), boundary.position(), boundary.id);
        let cut = found[limit].path == path;
        found.truncate(limit);
        if !cut {
            return Ok((found, true));
        }
        let tail = self.read_merged(
            narrowing,
            ValidateStatement::DocumentTail,
            Some((&path, position, id)),
            0,
            lookups,
            work,
        )?;
        found.extend(tail);
        let beyond = self.read_merged(
            narrowing,
            ValidateStatement::MergedPage,
            Some(past_document(&path)),
            1,
            lookups,
            work,
        )?;
        Ok((found, !beyond.is_empty()))
    }

    /// The findings one merged statement reads, counted into `work`: `rows`
    /// at most for a merged page, and every finding of the document for its
    /// tail.
    fn read_merged(
        &self,
        narrowing: &Narrowing,
        statement: ValidateStatement,
        after: Option<(&str, i64, i64)>,
        rows: usize,
        lookups: &mut Lookups,
        work: &mut ValidateWork,
    ) -> Result<Vec<FindingBase>, StoreError> {
        let composed = compose_findings(&Findings {
            statement,
            fingerprint: &narrowing.fingerprint,
            rule: narrowing.rule.as_deref(),
            kinds: &narrowing.kinds,
            severities: narrowing.severities.as_deref(),
            after,
            filters: &narrowing.conjunction.filters,
            rows,
        });
        let merged = Ran::new(statement, composed).narrowed_by(narrowing.shapes());
        let found: Vec<FindingBase> = self
            .run_statement(&mut lookups.ran, merged, finding_base)
            .map_err(|problem| error::sql("reading a batch of findings", problem))?
            .into_iter()
            .collect::<Result<_, _>>()?;
        work.rows_read += found.len() as u64;
        work.stepped(
            lookups
                .ran
                .last()
                .expect("the merged read was just recorded")
                .stepped,
        );
        Ok(found)
    }
}

/// The place after every finding at `path`, which a batch resumes from: the
/// greatest position and id there are.
fn past_document(path: &str) -> (&str, i64, i64) {
    (path, i64::MAX, i64::MAX)
}

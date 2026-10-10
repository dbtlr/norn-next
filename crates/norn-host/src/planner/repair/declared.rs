//! Declared fixes: what the schema's owner wrote as the answer to a finding,
//! composed into a document one fix at a time and judged as the applier will
//! judge it.
//!
//! **Three kinds of finding have a declared fix.** The candidates for a
//! selected finding are what the rules its judgment cites — the combined
//! constraint's contributing rules, read on the composed document — declare
//! for it, and a rule declaring nothing for it proposes nothing:
//!
//! - a missing required field (`field/required-missing`) fills from the
//!   default the rules requiring it declare, each filled as the defaults
//!   fixpoint fills one
//!   ([`RuleDefault::fill`](norn_config::schema::RuleDefault::fill)): a
//!   `{{path.<name>}}` from what the rule's own `match.path` binds in the
//!   document's path, and a clock token from the plan's one reading. The
//!   candidates agree only as one written value, as the fixpoint's do: `1`
//!   and `1.0` write different bytes, so they disagree. Repair fills that one
//!   field and cascades nothing: a default whose fill brings in a rule
//!   requiring another field does not fill that field too (ADR 0035's
//!   fixpoint is `new`'s and inbox capture's alone);
//! - a value outside a closed set (`field/not-one-of`) is replaced by the
//!   member a rule's synonym for it maps to ([`synonyms`]), the candidates
//!   agreeing as the field's typed equality reads them;
//! - a forbidden field (`field/forbidden`) is removed or renamed as a rule
//!   declares ([`forbidden`]).
//!
//! **What is skipped, and with what.** No fix declared is
//! [`SkipReason::NoDeclaredFix`], with the value the finding judged; defaults
//! that disagree are [`SkipReason::ConflictingDefaults`], and synonyms or
//! forbidden fixes that disagree are [`SkipReason::Tie`], each with every
//! candidate and the rule proposing it; a default reading a capture its rule's
//! `match.path` binds several ways is [`SkipReason::AmbiguousCapture`], noting
//! two of the bindings; a rename onto a field the composed document already
//! holds is [`SkipReason::RenameOntoOccupiedField`]; a fix that brings in a
//! required field the document lacks is [`SkipReason::BringsInRequiredFields`],
//! naming each such field and the default its rules declare, whatever else it
//! introduces; and a fix that introduces any other violation, or that cannot
//! be set into the document, is [`SkipReason::JudgeWouldRefuse`], with its
//! candidates. **A default reading a clock that gives no reading is no
//! skip**: the repair is refused as a creation is, the default's operation
//! left unresolved ([`Planned::unresolved`]).
//!
//! **Composition.** A document's findings are taken in finding order — kind,
//! field, offending value — and each fix is set into the bytes the earlier
//! fixes left ([`edited`], the composition a `set` writes a field by), then
//! judged against the document's own before-state by the applier's one judge
//! ([`verdict`]): the plan the fixes resolve to is refused exactly where a
//! fix introduces a violation, so a refused fix is skipped and the next
//! composes without it. What a finding is, and whether the composed document
//! still holds it, are read off the same judgment: a row is held where the
//! judgment concludes a finding of its kind on its field whose offending
//! value has the row's head — its bytes, length and hash, so a head cut short
//! still names one value. A selected finding the composed document no longer
//! holds is dropped, neither fixed nor skipped.
//!
//! **A list's elements are fixed one by one into one change to the field.**
//! Every selected `field/not-one-of` finding of one field composes into one
//! `set_frontmatter` of the whole list, each element's replacement judged on
//! the state the earlier ones left; an element whose fix is skipped stands,
//! and never blocks its neighbours'. The one operation cites every finding it
//! fixes.

mod forbidden;
mod synonyms;

use std::collections::BTreeSet;
use std::sync::Arc;

use norn_config::schema::{Breach, VaultSchema};
use norn_wire::{
    AuthoredValue, Binding, Captures, Citation, CitedFinding, Confidence, DocumentPath,
    FindingKind, FindingRow, Operation, OperationId, OperationKind, RequiredField,
    RequiredFieldHead, SchemaViolation, SkipReason, SkippedCandidates, SkippedFinding,
    UnresolvedOperation, UnresolvedReason, ValueCandidate, ValueCandidateHead, ValueHead,
    WriteTarget,
};

use super::{Planned, Repairing};
use crate::applier::{Held, Standing, standing, verdict};
use crate::derivation::{stored_spelling, written_fields};
use crate::planner::edit::edited;

/// Whether `row` is a finding a declared fix may answer, so its document's
/// bytes are read: a missing required field, a value outside a closed set, or
/// a forbidden field.
pub(super) fn has_fix(row: &FindingRow) -> bool {
    matches!(
        row.kind,
        FindingKind::RequiredMissing | FindingKind::NotOneOf | FindingKind::Forbidden
    )
}

/// Compose the declared fixes of `document` — one document's findings, in
/// batch order — onto `before`, the bytes it holds, adding each fix to
/// `planned` with its citation; what each row was decided, in the rows'
/// order: the skip it is left alone with, or `None` where it is fixed or
/// dropped.
pub(super) fn compose(
    document: &[FindingRow],
    before: &Arc<[u8]>,
    repairing: &Repairing<'_>,
    planned: &mut Planned,
) -> Vec<Option<SkippedFinding>> {
    let path = &document[0].path;
    // The before-state is judged once, however many fixes are made to it.
    let composing = Document {
        path,
        before: standing(path, before, repairing.declared, repairing.case),
        repairing,
    };
    let mut state = State {
        bytes: Arc::clone(before),
        holds: composing.before.holds(),
    };
    let mut decided = vec![None; document.len()];
    let order = finding_order(document);
    let mut next = 0;
    while let Some(&at) = order.get(next) {
        next += 1;
        let row = &document[at];
        if !is_rule_kind(row.kind) {
            decided[at] = Some(super::skipped(row));
            continue;
        }
        if row.kind == FindingKind::NotOneOf
            && let Some(field) = row.target.as_deref()
        {
            // The finding order keeps a field's offending values together:
            // they are one list's elements.
            let siblings = order[next..]
                .iter()
                .take_while(|&&other| {
                    document[other].kind == row.kind && document[other].target == row.target
                })
                .count();
            let rows: Vec<(usize, &FindingRow)> = order[next - 1..=next + siblings - 1]
                .iter()
                .map(|&at| (at, &document[at]))
                .collect();
            next += siblings;
            let mapped = synonyms::fix_field(&composing, &state, field, &rows);
            decided_skips(&mut decided, mapped.skipped);
            if let Some(fix) = mapped.fix {
                state = push(planned, fix);
            }
            continue;
        }
        let Some(held) = holding(row, &state.holds) else {
            continue;
        };
        let (true, Some(field)) = (has_fix(row), row.target.as_deref()) else {
            decided[at] = Some(super::skipped(row));
            continue;
        };
        let rules = held.rules.clone();
        let made = match row.kind {
            FindingKind::Forbidden => {
                forbidden::fix(&composing, &state, row, field, &rules).map_err(Unmade::Skipped)
            }
            _ => Composing {
                document: &composing,
                state: &state,
                row,
                field,
            }
            .defaulted(&rules, next_id(planned)),
        };
        match made {
            Ok(fix) => state = push(planned, fix),
            Err(Unmade::Skipped(skip)) => decided[at] = Some(*skip),
            // The repair is refused, as a creation is: the finding is
            // neither fixed nor skipped.
            Err(Unmade::ClockUnread(left)) => planned.unresolved.push(*left),
        }
    }
    decided
}

/// Record each skip of `skipped` against the row position it is for.
fn decided_skips(decided: &mut [Option<SkippedFinding>], skipped: Vec<(usize, SkippedFinding)>) {
    for (at, skip) in skipped {
        decided[at] = Some(skip);
    }
}

/// Add the operations of `fix` to `planned`, each numbered and cited for the
/// findings the fix answers, a later one requiring the one before it; the
/// state the fix composes to.
fn push(planned: &mut Planned, fix: Fix) -> State {
    let mut earlier: Option<OperationId> = None;
    for kind in fix.operations {
        let id = next_id(planned);
        let operation = Operation::new(kind).with_id(id.clone());
        planned.operations.push(match earlier.take() {
            Some(earlier) => operation.with_requires(vec![earlier]),
            None => operation,
        });
        planned
            .citations
            .push(Citation::new(id.clone(), fix.cited.clone()));
        earlier = Some(id);
    }
    fix.state
}

/// The id the next operation of `planned` takes: `repair-1`, `repair-2` and
/// on, in plan order, an operation left unresolved holding its place.
fn next_id(planned: &Planned) -> OperationId {
    let taken = planned.operations.len() + planned.unresolved.len();
    OperationId::new(format!("repair-{}", taken + 1)).expect("a repair's operation id is not empty")
}

/// The positions of `document`'s rows in finding order: kind, field,
/// offending value, then identity.
fn finding_order(document: &[FindingRow]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..document.len()).collect();
    order.sort_by(|&left, &right| {
        let key = |row: &FindingRow| {
            (
                row.kind.as_str(),
                row.target.clone(),
                row.value.as_ref().map(|value| value.text().to_string()),
                row.id,
            )
        };
        key(&document[left]).cmp(&key(&document[right]))
    });
    order
}

/// Whether a finding of `kind` is one a schema rule or a field declaration
/// concludes, which the composed document's judgment says it holds or not.
fn is_rule_kind(kind: FindingKind) -> bool {
    Breach::ALL.iter().any(|breach| breach.kind() == kind)
}

/// The finding of `holds` that `row` names: its kind, its field, and the
/// offending value whose head the row carries.
fn holding<'h>(row: &FindingRow, holds: &'h [Held]) -> Option<&'h Held> {
    holds.iter().find(|held| {
        held.kind == row.kind
            && held.field == row.target
            && held.value.as_deref().map(norn_store::value_head) == row.value
    })
}

/// The value the fields of the document `bytes` spell hold under `name`: none
/// where the document's fields cannot be read, and none (inside) where it
/// holds no such field.
fn field_of(bytes: &[u8], name: &str) -> Option<Option<AuthoredValue>> {
    let fields = written_fields(bytes)?;
    Some(
        fields
            .entries()
            .iter()
            .find(|(held, _)| held == name)
            .map(|(_, value)| value.clone()),
    )
}

/// The citation of the finding `row` fixed by setting `field`, noting where
/// the value written is the repair's own time.
fn cited(row: &FindingRow, field: &str, clocked: bool) -> CitedFinding {
    let cited = CitedFinding::new(row.id, Confidence::Declared);
    let cited = match &row.value {
        Some(value) => cited.with_value(value.clone()),
        None => cited,
    };
    if clocked {
        cited.with_notes(vec![format!(
            "`{field}` is filled from a rule default reading the clock, so the value is the \
             repair's time"
        )])
    } else {
        cited
    }
}

/// One document being composed: where it stands, what it held before any fix,
/// and what the fixes are made under.
struct Document<'c> {
    path: &'c DocumentPath,
    before: Standing,
    repairing: &'c Repairing<'c>,
}

/// What the fixes made so far have composed: the document's bytes, and the
/// findings those bytes hold.
struct State {
    bytes: Arc<[u8]>,
    holds: Vec<Held>,
}

impl Document<'_> {
    /// The schema the document is judged under.
    fn schema(&self) -> &VaultSchema {
        self.repairing.declared.schema()
    }

    /// The operation setting `field` of the document to `value`.
    fn set(&self, field: &str, value: AuthoredValue) -> OperationKind {
        OperationKind::set_frontmatter(WriteTarget::path(self.path.clone()), field, value)
    }

    /// The operation removing `field` from the document.
    fn remove(&self, field: &str) -> OperationKind {
        OperationKind::remove_frontmatter(WriteTarget::path(self.path.clone()), field)
    }

    /// The state `edits` compose `bytes` to, each set into the bytes the one
    /// before left and the result judged against the document's before-state;
    /// or the skip of the finding `row`, with `candidates`, for the reason
    /// the judgment gives: a fix that brings in required fields the document
    /// lacks, one that introduces any other violation, or one that cannot be
    /// set into the document.
    fn admit(
        &self,
        bytes: &Arc<[u8]>,
        edits: &[OperationKind],
        row: &FindingRow,
        candidates: &SkippedCandidates,
    ) -> Result<State, Box<SkippedFinding>> {
        let skip = |reason| SkippedFinding::new(row.id, reason);
        let mut composed = Arc::clone(bytes);
        for edit in edits {
            composed = edited(edit, &composed).map_err(|detail| {
                Box::new(
                    skip(SkipReason::JudgeWouldRefuse)
                        .with_candidates(candidates.clone())
                        .with_note(format!("the fix cannot be set into the document: {detail}")),
                )
            })?;
        }
        let verdict = verdict(
            &self.before,
            self.path,
            &composed,
            self.repairing.declared,
            self.repairing.case,
        );
        let brought = brought_in(&verdict.introduced);
        if !brought.is_empty() {
            return Err(Box::new(
                skip(SkipReason::BringsInRequiredFields).with_required_fields(required_fields(
                    &brought,
                    &verdict.holds,
                    self.schema(),
                )),
            ));
        }
        if !verdict.introduced.is_empty() {
            return Err(Box::new(
                skip(SkipReason::JudgeWouldRefuse).with_candidates(candidates.clone()),
            ));
        }
        Ok(State {
            bytes: composed,
            holds: verdict.holds,
        })
    }
}

/// Why a fix is not made.
enum Unmade {
    /// The finding is left alone, for the reason and with the data it names.
    Skipped(Box<SkippedFinding>),
    /// The fix reads a clock that gives no reading, so the repair is refused
    /// as a creation is: the operation the fix would have been, as its
    /// default is written, left unresolved.
    ClockUnread(Box<UnresolvedOperation>),
}

/// A fix the judge admits: the operations it makes, in order, each answering
/// the findings `cited`, and the state they compose to.
struct Fix {
    operations: Vec<OperationKind>,
    cited: Vec<CitedFinding>,
    state: State,
}

/// One value a rule's declaration proposes for a field.
struct Proposal {
    value: AuthoredValue,
    rule: String,
    clocked: bool,
}

/// One missing required field's fix being composed: a selected finding's, on
/// the state the earlier fixes left.
struct Composing<'c> {
    document: &'c Document<'c>,
    state: &'c State,
    row: &'c FindingRow,
    field: &'c str,
}

/// What the defaults of the rules requiring a field propose for it.
enum Proposed {
    /// The value each rule declaring a default fills it to, in rule name
    /// order.
    Candidates(Vec<Proposal>),
    /// A default reading a capture its rule's `match.path` binds several ways.
    AmbiguousCapture {
        rule: String,
        bindings: Box<[Captures; 2]>,
    },
    /// A default reading the clock, which gives no reading a default can
    /// fill: the default as its rule writes it.
    NoClockReading { source: AuthoredValue },
}

impl Composing<'_> {
    /// The fix of a missing required field — the default `rules`, the rules
    /// requiring it, declare — or why it is not made. `id` is the operation's
    /// id, which an operation left unresolved takes.
    fn defaulted(&self, rules: &BTreeSet<String>, id: OperationId) -> Result<Fix, Unmade> {
        let skip = |reason| SkippedFinding::new(self.row.id, reason);
        let skipped = |finding| Err(Unmade::Skipped(Box::new(finding)));
        let proposals = match self.proposed(rules) {
            Proposed::Candidates(proposals) => proposals,
            Proposed::AmbiguousCapture { rule, bindings } => {
                return skipped(skip(SkipReason::AmbiguousCapture).with_note(format!(
                    "the rule `{rule}` defaults `{}` from a capture its `match.path` binds \
                     several ways in `{}`: {} and {}",
                    self.field,
                    self.document.path,
                    spelled(&bindings[0]),
                    spelled(&bindings[1]),
                )));
            }
            Proposed::NoClockReading { source } => {
                let operation = Operation::new(self.document.set(self.field, source)).with_id(id);
                let reason = UnresolvedReason::no_longer_resolves(crate::clock::cannot_fill(
                    &format!("the rule default for `{}`", self.field),
                ));
                return Err(Unmade::ClockUnread(Box::new(UnresolvedOperation::new(
                    operation, reason,
                ))));
            }
        };
        let Some((first, rest)) = proposals.split_first() else {
            return skipped(skip(SkipReason::NoDeclaredFix));
        };
        if rest.iter().any(|proposal| proposal.value != first.value) {
            return skipped(
                skip(SkipReason::ConflictingDefaults).with_candidates(candidates(&proposals)),
            );
        }
        let set = self.document.set(self.field, first.value.clone());
        let state = self
            .document
            .admit(
                &self.state.bytes,
                std::slice::from_ref(&set),
                self.row,
                &candidates(&proposals),
            )
            .map_err(Unmade::Skipped)?;
        let clocked = proposals.iter().any(|proposal| proposal.clocked);
        Ok(Fix {
            operations: vec![set],
            cited: vec![cited(self.row, self.field, clocked)],
            state,
        })
    }

    /// What the default each of `rules` declares for the field fills to, in
    /// rule name order, the clock read only for a default reading it.
    fn proposed(&self, rules: &BTreeSet<String>) -> Proposed {
        let repairing = self.document.repairing;
        let schema = self.document.schema();
        let mut proposals = Vec::new();
        for name in rules {
            let Some(rule) = schema.rule(name) else {
                continue;
            };
            let Some(default) = rule
                .required()
                .find(|(required, _)| *required == self.field)
                .and_then(|(_, default)| default)
            else {
                continue;
            };
            let captures = if default.reads_captures() {
                match rule
                    .selector()
                    .path()
                    .map(|glob| glob.bind(self.document.path.as_str(), repairing.case))
                {
                    Some(Binding::Unique(captures)) => captures,
                    Some(Binding::Several(bindings)) => {
                        return Proposed::AmbiguousCapture {
                            rule: name.clone(),
                            bindings,
                        };
                    }
                    // A rule selecting the document matches its path; a
                    // default reading a capture is refused at read where its
                    // rule has no `match.path`.
                    Some(Binding::Unmatched) | None => Captures::default(),
                }
            } else {
                Captures::default()
            };
            let at = if default.reads_clock() {
                match repairing.clock.get() {
                    Ok(at) => Some(at),
                    Err(_) => {
                        return Proposed::NoClockReading {
                            source: default.source(),
                        };
                    }
                }
            } else {
                None
            };
            let value = default.fill(at, captures).expect(
                "a rule default's tokens are the clock's and its own rule's captures, judged at read",
            );
            proposals.push(Proposal {
                value,
                rule: name.clone(),
                clocked: default.reads_clock(),
            });
        }
        Proposed::Candidates(proposals)
    }
}

/// Each field a missing-required violation among `introduced` names, once,
/// in key order.
fn brought_in(introduced: &[SchemaViolation]) -> BTreeSet<&str> {
    introduced
        .iter()
        .filter(|violation| violation.kind == FindingKind::RequiredMissing)
        .filter_map(|violation| violation.target.as_deref())
        .collect()
}

/// The fields `brought` names, each with the default the rules requiring it
/// on the composed document — those its finding among `holds` cites —
/// declare, as the schema writes it, where they declare one and agree on it.
fn required_fields(
    brought: &BTreeSet<&str>,
    holds: &[Held],
    schema: &VaultSchema,
) -> RequiredFieldHead {
    let fields = brought.iter().map(|field| {
        let rules = holds
            .iter()
            .find(|held| {
                held.kind == FindingKind::RequiredMissing && held.field.as_deref() == Some(field)
            })
            .map(|held| &held.rules);
        let declared: Vec<AuthoredValue> = rules
            .into_iter()
            .flatten()
            .filter_map(|name| schema.rule(name))
            .filter_map(|rule| {
                rule.required()
                    .find(|(required, _)| required == field)
                    .and_then(|(_, default)| default)
                    .map(|default| default.source())
            })
            .collect();
        let required = RequiredField::new(*field);
        match declared.split_first() {
            Some((first, rest)) if rest.iter().all(|value| value == first) => {
                required.with_default(head(first))
            }
            _ => required,
        }
    });
    RequiredFieldHead::new(fields, brought.len() as u64).expect("a head of every field it names")
}

/// Every proposal, as the value candidates a skip carries.
fn candidates(proposals: &[Proposal]) -> SkippedCandidates {
    spelled_candidates(
        proposals
            .iter()
            .map(|proposal| (head(&proposal.value), proposal.rule.as_str())),
    )
}

/// The candidates `proposed` names, each a value head and the rule proposing
/// it, as a skip carries them.
fn spelled_candidates<'p>(
    proposed: impl ExactSizeIterator<Item = (ValueHead, &'p str)>,
) -> SkippedCandidates {
    let total = proposed.len() as u64;
    let candidates = proposed.map(|(value, rule)| ValueCandidate::new(value).by_rule(rule));
    SkippedCandidates::values(
        ValueCandidateHead::new(candidates, total).expect("a head of every candidate it names"),
    )
}

/// `value`'s head, spelled as the store keeps a field value. A default is
/// never null, which alone has no spelling.
fn head(value: &AuthoredValue) -> ValueHead {
    norn_store::value_head(&stored_spelling(value).unwrap_or_default())
}

/// One binding of a rule's captures, as a note names it.
fn spelled(captures: &Captures) -> String {
    let named: Vec<String> = captures
        .iter()
        .map(|(name, segment)| format!("{name}={segment}"))
        .collect();
    format!("{{{}}}", named.join(", "))
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::BTreeMap;

    use norn_config::schema::{LocalTimestamp, NotALocalTimestamp, VaultSchema};
    use norn_wire::{CandidateHead, Severity};

    use super::super::{Before, Planned, plan};
    use super::*;
    use crate::clock::OneReading;
    use crate::derivation::Declared;

    /// The one reading every case's clock gives: 1 October 2026, 09:30:15,
    /// two hours east of UTC.
    fn reading() -> LocalTimestamp {
        LocalTimestamp::new(2026, 10, 1, 9, 30, 15, 120).expect("a reading")
    }

    /// The vault the cases plan over: its schema, and the bytes each document
    /// holds.
    struct Vault {
        declared: Declared,
        documents: BTreeMap<String, Arc<[u8]>>,
    }

    impl Vault {
        fn of(schema: &str, documents: &[(&str, &str)]) -> Self {
            Vault {
                declared: Declared::pinned(
                    VaultSchema::parse(schema.as_bytes()).expect("a schema"),
                    "a fingerprint",
                ),
                documents: documents
                    .iter()
                    .map(|(path, text)| (path.to_string(), Arc::from(text.as_bytes())))
                    .collect(),
            }
        }

        /// `rows` planned over the vault, the clock giving the one reading
        /// and counting how often it is read in `clock_reads`; and every
        /// document read, in the order read.
        fn planned(
            &self,
            rows: &[FindingRow],
            clock_reads: &Cell<usize>,
        ) -> (Planned, Vec<String>) {
            let clock = || -> Result<LocalTimestamp, NotALocalTimestamp> {
                clock_reads.set(clock_reads.get() + 1);
                Ok(reading())
            };
            let one = OneReading::of(&clock);
            let repairing = Repairing {
                declared: &self.declared,
                case: norn_wire::CaseFold::Exact,
                clock: &one,
            };
            let reads = RefCell::new(Vec::new());
            let read = &mut |path: &DocumentPath| -> Result<Before, std::convert::Infallible> {
                reads.borrow_mut().push(path.as_str().to_string());
                Ok(match self.documents.get(path.as_str()) {
                    Some(bytes) => Before::Held(Arc::clone(bytes)),
                    None => Before::Unread,
                })
            };
            let Ok(planned) = plan(rows, &repairing, read);
            (planned, reads.into_inner())
        }

        /// `rows` planned over the vault.
        fn plan(&self, rows: &[FindingRow]) -> Planned {
            self.planned(rows, &Cell::new(0)).0
        }

        /// `rows` planned over the vault, the clock giving no reading.
        fn planned_unread(&self, rows: &[FindingRow]) -> Planned {
            let clock =
                || -> Result<LocalTimestamp, NotALocalTimestamp> { Err(NotALocalTimestamp) };
            let one = OneReading::of(&clock);
            let repairing = Repairing {
                declared: &self.declared,
                case: norn_wire::CaseFold::Exact,
                clock: &one,
            };
            let read = &mut |path: &DocumentPath| -> Result<Before, std::convert::Infallible> {
                Ok(match self.documents.get(path.as_str()) {
                    Some(bytes) => Before::Held(Arc::clone(bytes)),
                    None => Before::Unread,
                })
            };
            let Ok(planned) = plan(rows, &repairing, read);
            planned
        }
    }

    /// The finding `id`: `field`, which the rules of the document at `at`
    /// require, missing.
    fn missing(id: u64, at: &str, field: &str) -> FindingRow {
        finding(id, FindingKind::RequiredMissing, at, field)
    }

    /// The finding `id` of `kind` on `field` of the document at `at`, naming
    /// no value.
    fn finding(id: u64, kind: FindingKind, at: &str, field: &str) -> FindingRow {
        FindingRow::new(
            id,
            kind,
            Severity::Error,
            DocumentPath::new(at).expect("a document path"),
            Some(field.to_string()),
            None,
            CandidateHead::new([], 0).expect("an empty head"),
            None,
            "a finding",
            1,
        )
    }

    /// The operation `repair-<n>`, setting `field` of `at` to `value`.
    fn set(n: usize, at: &str, field: &str, value: &str) -> Operation {
        Operation::new(OperationKind::set_frontmatter(
            WriteTarget::path(DocumentPath::new(at).expect("a document path")),
            field,
            AuthoredValue::string(value),
        ))
        .with_id(OperationId::new(format!("repair-{n}")).expect("an id"))
    }

    /// The citation of `repair-<n>` fixing the findings `ids`, each about no
    /// value, at the declared level.
    fn citing(n: usize, ids: &[u64]) -> Citation {
        Citation::new(
            OperationId::new(format!("repair-{n}")).expect("an id"),
            ids.iter()
                .map(|id| CitedFinding::new(*id, Confidence::Declared))
                .collect(),
        )
    }

    /// The candidates `values`, each with the rule proposing it.
    fn values(values: &[(&str, &str)]) -> SkippedCandidates {
        SkippedCandidates::values(
            ValueCandidateHead::new(
                values.iter().map(|(value, rule)| {
                    ValueCandidate::new(norn_store::value_head(value)).by_rule(*rule)
                }),
                values.len() as u64,
            )
            .expect("a head"),
        )
    }

    const TASKS: &str = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    required:\n      status: {default: todo}\n";

    /// **A missing required field fills from its rule default**: one
    /// `set_frontmatter` of the default, cited at the declared level.
    #[test]
    fn a_missing_required_field_fills_from_its_rule_default() {
        let vault = Vault::of(TASKS, &[("a.md", "---\ntype: task\n---\n# A\n")]);

        let planned = vault.plan(&[missing(7, "a.md", "status")]);

        assert_eq!(planned.operations, vec![set(1, "a.md", "status", "todo")]);
        assert_eq!(planned.citations, vec![citing(1, &[7])]);
        assert!(planned.skipped.is_empty());
    }

    /// **A required field no rule declares a default for has no fix**, and
    /// is skipped as such.
    #[test]
    fn a_required_field_no_rule_defaults_skips_as_no_declared_fix() {
        let schema = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    required:\n      status:\n";
        let vault = Vault::of(schema, &[("a.md", "---\ntype: task\n---\n")]);

        let planned = vault.plan(&[missing(7, "a.md", "status")]);

        assert!(planned.operations.is_empty());
        assert_eq!(
            planned.skipped,
            vec![SkippedFinding::new(7, SkipReason::NoDeclaredFix)]
        );
    }

    /// **Co-selecting rules whose defaults disagree skip the field as
    /// conflicting defaults, naming both candidates and their rules, while an
    /// unrelated fix on the same document applies.** `1` and `1.0` would
    /// disagree too: candidates agree only as one written value.
    #[test]
    fn disagreeing_defaults_skip_as_conflicting_defaults_while_an_unrelated_fix_applies() {
        let schema = "version: 1\nrules:\n  a-rule:\n    match: {frontmatter: {type: task}}\n    required:\n      status: {default: todo}\n      priority: {default: normal}\n  b-rule:\n    match: {frontmatter: {type: task}}\n    required:\n      status: {default: doing}\n";
        let vault = Vault::of(schema, &[("a.md", "---\ntype: task\n---\n")]);

        let planned = vault.plan(&[missing(1, "a.md", "priority"), missing(2, "a.md", "status")]);

        assert_eq!(
            planned.operations,
            vec![set(1, "a.md", "priority", "normal")]
        );
        assert_eq!(planned.citations, vec![citing(1, &[1])]);
        assert_eq!(
            planned.skipped,
            vec![
                SkippedFinding::new(2, SkipReason::ConflictingDefaults)
                    .with_candidates(values(&[("todo", "a-rule"), ("doing", "b-rule")]))
            ]
        );
    }

    /// **A default reading a capture its rule's `match.path` binds several
    /// ways skips as an ambiguous capture**, noting two of the bindings.
    #[test]
    fn a_default_reading_a_capture_bound_several_ways_skips_as_ambiguous_capture() {
        let schema = "version: 1\nrules:\n  areas:\n    match: {path: '**/<area>/**'}\n    required:\n      area: {default: '{{path.area}}'}\n";
        let vault = Vault::of(schema, &[("red/blue/a.md", "---\ntitle: A\n---\n")]);

        let planned = vault.plan(&[missing(3, "red/blue/a.md", "area")]);

        assert!(planned.operations.is_empty());
        let [skipped] = planned.skipped.as_slice() else {
            panic!("one skip: {:?}", planned.skipped);
        };
        assert_eq!(skipped.reason, SkipReason::AmbiguousCapture);
        let note = skipped
            .note
            .as_deref()
            .expect("the skip notes the bindings");
        assert!(
            note.contains("{area=red}") && note.contains("{area=blue}"),
            "{note}"
        );
    }

    /// **A default fill bringing a document under a forbidding rule skips as
    /// one the judge would refuse**, with its candidate: `kind: special`
    /// brings in the rule forbidding the `scratch` the document holds.
    #[test]
    fn a_default_fill_bringing_a_document_under_a_forbidding_rule_skips_as_judge_would_refuse() {
        let schema = "version: 1\nrules:\n  kinds:\n    match: {frontmatter: {type: task}}\n    required:\n      kind: {default: special}\n  specials:\n    match: {frontmatter: {kind: special}}\n    forbidden:\n      scratch:\n";
        let vault = Vault::of(schema, &[("a.md", "---\ntype: task\nscratch: x\n---\n")]);

        let planned = vault.plan(&[missing(4, "a.md", "kind")]);

        assert!(planned.operations.is_empty());
        assert_eq!(
            planned.skipped,
            vec![
                SkippedFinding::new(4, SkipReason::JudgeWouldRefuse)
                    .with_candidates(values(&[("special", "kinds")]))
            ]
        );
    }

    const KINDS: &str = "version: 1\nrules:\n  kinds:\n    match: {frontmatter: {type: work}}\n    required:\n      kind: {default: task}\n  tasks:\n    match: {frontmatter: {kind: task}}\n    required:\n      status: {default: todo}\n";

    /// **Filling `kind` where `kind: task` brings in a rule requiring
    /// `status` skips as bringing in required fields, naming `status` and
    /// its declared default, and fills no `status`**: repair cascades no
    /// default.
    #[test]
    fn filling_kind_that_brings_in_a_rule_requiring_status_skips_as_brings_in_required_fields() {
        let vault = Vault::of(KINDS, &[("a.md", "---\ntype: work\n---\n")]);

        let planned = vault.plan(&[missing(5, "a.md", "kind")]);

        assert!(planned.operations.is_empty());
        assert!(planned.citations.is_empty());
        let fields = RequiredFieldHead::new(
            [RequiredField::new("status").with_default(norn_store::value_head("todo"))],
            1,
        )
        .expect("a head");
        assert_eq!(
            planned.skipped,
            vec![
                SkippedFinding::new(5, SkipReason::BringsInRequiredFields)
                    .with_required_fields(fields)
            ]
        );
    }

    const COMPOSED: &str = "version: 1\nrules:\n  base:\n    match: {frontmatter: {type: task}}\n    required:\n      a_owner: {default: me}\n      kind: {default: special}\n      z_due: {default: soon}\n  specials:\n    match: {frontmatter: {kind: special}}\n    forbidden:\n      scratch:\n";

    /// **Fixes compose in finding order, and a fix the judge refuses skips
    /// while earlier ones stand**: `a_owner` fills, `kind` would bring in
    /// the rule forbidding `scratch` and skips, and `z_due` composes without
    /// it — whatever order the batch read the rows in.
    #[test]
    fn fixes_compose_in_finding_order_and_a_fix_the_judge_refuses_skips_while_earlier_ones_stand() {
        let vault = Vault::of(COMPOSED, &[("a.md", "---\ntype: task\nscratch: x\n---\n")]);

        let planned = vault.plan(&[
            missing(9, "a.md", "z_due"),
            missing(8, "a.md", "kind"),
            missing(7, "a.md", "a_owner"),
        ]);

        assert_eq!(
            planned.operations,
            vec![
                set(1, "a.md", "a_owner", "me"),
                set(2, "a.md", "z_due", "soon")
            ]
        );
        assert_eq!(planned.citations, vec![citing(1, &[7]), citing(2, &[9])]);
        assert_eq!(
            planned.skipped,
            vec![
                SkippedFinding::new(8, SkipReason::JudgeWouldRefuse)
                    .with_candidates(values(&[("special", "base")]))
            ]
        );
    }

    /// **A selected finding the composed document no longer holds is
    /// dropped**, in neither the operations nor the skipped findings: the
    /// row says `status` is missing, and the document holds one.
    #[test]
    fn a_selected_finding_the_document_no_longer_holds_is_dropped() {
        let vault = Vault::of(TASKS, &[("a.md", "---\ntype: task\nstatus: done\n---\n")]);

        let planned = vault.plan(&[missing(7, "a.md", "status")]);

        assert_eq!(planned, Planned::default());
    }

    /// **A document whose bytes cannot be read skips the findings a fix
    /// would answer as unreadable**, and the rest as their rows say.
    #[test]
    fn a_document_that_cannot_be_read_skips_its_fixable_findings_as_unreadable() {
        let vault = Vault::of(TASKS, &[]);

        let planned = vault.plan(&[
            missing(7, "gone.md", "status"),
            finding(8, FindingKind::NotOneOf, "gone.md", "status"),
            finding(9, FindingKind::Forbidden, "gone.md", "scratch"),
            finding(10, FindingKind::Broken, "gone.md", "nowhere"),
        ]);

        assert!(planned.operations.is_empty());
        assert_eq!(
            planned.skipped,
            vec![
                SkippedFinding::new(7, SkipReason::Unreadable),
                SkippedFinding::new(8, SkipReason::Unreadable),
                SkippedFinding::new(9, SkipReason::Unreadable),
                SkippedFinding::new(10, SkipReason::NoDeclaredFix),
            ]
        );
    }

    /// **Only a document a fix may be made to is read, and once**, however
    /// many of its findings the batch holds.
    #[test]
    fn only_a_document_a_fix_may_be_made_to_is_read_and_once() {
        let vault = Vault::of(
            TASKS,
            &[
                ("a.md", "---\ntype: task\n---\n"),
                ("b.md", "---\ntype: task\nstatus: x\n---\n"),
            ],
        );

        let (_, reads) = vault.planned(
            &[
                missing(1, "a.md", "status"),
                finding(2, FindingKind::Broken, "a.md", "nowhere"),
                finding(3, FindingKind::TooLong, "b.md", "status"),
            ],
            &Cell::new(0),
        );

        assert_eq!(reads, ["a.md"]);
    }

    const STAMPED: &str = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    required:\n      created: {default: '{{now}}'}\n      day: {default: '{{date}}'}\n";

    /// **Every default a plan fills comes from one clock reading**, across
    /// fields and documents, each operation noting the value is the repair's
    /// time; **a plan filling no default that reads the clock reads none.**
    #[test]
    fn every_default_a_plan_fills_comes_from_one_clock_reading_and_none_reads_none() {
        let vault = Vault::of(
            STAMPED,
            &[
                ("a.md", "---\ntype: task\n---\n"),
                ("b.md", "---\ntype: task\n---\n"),
            ],
        );
        let reads = Cell::new(0);

        let (planned, _) = vault.planned(
            &[
                missing(1, "a.md", "created"),
                missing(2, "a.md", "day"),
                missing(3, "b.md", "created"),
            ],
            &reads,
        );

        assert_eq!(reads.get(), 1);
        assert_eq!(
            planned.operations,
            vec![
                set(1, "a.md", "created", "2026-10-01T09:30:15+02:00"),
                set(2, "a.md", "day", "2026-10-01"),
                set(3, "b.md", "created", "2026-10-01T09:30:15+02:00"),
            ]
        );
        for citation in &planned.citations {
            let [cited] = citation.findings.as_slice() else {
                panic!("one finding per fix: {citation:?}");
            };
            assert!(
                cited
                    .notes
                    .iter()
                    .any(|note| note.contains("the repair's time")),
                "{cited:?}"
            );
        }

        let unstamped = Vault::of(TASKS, &[("a.md", "---\ntype: task\n---\n")]);
        let reads = Cell::new(0);
        let (planned, _) = unstamped.planned(&[missing(1, "a.md", "status")], &reads);
        assert_eq!(planned.operations.len(), 1);
        assert_eq!(reads.get(), 0);
        assert!(planned.citations[0].findings[0].notes.is_empty());
    }

    /// **A document's before-state is judged once, however many fixes are made
    /// to it**: the rule work a plan pays is the judgment of the before-state
    /// and of each composed after-state, and no more.
    #[test]
    fn a_documents_before_state_is_judged_once_however_many_fixes_are_made() {
        let schema = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    required:\n      priority: {default: normal}\n      status: {default: todo}\n";
        let vault = Vault::of(schema, &[("a.md", "---\ntype: task\n---\n")]);
        let judged = |bytes: &[u8]| {
            let hash = norn_fs::ContentHash::of(bytes).to_string();
            crate::derivation::plan_document(
                std::path::Path::new("a.md"),
                "a.md",
                bytes,
                hash,
                None,
                &vault.declared,
                norn_wire::CaseFold::Exact,
            )
            .rule_work
        };
        let path = DocumentPath::new("a.md").expect("a path");
        let mut states = vec![Arc::<[u8]>::from("---\ntype: task\n---\n".as_bytes())];
        for (field, value) in [("priority", "normal"), ("status", "todo")] {
            let set = OperationKind::set_frontmatter(
                WriteTarget::path(path.clone()),
                field,
                AuthoredValue::string(value),
            );
            let next = edited(&set, states.last().expect("a state")).expect("an edit");
            states.push(next);
        }
        let expected = states
            .iter()
            .fold(norn_config::schema::RuleWork::NONE, |work, bytes| {
                work.plus(judged(bytes))
            });

        let (planned, paid) = crate::evidence::rule_work_of(|| {
            vault.plan(&[missing(1, "a.md", "priority"), missing(2, "a.md", "status")])
        });

        assert_eq!(planned.operations.len(), 2);
        assert_eq!(paid, expected);
    }

    /// **A clock that gives no reading leaves each default that reads it
    /// unresolved, as `new` leaves a creation, and skips none**: the operation
    /// is the default as the schema writes it, numbered as it would be, the
    /// reason names the clock, and a default reading no clock still plans.
    #[test]
    fn a_clock_that_gives_no_reading_leaves_each_clock_default_unresolved() {
        let schema = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    required:\n      created: {default: '{{now}}'}\n      status: {default: todo}\n";
        let vault = Vault::of(schema, &[("a.md", "---\ntype: task\n---\n")]);

        let planned =
            vault.planned_unread(&[missing(1, "a.md", "created"), missing(2, "a.md", "status")]);

        assert_eq!(planned.operations, vec![set(2, "a.md", "status", "todo")]);
        assert!(planned.skipped.is_empty(), "{:?}", planned.skipped);
        let [left] = planned.unresolved.as_slice() else {
            panic!("one unresolved: {:?}", planned.unresolved);
        };
        assert_eq!(
            left.operation,
            set(1, "a.md", "created", "{{now}}"),
            "the default as the schema writes it"
        );
        let UnresolvedReason::NoLongerResolves { detail, .. } = &left.reason else {
            panic!("left out for {:?}", left.reason);
        };
        assert_eq!(
            detail,
            &crate::clock::cannot_fill("the rule default for `created`")
        );
    }

    /// **Operations are numbered `repair-1`, `repair-2` and on in plan order
    /// across documents, each cited at the declared level.**
    #[test]
    fn operations_are_numbered_repair_n_and_cited_at_declared_confidence() {
        let vault = Vault::of(
            TASKS,
            &[
                ("a.md", "---\ntype: task\n---\n"),
                ("b.md", "---\ntype: task\n---\n"),
            ],
        );

        let planned = vault.plan(&[missing(4, "a.md", "status"), missing(6, "b.md", "status")]);

        assert_eq!(
            planned.operations,
            vec![
                set(1, "a.md", "status", "todo"),
                set(2, "b.md", "status", "todo")
            ]
        );
        assert_eq!(planned.citations, vec![citing(1, &[4]), citing(2, &[6])]);
    }

    /// The finding `id` of `kind` on `field` of the document at `at`,
    /// offending with `value`.
    fn offending(id: u64, kind: FindingKind, at: &str, field: &str, value: &str) -> FindingRow {
        finding(id, kind, at, field).with_value(norn_store::value_head(value))
    }

    /// The list of strings `items`.
    fn list(items: &[&str]) -> AuthoredValue {
        AuthoredValue::List(
            items
                .iter()
                .map(|item| AuthoredValue::string(*item))
                .collect(),
        )
    }

    /// The operation `repair-<n>`, setting `field` of `at` to `value`.
    fn set_to(n: usize, at: &str, field: &str, value: AuthoredValue) -> Operation {
        Operation::new(OperationKind::set_frontmatter(
            WriteTarget::path(DocumentPath::new(at).expect("a document path")),
            field,
            value,
        ))
        .with_id(OperationId::new(format!("repair-{n}")).expect("an id"))
    }

    /// The operation `repair-<n>`, removing `field` of `at`.
    fn removal(n: usize, at: &str, field: &str) -> Operation {
        Operation::new(OperationKind::remove_frontmatter(
            WriteTarget::path(DocumentPath::new(at).expect("a document path")),
            field,
        ))
        .with_id(OperationId::new(format!("repair-{n}")).expect("an id"))
    }

    /// The citation of `repair-<n>` fixing the findings `fixed`, each about
    /// its value, at the declared level.
    fn citing_values(n: usize, fixed: &[(u64, &str)]) -> Citation {
        Citation::new(
            OperationId::new(format!("repair-{n}")).expect("an id"),
            fixed
                .iter()
                .map(|(id, value)| {
                    CitedFinding::new(*id, Confidence::Declared)
                        .with_value(norn_store::value_head(value))
                })
                .collect(),
        )
    }

    /// One rule closing `status` over `todo` and `done`, mapping `complete`
    /// to `done`; `status` is a list.
    const LISTED: &str = "version: 1\nfields:\n  status: {type: text, shape: list}\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    one_of:\n      status: {values: [todo, done], synonyms: {complete: done}}\n";

    const NOT_ONE_OF: FindingKind = FindingKind::NotOneOf;

    /// **A list's elements are fixed one by one into one change to the
    /// field**: `complete` becomes `done`, `bogus` has no synonym and stands,
    /// skipped with its value; the one operation writes the whole list in its
    /// order and cites the finding it fixes.
    #[test]
    fn a_list_field_fixes_its_elements_into_one_set_and_an_element_without_a_synonym_stands() {
        let vault = Vault::of(
            LISTED,
            &[("a.md", "---\ntype: task\nstatus: [complete, bogus]\n---\n")],
        );

        let planned = vault.plan(&[
            offending(1, NOT_ONE_OF, "a.md", "status", "complete"),
            offending(2, NOT_ONE_OF, "a.md", "status", "bogus"),
        ]);

        assert_eq!(
            planned.operations,
            vec![set_to(1, "a.md", "status", list(&["done", "bogus"]))]
        );
        assert_eq!(
            planned.citations,
            vec![citing_values(1, &[(1, "complete")])]
        );
        assert_eq!(
            planned.skipped,
            vec![
                SkippedFinding::new(2, SkipReason::NoDeclaredFix)
                    .with_value(norn_store::value_head("bogus"))
            ]
        );
    }

    /// **Every fixed element of a list rides one operation citing every
    /// finding it fixes, and a skipped element leaves its neighbours' fixes
    /// standing**, whatever order the batch read the rows in.
    #[test]
    fn one_operation_cites_every_element_it_fixes_and_a_skipped_element_leaves_the_rest() {
        let schema = "version: 1\nfields:\n  status: {type: text, shape: list}\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    one_of:\n      status: {values: [todo, done, doing], synonyms: {complete: done, wip: doing}}\n";
        let vault = Vault::of(
            schema,
            &[(
                "a.md",
                "---\ntype: task\nstatus: [wip, bogus, complete]\n---\n",
            )],
        );

        let planned = vault.plan(&[
            offending(3, NOT_ONE_OF, "a.md", "status", "wip"),
            offending(2, NOT_ONE_OF, "a.md", "status", "bogus"),
            offending(1, NOT_ONE_OF, "a.md", "status", "complete"),
        ]);

        assert_eq!(
            planned.operations,
            vec![set_to(
                1,
                "a.md",
                "status",
                list(&["doing", "bogus", "done"])
            )]
        );
        assert_eq!(
            planned.citations,
            vec![citing_values(1, &[(1, "complete"), (3, "wip")])]
        );
        assert_eq!(planned.skipped.len(), 1);
        assert_eq!(planned.skipped[0].finding, 2);
    }

    /// **A repeated offending element is one finding whose fix rewrites
    /// every occurrence.**
    #[test]
    fn a_repeated_offending_element_is_one_finding_whose_fix_rewrites_every_occurrence() {
        let vault = Vault::of(
            LISTED,
            &[(
                "a.md",
                "---\ntype: task\nstatus: [complete, todo, complete]\n---\n",
            )],
        );

        let planned = vault.plan(&[offending(1, NOT_ONE_OF, "a.md", "status", "complete")]);

        assert_eq!(
            planned.operations,
            vec![set_to(1, "a.md", "status", list(&["done", "todo", "done"]))]
        );
        assert!(planned.skipped.is_empty());
    }

    /// **Elements are told apart by the field's typed equality, not by their
    /// spelling**: `4` and `4.0` are one number, one finding, and a synonym
    /// written `4.0` names it; the member is written as the number it is.
    #[test]
    fn elements_are_compared_by_the_fields_typed_equality() {
        let schema = "version: 1\nfields:\n  rank: {type: number, shape: list}\nrules:\n  ranked:\n    match: {frontmatter: {type: task}}\n    one_of:\n      rank: {values: [1, 2, 3], synonyms: {'4.0': 3}}\n";
        let vault = Vault::of(
            schema,
            &[("a.md", "---\ntype: task\nrank: [4, 2, 4.0]\n---\n")],
        );

        let planned = vault.plan(&[offending(1, NOT_ONE_OF, "a.md", "rank", "4")]);

        assert_eq!(
            planned.operations,
            vec![set_to(
                1,
                "a.md",
                "rank",
                AuthoredValue::List(vec![
                    AuthoredValue::Integer(3),
                    AuthoredValue::Integer(2),
                    AuthoredValue::Integer(3)
                ])
            )]
        );
        assert!(planned.skipped.is_empty(), "{:?}", planned.skipped);
    }

    /// **A scalar field is one set of the member.**
    #[test]
    fn a_scalar_field_sets_the_member() {
        let schema = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    one_of:\n      status: {values: [todo, done], synonyms: {complete: done}}\n";
        let vault = Vault::of(
            schema,
            &[("a.md", "---\ntype: task\nstatus: complete\n---\n")],
        );

        let planned = vault.plan(&[offending(1, NOT_ONE_OF, "a.md", "status", "complete")]);

        assert_eq!(planned.operations, vec![set(1, "a.md", "status", "done")]);
        assert_eq!(
            planned.citations,
            vec![citing_values(1, &[(1, "complete")])]
        );
    }

    /// **Co-selecting rules mapping one value to different members skip as a
    /// tie with both candidates and their rules**; a rule declaring no
    /// synonym for the value proposes nothing, and so does not tie.
    #[test]
    fn rules_mapping_one_value_to_different_members_skip_as_a_tie() {
        let schema = "version: 1\nrules:\n  a-rule:\n    match: {frontmatter: {type: task}}\n    one_of:\n      status: {values: [todo, done], synonyms: {complete: done}}\n  b-rule:\n    match: {frontmatter: {type: task}}\n    one_of:\n      status: {values: [todo, done], synonyms: {complete: todo}}\n  c-rule:\n    match: {frontmatter: {type: task}}\n    one_of:\n      status: {values: [todo, done], synonyms: {finished: done}}\n";
        let vault = Vault::of(
            schema,
            &[("a.md", "---\ntype: task\nstatus: complete\n---\n")],
        );

        let planned = vault.plan(&[offending(1, NOT_ONE_OF, "a.md", "status", "complete")]);

        assert!(planned.operations.is_empty());
        assert_eq!(
            planned.skipped,
            vec![
                SkippedFinding::new(1, SkipReason::Tie)
                    .with_candidates(values(&[("done", "a-rule"), ("todo", "b-rule")]))
            ]
        );
    }

    /// **Rules mapping one value to the same member agree**, whichever of
    /// them declares it.
    #[test]
    fn rules_mapping_one_value_to_one_member_agree() {
        let schema = "version: 1\nrules:\n  a-rule:\n    match: {frontmatter: {type: task}}\n    one_of:\n      status: {values: [todo, done], synonyms: {complete: done}}\n  b-rule:\n    match: {frontmatter: {type: task}}\n    one_of:\n      status: {values: [todo, done], synonyms: {complete: done}}\n";
        let vault = Vault::of(
            schema,
            &[("a.md", "---\ntype: task\nstatus: complete\n---\n")],
        );

        let planned = vault.plan(&[offending(1, NOT_ONE_OF, "a.md", "status", "complete")]);

        assert_eq!(planned.operations, vec![set(1, "a.md", "status", "done")]);
    }

    /// **A synonym the judge refuses skips as one it would refuse, with its
    /// candidates, while an earlier fix on the document stands**: `done` is
    /// outside the combined closed set `strict` narrows `status` to.
    #[test]
    fn a_synonym_the_judge_refuses_skips_while_an_earlier_fix_stands() {
        let schema = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    one_of:\n      a_priority: {values: [low, high], synonyms: {hi: high}}\n      status: {values: [todo, done], synonyms: {complete: done}}\n  strict:\n    match: {frontmatter: {type: task}}\n    one_of:\n      status: {values: [todo]}\n";
        let vault = Vault::of(
            schema,
            &[(
                "a.md",
                "---\ntype: task\na_priority: hi\nstatus: complete\n---\n",
            )],
        );

        let planned = vault.plan(&[
            offending(2, NOT_ONE_OF, "a.md", "status", "complete"),
            offending(1, NOT_ONE_OF, "a.md", "a_priority", "hi"),
        ]);

        assert_eq!(
            planned.operations,
            vec![set(1, "a.md", "a_priority", "high")]
        );
        assert_eq!(
            planned.skipped,
            vec![
                SkippedFinding::new(2, SkipReason::JudgeWouldRefuse)
                    .with_candidates(values(&[("done", "tasks")]))
            ]
        );
    }

    /// **A synonym that brings the document under a rule requiring `status`
    /// skips as bringing in required fields, naming `status` and its
    /// default**: `kind: task` selects the rule.
    #[test]
    fn a_synonym_that_brings_in_a_rule_requiring_status_skips_as_brings_in_required_fields() {
        let schema = "version: 1\nrules:\n  kinds:\n    match: {frontmatter: {type: work}}\n    one_of:\n      kind: {values: [task, note], synonyms: {todo: task}}\n  tasks:\n    match: {frontmatter: {kind: task}}\n    required:\n      status: {default: todo}\n";
        let vault = Vault::of(schema, &[("a.md", "---\ntype: work\nkind: todo\n---\n")]);

        let planned = vault.plan(&[offending(1, NOT_ONE_OF, "a.md", "kind", "todo")]);

        assert!(planned.operations.is_empty());
        let fields = RequiredFieldHead::new(
            [RequiredField::new("status").with_default(norn_store::value_head("todo"))],
            1,
        )
        .expect("a head");
        assert_eq!(
            planned.skipped,
            vec![
                SkippedFinding::new(1, SkipReason::BringsInRequiredFields)
                    .with_required_fields(fields)
            ]
        );
    }

    /// **A finding of a value no rule has a synonym for is skipped as having
    /// no declared fix, with its value.**
    #[test]
    fn a_value_no_rule_has_a_synonym_for_has_no_declared_fix() {
        let vault = Vault::of(
            LISTED,
            &[("a.md", "---\ntype: task\nstatus: [bogus]\n---\n")],
        );

        let planned = vault.plan(&[offending(1, NOT_ONE_OF, "a.md", "status", "bogus")]);

        assert_eq!(
            planned.skipped,
            vec![
                SkippedFinding::new(1, SkipReason::NoDeclaredFix)
                    .with_value(norn_store::value_head("bogus"))
            ]
        );
    }

    const BANNED: &str = "version: 1\nrules:\n  bans:\n    match: {frontmatter: {type: task}}\n    forbidden:\n      scratch: remove\n      due_date: {rename_to: due}\n      legacy:\n";

    /// **A forbidden field a rule removes is removed.**
    #[test]
    fn a_forbidden_field_a_rule_removes_is_removed() {
        let vault = Vault::of(BANNED, &[("a.md", "---\ntype: task\nscratch: x\n---\n")]);

        let planned = vault.plan(&[offending(1, FindingKind::Forbidden, "a.md", "scratch", "x")]);

        assert_eq!(planned.operations, vec![removal(1, "a.md", "scratch")]);
        assert_eq!(planned.citations, vec![citing_values(1, &[(1, "x")])]);
        assert!(planned.skipped.is_empty());
    }

    /// **A forbidden field a rule renames is set under the new name with its
    /// whole value and removed under the old, the removal requiring the
    /// set, both cited for the finding.**
    #[test]
    fn a_forbidden_field_a_rule_renames_is_set_under_the_new_name_and_removed() {
        let vault = Vault::of(
            BANNED,
            &[("a.md", "---\ntype: task\ndue_date: [soon, later]\n---\n")],
        );

        let whole = stored_spelling(&list(&["soon", "later"])).expect("a spelling");
        let planned = vault.plan(&[offending(
            1,
            FindingKind::Forbidden,
            "a.md",
            "due_date",
            &whole,
        )]);

        assert_eq!(
            planned.operations,
            vec![
                set_to(1, "a.md", "due", list(&["soon", "later"])),
                removal(2, "a.md", "due_date")
                    .with_requires(vec![OperationId::new("repair-1").expect("an id")]),
            ]
        );
        assert_eq!(planned.citations.len(), 2);
        for citation in &planned.citations {
            assert_eq!(citation.findings.len(), 1);
            assert_eq!(citation.findings[0].finding, 1);
            assert_eq!(citation.findings[0].confidence, Confidence::Declared);
        }
        assert!(planned.skipped.is_empty());
    }

    /// **A rename onto a field the document already holds is skipped as one
    /// onto an occupied field**, and writes nothing.
    #[test]
    fn a_rename_onto_an_occupied_field_is_skipped() {
        let vault = Vault::of(
            BANNED,
            &[("a.md", "---\ntype: task\ndue_date: soon\ndue: later\n---\n")],
        );

        let planned = vault.plan(&[offending(
            1,
            FindingKind::Forbidden,
            "a.md",
            "due_date",
            "soon",
        )]);

        assert!(planned.operations.is_empty());
        let [skipped] = planned.skipped.as_slice() else {
            panic!("one skip: {:?}", planned.skipped);
        };
        assert_eq!(skipped.reason, SkipReason::RenameOntoOccupiedField);
        assert!(
            skipped
                .note
                .as_deref()
                .is_some_and(|note| note.contains("`due`"))
        );
    }

    /// **A forbidden field no rule declares a fix for is skipped with its
    /// value.**
    #[test]
    fn a_forbidden_field_with_no_declared_fix_is_skipped_with_its_value() {
        let vault = Vault::of(BANNED, &[("a.md", "---\ntype: task\nlegacy: old\n---\n")]);

        let planned = vault.plan(&[offending(
            1,
            FindingKind::Forbidden,
            "a.md",
            "legacy",
            "old",
        )]);

        assert_eq!(
            planned.skipped,
            vec![
                SkippedFinding::new(1, SkipReason::NoDeclaredFix)
                    .with_value(norn_store::value_head("old"))
            ]
        );
    }

    /// **Rules that remove and rename one forbidden field, or rename it two
    /// ways, skip as a tie with each candidate and its rule**; a rule
    /// declaring no fix proposes nothing, and a rule that removes agrees with
    /// another that removes.
    #[test]
    fn disagreeing_forbidden_fixes_skip_as_a_tie() {
        let schema = "version: 1\nrules:\n  a-rule:\n    match: {frontmatter: {type: task}}\n    forbidden:\n      scratch: remove\n      old: {rename_to: new}\n      gone: remove\n  b-rule:\n    match: {frontmatter: {type: task}}\n    forbidden:\n      scratch: {rename_to: notes}\n      old: {rename_to: other}\n      gone: remove\n  c-rule:\n    match: {frontmatter: {type: task}}\n    forbidden:\n      scratch:\n";
        let vault = Vault::of(
            schema,
            &[(
                "a.md",
                "---\ntype: task\nscratch: x\nold: y\ngone: z\n---\n",
            )],
        );

        let planned = vault.plan(&[
            offending(1, FindingKind::Forbidden, "a.md", "scratch", "x"),
            offending(2, FindingKind::Forbidden, "a.md", "old", "y"),
            offending(3, FindingKind::Forbidden, "a.md", "gone", "z"),
        ]);

        assert_eq!(planned.operations, vec![removal(1, "a.md", "gone")]);
        assert_eq!(
            planned.skipped,
            vec![
                SkippedFinding::new(1, SkipReason::Tie).with_candidates(values(&[
                    ("remove", "a-rule"),
                    ("rename_to: notes", "b-rule")
                ])),
                SkippedFinding::new(2, SkipReason::Tie).with_candidates(values(&[
                    ("rename_to: new", "a-rule"),
                    ("rename_to: other", "b-rule")
                ])),
            ]
        );
    }

    /// **A rename the judge refuses skips through the judge's path**: the new
    /// field would breach its own closed set.
    #[test]
    fn a_rename_the_judge_refuses_is_skipped_as_one_the_judge_would_refuse() {
        let schema = "version: 1\nrules:\n  bans:\n    match: {frontmatter: {type: task}}\n    forbidden:\n      due_date: {rename_to: due}\n    one_of:\n      due: {values: [soon, later]}\n";
        let vault = Vault::of(
            schema,
            &[("a.md", "---\ntype: task\ndue_date: never\n---\n")],
        );

        let planned = vault.plan(&[offending(
            1,
            FindingKind::Forbidden,
            "a.md",
            "due_date",
            "never",
        )]);

        assert!(planned.operations.is_empty());
        assert_eq!(
            planned.skipped,
            vec![
                SkippedFinding::new(1, SkipReason::JudgeWouldRefuse)
                    .with_candidates(values(&[("rename_to: due", "bans")]))
            ]
        );
    }
}

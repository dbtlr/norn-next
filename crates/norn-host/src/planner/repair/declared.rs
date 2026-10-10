//! Declared fixes: what the schema's owner wrote as the answer to a finding,
//! composed into a document one fix at a time and judged as the applier will
//! judge it.
//!
//! **Four kinds of finding have a declared fix.** The candidates for a
//! selected finding are what the rules its judgment cites — the combined
//! constraint's contributing rules, read on the composed document — declare
//! for it, and a rule declaring nothing for it proposes nothing:
//!
//! - a missing required field (`field/required-missing`) fills from the
//!   default the rules requiring it declare, each filled as the defaults
//!   fixpoint fills one
//!   ([`Rule::fill_default`](norn_config::schema::Rule::fill_default), the one
//!   way a default is bound to its path and filled): a
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
//!   declares ([`forbidden`]);
//! - a misplaced document (`document/misplaced`) is moved into the folder its
//!   placement rules' `allowed_paths` route names, keeping its file name
//!   ([`routes`]).
//!
//! **Each kind's candidates agree by its own rule.** Defaults agree as one
//! written value (`1` and `1.0` disagree). Synonyms agree by the field's typed
//! equality (`4` and `4.0` are one number under a `number` field), and where
//! rules agree typed but spell the member differently, the member is written
//! as the first rule in name order spells it. Routes agree by the destination's
//! exact spelling, so on a root folding case `Tasks/` and `tasks/` are two
//! destinations and tie. Forbidden fixes agree when every one removes the
//! field, or every one renames it to the same field.
//!
//! **What is skipped, and with what.** No fix declared is
//! [`SkipReason::NoDeclaredFix`], with the value the finding judged; defaults
//! that disagree are [`SkipReason::ConflictingDefaults`], and synonyms or
//! forbidden fixes that disagree are [`SkipReason::Tie`], each with every
//! candidate and the rule proposing it; a default reading a capture its rule's
//! `match.path` binds several ways is [`SkipReason::AmbiguousCapture`], noting
//! two of the bindings; a rename onto a field the composed document already
//! holds is [`SkipReason::RenameOntoOccupiedField`]; a route to a place
//! something stands at, or another route of the batch moves a document to,
//! is [`SkipReason::DestinationTaken`]; a fix that brings in a
//! required field the document lacks is [`SkipReason::BringsInRequiredFields`],
//! naming each such field and the default its rules declare (none, with the
//! note saying so, where its rules declare differing defaults), whatever else
//! it introduces, and a route's carrying the destination as its candidate; and a fix that introduces any other violation, or that cannot
//! be set into the document, is [`SkipReason::JudgeWouldRefuse`], with its
//! candidates. **A default reading a clock that gives no reading is no
//! skip**: the repair is refused as a creation is, the default's operation
//! left unresolved ([`Planned::unresolved`]), and so is a route reading it.
//! Every skip carries the value its finding judged, where the finding names
//! one.
//!
//! **Composition.** A document's findings are taken in finding order — its
//! route first, then kind, field, offending value — and each fix is set into
//! the bytes and the place the earlier fixes left ([`edited`], the
//! composition a `set` writes a field by; a route moves the bytes it is given
//! unchanged), then judged at that place by the applier's one judge
//! ([`verdict`]) against the document's own before-state and against the
//! state it composes onto: the plan the fixes resolve to is refused exactly
//! where a fix introduces a violation against the before-state, and a fix
//! introducing one against the composed state would bring back what an
//! earlier fix took away, so either is skipped and the next composes without
//! it. A fix after which the document still holds the finding it answers —
//! a synonym mapping a value onto itself, outside the set co-selecting rules
//! narrow the field to — is no fix, and skips as
//! [`SkipReason::JudgeWouldRefuse`] too. **The route comes
//! first so that every other fix is judged where the document will stand**:
//! a rule the route takes the document out of no longer selects it there, so
//! a finding that rule concluded is no longer held and is dropped rather
//! than fixed, and a field fix names the document at its destination, after
//! the move in plan order, as a Layer 4 plan moving and then editing a
//! document does. What a finding is, and whether the composed document
//! still holds it, are read off the same judgment: a row names the finding
//! the before-state's judgment concludes of its kind on its field or tag
//! whose offending value has the row's head — its bytes, length and hash, so
//! a head cut short still names one value — and the composition asks after
//! that finding by its identity. A selected finding the composed document no
//! longer holds is dropped, neither fixed nor skipped, whatever its kind: a
//! rule's, an undeclared tag, or one about the document whole, each decided
//! against the document as the last fix leaves it.
//!
//! **A list's elements are fixed one by one into one change to the field.**
//! Every selected `field/not-one-of` finding of one field composes into one
//! `set_frontmatter` of the whole list, each element's replacement judged on
//! the state the earlier ones left; an element whose fix is skipped stands,
//! and never blocks its neighbours'. The one operation cites every finding it
//! fixes.

mod forbidden;
mod routes;
mod synonyms;

use std::collections::BTreeSet;
use std::sync::Arc;

use norn_config::schema::{FillError, FillRefusal, PathBindings, RuleWork, VaultSchema};
use norn_wire::{
    AuthoredValue, Captures, Citation, CitedFinding, Confidence, DocumentPath, FindingKind,
    FindingRow, Operation, OperationId, OperationKind, RequiredField, RequiredFieldHead,
    SchemaViolation, SkipReason, SkippedCandidates, SkippedFinding, UnresolvedOperation,
    UnresolvedReason, ValueCandidate, ValueCandidateHead, ValueHead, WriteTarget,
};

use super::{Claimed, Planned, Reading, Repairing, skip};
use crate::applier::{Held, Standing, standing, verdict};
use crate::derivation::{judged_by_bytes, stored_spelling, written_fields};
use crate::evidence::count_rule_work;
use crate::planner::edit::edited;

/// Whether `row` is a finding a declared fix may answer, so its document's
/// bytes are read: a missing required field, a value outside a closed set, a
/// forbidden field, or a misplaced document.
pub(super) fn has_fix(row: &FindingRow) -> bool {
    matches!(
        row.kind,
        FindingKind::RequiredMissing
            | FindingKind::NotOneOf
            | FindingKind::Forbidden
            | FindingKind::Misplaced
    )
}

/// What a route is judged against beyond its own document: the vault, for
/// what stands at its destination, and the destinations the batch's earlier
/// routes took.
pub(super) struct Routing<'r, R> {
    pub(super) vault: &'r R,
    pub(super) claimed: &'r mut Claimed,
}

/// Compose the declared fixes of `document` — one document's findings, in
/// batch order — onto `before`, the bytes it holds, adding each fix to
/// `planned` with its citation; what each row was decided, in the rows'
/// order: the skip it is left alone with, or `None` where it is fixed or
/// dropped.
///
/// **Every finding is decided against the composed document at the end.**
/// A row names its finding by the before-state's judgment, and the
/// composition asks after that finding by its identity; a skipped finding
/// the composed document no longer holds — one a later fix eliminated, or of
/// a kind no fix answers that a fix eliminated — is dropped. Each addition is
/// judged against the state it composes onto as well ([`verdict`]), so a
/// finding fixed or dropped never holds again.
pub(super) fn compose<R: Reading>(
    document: &[FindingRow],
    before: &Arc<[u8]>,
    repairing: &Repairing<'_>,
    mut routing: Routing<'_, R>,
    planned: &mut Planned,
) -> Result<Vec<Option<SkippedFinding>>, R::Error> {
    let path = &document[0].path;
    // The before-state is judged once, however many fixes are made to it.
    let composing = Document {
        before: standing(path, before, repairing.declared, repairing.case),
        repairing,
    };
    let mut state = State::of(path.clone(), Arc::clone(before), composing.before.clone());
    // The finding each row names, as the before-state holds it; a row it does
    // not hold, or of a kind no judgment of the bytes concludes, names none.
    let found: Vec<Option<Held>> = document
        .iter()
        .map(|row| {
            judged_by_bytes(row.kind)
                .then(|| holding(row, &state.holds).cloned())
                .flatten()
        })
        .collect();
    let mut decided = vec![None; document.len()];
    // The rows whose fix reads a clock that gives no reading: neither fixed
    // nor skipped, and still held.
    let mut unresolved = BTreeSet::new();
    let order = finding_order(document, &found);
    let mut next = 0;
    while let Some(&at) = order.get(next) {
        next += 1;
        let row = &document[at];
        if !judged_by_bytes(row.kind) {
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
            let rows: Vec<synonyms::Element<'_>> = order[next - 1..=next + siblings - 1]
                .iter()
                .filter_map(|&at| {
                    Some(synonyms::Element {
                        at,
                        row: &document[at],
                        finding: found[at].as_ref()?,
                    })
                })
                .collect();
            next += siblings;
            let mapped = synonyms::fix_field(&composing, &state, field, &rows);
            decided_skips(&mut decided, mapped.skipped);
            if let Some(fix) = mapped.fix {
                state = push(planned, fix);
            }
            continue;
        }
        // A row whose finding the composed document no longer holds is
        // dropped.
        let Some(finding) = &found[at] else {
            continue;
        };
        let Some(held) = state.holding(finding) else {
            continue;
        };
        if row.kind == FindingKind::Misplaced {
            let rules = held.rules.clone();
            match routes::fix(
                &composing,
                &state,
                row,
                finding,
                &rules,
                next_id(planned),
                &mut routing,
            )? {
                Ok(fix) => state = push(planned, fix),
                Err(Unmade::Skipped(skip)) => decided[at] = Some(*skip),
                Err(Unmade::ClockUnread(left)) => {
                    unresolved.insert(at);
                    planned.unresolved.push(*left);
                }
            }
            continue;
        }
        let (true, Some(field)) = (has_fix(row), row.target.as_deref()) else {
            decided[at] = Some(super::skipped(row));
            continue;
        };
        let rules = held.rules.clone();
        let made = match row.kind {
            FindingKind::Forbidden => {
                forbidden::fix(&composing, &state, row, finding, field, &rules)
                    .map_err(Unmade::Skipped)
            }
            _ => Composing {
                document: &composing,
                state: &state,
                row,
                finding,
                field,
            }
            .defaulted(&rules, next_id(planned)),
        };
        match made {
            Ok(fix) => state = push(planned, fix),
            Err(Unmade::Skipped(skip)) => decided[at] = Some(*skip),
            // The repair is refused, as a creation is: the finding is
            // neither fixed nor skipped.
            Err(Unmade::ClockUnread(left)) => {
                unresolved.insert(at);
                planned.unresolved.push(*left);
            }
        }
    }
    for (at, (decision, finding)) in decided.iter_mut().zip(&found).enumerate() {
        let Some(finding) = finding else {
            continue;
        };
        if unresolved.contains(&at) {
            continue;
        }
        let stands = state.holding(finding).is_some();
        if decision.is_some() && !stands {
            *decision = None;
        }
        // Each addition introduces nothing against the state it composed
        // onto, so a finding fixed or dropped on the way holds at the end only
        // through a defect of the composition.
        debug_assert!(
            decision.is_some() || !stands,
            "a finding fixed or dropped holds again on the composed document: {finding:?}"
        );
    }
    Ok(decided)
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

/// The positions of `document`'s rows in finding order: a misplaced
/// document's route first, so every other fix is judged where it will stand,
/// then kind, field, offending value, and identity.
///
/// **A value is compared whole**: as the finding `found` names for the row
/// holds it, so two values sharing a head longer than a row carries compare
/// by what follows; a row naming no held finding, by its head.
fn finding_order(document: &[FindingRow], found: &[Option<Held>]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..document.len()).collect();
    let key = |at: usize| {
        let row = &document[at];
        let value = found[at]
            .as_ref()
            .and_then(|held| held.value.as_deref())
            .or_else(|| row.value.as_ref().map(ValueHead::text));
        (
            row.kind != FindingKind::Misplaced,
            row.kind.as_str(),
            row.target.as_deref(),
            value,
            row.id,
        )
    };
    order.sort_by(|&left, &right| key(left).cmp(&key(right)));
    order
}

/// The finding of `holds` that `row` names: its kind, its field or tag, and
/// the offending value whose head the row carries.
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

/// One document being composed: what it held before any fix, and what the
/// fixes are made under.
struct Document<'c> {
    before: Standing,
    repairing: &'c Repairing<'c>,
}

/// What the fixes made so far have composed: where the document stands, its
/// bytes, their judgment there, and the findings that judgment concludes.
#[derive(Clone)]
struct State {
    at: DocumentPath,
    bytes: Arc<[u8]>,
    standing: Standing,
    holds: Vec<Held>,
}

impl State {
    /// The document standing at `at` holding `bytes`, judged as `standing`.
    fn of(at: DocumentPath, bytes: Arc<[u8]>, standing: Standing) -> Self {
        State {
            at,
            bytes,
            holds: standing.holds(),
            standing,
        }
    }

    /// The finding `finding` is, as this state holds it: by its identity,
    /// whatever rules state it here; `None` where the state no longer holds
    /// it.
    fn holding(&self, finding: &Held) -> Option<&Held> {
        self.holds.iter().find(|held| held.is(finding))
    }
}

impl Document<'_> {
    /// The schema the document is judged under.
    fn schema(&self) -> &VaultSchema {
        self.repairing.declared.schema()
    }

    /// The operation setting `field` of the document standing at `at` to
    /// `value`.
    fn set(&self, at: &DocumentPath, field: &str, value: AuthoredValue) -> OperationKind {
        OperationKind::set_frontmatter(WriteTarget::path(at.clone()), field, value)
    }

    /// The operation removing `field` from the document standing at `at`.
    fn remove(&self, at: &DocumentPath, field: &str) -> OperationKind {
        OperationKind::remove_frontmatter(WriteTarget::path(at.clone()), field)
    }

    /// The state `edits` compose `from` to, each set into the bytes the one
    /// before left and the result judged where `from` stands
    /// ([`Self::judged`]); or the skip of the finding `row` names,
    /// `finding`, with `candidates`, where an edit cannot be set into the
    /// document or the judgment refuses the result.
    fn admit(
        &self,
        from: &State,
        edits: &[OperationKind],
        fixing: Fixing<'_>,
    ) -> Result<State, Box<SkippedFinding>> {
        let mut composed = Arc::clone(&from.bytes);
        for edit in edits {
            composed = edited(edit, &composed).map_err(|detail| {
                Box::new(
                    skip(fixing.row, SkipReason::JudgeWouldRefuse)
                        .with_candidates(fixing.candidates.clone())
                        .with_note(format!("the fix cannot be set into the document: {detail}")),
                )
            })?;
        }
        self.judged(
            from,
            &from.at,
            composed,
            fixing,
            "the fix leaves it still standing",
        )
    }

    /// The state the document holding `composed` at `at` is, judged there
    /// against its before-state and against `from`, the state it was composed
    /// from; or the skip of the finding `fixing` names, with its candidates,
    /// for the reason the judgment gives: a fix that brings in required
    /// fields the document lacks, one that introduces any other violation,
    /// or one after which the document still holds the finding, noted as
    /// `still`.
    fn judged(
        &self,
        from: &State,
        at: &DocumentPath,
        composed: Arc<[u8]>,
        fixing: Fixing<'_>,
        still: &str,
    ) -> Result<State, Box<SkippedFinding>> {
        let Fixing {
            row,
            finding,
            candidates,
        } = fixing;
        let verdict = verdict(
            &self.before,
            &from.standing,
            at,
            &composed,
            self.repairing.declared,
            self.repairing.case,
        );
        let state = State::of(at.clone(), composed, verdict.standing);
        let brought = brought_in(&verdict.introduced);
        if !brought.is_empty() {
            let (fields, differing) = required_fields(&brought, &state.holds, self.schema());
            let skipped =
                skip(row, SkipReason::BringsInRequiredFields).with_required_fields(fields);
            return Err(Box::new(if differing.is_empty() {
                skipped
            } else {
                skipped.with_note(format!(
                    "the rules requiring {} declare differing defaults, so no default is named",
                    differing
                        .iter()
                        .map(|field| format!("`{field}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            }));
        }
        if !verdict.introduced.is_empty() {
            return Err(Box::new(
                skip(row, SkipReason::JudgeWouldRefuse).with_candidates(candidates.clone()),
            ));
        }
        // A fix is cited only for a finding the result no longer holds.
        if state.holding(finding).is_some() {
            return Err(Box::new(
                skip(row, SkipReason::JudgeWouldRefuse)
                    .with_candidates(candidates.clone())
                    .with_note(still.to_string()),
            ));
        }
        Ok(state)
    }
}

/// What one fix answers: the selected finding `row`, the finding it names
/// as the before-state holds it, and the candidates a skip of it carries.
#[derive(Clone, Copy)]
struct Fixing<'f> {
    row: &'f FindingRow,
    finding: &'f Held,
    candidates: &'f SkippedCandidates,
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
    finding: &'c Held,
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
    /// A default that fills to no value, and why.
    Unfillable { rule: String, error: FillError },
}

impl Composing<'_> {
    /// The fix of a missing required field — the default `rules`, the rules
    /// requiring it, declare — or why it is not made. `id` is the operation's
    /// id, which an operation left unresolved takes.
    fn defaulted(&self, rules: &BTreeSet<String>, id: OperationId) -> Result<Fix, Unmade> {
        let row = self.row;
        let skipped = |finding| Err(Unmade::Skipped(Box::new(finding)));
        let proposals = match self.proposed(rules) {
            Proposed::Candidates(proposals) => proposals,
            Proposed::AmbiguousCapture { rule, bindings } => {
                return skipped(skip(row, SkipReason::AmbiguousCapture).with_note(format!(
                    "the rule `{rule}` defaults `{}` from a capture its `match.path` binds \
                     several ways in `{}`: {} and {}",
                    self.field,
                    self.state.at,
                    spelled(&bindings[0]),
                    spelled(&bindings[1]),
                )));
            }
            Proposed::NoClockReading { source } => {
                let operation =
                    Operation::new(self.document.set(&self.state.at, self.field, source))
                        .with_id(id);
                let reason = UnresolvedReason::no_longer_resolves(crate::clock::cannot_fill(
                    &format!("the rule default for `{}`", self.field),
                ));
                return Err(Unmade::ClockUnread(Box::new(UnresolvedOperation::new(
                    operation, reason,
                ))));
            }
            Proposed::Unfillable { rule, error } => {
                return skipped(skip(row, SkipReason::JudgeWouldRefuse).with_note(format!(
                    "the rule `{rule}` defaults `{}` to no value in `{}`: {error}",
                    self.field, self.state.at,
                )));
            }
        };
        let Some((first, rest)) = proposals.split_first() else {
            return skipped(skip(row, SkipReason::NoDeclaredFix));
        };
        if rest.iter().any(|proposal| proposal.value != first.value) {
            return skipped(
                skip(row, SkipReason::ConflictingDefaults).with_candidates(candidates(&proposals)),
            );
        }
        let set = self
            .document
            .set(&self.state.at, self.field, first.value.clone());
        let state = self
            .document
            .admit(
                self.state,
                std::slice::from_ref(&set),
                Fixing {
                    row: self.row,
                    finding: self.finding,
                    candidates: &candidates(&proposals),
                },
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
    /// rule name order, filled as the defaults fixpoint fills one
    /// ([`Rule::fill_default`](norn_config::schema::Rule::fill_default)) — the clock read only for a default reading
    /// it — and its work tallied on the logical rule counters.
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
            let mut clocked = false;
            let mut work = RuleWork::default();
            let filled = rule.fill_default(
                self.field,
                self.state.at.as_str(),
                repairing.case,
                &mut || {
                    clocked = true;
                    repairing.clock.get()
                },
                &mut PathBindings::default(),
                &mut work,
            );
            count_rule_work(work);
            let Some(filled) = filled else {
                continue;
            };
            // Schema read holds a default's tokens to the clock's and its own
            // rule's captures, so a default fills; a failure is answered as a
            // skip, never a panic.
            match filled {
                Ok(value) => proposals.push(Proposal {
                    value,
                    rule: name.clone(),
                    clocked,
                }),
                Err(FillRefusal::AmbiguousCapture { bindings }) => {
                    return Proposed::AmbiguousCapture {
                        rule: name.clone(),
                        bindings,
                    };
                }
                Err(FillRefusal::NoClockReading(_)) => {
                    return Proposed::NoClockReading {
                        source: default.source(),
                    };
                }
                Err(FillRefusal::Unfillable(error)) => {
                    return Proposed::Unfillable {
                        rule: name.clone(),
                        error,
                    };
                }
            }
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
/// declare, as the schema writes it, where they declare one and agree on it;
/// and the fields whose rules declare differing defaults, which are named
/// with none.
fn required_fields<'b>(
    brought: &BTreeSet<&'b str>,
    holds: &[Held],
    schema: &VaultSchema,
) -> (RequiredFieldHead, Vec<&'b str>) {
    let mut differing = Vec::new();
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
            Some(_) => {
                differing.push(*field);
                required
            }
            None => required,
        }
    });
    let head = RequiredFieldHead::new(fields.collect::<Vec<_>>(), brought.len() as u64)
        .expect("a head of every field it names");
    (head, differing)
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

    use super::super::{Before, Destination, Planned, Reading, plan};
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
            let refused = BTreeMap::new();
            let repairing = Repairing {
                declared: &self.declared,
                case: norn_wire::CaseFold::Exact,
                clock: &one,
                refused: &refused,
            };
            let reading = Read {
                vault: self,
                reads: RefCell::new(Vec::new()),
            };
            let Ok(planned) = plan(rows, &repairing, &reading);
            (planned, reading.reads.into_inner())
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
            let refused = BTreeMap::new();
            let repairing = Repairing {
                declared: &self.declared,
                case: norn_wire::CaseFold::Exact,
                clock: &one,
                refused: &refused,
            };
            let reading = Read {
                vault: self,
                reads: RefCell::new(Vec::new()),
            };
            let Ok(planned) = plan(rows, &repairing, &reading);
            planned
        }
    }

    /// The vault as a repair reads it, each document read noted in order.
    struct Read<'v> {
        vault: &'v Vault,
        reads: RefCell<Vec<String>>,
    }

    impl Reading for Read<'_> {
        type Error = std::convert::Infallible;

        fn before(&self, path: &DocumentPath) -> Result<Before, Self::Error> {
            self.reads.borrow_mut().push(path.as_str().to_string());
            Ok(match self.vault.documents.get(path.as_str()) {
                Some(bytes) => Before::Held(Arc::clone(bytes)),
                None => Before::Unread,
            })
        }

        /// A document's path is taken, and so is each folder above one; a
        /// path beneath `.norn/` is closed.
        fn destination(&self, path: &DocumentPath) -> Result<Destination, Self::Error> {
            let spelled = path.as_str();
            if spelled.starts_with(".norn/") {
                return Ok(Destination::Closed("the vault's mechanism folder".into()));
            }
            let taken = self.vault.documents.keys().any(|held| {
                held == spelled
                    || held
                        .strip_prefix(spelled)
                        .is_some_and(|rest| rest.starts_with('/'))
            });
            Ok(if taken {
                Destination::Taken
            } else {
                let normalizer =
                    norn_fs::PathNormalizer::for_sensitivity(norn_fs::CaseSensitivity::Sensitive);
                let normalized = |spelled: &str| {
                    normalizer
                        .normalize(std::path::Path::new(spelled))
                        .expect("a document path normalizes")
                };
                Destination::Free {
                    at: normalized(spelled),
                    folders: spelled
                        .match_indices('/')
                        .map(|(end, _)| normalized(&spelled[..end]))
                        .collect(),
                }
            })
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

    /// **A brought-in required field whose rules declare differing defaults
    /// names no default, and the skip says why**: `status` is required by two
    /// rules the filled `kind` brings in, which default it differently.
    #[test]
    fn a_brought_in_field_whose_rules_disagree_on_its_default_names_none_and_says_so() {
        let schema = "version: 1\nrules:\n  kinds:\n    match: {frontmatter: {type: work}}\n    required:\n      kind: {default: task}\n  a-tasks:\n    match: {frontmatter: {kind: task}}\n    required:\n      status: {default: todo}\n  b-tasks:\n    match: {frontmatter: {kind: task}}\n    required:\n      status: {default: doing}\n";
        let vault = Vault::of(schema, &[("a.md", "---\ntype: work\n---\n")]);

        let planned = vault.plan(&[missing(5, "a.md", "kind")]);

        assert!(planned.operations.is_empty());
        let fields = RequiredFieldHead::new([RequiredField::new("status")], 1).expect("a head");
        let expected =
            SkippedFinding::new(5, SkipReason::BringsInRequiredFields).with_required_fields(fields);
        let [skipped] = planned.skipped.as_slice() else {
            panic!("one skip: {:?}", planned.skipped);
        };
        let note = skipped.note.as_deref().expect("the skip says why");
        assert!(
            note.contains("`status`") && note.contains("differing defaults"),
            "{note}"
        );
        assert_eq!(
            &SkippedFinding {
                note: None,
                ..skipped.clone()
            },
            &expected
        );
    }

    /// **A default's path capture is bound and tallied as the defaults
    /// fixpoint's is**: one binding on the logical rule counters.
    #[test]
    fn a_default_reading_a_capture_tallies_its_binding_on_the_rule_counters() {
        let schema = "version: 1\nrules:\n  areas:\n    match: {path: '<area>/**'}\n    required:\n      area: {default: '{{path.area}}'}\n";
        let vault = Vault::of(schema, &[("red/a.md", "---\ntitle: A\n---\n")]);

        let (planned, work) =
            crate::evidence::rule_work_of(|| vault.plan(&[missing(1, "red/a.md", "area")]));

        assert_eq!(planned.operations, vec![set(1, "red/a.md", "area", "red")]);
        assert_eq!(work.captures_bound, 1, "{work:?}");
    }

    /// **A route's path capture is bound and tallied the same way.**
    #[test]
    fn a_route_reading_a_capture_tallies_its_binding_on_the_rule_counters() {
        let vault = Vault::of(AREAS, &[("work/a.md", "---\ntype: task\n---\n# A\n")]);

        let (planned, work) =
            crate::evidence::rule_work_of(|| vault.plan(&[misplaced(3, "work/a.md")]));

        assert_eq!(planned.operations.len(), 1, "{planned:?}");
        assert_eq!(work.captures_bound, 1, "{work:?}");
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
    /// would answer as unreadable**, each with the value it judged, and the
    /// rest as their rows say.
    #[test]
    fn a_document_that_cannot_be_read_skips_its_fixable_findings_as_unreadable() {
        let vault = Vault::of(TASKS, &[]);

        let planned = vault.plan(&[
            missing(7, "gone.md", "status"),
            offending(8, FindingKind::NotOneOf, "gone.md", "status", "stalled"),
            offending(9, FindingKind::Forbidden, "gone.md", "scratch", "x"),
            finding(10, FindingKind::Broken, "gone.md", "nowhere"),
            misplaced(11, "gone.md"),
        ]);

        assert!(planned.operations.is_empty());
        assert_eq!(
            planned.skipped,
            vec![
                SkippedFinding::new(7, SkipReason::Unreadable),
                SkippedFinding::new(8, SkipReason::Unreadable)
                    .with_value(norn_store::value_head("stalled")),
                SkippedFinding::new(9, SkipReason::Unreadable)
                    .with_value(norn_store::value_head("x")),
                SkippedFinding::new(10, SkipReason::NoDeclaredFix),
                SkippedFinding::new(11, SkipReason::Unreadable),
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

    /// **A list's elements compose in offending value order, whatever the
    /// batch's ids say, and an element's fix never brings back a value an
    /// earlier element's fix replaced.** Two rules narrow `color` to `b`:
    /// `a` maps to `b`, and `x` maps to `a`. Taken in value order, `a`
    /// becomes `b` first, so `x`'s fix would write the `a` just fixed and
    /// skips; taken by id, `x` would go first and both would be fixed.
    #[test]
    fn elements_compose_in_value_order_and_never_bring_back_a_replaced_value() {
        let schema = "version: 1\nfields:\n  color: {type: text, shape: list}\nrules:\n  a-rule:\n    match: {frontmatter: {type: task}}\n    one_of:\n      color: {values: [a, b], synonyms: {x: a}}\n  b-rule:\n    match: {frontmatter: {type: task}}\n    one_of:\n      color: {values: [b, c], synonyms: {a: b}}\n";
        let vault = Vault::of(schema, &[("a.md", "---\ntype: task\ncolor: [a, x]\n---\n")]);

        let planned = vault.plan(&[
            offending(1, NOT_ONE_OF, "a.md", "color", "x"),
            offending(2, NOT_ONE_OF, "a.md", "color", "a"),
        ]);

        assert_eq!(
            planned.operations,
            vec![set_to(1, "a.md", "color", list(&["b", "x"]))]
        );
        assert_eq!(planned.citations, vec![citing_values(1, &[(2, "a")])]);
        assert_eq!(
            planned.skipped,
            vec![
                SkippedFinding::new(1, SkipReason::JudgeWouldRefuse)
                    .with_value(norn_store::value_head("x"))
                    .with_candidates(values(&[("a", "a-rule")]))
            ]
        );
    }

    /// **Elements whose heads agree compose in the order of their whole
    /// values**: the case above with every value 300 bytes of `v` before its
    /// letter, so the heads the rows carry are cut short alike and only the
    /// whole values tell `a` from `x`; by head and then id, `x` would go
    /// first and both would be fixed.
    #[test]
    fn elements_whose_heads_agree_compose_in_whole_value_order() {
        let prefix = "v".repeat(300);
        let [a, b, c, x] = ["a", "b", "c", "x"].map(|letter| format!("{prefix}{letter}"));
        let schema = format!(
            "version: 1\nfields:\n  color: {{type: text, shape: list}}\nrules:\n  a-rule:\n    match: {{frontmatter: {{type: task}}}}\n    one_of:\n      color: {{values: ['{a}', '{b}'], synonyms: {{'{x}': '{a}'}}}}\n  b-rule:\n    match: {{frontmatter: {{type: task}}}}\n    one_of:\n      color: {{values: ['{b}', '{c}'], synonyms: {{'{a}': '{b}'}}}}\n"
        );
        let document = format!("---\ntype: task\ncolor: ['{a}', '{x}']\n---\n");
        let vault = Vault::of(&schema, &[("a.md", &document)]);

        let planned = vault.plan(&[
            offending(1, NOT_ONE_OF, "a.md", "color", &x),
            offending(2, NOT_ONE_OF, "a.md", "color", &a),
        ]);

        assert_eq!(
            planned.operations,
            vec![set_to(1, "a.md", "color", list(&[&b, &x]))]
        );
        assert_eq!(planned.citations, vec![citing_values(1, &[(2, &a)])]);
        assert_eq!(planned.skipped.len(), 1);
        assert_eq!(planned.skipped[0].finding, 1);
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

    /// **A synonym's member is written as the schema wrote it**, not as the
    /// field's type would re-spell it: `"1.50"`, a string the owner quoted
    /// under a `number` field, is not rewritten as the float `1.5`; and a
    /// member written as a number stays that number.
    #[test]
    fn a_synonyms_member_is_written_as_the_schema_wrote_it() {
        let schema = "version: 1\nfields:\n  score: {type: number}\nrules:\n  scored:\n    match: {frontmatter: {type: task}}\n    one_of:\n      score: {values: ['1.50', 3], synonyms: {'7': '1.50', '8': 3}}\n";
        let vault = Vault::of(
            schema,
            &[
                ("a.md", "---\ntype: task\nscore: 7\n---\n"),
                ("b.md", "---\ntype: task\nscore: 8\n---\n"),
            ],
        );

        let planned = vault.plan(&[
            offending(1, NOT_ONE_OF, "a.md", "score", "7"),
            offending(2, NOT_ONE_OF, "b.md", "score", "8"),
        ]);

        assert_eq!(
            planned.operations,
            vec![
                set_to(1, "a.md", "score", AuthoredValue::string("1.50")),
                set_to(2, "b.md", "score", AuthoredValue::Integer(3)),
            ]
        );
        assert!(planned.skipped.is_empty(), "{:?}", planned.skipped);
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
                    .with_value(norn_store::value_head("complete"))
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
                    .with_value(norn_store::value_head("complete"))
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
                    .with_value(norn_store::value_head("todo"))
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
        assert_eq!(skipped.value, Some(norn_store::value_head("soon")));
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
                SkippedFinding::new(1, SkipReason::Tie)
                    .with_value(norn_store::value_head("x"))
                    .with_candidates(values(&[
                        ("remove", "a-rule"),
                        ("rename_to: notes", "b-rule")
                    ])),
                SkippedFinding::new(2, SkipReason::Tie)
                    .with_value(norn_store::value_head("y"))
                    .with_candidates(values(&[
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
                    .with_value(norn_store::value_head("never"))
                    .with_candidates(values(&[("rename_to: due", "bans")]))
            ]
        );
    }

    /// The finding `id`: the document at `at` standing where its rules do
    /// not allow.
    fn misplaced(id: u64, at: &str) -> FindingRow {
        FindingRow::new(
            id,
            FindingKind::Misplaced,
            Severity::Warning,
            DocumentPath::new(at).expect("a document path"),
            None,
            None,
            CandidateHead::new([], 0).expect("an empty head"),
            None,
            "a finding",
            1,
        )
    }

    /// The operation `repair-<n>`, moving the document at `from` to `to`.
    fn moving(n: usize, from: &str, to: &str) -> Operation {
        Operation::new(OperationKind::move_document(
            DocumentPath::new(from).expect("a document path"),
            DocumentPath::new(to).expect("a document path"),
        ))
        .with_id(OperationId::new(format!("repair-{n}")).expect("an id"))
    }

    /// One rule placing a task in its area's `tasks/` folder, the area the
    /// first segment of where it stands, and routing it there.
    const AREAS: &str = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}, path: '<area>/**'}\n    allowed_paths: {paths: ['*/tasks/**'], route: '{{path.area}}/tasks/'}\n";

    /// **A route moves a misplaced document into its rule's route folder,
    /// keeping its file name, a capture filled from where it stands**: one
    /// `move_document`, cited at the declared level.
    #[test]
    fn a_route_moves_a_misplaced_document_to_its_route_folder_with_a_capture_filled() {
        let vault = Vault::of(AREAS, &[("work/a.md", "---\ntype: task\n---\n# A\n")]);

        let planned = vault.plan(&[misplaced(3, "work/a.md")]);

        assert_eq!(
            planned.operations,
            vec![moving(1, "work/a.md", "work/tasks/a.md")]
        );
        assert_eq!(planned.citations, vec![citing(1, &[3])]);
        assert!(planned.skipped.is_empty(), "{:?}", planned.skipped);
    }

    /// **A route reading a capture its rule's `match.path` binds several ways
    /// skips as an ambiguous capture**, noting two of the bindings.
    #[test]
    fn a_route_reading_a_capture_bound_several_ways_skips_as_ambiguous_capture() {
        let schema = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}, path: '**/<area>/**'}\n    allowed_paths: {paths: ['**/tasks/**'], route: '{{path.area}}/tasks/'}\n";
        let vault = Vault::of(schema, &[("red/blue/a.md", "---\ntype: task\n---\n")]);

        let planned = vault.plan(&[misplaced(3, "red/blue/a.md")]);

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

    /// One rule placing a task in `tasks/` and routing it there.
    const TASKED: &str = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    allowed_paths: {paths: ['tasks/**'], route: 'tasks/'}\n";

    /// **A route whose destination something stands at skips as destination
    /// taken**, with its candidate, and moves nothing.
    #[test]
    fn a_route_whose_destination_is_taken_skips_as_destination_taken() {
        let vault = Vault::of(
            TASKED,
            &[
                ("loose/a.md", "---\ntype: task\n---\n"),
                ("tasks/a.md", "---\ntype: task\n---\n"),
            ],
        );

        let planned = vault.plan(&[misplaced(3, "loose/a.md")]);

        assert!(planned.operations.is_empty());
        let [skipped] = planned.skipped.as_slice() else {
            panic!("one skip: {:?}", planned.skipped);
        };
        assert_eq!(skipped.reason, SkipReason::DestinationTaken);
        assert_eq!(skipped.candidates, Some(values(&[("tasks/a.md", "tasks")])));
    }

    /// **Two routes of one batch to one destination: the first in batch
    /// order moves, and the later skips as destination taken.**
    #[test]
    fn two_routes_to_one_destination_the_later_skips_as_destination_taken() {
        let vault = Vault::of(
            TASKED,
            &[
                ("x/a.md", "---\ntype: task\n---\n"),
                ("y/a.md", "---\ntype: task\n---\n"),
            ],
        );

        let planned = vault.plan(&[misplaced(1, "x/a.md"), misplaced(2, "y/a.md")]);

        assert_eq!(planned.operations, vec![moving(1, "x/a.md", "tasks/a.md")]);
        let [skipped] = planned.skipped.as_slice() else {
            panic!("one skip: {:?}", planned.skipped);
        };
        assert_eq!(
            (skipped.finding, skipped.reason),
            (2, SkipReason::DestinationTaken)
        );
        assert!(
            skipped
                .note
                .as_deref()
                .is_some_and(|note| note.contains("an earlier route")),
            "{skipped:?}"
        );
    }

    /// **A route out of a rule's area drops that rule's selected
    /// `required-missing` finding**: the inbox rule no longer selects the
    /// document where the route takes it, so its `triage` is neither filled
    /// nor skipped — whatever order the batch read the rows in.
    #[test]
    fn a_route_out_of_a_rules_area_drops_its_selected_required_missing_finding() {
        let schema = "version: 1\nrules:\n  inbox:\n    match: {path: 'inbox/**'}\n    required:\n      triage: {default: later}\n  tasks:\n    match: {frontmatter: {type: task}}\n    allowed_paths: {paths: ['tasks/**'], route: 'tasks/'}\n";
        let vault = Vault::of(schema, &[("inbox/a.md", "---\ntype: task\n---\n")]);

        for rows in [
            [
                misplaced(1, "inbox/a.md"),
                missing(2, "inbox/a.md", "triage"),
            ],
            [
                missing(2, "inbox/a.md", "triage"),
                misplaced(1, "inbox/a.md"),
            ],
        ] {
            let planned = vault.plan(&rows);

            assert_eq!(
                planned.operations,
                vec![moving(1, "inbox/a.md", "tasks/a.md")]
            );
            assert_eq!(planned.citations, vec![citing(1, &[1])]);
            assert!(planned.skipped.is_empty(), "{:?}", planned.skipped);
        }
    }

    /// **A field fix after a route names the document where the route puts
    /// it**, in plan order after the move, and is judged there.
    #[test]
    fn a_field_fix_after_a_route_names_the_document_at_its_destination() {
        let schema = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    required:\n      status: {default: todo}\n    allowed_paths: {paths: ['tasks/**'], route: 'tasks/'}\n";
        let vault = Vault::of(schema, &[("loose/a.md", "---\ntype: task\n---\n")]);

        let planned = vault.plan(&[
            missing(2, "loose/a.md", "status"),
            misplaced(1, "loose/a.md"),
        ]);

        assert_eq!(
            planned.operations,
            vec![
                moving(1, "loose/a.md", "tasks/a.md"),
                set(2, "tasks/a.md", "status", "todo"),
            ]
        );
        assert_eq!(planned.citations, vec![citing(1, &[1]), citing(2, &[2])]);
    }

    /// **A route the judge refuses at its destination skips as one the judge
    /// would refuse**, with its candidate: the rule on `tasks/` forbids the
    /// `scratch` the document holds.
    #[test]
    fn a_route_the_judge_refuses_at_its_destination_skips_as_judge_would_refuse() {
        let schema = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    allowed_paths: {paths: ['tasks/**'], route: 'tasks/'}\n  shelved:\n    match: {path: 'tasks/**'}\n    forbidden:\n      scratch:\n";
        let vault = Vault::of(
            schema,
            &[("loose/a.md", "---\ntype: task\nscratch: x\n---\n")],
        );

        let planned = vault.plan(&[misplaced(1, "loose/a.md")]);

        assert!(planned.operations.is_empty());
        assert_eq!(
            planned.skipped,
            vec![
                SkippedFinding::new(1, SkipReason::JudgeWouldRefuse)
                    .with_candidates(values(&[("tasks/a.md", "tasks")]))
            ]
        );
    }

    /// **A route that brings the document under a rule requiring a field
    /// skips as bringing in required fields**, naming it and its default, and
    /// carries the destination as its candidate as the route's other refusals
    /// do.
    #[test]
    fn a_route_that_brings_in_a_rule_requiring_a_field_skips_as_brings_in_required_fields() {
        let schema = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    allowed_paths: {paths: ['tasks/**'], route: 'tasks/'}\n  tracked:\n    match: {path: 'tasks/**'}\n    required:\n      status: {default: todo}\n";
        let vault = Vault::of(schema, &[("loose/a.md", "---\ntype: task\n---\n")]);

        let planned = vault.plan(&[misplaced(1, "loose/a.md")]);

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
                    .with_candidates(values(&[("tasks/a.md", "tasks")]))
                    .with_required_fields(fields)
            ]
        );
    }

    /// **A route to a place another of the document's placement rules does
    /// not allow skips as one the judge would refuse**: the document would
    /// still be misplaced there.
    #[test]
    fn a_route_to_a_place_still_misplaced_skips_as_judge_would_refuse() {
        let schema = "version: 1\nrules:\n  a-rule:\n    match: {frontmatter: {type: task}}\n    allowed_paths: {paths: ['a/**', 'b/**'], route: 'a/'}\n  b-rule:\n    match: {frontmatter: {type: task}}\n    allowed_paths: {paths: ['b/**']}\n";
        let vault = Vault::of(schema, &[("c/x.md", "---\ntype: task\n---\n")]);

        let planned = vault.plan(&[misplaced(1, "c/x.md")]);

        assert!(planned.operations.is_empty());
        let [skipped] = planned.skipped.as_slice() else {
            panic!("one skip: {:?}", planned.skipped);
        };
        assert_eq!(skipped.reason, SkipReason::JudgeWouldRefuse);
        assert!(
            skipped
                .note
                .as_deref()
                .is_some_and(|note| note.contains("still stands")),
            "{skipped:?}"
        );
    }

    /// **Co-selecting rules routing a document to different folders skip it
    /// as a tie, with each destination and its rule**; a rule declaring no
    /// route proposes nothing.
    #[test]
    fn disagreeing_routes_skip_as_a_tie() {
        let schema = "version: 1\nrules:\n  a-rule:\n    match: {frontmatter: {type: task}}\n    allowed_paths: {paths: ['a/**', 'b/**'], route: 'a/'}\n  b-rule:\n    match: {frontmatter: {type: task}}\n    allowed_paths: {paths: ['a/**', 'b/**'], route: 'b/'}\n  c-rule:\n    match: {frontmatter: {type: task}}\n    allowed_paths: {paths: ['a/**', 'b/**']}\n";
        let vault = Vault::of(schema, &[("c/x.md", "---\ntype: task\n---\n")]);

        let planned = vault.plan(&[misplaced(1, "c/x.md")]);

        assert!(planned.operations.is_empty());
        assert_eq!(
            planned.skipped,
            vec![
                SkippedFinding::new(1, SkipReason::Tie)
                    .with_candidates(values(&[("a/x.md", "a-rule"), ("b/x.md", "b-rule")]))
            ]
        );
    }

    /// **A placement rule declaring no route has no fix for a misplaced
    /// document**, which is skipped as such.
    #[test]
    fn a_misplaced_document_no_rule_routes_skips_as_no_declared_fix() {
        let schema = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    allowed_paths: {paths: ['tasks/**']}\n";
        let vault = Vault::of(schema, &[("loose/a.md", "---\ntype: task\n---\n")]);

        let planned = vault.plan(&[misplaced(1, "loose/a.md")]);

        assert_eq!(
            planned.skipped,
            vec![SkippedFinding::new(1, SkipReason::NoDeclaredFix)]
        );
    }

    /// A rule stamping a task's `created` and `day` and routing it into the
    /// day's folder of `log/`.
    const LOGGED: &str = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    required:\n      created: {default: '{{now}}'}\n      day: {default: '{{date}}'}\n    allowed_paths: {paths: ['log/**'], route: 'log/{{date}}/'}\n";

    /// **Every route and default a plan fills comes from one clock reading**,
    /// across documents, the route filling the day's folder and the defaults
    /// filled where the route puts the document, each noting it is the
    /// repair's time.
    #[test]
    fn every_route_and_default_a_plan_fills_comes_from_one_clock_reading() {
        let vault = Vault::of(
            LOGGED,
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
                misplaced(3, "a.md"),
                missing(4, "b.md", "created"),
            ],
            &reads,
        );

        assert_eq!(reads.get(), 1);
        assert_eq!(
            planned.operations,
            vec![
                moving(1, "a.md", "log/2026-10-01/a.md"),
                set(
                    2,
                    "log/2026-10-01/a.md",
                    "created",
                    "2026-10-01T09:30:15+02:00"
                ),
                set(3, "log/2026-10-01/a.md", "day", "2026-10-01"),
                set(4, "b.md", "created", "2026-10-01T09:30:15+02:00"),
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
    }

    /// **A clock that gives no reading leaves a route reading it
    /// unresolved**, the move to the route as the schema writes it, and the
    /// document's other fixes are composed where it stands.
    #[test]
    fn a_clock_that_gives_no_reading_leaves_a_clock_route_unresolved() {
        let schema = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    required:\n      status: {default: todo}\n    allowed_paths: {paths: ['log/**'], route: 'log/{{date}}/'}\n";
        let vault = Vault::of(schema, &[("a.md", "---\ntype: task\n---\n")]);

        let planned = vault.planned_unread(&[misplaced(1, "a.md"), missing(2, "a.md", "status")]);

        assert_eq!(planned.operations, vec![set(2, "a.md", "status", "todo")]);
        assert!(planned.skipped.is_empty(), "{:?}", planned.skipped);
        let [left] = planned.unresolved.as_slice() else {
            panic!("one unresolved: {:?}", planned.unresolved);
        };
        assert_eq!(left.operation, moving(1, "a.md", "log/{{date}}/a.md"));
    }
}

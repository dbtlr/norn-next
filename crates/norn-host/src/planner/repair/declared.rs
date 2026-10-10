//! Declared fixes: what the schema's owner wrote as the answer to a finding,
//! composed into a document one fix at a time and judged as the applier will
//! judge it.
//!
//! **A missing required field fills from its rule default.** The candidates
//! for a selected `field/required-missing` finding are the defaults declared
//! by the rules requiring the field on the composed document — the rules its
//! judgment cites — each filled as the defaults fixpoint fills one
//! ([`RuleDefault::fill`](norn_config::schema::RuleDefault::fill)): a `{{path.<name>}}` from what the rule's own
//! `match.path` binds in the document's path, and a clock token from the
//! plan's one reading. A rule declaring no default proposes nothing. The
//! candidates agree only as one written value, as the fixpoint's do: `1` and
//! `1.0` write different bytes, so they disagree. Repair fills that one field
//! and cascades nothing: a default whose fill brings in a rule requiring
//! another field does not fill that field too (ADR 0035's fixpoint is
//! `new`'s and inbox capture's alone).
//!
//! **What is skipped, and with what.** No default declared is
//! [`SkipReason::NoDeclaredFix`]; defaults that disagree are
//! [`SkipReason::ConflictingDefaults`], with every candidate and the rule
//! proposing it; a default reading a capture its rule's `match.path` binds
//! several ways is [`SkipReason::AmbiguousCapture`], noting two of the
//! bindings; a fill that brings in a required field the document lacks is
//! [`SkipReason::BringsInRequiredFields`], naming each such field and the
//! default its rules declare, whatever else it introduces; and a fill that
//! introduces any other violation, or that cannot be set into the document,
//! is [`SkipReason::JudgeWouldRefuse`], with its candidates.
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

use std::collections::BTreeSet;
use std::sync::Arc;

use norn_config::schema::{Breach, VaultSchema};
use norn_wire::{
    AuthoredValue, Binding, Captures, Citation, CitedFinding, Confidence, DocumentPath,
    FindingKind, FindingRow, Operation, OperationId, OperationKind, RequiredField,
    RequiredFieldHead, SchemaViolation, SkipReason, SkippedCandidates, SkippedFinding,
    ValueCandidate, ValueCandidateHead, ValueHead, WriteTarget,
};

use super::{Planned, Repairing};
use crate::applier::{Held, Standing, Verdict, standing, verdict};
use crate::derivation::stored_spelling;
use crate::planner::edit::edited;

/// Whether `row` is a finding a declared fix may answer, so its document's
/// bytes are read: a missing required field.
pub(super) fn has_fix(row: &FindingRow) -> bool {
    row.kind == FindingKind::RequiredMissing
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
    let mut composed = Arc::clone(before);
    // The before-state is judged once, however many fixes are made to it.
    let before = standing(path, before, repairing.declared, repairing.case);
    let mut holds = before.holds();
    let mut decided = vec![None; document.len()];
    for at in finding_order(document) {
        let row = &document[at];
        if !is_rule_kind(row.kind) {
            decided[at] = Some(super::skipped(row));
            continue;
        }
        let Some(held) = holding(row, &holds) else {
            continue;
        };
        let (true, Some(field)) = (has_fix(row), row.target.as_deref()) else {
            decided[at] = Some(super::skipped(row));
            continue;
        };
        let rules = held.rules.clone();
        let fix = Composing {
            row,
            field,
            path,
            before: &before,
            composed: &composed,
            repairing,
        }
        .defaulted(&rules);
        match fix {
            Ok(fix) => {
                let id = OperationId::new(format!("repair-{}", planned.operations.len() + 1))
                    .expect("a repair's operation id is not empty");
                let set = OperationKind::set_frontmatter(
                    WriteTarget::path(path.clone()),
                    field,
                    fix.value,
                );
                planned
                    .operations
                    .push(Operation::new(set).with_id(id.clone()));
                planned
                    .citations
                    .push(Citation::new(id, vec![cited(row, field, fix.clocked)]));
                composed = fix.bytes;
                holds = fix.holds;
            }
            Err(skip) => decided[at] = Some(*skip),
        }
    }
    decided
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

/// What the applier concludes of `after`, composed at `path` from the
/// document `before` judges.
fn judged(
    path: &DocumentPath,
    before: &Standing,
    after: &[u8],
    repairing: &Repairing<'_>,
) -> Verdict {
    verdict(before, path, after, repairing.declared, repairing.case)
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

/// One fix being composed: a selected finding's, on the document at `path`
/// holding `before`, composed so far to `composed`.
struct Composing<'c> {
    row: &'c FindingRow,
    field: &'c str,
    path: &'c DocumentPath,
    before: &'c Standing,
    composed: &'c Arc<[u8]>,
    repairing: &'c Repairing<'c>,
}

/// A fix the judge admits: the value it writes, the bytes it composes to and
/// what those bytes hold.
struct Fix {
    value: AuthoredValue,
    clocked: bool,
    bytes: Arc<[u8]>,
    holds: Vec<Held>,
}

/// One value a rule's default fills to.
struct Proposal {
    value: AuthoredValue,
    rule: String,
    clocked: bool,
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
    /// fill.
    NoClockReading,
}

impl Composing<'_> {
    /// The fix of a missing required field — the default `rules`, the rules
    /// requiring it, declare — or the skip it is left alone with.
    fn defaulted(&self, rules: &BTreeSet<String>) -> Result<Fix, Box<SkippedFinding>> {
        let skip = |reason| SkippedFinding::new(self.row.id, reason);
        let skipped = |finding| Err(Box::new(finding));
        let proposals = match self.proposed(rules) {
            Proposed::Candidates(proposals) => proposals,
            Proposed::AmbiguousCapture { rule, bindings } => {
                return skipped(skip(SkipReason::AmbiguousCapture).with_note(format!(
                    "the rule `{rule}` defaults `{}` from a capture its `match.path` binds \
                     several ways in `{}`: {} and {}",
                    self.field,
                    self.path,
                    spelled(&bindings[0]),
                    spelled(&bindings[1]),
                )));
            }
            Proposed::NoClockReading => {
                return skipped(skip(SkipReason::NoDeclaredFix).with_note(format!(
                    "the default for `{}` reads the clock, and the host's clock cannot be read \
                     as a local time a default can fill",
                    self.field
                )));
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
        let value = first.value.clone();
        let set = OperationKind::set_frontmatter(
            WriteTarget::path(self.path.clone()),
            self.field,
            value.clone(),
        );
        let bytes = match edited(&set, self.composed) {
            Ok(bytes) => bytes,
            Err(detail) => {
                return skipped(
                    skip(SkipReason::JudgeWouldRefuse)
                        .with_candidates(candidates(&proposals))
                        .with_note(format!(
                            "the default cannot be set into the document: {detail}"
                        )),
                );
            }
        };
        let verdict = judged(self.path, self.before, &bytes, self.repairing);
        let brought = brought_in(&verdict.introduced);
        if !brought.is_empty() {
            return skipped(
                skip(SkipReason::BringsInRequiredFields).with_required_fields(required_fields(
                    &brought,
                    &verdict.holds,
                    self.repairing.declared.schema(),
                )),
            );
        }
        if !verdict.introduced.is_empty() {
            return skipped(
                skip(SkipReason::JudgeWouldRefuse).with_candidates(candidates(&proposals)),
            );
        }
        Ok(Fix {
            value,
            clocked: proposals.iter().any(|proposal| proposal.clocked),
            bytes,
            holds: verdict.holds,
        })
    }

    /// What the default each of `rules` declares for the field fills to, in
    /// rule name order, the clock read only for a default reading it.
    fn proposed(&self, rules: &BTreeSet<String>) -> Proposed {
        let schema = self.repairing.declared.schema();
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
                    .map(|glob| glob.bind(self.path.as_str(), self.repairing.case))
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
                match self.repairing.clock.get() {
                    Ok(at) => Some(at),
                    Err(_) => return Proposed::NoClockReading,
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
    let candidates = proposals
        .iter()
        .map(|proposal| ValueCandidate::new(head(&proposal.value)).by_rule(&proposal.rule));
    SkippedCandidates::values(
        ValueCandidateHead::new(candidates, proposals.len() as u64)
            .expect("a head of every candidate it names"),
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
        ]);

        assert!(planned.operations.is_empty());
        assert_eq!(
            planned.skipped,
            vec![
                SkippedFinding::new(7, SkipReason::Unreadable),
                SkippedFinding::new(8, SkipReason::NoDeclaredFix),
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
                finding(3, FindingKind::NotOneOf, "b.md", "status"),
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
}

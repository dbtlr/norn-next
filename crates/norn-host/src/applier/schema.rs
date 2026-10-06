//! The schema check on a composed result: a plan refuses a violation it
//! introduces, and one on a field it writes, unless it is forced.
//!
//! **A force bypasses this check and nothing else**, and it is loud: a forced
//! plan's violations are listed, in the shape a refusal carries them in, on
//! the forecast of its preview and on its applied report. A forced plan whose
//! results are all valid lists nothing.
//!
//! **The judge is the derivation's own.** What a finding over a document would
//! be filed under is what [`plan_document`] concludes from its bytes under the
//! pinned declaration, so the check runs that one derivation on the composed
//! bytes and on the bytes the target held before, and compares the document
//! findings each concludes. No second reading of the schema is written for
//! the applier. A link's health is not a schema violation: the store judges it
//! with the changeset, and a link a plan breaks surfaces as a finding (ADR
//! 0032).
//!
//! **What a violation is about** is its kind and its subject inside the
//! document: the tag a tag breach names, or the whole document for a block
//! nothing read and a document nothing derives. A violation stood before when
//! the same kind about the same subject is concluded from the bytes the target
//! was composed from — the target's own before-state, or, where it was absent,
//! a document the plan takes away, which is where a moved document's content
//! came from.
//!
//! **A violation that stood before still refuses where the plan writes its
//! subject** (ADR 0032). Two things write a subject:
//!
//! - a frontmatter kind — set, remove, push or pop — names the field it
//!   writes, and a violation standing on that field in the result refuses.
//!   Only an undeclared tag stands on a field: the `tags` field
//!   ([`norn_text::TAGS_FIELD`]), where the result's frontmatter carries the
//!   tag. The same tag written only in the body stands on no field. Every
//!   other kind the derivation concludes is about the whole document — its
//!   path, its bytes, its frontmatter block as a whole — and names no field;
//! - any kind writes a tag the result carries a different number of times
//!   than the before-state did, which refuses a standing undeclared tag
//!   whichever kind wrote it.
//!
//! The fields a plan writes are read from its operations alone
//! ([`written_fields`]), so a preview and an apply of one plan judge alike. A
//! whole-document kind — a create, a text replaced, a body or a section
//! edited — writes no named field, so a violation about the whole document
//! that stood before, and one on a field only such a kind rewrote, refuse
//! only by the count above.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use norn_fs::{NormalizedPath, PathNormalizer};
use norn_store::{Change, TagSource};
use norn_wire::{
    CaseFold, DocumentPath, FindingKind, OperationKind, ResolvedPlan, SchemaViolation,
};

use super::observe::identity;
use crate::derivation::{Cause, Declared, plan_document};

/// What the derivation concludes about one document's bytes: each violation,
/// by kind and subject, with its message, how many times it writes each tag,
/// and which tags its frontmatter carries, each folded.
pub(super) struct Judged {
    violations: Vec<((FindingKind, Option<String>), String)>,
    tags: BTreeMap<String, usize>,
    frontmatter_tags: BTreeSet<String>,
}

/// Judge `bytes` as the document at `path` under `declared`, its rules' path
/// globs comparing letters as `case` says.
///
/// **The rule breaches are left out, a dormant carrier.** The derivation's
/// judgment concludes them, and they gate a write under ADR 0037 by their
/// identity — kind, field, offending value and combined constraint — which is
/// the write gate NORN-359 builds. Read here by kind and subject alone they
/// would refuse a repair of one element over every element it leaves
/// standing, and pass a write that swaps one offending value for another, so
/// until that gate lands they refuse nothing and are not listed.
pub(super) fn judge(
    path: &DocumentPath,
    bytes: &[u8],
    declared: &Declared,
    case: CaseFold,
) -> Judged {
    let hash = norn_fs::ContentHash::of(bytes).to_string();
    let plan = plan_document(
        Path::new(path.as_str()),
        path.as_str(),
        bytes,
        hash,
        None,
        declared,
        case,
    );
    let violations = plan
        .findings
        .into_iter()
        .filter(|finding| !matches!(finding.cause, Cause::RuleBreach(_)))
        .map(|finding| {
            (
                (finding.cause.kind(), finding.target.clone()),
                finding.cause.message(&finding.subject),
            )
        })
        .collect();
    let mut tags = BTreeMap::new();
    let mut frontmatter_tags = BTreeSet::new();
    if let Some(Change::Upsert(facts)) = &plan.change {
        for tag in &facts.tags {
            let folded = norn_wire::fold_tag(&tag.name).to_string();
            if tag.source == TagSource::Frontmatter {
                frontmatter_tags.insert(folded.clone());
            }
            *tags.entry(folded).or_default() += 1;
        }
    }
    Judged {
        violations,
        tags,
        frontmatter_tags,
    }
}

/// The frontmatter fields each file's operations in `plan` write, by the
/// file's identity: the field a set, a remove, a push or a pop names, at the
/// document its path target names. A pure function of the operations.
pub(super) fn written_fields(
    plan: &ResolvedPlan,
    normalizer: &PathNormalizer,
) -> BTreeMap<NormalizedPath, BTreeSet<String>> {
    let mut written: BTreeMap<NormalizedPath, BTreeSet<String>> = BTreeMap::new();
    for operation in &plan.operations {
        let (target, field) = match &operation.kind {
            OperationKind::SetFrontmatter { target, field, .. }
            | OperationKind::RemoveFrontmatter { target, field }
            | OperationKind::PushFrontmatter { target, field, .. }
            | OperationKind::PopFrontmatter { target, field, .. } => (target, field),
            _ => continue,
        };
        let Some(file) = target
            .as_path()
            .and_then(|path| identity(normalizer, path.as_str()))
        else {
            continue;
        };
        written.entry(file).or_default().insert(field.clone());
    }
    written
}

/// Whether the violation `kind` about `subject` stands on a field in
/// `written`, the fields the plan writes into the result `after`.
///
/// **The field a kind stands on**: an undeclared tag stands on the `tags`
/// field where the result's frontmatter carries it, and on no field where
/// only its body does. Every other kind names no field here: the path, bytes
/// and frontmatter-block kinds are about the whole document, a link's health
/// is not a schema violation, and the schema-rule kinds are left out of the
/// judgment this check reads until the write gate judges them ([`judge`]).
fn on_written_field(
    kind: FindingKind,
    subject: Option<&str>,
    after: &Judged,
    written: &BTreeSet<String>,
) -> bool {
    match kind {
        FindingKind::UndeclaredTag => {
            written.contains(norn_text::TAGS_FIELD)
                && subject.is_some_and(|tag| {
                    after
                        .frontmatter_tags
                        .contains(&norn_wire::fold_tag(tag).to_string())
                })
        }
        FindingKind::PathBytesNotUtf8
        | FindingKind::PathNamesNoDocument
        | FindingKind::BodyBytesNotUtf8
        | FindingKind::FrontmatterTooLarge
        | FindingKind::FrontmatterUnclosed
        | FindingKind::FrontmatterUnreadable
        | FindingKind::Broken
        | FindingKind::Ambiguous
        | FindingKind::MissingAnchor => false,
        // The schema kinds of ADR 0035 stand on the field their target names,
        // or on the whole document, but the judgment here leaves them out:
        // the write gate that judges a plan's documents by the rules is
        // NORN-359, and under ADR 0037 it refuses a violation by its
        // identity, never for standing on a field the plan writes.
        FindingKind::Misplaced
        | FindingKind::DocumentRulesConflict
        | FindingKind::RequiredMissing
        | FindingKind::Forbidden
        | FindingKind::NotOneOf
        | FindingKind::TooLong
        | FindingKind::TypeMismatch
        | FindingKind::ShapeMismatch
        | FindingKind::FieldRulesConflict => false,
        // A kind minted after these is about the whole document until it is
        // given a field here.
        _ => false,
    }
}

/// The violations `after`, the composed result at `path`, introduces against
/// what stood in `before`, each document it was composed from, where the plan
/// writes the frontmatter fields `written` into it: each refuses an unforced
/// plan, and a forced plan lets each through and lists it.
pub(super) fn introduced(
    path: &DocumentPath,
    after: &Judged,
    before: &[Judged],
    written: &BTreeSet<String>,
) -> Vec<SchemaViolation> {
    let stood: Vec<&(FindingKind, Option<String>)> = before
        .iter()
        .flat_map(|judged| judged.violations.iter().map(|(violation, _)| violation))
        .collect();
    after
        .violations
        .iter()
        .filter(|(violation, _)| {
            if !stood.contains(&violation)
                || on_written_field(violation.0, violation.1.as_deref(), after, written)
            {
                return true;
            }
            match &violation.1 {
                Some(tag) if violation.0 == FindingKind::UndeclaredTag => {
                    let folded = norn_wire::fold_tag(tag).to_string();
                    let written = |judged: &Judged| judged.tags.get(&folded).copied();
                    before
                        .iter()
                        .all(|judged| written(judged) != written(after))
                }
                _ => false,
            }
        })
        .map(|((kind, target), message)| {
            SchemaViolation::new(path.clone(), *kind, target.clone(), message.clone())
        })
        .collect()
}

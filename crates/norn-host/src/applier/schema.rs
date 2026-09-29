//! The schema check on a composed result: a plan refuses a violation it
//! introduces, and one on a field it writes.
//!
//! **The judge is the derivation's own.** What a finding over a document would
//! be filed under is what [`plan_document`] concludes from its bytes under the
//! pinned declaration, so the check runs that one derivation on the composed
//! bytes and on the bytes the target held before, and compares the document
//! findings each concludes. No second reading of the schema is written for
//! the applier. A link's health is not a schema violation: the store judges it
//! with the changeset, and a link a plan breaks surfaces as a finding (ADR
//! 0031).
//!
//! **What a violation is about** is its kind and its subject inside the
//! document: the tag a tag breach names, or the whole document for a block
//! nothing read and a document nothing derives. A violation stood before when
//! the same kind about the same subject is concluded from the bytes the target
//! was composed from — the target's own before-state, or, where it was absent,
//! a document the plan takes away, which is where a moved document's content
//! came from. A violation that stood before still refuses where the plan
//! writes its subject: a tag the result writes a different number of times
//! than the before-state did. A violation about the whole document that stood
//! before names no field, so an edit elsewhere in the document does not
//! refuse on it.

use std::collections::BTreeMap;
use std::path::Path;

use norn_store::Change;
use norn_wire::{DocumentPath, FindingKind, RefusedCheck};

use crate::derivation::{Declared, plan_document};

/// What the derivation concludes about one document's bytes: each violation,
/// by kind and subject, with its message, and how many times it writes each
/// tag, folded.
pub(super) struct Judged {
    violations: Vec<((FindingKind, Option<String>), String)>,
    tags: BTreeMap<String, usize>,
}

/// Judge `bytes` as the document at `path` under `declared`.
pub(super) fn judge(path: &DocumentPath, bytes: &[u8], declared: &Declared) -> Judged {
    let hash = norn_fs::ContentHash::of(bytes).to_string();
    let plan = plan_document(
        Path::new(path.as_str()),
        path.as_str(),
        bytes,
        hash,
        None,
        declared,
    );
    let violations = plan
        .findings
        .into_iter()
        .map(|finding| {
            (
                (finding.cause.kind(), finding.target.clone()),
                finding.cause.message(&finding.subject),
            )
        })
        .collect();
    let mut tags = BTreeMap::new();
    if let Some(Change::Upsert(facts)) = &plan.change {
        for tag in &facts.tags {
            *tags
                .entry(norn_wire::fold_tag(&tag.name).to_string())
                .or_default() += 1;
        }
    }
    Judged { violations, tags }
}

/// The checks refusing `after`, the composed result at `path`, against what
/// stood in `before`: each document it was composed from.
pub(super) fn refused(path: &DocumentPath, after: &Judged, before: &[Judged]) -> Vec<RefusedCheck> {
    let stood: Vec<&(FindingKind, Option<String>)> = before
        .iter()
        .flat_map(|judged| judged.violations.iter().map(|(violation, _)| violation))
        .collect();
    after
        .violations
        .iter()
        .filter(|(violation, _)| {
            if !stood.contains(&violation) {
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
            RefusedCheck::schema_violation(path.clone(), *kind, target.clone(), message.clone())
        })
        .collect()
}

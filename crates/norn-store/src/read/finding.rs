//! The finding row accessor: a finding's own columns as a statement selects
//! them, and the row every read builder that answers findings hands out.
//!
//! **A finding row is read one way.** `validate` pages findings and a find
//! projects the findings standing over each document it found; both select a
//! finding's own columns as [`FINDING_ROW_COLUMNS`] spells them, read each row
//! through [`finding_base`], and hand what they read to
//! [`Snapshot::finding_rows`], which reads the candidate heads and the classes
//! of those findings and nothing else. So the head and the hint a finding
//! carries are the same on every verb that carries it.
//!
//! **The head and the hint are read off what the pillar stores.** The head is
//! the finding's candidate rows, which were bounded at
//! [`crate::CANDIDATE_HEAD`] when they were written, and the total beside them
//! is the one the finding was recorded with; no class is resolved again at
//! read time. The hint names the address the finding's longest class key
//! reads back as ([`crate::ClassKey::address`]): a target's reductions differ
//! only in the leaf, the leaf as written is the longer key, and its address's
//! probe opens every class the target's probe did. A finding in no class has
//! no hint, and neither does one whose address a request could not spell: an
//! address holding `#` reads as a target with an anchor, which names another
//! address.

use std::collections::{BTreeSet, HashMap};

use norn_db::rusqlite::{self, Row};
use norn_wire::{
    Candidate, CandidateHead, FindingKind, FindingRow, Hint, ResolutionTarget, Severity,
};

use super::Ran;
use crate::error::{self, StoreError};
use crate::facts::Span;
use crate::find::{FindStatement, compose_finding_candidates, compose_finding_classes};
use crate::path::ClassKey;
use crate::request::unreadable;
use crate::store::Snapshot;

/// A finding's own columns, in the order [`finding_base`] reads them, under
/// the alias `f`.
pub(crate) const FINDING_ROW_COLUMNS: &str = "f.id, f.kind, f.severity, f.path, f.target, \
     f.span_line, f.span_column, f.span_offset, f.candidates_total, f.message, f.generation";

/// A finding's own columns, as a statement read them, before its head and its
/// hint are read beside them.
#[derive(Clone, Debug)]
pub(crate) struct FindingBase {
    /// The finding's row id, which its candidates and classes are read by.
    pub(crate) id: i64,
    /// The kind, as the pillar stores it.
    pub(crate) kind: String,
    severity: String,
    /// The path the finding stands at, as the pillar stores it.
    pub(crate) path: String,
    target: Option<String>,
    span: Option<Span>,
    candidates_total: u64,
    message: String,
    generation: i64,
}

/// One row of a statement selecting [`FINDING_ROW_COLUMNS`] first.
pub(crate) fn finding_base(row: &Row<'_>) -> rusqlite::Result<FindingBase> {
    let span = match (
        row.get::<_, Option<u64>>(5)?,
        row.get::<_, Option<u64>>(6)?,
        row.get::<_, Option<u64>>(7)?,
    ) {
        (Some(line), Some(column), Some(byte_offset)) => Some(Span {
            line,
            column,
            byte_offset,
        }),
        _ => None,
    };
    Ok(FindingBase {
        id: row.get(0)?,
        kind: row.get(1)?,
        severity: row.get(2)?,
        path: row.get(3)?,
        target: row.get(4)?,
        span,
        candidates_total: row.get(8)?,
        message: row.get(9)?,
        generation: row.get(10)?,
    })
}

impl Snapshot {
    /// The rows of the findings `bases` holds, in its order, each with its
    /// candidate head and its hint, read by two statements over all of them:
    /// [`FindStatement::FindingCandidates`] and
    /// [`FindStatement::FindingClasses`], each recorded in `record`. No
    /// finding, no statement.
    pub(crate) fn finding_rows(
        &self,
        record: &mut Vec<Ran>,
        bases: Vec<FindingBase>,
    ) -> Result<Vec<FindingRow>, StoreError> {
        const OPERATION: &str = "reading the heads of the findings a read found";
        if bases.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<i64> = bases.iter().map(|base| base.id).collect();
        let mut candidates: HashMap<i64, Vec<(i64, Candidate)>> = HashMap::new();
        let read = self
            .run_statement(
                record,
                Ran::new(
                    FindStatement::FindingCandidates,
                    compose_finding_candidates(&ids),
                ),
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .map_err(|problem| error::sql(OPERATION, problem))?;
        for (finding, rank, path, suffix) in read {
            let path = norn_wire::DocumentPath::new(&path)
                .map_err(|_| unreadable("finding_candidates.path", &path))?;
            candidates
                .entry(finding)
                .or_default()
                .push((rank, Candidate::new(path, suffix)));
        }
        let mut classes: HashMap<i64, BTreeSet<ClassKey>> = HashMap::new();
        let read = self
            .run_statement(
                record,
                Ran::new(FindStatement::FindingClasses, compose_finding_classes(&ids)),
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .map_err(|problem| error::sql(OPERATION, problem))?;
        for (finding, written) in read {
            let class = ClassKey::new(&written)
                .map_err(|_| unreadable("finding_classes.class_key", &written))?;
            classes.entry(finding).or_default().insert(class);
        }

        bases
            .into_iter()
            .map(|base| {
                let mut head = candidates.remove(&base.id).unwrap_or_default();
                // The primary key hands a finding's candidates back in rank
                // order; the statement states none, so the order is taken
                // here, over a head the pillar bounded.
                head.sort_by_key(|(rank, _)| *rank);
                let hint = classes.get(&base.id).and_then(hint_of);
                finding_row(base, head.into_iter().map(|(_, candidate)| candidate), hint)
            })
            .collect()
    }
}

/// The request that enumerates the classes `classes` holds: a `find` resolving
/// the address the longest of them reads back as, ties to the byte-least.
/// `None` for no class, and for an address a target cannot spell.
fn hint_of(classes: &BTreeSet<ClassKey>) -> Option<Hint> {
    let longest = classes
        .iter()
        .rev()
        .max_by_key(|class| class.as_str().len())?;
    let address = longest.address();
    if address.contains('#') {
        return None;
    }
    ResolutionTarget::new(address).ok().map(Hint::resolves)
}

/// The row `base` reads as, over its candidate head and its hint.
fn finding_row(
    base: FindingBase,
    candidates: impl Iterator<Item = Candidate>,
    hint: Option<Hint>,
) -> Result<FindingRow, StoreError> {
    let kind = FindingKind::try_from(base.kind.as_str())
        .map_err(|_| unreadable("findings.kind", &base.kind))?;
    let severity = Severity::try_from(base.severity.as_str())
        .map_err(|_| unreadable("findings.severity", &base.severity))?;
    let path = norn_wire::DocumentPath::new(&base.path)
        .map_err(|_| unreadable("findings.path", &base.path))?;
    let head = CandidateHead::new(candidates, base.candidates_total).map_err(|problem| {
        StoreError::Damaged {
            what: format!("a finding's candidate head outgrew its total: {problem}"),
        }
    })?;
    let id = u64::try_from(base.id).map_err(|_| unreadable("findings.id", &base.id.to_string()))?;
    let generation = u64::try_from(base.generation)
        .map_err(|_| unreadable("findings.generation", &base.generation.to_string()))?;
    Ok(FindingRow::new(
        id,
        kind,
        severity,
        path,
        base.target,
        base.span
            .map(|span| norn_wire::Span::new(span.line, span.column, span.byte_offset)),
        head,
        hint,
        base.message,
        generation,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classes(keys: &[&str]) -> BTreeSet<ClassKey> {
        keys.iter()
            .map(|key| ClassKey::new(key).expect("a class key"))
            .collect()
    }

    /// **A hint names the address the longest class reads back as**, whose
    /// probe opens every class the finding is in; a finding in no class, and
    /// one whose address carries `#`, has none.
    #[test]
    fn a_hint_names_the_address_whose_probe_opens_every_class() {
        let target = |hint: Option<Hint>| match hint {
            Some(Hint::Resolves { target, .. }) => Some(target.to_string()),
            _ => None,
        };
        assert_eq!(
            target(hint_of(&classes(&["glossary/norn/"]))),
            Some("norn/glossary".to_string())
        );
        assert_eq!(
            target(hint_of(&classes(&["v1/", "v1.2/"]))),
            Some("v1.2".to_string())
        );
        assert_eq!(hint_of(&BTreeSet::new()), None);
        assert_eq!(hint_of(&classes(&["a#b/"])), None);
    }
}

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
//! **A rule finding's citation and value are read off the row too.** The row
//! carries the identity of the rule set it cites and the head of the value it
//! judged, both as the pillar stores them; resolving a set to its rules is a
//! validate page's, once per set it holds rather than once per row.
//!
//! **The head and the hint are read off what the pillar stores.** The head is
//! the finding's candidate rows, which were bounded at
//! [`crate::CANDIDATE_HEAD`] when they were written, and the total beside them
//! is the one the finding was recorded with; no class is resolved again at
//! read time. Only an ambiguous link's finding (`link/ambiguous`) carries a
//! hint, because only its candidates run past a head a request can enumerate.
//! The hint names the address the finding's longest class key reads back as
//! ([`crate::ClassKey::address`]): a target's reductions differ only in the
//! leaf, the leaf as written is the longer key, and its address's probe opens
//! every class the target's probe did. An ambiguous finding in no class — a
//! rooted name's, keyed by paths alone — has no hint, and neither does one
//! whose address a request could not spell: an address holding `#` reads as a
//! target with an anchor, which names another address.

use std::collections::{BTreeSet, HashMap};

use norn_db::rusqlite::Row;
use norn_wire::{
    Candidate, CandidateHead, CursorKey, FindingKind, FindingRow, Hint, ResolutionTarget, Severity,
};

use super::Ran;
use crate::ddl::findings::DOCUMENT_POSITION;
use crate::error::{self, StoreError};
use crate::facts::Span;
use crate::find::{FindStatement, compose_finding_candidates, compose_finding_classes};
use crate::path::ClassKey;
use crate::request::{Reading, optional_span, optional_value_head, unreadable};
use crate::store::Snapshot;

/// A finding's own columns, in the order [`finding_base`] reads them, under
/// the alias `f`.
pub(crate) const FINDING_ROW_COLUMNS: &str = "f.id, f.kind, f.severity, f.path, f.target, \
     f.span_line, f.span_column, f.span_offset, f.candidates_total, f.message, f.generation, \
     f.ordinal, f.position, f.rule_set, f.value_head, f.value_bytes, f.value_hash";

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
    /// The ordinal of the link the finding is about, and `None` for a finding
    /// about the document at its path.
    ordinal: Option<i64>,
    /// Where the finding stands among its path's findings, as the column
    /// `findings.position` holds it.
    position: i64,
    /// The rule set the finding cites, and `None` for one citing none.
    pub(crate) rule_set: Option<i64>,
    value: Option<norn_wire::ValueHead>,
}

impl FindingBase {
    /// Where the finding stands among its path's findings, as the column
    /// `findings.position` holds it: the link's ordinal, or
    /// [`DOCUMENT_POSITION`] for a finding about the document.
    pub(crate) const fn position(&self) -> i64 {
        self.position
    }

    /// The cursor key a validate's page that stopped at this finding
    /// continues from.
    pub(crate) fn finding_key(&self) -> Result<CursorKey, StoreError> {
        let (kind, ordinal, id) = self.key_parts()?;
        Ok(CursorKey::finding(kind, self.path.clone(), ordinal, id))
    }

    /// The cursor key a get's page of one document's findings that stopped at
    /// this finding continues from.
    pub(crate) fn document_finding_key(&self) -> Result<CursorKey, StoreError> {
        let (kind, ordinal, id) = self.key_parts()?;
        Ok(CursorKey::document_finding(
            self.path.clone(),
            ordinal,
            kind,
            id,
        ))
    }

    /// The kind, the ordinal and the id as a cursor carries them.
    fn key_parts(&self) -> Result<(FindingKind, Option<u64>, u64), StoreError> {
        let kind = FindingKind::try_from(self.kind.as_str())
            .map_err(|_| unreadable("findings.kind", &self.kind))?;
        let ordinal = self
            .ordinal
            .map(|ordinal| {
                u64::try_from(ordinal)
                    .map_err(|_| unreadable("findings.ordinal", &ordinal.to_string()))
            })
            .transpose()?;
        let id =
            u64::try_from(self.id).map_err(|_| unreadable("findings.id", &self.id.to_string()))?;
        Ok((kind, ordinal, id))
    }
}

/// Where a finding a cursor names stands among its path's findings, as the
/// column `findings.position` holds it, and `None` for an ordinal no stored
/// finding carries.
pub(crate) fn cursor_position(ordinal: Option<u64>) -> Option<i64> {
    ordinal.map_or(Some(DOCUMENT_POSITION), |ordinal| {
        i64::try_from(ordinal).ok()
    })
}

/// One row of a statement selecting [`FINDING_ROW_COLUMNS`] first.
///
/// The span is read through [`optional_span`], the reader every stored
/// position is read through, so a position this crate could not have written
/// is [`StoreError::Damaged`] rather than a failed statement.
pub(crate) fn finding_base(row: &Row<'_>) -> Reading<FindingBase> {
    let span = match optional_span(row, 5, "findings")? {
        Ok(span) => span,
        Err(damaged) => return Ok(Err(damaged)),
    };
    let value = match optional_value_head(row, 14)? {
        Ok(value) => value,
        Err(damaged) => return Ok(Err(damaged)),
    };
    Ok(Ok(FindingBase {
        id: row.get(0)?,
        kind: row.get(1)?,
        severity: row.get(2)?,
        path: row.get(3)?,
        target: row.get(4)?,
        span,
        candidates_total: row.get(8)?,
        message: row.get(9)?,
        generation: row.get(10)?,
        ordinal: row.get(11)?,
        position: row.get(12)?,
        rule_set: row.get(13)?,
        value,
    }))
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
                let classes = classes.remove(&base.id).unwrap_or_default();
                finding_row(
                    base,
                    head.into_iter().map(|(_, candidate)| candidate),
                    &classes,
                )
            })
            .collect()
    }
}

/// The request that enumerates the class a finding of `kind` about the link
/// written `target` is about: a `find` resolving the address of the class in
/// `classes` that the link's own address spells — as written, or folded by
/// ASCII case where the root folds it. Only an ambiguous link's finding has
/// candidates past its head to enumerate, so every other kind has none; and
/// neither has a finding in no class its link spells, nor one whose address a
/// target cannot spell.
///
/// A finding is in the classes its link's keys open and in the classes its
/// candidates are named in, and only the first name the link: the class the
/// link spells is the longest of its keys', and a candidate's naming class
/// can be longer, so the hint is found by the link's own address rather than
/// by length. An ambiguous finding in a class — a suffix-addressed link's —
/// carries the hint whether or not its head holds the whole total: the hint
/// names the class, not the candidates the head left out. An ambiguous rooted
/// name is written with its protocol and spells no class, and so carries
/// none.
fn hint_of(kind: FindingKind, target: Option<&str>, classes: &BTreeSet<ClassKey>) -> Option<Hint> {
    if kind != FindingKind::Ambiguous {
        return None;
    }
    let written = target?;
    if written.contains("://") {
        return None;
    }
    let spelled = written.split('#').next()?;
    let class = classes.iter().find(|class| {
        let address = class.address();
        address == spelled || address == spelled.to_ascii_lowercase()
    })?;
    ResolutionTarget::new(class.address())
        .ok()
        .map(Hint::resolves)
}

/// The row `base` reads as, over its candidate head and the hint its kind
/// and `classes` give it.
fn finding_row(
    base: FindingBase,
    candidates: impl Iterator<Item = Candidate>,
    classes: &BTreeSet<ClassKey>,
) -> Result<FindingRow, StoreError> {
    let kind = FindingKind::try_from(base.kind.as_str())
        .map_err(|_| unreadable("findings.kind", &base.kind))?;
    let hint = hint_of(kind, base.target.as_deref(), classes);
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
    let rule_set = base
        .rule_set
        .map(|rule_set| {
            u64::try_from(rule_set)
                .map_err(|_| unreadable("findings.rule_set", &rule_set.to_string()))
        })
        .transpose()?;
    let row = FindingRow::new(
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
    );
    let row = match rule_set {
        Some(rule_set) => row.citing(rule_set),
        None => row,
    };
    Ok(match base.value {
        Some(value) => row.with_value(value),
        None => row,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classes(keys: &[&str]) -> BTreeSet<ClassKey> {
        keys.iter()
            .map(|key| ClassKey::new(key).expect("a class key"))
            .collect()
    }

    /// **A hint names the class the link's own address spells**, which is
    /// the class every other class the link opens is a reduction of, and
    /// never a class a candidate is named in, however long: `[[crowd]]`
    /// whose candidates are named in `crowd.md/` names `crowd`, and on a
    /// folding root the folded class. An anchor is no part of the address. A
    /// finding in no class its link spells, a rooted name, and a finding of
    /// any other kind have none.
    #[test]
    fn a_hint_names_the_class_the_links_own_address_spells() {
        let target = |hint: Option<Hint>| match hint {
            Some(Hint::Resolves { target, .. }) => Some(target.to_string()),
            _ => None,
        };
        let ambiguous = |written: &str, keys: &[&str]| {
            target(hint_of(
                FindingKind::Ambiguous,
                Some(written),
                &classes(keys),
            ))
        };
        assert_eq!(
            ambiguous("norn/glossary", &["glossary/norn/", "glossary.md/"]),
            Some("norn/glossary".to_string())
        );
        assert_eq!(
            ambiguous("v1.2#Setup", &["v1/", "v1.2/", "v1.2.md/"]),
            Some("v1.2".to_string())
        );
        assert_eq!(
            ambiguous("crowd", &["crowd.md/", "crowd/"]),
            Some("crowd".to_string())
        );
        assert_eq!(
            ambiguous("Crowd", &["crowd.md/", "crowd/"]),
            Some("crowd".to_string())
        );
        assert_eq!(ambiguous("crowd", &["crowd.md/"]), None);
        assert_eq!(ambiguous("vault://v1.2", &["v1/", "v1.2/"]), None);
        assert_eq!(ambiguous("crowd", &[]), None);
        assert_eq!(
            hint_of(FindingKind::Ambiguous, None, &classes(&["crowd/"])),
            None
        );
        for kind in FindingKind::ALL {
            if kind != FindingKind::Ambiguous {
                assert_eq!(
                    hint_of(kind, Some("glossary"), &classes(&["glossary/"])),
                    None,
                    "{kind}"
                );
            }
        }
    }
}

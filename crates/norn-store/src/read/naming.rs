//! What a target names on a snapshot, read one way for every read that asks:
//! a get's target and a links-to part's through the one resolver, and each
//! link on a row through the addressing its syntax selects.
//!
//! A suffix address reads the class the one resolver compiles for it
//! ([`TargetClass`]) — a head of at most [`CANDIDATE_HEAD`] documents in the
//! resolution ladder's order, and the class's total where the head filled — and
//! a path reads the documents standing at it under the snapshot's path order,
//! the same way. Every candidate a head carries is named by its **minimal
//! disambiguating suffix**: the first of its path's suffix spellings
//! ([`DocumentPath::suffix_spellings`]) whose class holds that candidate alone,
//! each tried by one statement that stops at a class's second member; a
//! candidate no suffix names alone is named by its path.

use norn_wire::{Anchor, Candidate, CandidateHead, LinkRow};

use super::Ran;
use crate::error::{self, StoreError};
use crate::facts::{CANDIDATE_HEAD, LinkFact, LinkFamily};
use crate::find::{
    FindStatement, compose_candidate_suffix, compose_class_head, compose_class_total,
    compose_path_head, compose_path_total, wire_span,
};
use crate::link::Addressing;
use crate::path::DocumentPath;
use crate::resolve::{AmbiguityIgnore, TargetClass};
use crate::store::Snapshot;

/// What a target names: no document, one, or several.
pub(crate) enum Naming {
    /// The target names no document.
    Nothing,
    /// The target names exactly this document.
    One { document: i64, path: String },
    /// The target names more than one document: the head of them, each named
    /// by its minimal disambiguating suffix, and how many there were.
    Several(CandidateHead),
}

/// The documents a read found for a target, in the order it read them, and
/// how many there were.
struct Head {
    rows: Vec<(i64, String)>,
    total: u64,
}

impl Snapshot {
    /// What the suffix address `address` names on this snapshot, its class
    /// compiled under the snapshot's path order and less the places `ignore`
    /// names. An address that is no suffix address names nothing.
    pub(crate) fn name_target(
        &self,
        address: &str,
        ignore: &AmbiguityIgnore,
        record: &mut Vec<Ran>,
    ) -> Result<Naming, StoreError> {
        let Ok(class) = TargetClass::compile(address, self.path_order(), ignore) else {
            return Ok(Naming::Nothing);
        };
        let head = self.class_head(&class, record)?;
        match head.rows.as_slice() {
            [] => Ok(Naming::Nothing),
            [(document, path)] => Ok(Naming::One {
                document: *document,
                path: path.clone(),
            }),
            _ => Ok(Naming::Several(self.candidates(head, ignore, record)?)),
        }
    }

    /// `link`, held by the document at `holder`, as a row carries it: the
    /// documents its target names now, and the health they give it.
    ///
    /// A link that names no document names none, and is not judged. A suffix
    /// address reads its class through the one resolver, and a path reads the
    /// documents standing at it; a target that is neither names none. A
    /// heading or block anchor is carried as written and not checked: the
    /// row's health is about which document the link names.
    pub(crate) fn link_row(
        &self,
        link: LinkFact,
        holder: &DocumentPath,
        ignore: &AmbiguityIgnore,
        record: &mut Vec<Ran>,
    ) -> Result<LinkRow, StoreError> {
        let head = match Addressing::of(&link, holder) {
            Addressing::NotJudged | Addressing::Path(None) => None,
            Addressing::Suffix(target) => {
                match TargetClass::compile(target, self.path_order(), ignore) {
                    Ok(class) => Some(self.class_head(&class, record)?),
                    Err(_) => None,
                }
            }
            Addressing::Path(Some(path)) => Some(self.path_head(&path, record)?),
        };
        let targets = match head {
            None => CandidateHead::new([], 0).expect("no candidate heads no document"),
            Some(head) => self.candidates(head, ignore, record)?,
        };
        let family = match link.family {
            LinkFamily::Wikilink => norn_wire::LinkFamily::Wikilink,
            LinkFamily::Markdown => norn_wire::LinkFamily::Markdown,
        };
        let anchor = match (link.block_ref, link.anchor) {
            (Some(id), _) => Some(Anchor::block(id)),
            (None, Some(text)) => Some(Anchor::heading(text)),
            (None, None) => None,
        };
        Ok(LinkRow::new(
            family,
            link.embed,
            link.protocol,
            link.target,
            link.title,
            anchor,
            wire_span(link.span),
            targets,
        ))
    }

    /// The head of `class`, at most [`CANDIDATE_HEAD`] of it in the resolution
    /// ladder's order, and its total: counted where the head filled, and the
    /// head's own length where it did not.
    fn class_head(&self, class: &TargetClass, record: &mut Vec<Ran>) -> Result<Head, StoreError> {
        const OPERATION: &str = "reading the documents a target names";
        let rows: Vec<(i64, String)> = self
            .run_statement(
                record,
                Ran::new(
                    FindStatement::ClassHead,
                    compose_class_head(class, CANDIDATE_HEAD),
                ),
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|problem| error::sql(OPERATION, problem))?;
        let total = if rows.len() < CANDIDATE_HEAD {
            rows.len() as u64
        } else {
            self.one_count(
                record,
                Ran::new(FindStatement::ClassTotal, compose_class_total(class)),
                OPERATION,
            )?
        };
        Ok(Head { rows, total })
    }

    /// The documents standing at `path` under the snapshot's path order, at
    /// most [`CANDIDATE_HEAD`] of them in path order, and how many there are.
    fn path_head(&self, path: &str, record: &mut Vec<Ran>) -> Result<Head, StoreError> {
        const OPERATION: &str = "reading the documents a path names";
        let order = self.path_order();
        let rows: Vec<(i64, String)> = self
            .run_statement(
                record,
                Ran::new(
                    FindStatement::PathHead,
                    compose_path_head(path, order, CANDIDATE_HEAD),
                ),
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|problem| error::sql(OPERATION, problem))?;
        let total = if rows.len() < CANDIDATE_HEAD {
            rows.len() as u64
        } else {
            self.one_count(
                record,
                Ran::new(FindStatement::PathTotal, compose_path_total(path, order)),
                OPERATION,
            )?
        };
        Ok(Head { rows, total })
    }

    /// The one count `ran` answers.
    fn one_count(
        &self,
        record: &mut Vec<Ran>,
        ran: Ran,
        operation: &'static str,
    ) -> Result<u64, StoreError> {
        Ok(self
            .run_statement(record, ran, |row| row.get::<_, u64>(0))
            .map_err(|problem| error::sql(operation, problem))?
            .into_iter()
            .next()
            .unwrap_or_default())
    }

    /// `head` as the candidate head a wire row carries, each candidate named by
    /// its minimal disambiguating suffix under `ignore`.
    fn candidates(
        &self,
        head: Head,
        ignore: &AmbiguityIgnore,
        record: &mut Vec<Ran>,
    ) -> Result<CandidateHead, StoreError> {
        let mut candidates = Vec::with_capacity(head.rows.len());
        for (document, path) in &head.rows {
            let suffix = self.candidate_suffix(*document, path, ignore, record)?;
            candidates.push(Candidate::new(wire_path(path)?, suffix));
        }
        CandidateHead::new(candidates, head.total).map_err(|problem| StoreError::Damaged {
            what: format!("a class's head outgrew its count: {problem}"),
        })
    }

    /// The minimal disambiguating suffix of the candidate `document` at
    /// `path`: the first of its suffix spellings whose class holds it alone,
    /// or its path where none does.
    fn candidate_suffix(
        &self,
        document: i64,
        path: &str,
        ignore: &AmbiguityIgnore,
        record: &mut Vec<Ran>,
    ) -> Result<String, StoreError> {
        let at = DocumentPath::new(path)?;
        for spelling in at.suffix_spellings() {
            let Ok(class) = TargetClass::compile(&spelling, self.path_order(), ignore) else {
                continue;
            };
            let members: Vec<i64> = self
                .run_statement(
                    record,
                    Ran::new(
                        FindStatement::CandidateSuffix,
                        compose_candidate_suffix(&class),
                    ),
                    |row| row.get(0),
                )
                .map_err(|problem| error::sql("naming a candidate by its suffix", problem))?;
            if members == [document] {
                return Ok(spelling);
            }
        }
        Ok(path.to_string())
    }
}

/// `path` as the wire spells a document's path.
pub(crate) fn wire_path(path: &str) -> Result<norn_wire::DocumentPath, StoreError> {
    norn_wire::DocumentPath::new(path).map_err(|problem| StoreError::Damaged {
        what: format!("`documents.path` holds no document path: {problem}"),
    })
}

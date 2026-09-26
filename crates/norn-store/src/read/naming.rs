//! What a target names on a snapshot, read one way for every read that asks:
//! a get's target and a links-to part's through the one resolver, and a
//! page's links from the keys derivation held each link under.
//!
//! A target reads the class the one resolver compiles for it
//! ([`TargetClass`]) — a head of at most [`CANDIDATE_HEAD`] documents in the
//! resolution ladder's order, and the class's total where the head filled. **A
//! page's links are read as one set**: one statement reads what every link on
//! the page names from the keys the link index holds it under
//! ([`crate::link`]) — each distinct key once, a suffix key's class through the
//! one resolver's class predicate and a path key's documents at the path under
//! the snapshot's path order — each link's head at most [`CANDIDATE_HEAD`] of
//! them in the ladder's order, beside its total. The head is cut in the
//! statement, so the candidate rows it hands back, which the caller's work
//! counts, are at most [`CANDIDATE_HEAD`] per link. A link held under no key
//! names nothing.
//!
//! Every candidate a head carries is named by its **minimal disambiguating
//! suffix**: the first of its path's suffix spellings
//! ([`DocumentPath::suffix_spellings`]) whose class holds that candidate
//! alone. One statement answers it for every candidate a read names, each
//! range of each spelling's class stopping at its second document; a candidate
//! no suffix names alone is named by its path. So what a read of targets runs
//! is a fixed number of statements, however many links or candidates it names.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use norn_wire::{Anchor, Candidate, CandidateHead, LinkRow};

use super::Ran;
use crate::error::{self, StoreError};
use crate::facts::{CANDIDATE_HEAD, LinkFact};
use crate::find::{
    FindStatement, SpellingRange, compose_candidate_suffixes, compose_class_head,
    compose_class_total, compose_link_targets, wire_span,
};
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
#[derive(Default)]
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
            _ => Ok(Naming::Several(
                self.candidate_heads(vec![head], ignore, record)?
                    .pop()
                    .expect("one head names one candidate head"),
            )),
        }
    }

    /// `links`, each beside its row id, as a row carries them: the documents
    /// each names now, and the health they give it, in the order given.
    ///
    /// What they name is read in two statements, whatever their number: what
    /// every link names ([`FindStatement::LinkTargets`]), and the candidates'
    /// suffixes ([`FindStatement::CandidateSuffixes`]), which no link naming
    /// no document runs. A heading or block anchor is carried as written and
    /// not checked: the row's health is about which document the link names.
    /// The candidate rows what every link names was read as are added to
    /// `candidates_read`.
    pub(crate) fn link_rows(
        &self,
        links: Vec<(i64, LinkFact)>,
        ignore: &AmbiguityIgnore,
        record: &mut Vec<Ran>,
        candidates_read: &mut u64,
    ) -> Result<Vec<LinkRow>, StoreError> {
        let ids: Vec<i64> = links.iter().map(|(id, _)| *id).collect();
        let mut named = self.link_targets(&ids, ignore, record, candidates_read)?;
        let heads: Vec<Head> = ids
            .iter()
            .map(|id| named.remove(id).unwrap_or_default())
            .collect();
        let targets = self.candidate_heads(heads, ignore, record)?;
        links
            .into_iter()
            .zip(targets)
            .map(|((_, link), targets)| {
                let anchor = match (link.block_ref, link.anchor) {
                    (Some(id), _) => Some(Anchor::block(id)),
                    (None, Some(text)) => Some(Anchor::heading(text)),
                    (None, None) => None,
                };
                LinkRow::new(
                    link.family.wire(),
                    link.embed,
                    link.protocol,
                    link.target,
                    link.title,
                    anchor,
                    wire_span(link.span),
                    targets,
                )
                .map_err(|problem| StoreError::Damaged {
                    what: format!("a link addressed elsewhere named a document: {problem}"),
                })
            })
            .collect()
    }

    /// The head of what each link `links` names, by the link's id: at most
    /// [`CANDIDATE_HEAD`] documents in the resolution ladder's order and how
    /// many there were. A link naming no document has no entry. The rows the
    /// statement handed back, each one candidate of one link's head, are
    /// added to `candidates_read`.
    fn link_targets(
        &self,
        links: &[i64],
        ignore: &AmbiguityIgnore,
        record: &mut Vec<Ran>,
        candidates_read: &mut u64,
    ) -> Result<HashMap<i64, Head>, StoreError> {
        if links.is_empty() {
            return Ok(HashMap::new());
        }
        let rows: Vec<(i64, i64, String, u64)> = self
            .run_statement(
                record,
                Ran::new(
                    FindStatement::LinkTargets,
                    compose_link_targets(links, ignore, self.path_order(), CANDIDATE_HEAD),
                ),
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .map_err(|problem| error::sql("reading what a page's links name", problem))?;
        *candidates_read += rows.len() as u64;
        let mut heads: HashMap<i64, Head> = HashMap::new();
        for (link, document, path, total) in rows {
            let head = heads.entry(link).or_default();
            head.rows.push((document, path));
            head.total = total;
        }
        Ok(heads)
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
            self.run_statement(
                record,
                Ran::new(FindStatement::ClassTotal, compose_class_total(class)),
                |row| row.get::<_, u64>(0),
            )
            .map_err(|problem| error::sql(OPERATION, problem))?
            .into_iter()
            .next()
            .unwrap_or_default()
        };
        Ok(Head { rows, total })
    }

    /// `heads` as the candidate heads a wire row carries, in their order, each
    /// candidate named by its minimal disambiguating suffix under `ignore`.
    fn candidate_heads(
        &self,
        heads: Vec<Head>,
        ignore: &AmbiguityIgnore,
        record: &mut Vec<Ran>,
    ) -> Result<Vec<CandidateHead>, StoreError> {
        let named: BTreeMap<i64, &str> = heads
            .iter()
            .flat_map(|head| &head.rows)
            .map(|(document, path)| (*document, path.as_str()))
            .collect();
        let suffixes = self.candidate_suffixes(&named, ignore, record)?;
        heads
            .into_iter()
            .map(|head| {
                let candidates = head
                    .rows
                    .into_iter()
                    .map(|(document, path)| {
                        let suffix = suffixes
                            .get(&document)
                            .cloned()
                            .unwrap_or_else(|| path.clone());
                        Ok(Candidate::new(wire_path(&path)?, suffix))
                    })
                    .collect::<Result<Vec<Candidate>, StoreError>>()?;
                CandidateHead::new(candidates, head.total).map_err(|problem| StoreError::Damaged {
                    what: format!("a class's head outgrew its count: {problem}"),
                })
            })
            .collect()
    }

    /// The minimal disambiguating suffix of each candidate `named` holds, by
    /// its id: the first of its suffix spellings whose class holds it alone. A
    /// candidate no spelling names alone has no entry, and is named by its
    /// path.
    fn candidate_suffixes(
        &self,
        named: &BTreeMap<i64, &str>,
        ignore: &AmbiguityIgnore,
        record: &mut Vec<Ran>,
    ) -> Result<HashMap<i64, String>, StoreError> {
        if named.is_empty() {
            return Ok(HashMap::new());
        }
        let order = self.path_order();
        // Every spelling of every candidate, the classes compiled once and
        // held while the statement's ranges borrow their bounds.
        let mut spelled: Vec<(i64, String, TargetClass)> = Vec::new();
        for (document, path) in named {
            for spelling in DocumentPath::new(path)?.suffix_spellings() {
                if let Ok(class) = TargetClass::compile(&spelling, order, ignore) {
                    spelled.push((*document, spelling, class));
                }
            }
        }
        let mut ranges: Vec<SpellingRange<'_>> = Vec::new();
        let mut owners: Vec<usize> = Vec::new();
        for (at, (_, spelling, class)) in spelled.iter().enumerate() {
            for (lower, upper) in class.probe().ranges() {
                ranges.push(SpellingRange {
                    lower,
                    upper,
                    segments: spelling.split('/').count(),
                });
                owners.push(at);
            }
        }
        let rows: Vec<(usize, i64)> = self
            .run_statement(
                record,
                Ran::new(
                    FindStatement::CandidateSuffixes,
                    compose_candidate_suffixes(&ranges, ignore, order)?,
                ),
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|problem| error::sql("naming candidates by their suffixes", problem))?;
        let mut members: Vec<BTreeSet<i64>> = vec![BTreeSet::new(); spelled.len()];
        for (range, document) in rows {
            let owner = owners.get(range).ok_or_else(|| StoreError::Damaged {
                what: format!("a suffix read answered a range it was not asked, {range}"),
            })?;
            members[*owner].insert(document);
        }
        let mut suffixes: HashMap<i64, String> = HashMap::new();
        for ((document, spelling, _), members) in spelled.into_iter().zip(members) {
            if members.len() == 1 && members.contains(&document) {
                suffixes.entry(document).or_insert(spelling);
            }
        }
        Ok(suffixes)
    }
}

/// `path` as the wire spells a document's path.
pub(crate) fn wire_path(path: &str) -> Result<norn_wire::DocumentPath, StoreError> {
    norn_wire::DocumentPath::new(path).map_err(|problem| StoreError::Damaged {
        what: format!("`documents.path` holds no document path: {problem}"),
    })
}

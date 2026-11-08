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
//! the comparison key — each link's head at most [`CANDIDATE_HEAD`] of
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
//! no suffix names alone is named by its path. Naming targets runs a fixed
//! number of statements, however many links or candidates it names. With marks
//! present, zero-target path links additionally probe their ancestors.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use norn_db::rusqlite::types::Value;
use norn_wire::{Anchor, Candidate, CandidateHead, LinkAddress, LinkRow, TargetGap};

use super::Ran;
use crate::error::{self, StoreError};
use crate::facts::{CANDIDATE_HEAD, LinkAnchor, LinkFact};
use crate::find::{
    FindStatement, SpellingRange, compose_candidate_suffixes, compose_class_head,
    compose_class_total, compose_link_targets, wire_span,
};
use crate::path::{ClassKey, DocumentPath};
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
    /// Name a tie's bounded, path-ordered head through the same minimal suffixes
    /// and ambiguity-ignore rule as a resolution head, on this snapshot.
    /// Paths are live candidates already read on this snapshot.
    pub fn candidate_head(
        &self,
        paths: &[norn_wire::DocumentPath],
        total: u64,
        ignore: &AmbiguityIgnore,
    ) -> Result<CandidateHead, StoreError> {
        self.read_candidate_head(paths, total, ignore, &mut Vec::new())
    }

    /// Explain the statements used to name a tied candidate head.
    pub fn candidate_head_plans(
        &self,
        paths: &[norn_wire::DocumentPath],
        total: u64,
        ignore: &AmbiguityIgnore,
    ) -> Result<Vec<super::SimilarStemsPlan>, StoreError> {
        let mut record = Vec::new();
        self.read_candidate_head(paths, total, ignore, &mut record)?;
        self.explained(record, |statement, _, plan| super::SimilarStemsPlan {
            statement,
            plan,
        })
    }

    fn read_candidate_head(
        &self,
        paths: &[norn_wire::DocumentPath],
        total: u64,
        ignore: &AmbiguityIgnore,
        record: &mut Vec<Ran>,
    ) -> Result<CandidateHead, StoreError> {
        let mut rows = Vec::new();
        for path in paths.iter().take(CANDIDATE_HEAD) {
            let found = self
                .run_statement(
                    record,
                    Ran::new(
                        super::ReadStatement::CandidateDocument,
                        (
                            "SELECT id, path FROM documents WHERE path = ?1".into(),
                            vec![Value::Text(path.as_str().to_string())],
                        ),
                    ),
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
                )
                .map_err(|problem| error::sql("reading a tied candidate's document id", problem))?;
            rows.push(
                found
                    .into_iter()
                    .next()
                    .expect("a candidate from this snapshot"),
            );
        }
        Ok(self
            .candidate_heads(vec![Head { rows, total }], ignore, record)?
            .pop()
            .expect("one head"))
    }

    /// What the suffix address `address` names on this snapshot, its class
    /// compiled under the comparison key and less the places `ignore`
    /// names. An address that is no suffix address names nothing.
    pub(crate) fn name_target(
        &self,
        address: &str,
        ignore: &AmbiguityIgnore,
        record: &mut Vec<Ran>,
    ) -> Result<Naming, StoreError> {
        let Ok(class) = TargetClass::compile(address, ignore) else {
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
    /// no document runs. With directory marks present, a zero-target path
    /// link additionally probes its ancestors before choosing its health.
    /// A heading or block anchor is carried as written and
    /// not checked: the row's health is about which document the link names.
    /// The candidate rows what every link names was read as are added to
    /// `candidates_read`. Only zero-target path forms probe directory ancestors,
    /// and only while the snapshot's establishing read found a mark.
    pub(crate) fn link_rows(
        &self,
        links: Vec<(i64, LinkFact)>,
        ignore: &AmbiguityIgnore,
        record: &mut Vec<Ran>,
        candidates_read: &mut u64,
    ) -> Result<Vec<LinkRow>, StoreError> {
        if self.relations_loading {
            return links
                .into_iter()
                .map(|(_, link)| {
                    let anchor = link.anchor.map(|anchor| match anchor {
                        LinkAnchor::Heading { written, .. } => Anchor::heading(written),
                        LinkAnchor::Block { id } => Anchor::block(id),
                    });
                    LinkRow::new(
                        link.family.wire(),
                        link.embed,
                        link.protocol,
                        link.target,
                        link.title,
                        anchor,
                        link.span.map(wire_span),
                        None,
                    )
                    .map_err(|problem| StoreError::Damaged {
                        what: problem.to_string(),
                    })
                })
                .collect();
        }
        let ids: Vec<i64> = links.iter().map(|(id, _)| *id).collect();
        let mut named = self.link_targets(&ids, record, candidates_read)?;
        let heads: Vec<Head> = ids
            .iter()
            .map(|id| named.remove(id).unwrap_or_default())
            .collect();
        let targets = self.candidate_heads(heads, ignore, record)?;
        links
            .into_iter()
            .zip(targets)
            .map(|((id, link), targets)| {
                let path_form = matches!(
                    LinkAddress::of(link.family.wire(), link.protocol.as_deref(), &link.target),
                    LinkAddress::Rooted(_) | LinkAddress::Relative(_) | LinkAddress::RootedName(_)
                );
                let gap = if self.directory_marks_present && targets.total() == 0 && path_form {
                    let marked = self
                        .run_statement(
                            record,
                            Ran::new(
                                crate::ResolutionStatement::DirectoryMarks,
                                crate::health::statement::stored_directory_probe(id),
                            ),
                            |row| row.get::<_, bool>(0),
                        )
                        .map_err(|problem| {
                            error::sql("probing directory marks for a link row", problem)
                        })?;
                    if marked.first() == Some(&true) {
                        TargetGap::Marked
                    } else {
                        TargetGap::None
                    }
                } else {
                    TargetGap::None
                };
                let anchor = link.anchor.map(|anchor| match anchor {
                    LinkAnchor::Heading { written, .. } => Anchor::heading(written),
                    LinkAnchor::Block { id } => Anchor::block(id),
                });
                LinkRow::new(
                    link.family.wire(),
                    link.embed,
                    link.protocol,
                    link.target,
                    link.title,
                    anchor,
                    link.span.map(wire_span),
                    targets,
                )
                .map(|row| row.with_target_gap(gap))
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
                    compose_link_targets(links, CANDIDATE_HEAD),
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
        let spellings = SuffixSpellings::new(named, ignore)?;
        let rows: Vec<(usize, i64)> = self
            .run_statement(
                record,
                Ran::new(FindStatement::CandidateSuffixes, spellings.statement()?),
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|problem| error::sql("naming candidates by their suffixes", problem))?;
        spellings.suffixes(rows)
    }
}

/// Every suffix spelling of a set of candidates, each spelling's class
/// compiled once, and the ranges [`FindStatement::CandidateSuffixes`] reads
/// them through: the one way a candidate's minimal disambiguating suffix is
/// found, whichever connection the statement runs on.
///
/// Candidates of one class share their shorter spellings, so each distinct
/// spelling is compiled and read once, however many candidates spell it:
/// what the statement answers for a spelling depends on the spelling alone.
pub(crate) struct SuffixSpellings {
    /// Each distinct spelling of the candidates, and the class it opens.
    spellings: Vec<(String, TargetClass)>,
    /// Each candidate's id and the index in `spellings` of one of its
    /// spellings, in the order the candidate's spellings run.
    spelled: Vec<(i64, usize)>,
    /// The index in `spellings` of the spelling each range of the statement
    /// belongs to.
    owners: Vec<usize>,
}

impl SuffixSpellings {
    /// The spellings of each candidate `named` holds, by its id and its path,
    /// compiled under the comparison key less the places `ignore` names.
    pub(crate) fn new(
        named: &BTreeMap<i64, &str>,
        ignore: &AmbiguityIgnore,
    ) -> Result<Self, StoreError> {
        let mut spellings: Vec<(String, TargetClass)> = Vec::new();
        // A spelling no class compiles from is refused for every candidate
        // that spells it, and is no spelling of any of them.
        let mut compiled: HashMap<String, Option<usize>> = HashMap::new();
        let mut spelled: Vec<(i64, usize)> = Vec::new();
        for (document, path) in named {
            for spelling in DocumentPath::new(path)?.suffix_spellings() {
                let at = match compiled.get(&spelling) {
                    Some(at) => *at,
                    None => {
                        let at = TargetClass::compile(&spelling, ignore).ok().map(|class| {
                            spellings.push((spelling.clone(), class));
                            spellings.len() - 1
                        });
                        compiled.insert(spelling, at);
                        at
                    }
                };
                if let Some(at) = at {
                    spelled.push((*document, at));
                }
            }
        }
        let owners = spellings
            .iter()
            .enumerate()
            .flat_map(|(at, (_, class))| class.probe().ranges().map(move |_| at))
            .collect();
        Ok(SuffixSpellings {
            spellings,
            spelled,
            owners,
        })
    }

    /// The statement that reads at most two documents of each range of each
    /// distinct spelling's class, each row the range's index and a
    /// document's id.
    pub(crate) fn statement(&self) -> Result<(String, Vec<Value>), StoreError> {
        let ranges: Vec<SpellingRange<'_>> = self
            .spellings
            .iter()
            .flat_map(|(spelling, class)| {
                let segments = spelling.split('/').count();
                let exact_keys: Vec<&str> =
                    class.probe().ranges().map(|(lower, _)| lower).collect();
                class
                    .probe()
                    .ranges()
                    .map(move |(lower, upper)| SpellingRange {
                        lower,
                        upper,
                        segments,
                        exact_keys: exact_keys.clone(),
                    })
            })
            .collect();
        compose_candidate_suffixes(&ranges)
    }

    /// The classes each candidate's suffix is read from, by its id: the class
    /// ([`ClassKey::leading`]) of every key each of its spellings' classes
    /// opens, including both classes where a spelling ends in `.md`. The statement reads
    /// documents inside those classes and nowhere else, so a change moves a
    /// candidate's suffix only where it changes one of them.
    pub(crate) fn naming_classes(&self) -> HashMap<i64, BTreeSet<ClassKey>> {
        let mut classes: HashMap<i64, BTreeSet<ClassKey>> = HashMap::new();
        for (document, at) in &self.spelled {
            classes.entry(*document).or_default().extend(
                self.spellings[*at]
                    .1
                    .class_keys()
                    .iter()
                    .map(ClassKey::leading),
            );
        }
        classes
    }

    /// Each candidate's minimal disambiguating suffix, by its id, read off the
    /// rows [`SuffixSpellings::statement`] answered: the first of its
    /// spellings whose class held it alone.
    pub(crate) fn suffixes(
        self,
        rows: Vec<(usize, i64)>,
    ) -> Result<HashMap<i64, String>, StoreError> {
        let mut members: Vec<BTreeSet<i64>> = vec![BTreeSet::new(); self.spellings.len()];
        for (range, document) in rows {
            let owner = self.owners.get(range).ok_or_else(|| StoreError::Damaged {
                what: format!("a suffix read answered a range it was not asked, {range}"),
            })?;
            members[*owner].insert(document);
        }
        let mut suffixes: HashMap<i64, String> = HashMap::new();
        for (document, at) in self.spelled {
            if members[at].len() == 1 && members[at].contains(&document) {
                suffixes
                    .entry(document)
                    .or_insert_with(|| self.spellings[at].0.clone());
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use norn_db::rusqlite::types::Value;

    use super::SuffixSpellings;
    use crate::resolve::AmbiguityIgnore;

    /// How many ranges the suffix statement reads to name `paths`.
    fn ranges_read(paths: &[String]) -> usize {
        let named: BTreeMap<i64, &str> = (1..).zip(paths.iter().map(String::as_str)).collect();
        let spellings =
            SuffixSpellings::new(&named, &AmbiguityIgnore::none()).expect("the candidates spell");
        let (_, values) = spellings.statement().expect("the suffix statement");
        let [Value::Text(listed)] = &values[..] else {
            panic!("the suffix statement binds its ranges as one list: {values:?}");
        };
        serde_json::from_str::<Vec<serde_json::Value>>(listed)
            .expect("the ranges are a JSON list")
            .len()
    }

    /// **Candidates read the spellings they share once.** Each of
    /// `dN/hub.md` spells `hub` and `hub.md`, three ranges between them,
    /// which every candidate of the class shares, and `dN/hub` and
    /// `dN/hub.md`, three more of its own. So naming one candidate reads six
    /// ranges, and each candidate more reads three: the ranges that tell it
    /// apart, never again the ones its class shares.
    #[test]
    fn candidates_read_the_spellings_they_share_once() {
        let candidates = |count: usize| -> Vec<String> {
            (0..count).map(|at| format!("d{at}/hub.md")).collect()
        };
        assert_eq!(ranges_read(&candidates(1)), 6);
        assert_eq!(ranges_read(&candidates(2)), 9);
        assert_eq!(ranges_read(&candidates(5)), 18);
        // Candidates that share no spelling read every range of their own.
        assert_eq!(
            ranges_read(&["a/one.md".to_string(), "b/two.md".to_string()]),
            12
        );
    }
}

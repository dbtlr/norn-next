//! Bounded candidate retrieval over the stem trigram index: the documents
//! whose stems may reach the similarity cutoff against a query stem, with the
//! counts a score is computed from.
//!
//! **A dormant carrier.** Its consuming layer is Layer 5C's similarity
//! suggestion (NORN-400), the repair planner's broken-link fix, which scores
//! the candidates and decides the tie, too-common and too-short skips on its
//! planning snapshot. No planner reaches it yet, so its bars are its own seam's:
//! the store suite's retrieval and plan cases and the counter gate's
//! size-independence pair.

use crate::error::{self, StoreError};
use crate::path::DocumentPath;
use crate::read::{Ran, ReadStatement};
use crate::similarity::{POSTINGS_CAP, required_shared, trigrams};
use crate::store::Snapshot;
use norn_db::EmittedPlan;
use norn_db::rusqlite::types::Value;
use std::collections::BTreeSet;

/// A path reached by the prefix filter, with the counts a planner scores.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StemCandidate {
    pub path: DocumentPath,
    pub shared: usize,
    pub candidate_trigrams: usize,
}

/// The prefix filter's answer. `TooCommon` declines retrieval where a selected
/// posting list exceeds the cap, naming the rarest such trigram for the skip's
/// note; `Candidates` makes no scoring decision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StemMatches {
    TooCommon { trigram: String },
    Candidates(Vec<StemCandidate>),
}

/// One query trigram's rarity probe: at most the cap plus one of its postings,
/// so a count past the cap says "too common" without reading the rest.
const RARITY_SQL: &str = "SELECT document FROM document_trigrams WHERE trigram = ?1 LIMIT ?2";

/// One selected trigram's full postings, each with the path its stem is
/// recomputed from.
const POSTINGS_SQL: &str = "SELECT d.path
    FROM document_trigrams AS t JOIN documents AS d ON d.id = t.document
    WHERE t.trigram = ?1";

/// The candidates and query width, beside the logical rows spent reaching them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SimilarStems {
    pub matches: StemMatches,
    pub query_trigrams: usize,
    pub work: SimilarStemsWork,
}

/// Rows handed back by the rarity probes and selected postings reads. A row
/// returned by several statements counts on each, before the union deduplicates.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SimilarStemsWork {
    pub rarity_rows_read: u64,
    pub postings_rows_read: u64,
}

impl SimilarStemsWork {
    /// Every logical work count, including zero, in a stable vocabulary.
    pub fn readings(&self) -> impl Iterator<Item = (&'static str, u64)> + '_ {
        [
            ("rarity_rows_read", self.rarity_rows_read),
            ("postings_rows_read", self.postings_rows_read),
        ]
        .into_iter()
    }
}

/// A statement the stem read ran, explained with the text and values it used.
#[derive(Clone, Debug)]
pub struct SimilarStemsPlan {
    pub statement: ReadStatement,
    pub plan: EmittedPlan,
}

impl Snapshot {
    /// Retrieve stem candidates through the T-k+1 rarest query trigrams, where
    /// T is the query set's width and k = ceil(0.5*T). A candidate reaching the
    /// cutoff shares at least k of the T trigrams, hence at least one of these
    /// T-k+1: missing all of them leaves only k-1 to share. The candidate set
    /// is therefore complete relative to the cutoff, without scoring here.
    ///
    /// The rarity probes return at most T*(cap+1) rows, and the selected
    /// postings return at most (T-k+1)*cap rows. If any selected list exceeds
    /// cap, the answer is `TooCommon` and no postings statement runs.
    pub fn similar_stems(&self, stem: &str) -> Result<SimilarStems, StoreError> {
        self.read_similar_stems(stem, &mut Vec::new())
    }

    /// Explain every rarity and postings statement the read actually ran.
    pub fn similar_stems_plans(&self, stem: &str) -> Result<Vec<SimilarStemsPlan>, StoreError> {
        let mut record = Vec::new();
        self.read_similar_stems(stem, &mut record)?;
        self.explained(record, |statement, _, plan| SimilarStemsPlan {
            statement,
            plan,
        })
    }

    fn read_similar_stems(
        &self,
        stem: &str,
        record: &mut Vec<Ran>,
    ) -> Result<SimilarStems, StoreError> {
        let query = trigrams(stem);
        let t = query.len();
        let prefix = t - required_shared(t) + 1;
        let mut work = SimilarStemsWork::default();
        let mut rarity = Vec::new();
        for trigram in &query {
            let rows = self
                .run_statement(
                    record,
                    Ran::new(
                        ReadStatement::StemRarity,
                        (
                            RARITY_SQL.to_string(),
                            vec![
                                Value::Text(trigram.clone()),
                                Value::Integer((POSTINGS_CAP + 1) as i64),
                            ],
                        ),
                    ),
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|problem| error::sql("probing stem trigram rarity", problem))?;
            work.rarity_rows_read += rows.len() as u64;
            rarity.push((rows.len(), trigram));
        }
        rarity.sort();
        if let Some((_, trigram)) = rarity[..prefix]
            .iter()
            .find(|(count, _)| *count > POSTINGS_CAP)
        {
            return Ok(SimilarStems {
                matches: StemMatches::TooCommon {
                    trigram: (*trigram).clone(),
                },
                query_trigrams: t,
                work,
            });
        }
        let mut paths = BTreeSet::new();
        for (_, trigram) in &rarity[..prefix] {
            let rows = self
                .run_statement(
                    record,
                    Ran::new(
                        ReadStatement::StemPostings,
                        (
                            POSTINGS_SQL.to_string(),
                            vec![Value::Text((*trigram).clone())],
                        ),
                    ),
                    |row| row.get::<_, String>(0),
                )
                .map_err(|problem| error::sql("reading stem trigram postings", problem))?;
            work.postings_rows_read += rows.len() as u64;
            for path in rows {
                paths.insert(DocumentPath::new(&path)?);
            }
        }
        let candidates = paths
            .into_iter()
            .map(|path| {
                let candidate = trigrams(path.stem());
                StemCandidate {
                    shared: query.intersection(&candidate).count(),
                    candidate_trigrams: candidate.len(),
                    path,
                }
            })
            .collect();
        Ok(SimilarStems {
            matches: StemMatches::Candidates(candidates),
            query_trigrams: t,
            work,
        })
    }
}

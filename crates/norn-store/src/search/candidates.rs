//! What a search ranks beyond its lexical floor draws on from a snapshot: the
//! documents its conjunction admits, the judgment of a cursor a ranking over
//! them minted, and the rows of the hits that ranking answers.
//!
//! A rung above the floor ranks over state of its own, never over this
//! snapshot, so it reaches the snapshot through these three and no statement
//! of its own: the candidates are a find's pages, the judgment is the one a
//! lexical page's cursor is judged by, and the rows are hydrated by the one
//! hydration a find's rows are read through.

use norn_wire::{
    AnswerAdvisory, Column, Cursor, FindParams, Hit, HitResume, RungSet, Score, SidecarRevision,
    Unsatisfied,
};

use crate::fields::ContentModel;
use crate::find::{FindWork, FoundKey, Projection};
use crate::read::{Lookups, PageRefusal, ResolvesPart};
use crate::store::Snapshot;

/// A document a search's conjunction admits on one snapshot: the key its row
/// is hydrated by, and its path.
///
/// It names a row of the snapshot it was read from, so it is handed back to
/// that snapshot's [`Snapshot::hydrate_hits`] and to no other.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Candidate {
    key: FoundKey,
}

impl Candidate {
    /// The document's path.
    pub fn path(&self) -> &str {
        self.key.path()
    }
}

/// One page of the documents a search's conjunction admits, in path order.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidates {
    /// The documents, at most the page bound of them, in path order.
    pub candidates: Vec<Candidate>,
    /// Where the next page begins, and `None` where this page is the last.
    pub next: Option<Cursor>,
    /// The parts of the conjunction that could not be applied as asked, in
    /// the order the request names them, a `resolves` part among them.
    pub unsatisfied: Vec<Unsatisfied>,
    /// What the parts that were applied assumed.
    pub advisories: Vec<AnswerAdvisory>,
    /// What the page read.
    pub work: FindWork,
}

/// The hits a ranking answers, each carrying the row its request's columns
/// name.
#[derive(Clone, Debug, PartialEq)]
pub struct HitRows {
    /// The hits, in the order they were handed in.
    pub hits: Vec<Hit>,
    /// The projected keys the vault's field universe does not hold, in the
    /// order the columns name them.
    pub unsatisfied: Vec<Unsatisfied>,
    /// What the hydration read.
    pub work: FindWork,
}

impl Snapshot {
    /// One page of the documents `params`' conjunction admits as a search reads
    /// it: the find of `params`, whose `resolves` parts are reported as not
    /// applicable and filter nothing.
    ///
    /// Every statement it runs is a find's, judged, compiled and paged as a
    /// find is, and it is refused where that find is refused.
    pub fn search_candidates(
        &self,
        params: &FindParams,
        declared: &ContentModel,
    ) -> Result<Candidates, PageRefusal> {
        let paged = self.run_find(
            params,
            declared,
            ResolvesPart::NotApplicable,
            &mut Lookups::default(),
        )?;
        Ok(Candidates {
            candidates: paged
                .keys
                .into_iter()
                .map(|key| Candidate { key })
                .collect(),
            next: paged.found.next,
            unsatisfied: paged.found.unsatisfied,
            advisories: paged.found.advisories,
            work: paged.found.work,
        })
    }

    /// Judge a cursor continuing a ranking by `ladder` whose answer read the
    /// sidecar state `sidecar`: where it resumes, and what moved since.
    ///
    /// The judgment is the one a lexical page's cursor meets, against this
    /// snapshot's reading with `sidecar` beside it, so a cursor minted under
    /// another sidecar state reports that the sidecar moved.
    pub fn judge_hit_cursor<'c>(
        &self,
        cursor: &'c Cursor,
        ladder: &RungSet,
        sidecar: Option<SidecarRevision>,
    ) -> Result<HitResume<'c>, PageRefusal> {
        self.judge_ranked_reading(cursor, ladder, sidecar, &mut Lookups::default())
    }

    /// The hits `ranked` names, in its order, each scored as it names and
    /// carrying its document row where `columns` names a column.
    ///
    /// Hydrated by the one hydration a find's rows are read through, so a hit
    /// costs the columns it names, and refused as a find is for a column this
    /// build of the store does not know or a declaration read from another
    /// schema than the snapshot pins.
    pub fn hydrate_hits(
        &self,
        ranked: &[(Candidate, Score)],
        columns: &[Column],
        declared: &ContentModel,
    ) -> Result<HitRows, PageRefusal> {
        let mut lookups = Lookups::default();
        let projection = Projection::of(columns)?;
        self.declaration_pinned(declared, &mut lookups)?;
        let mut reports = Vec::new();
        let fields = self.projected_keys(&projection, declared, &mut lookups, &mut reports)?;
        let unsatisfied = self.resolve(reports, declared, &mut lookups)?;
        let mut work = FindWork::default();
        let mut rows = if columns.is_empty() {
            Vec::new()
        } else {
            let keys: Vec<FoundKey> = ranked
                .iter()
                .map(|(candidate, _)| candidate.key.clone())
                .collect();
            self.hydrate_rows(
                &keys,
                &projection,
                &fields,
                declared,
                &mut lookups,
                &mut work,
            )?
        }
        .into_iter();
        let hits = ranked
            .iter()
            .map(|(candidate, score)| {
                let path = crate::read::wire_path(candidate.path())?;
                let hit = Hit::new(path, *score);
                Ok(match rows.next() {
                    Some(row) => hit.with_document(row),
                    None => hit,
                })
            })
            .collect::<Result<Vec<_>, crate::error::StoreError>>()?;
        Ok(HitRows {
            hits,
            unsatisfied,
            work,
        })
    }
}

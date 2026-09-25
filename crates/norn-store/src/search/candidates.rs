//! What a search ranks beyond its lexical floor draws on from a snapshot: the
//! documents its conjunction admits, which of a set of paths the snapshot
//! holds, how far the change feeds run past a pair of generations, the
//! judgment of a cursor a ranking minted, and the rows of the hits that
//! ranking answers.
//!
//! A rung above the floor ranks over state of its own, never over this
//! snapshot, so it reaches the snapshot through these and no statement of its
//! own: the candidates are a find's pages, the held paths and the feed rows
//! are this builder's two bounded statements, the judgment is the one a
//! lexical page's cursor is judged by, and the rows are hydrated by the one
//! hydration a find's rows are read through.

use norn_wire::{
    AnswerAdvisory, Column, Cursor, FindParams, Hit, HitResume, RungSet, Score, SidecarRevision,
    Unsatisfied,
};

use std::collections::BTreeMap;

use super::SearchPlan;
use super::statement::{SearchStatement, compose_feed_rows_after, compose_held_paths};
use crate::error::{self, StoreError};
use crate::fields::ContentModel;
use crate::find::{FindWork, FoundKey, Projection};
use crate::read::{Lookups, PageRefusal, Ran, ResolvesPart};
use crate::store::Snapshot;

/// A document a search ranks on one snapshot — one its conjunction admits, or
/// one of the paths it asked the snapshot about that the snapshot holds: the
/// key its row is hydrated by, and its path.
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
    /// The reading the page was answered from, as a cursor carries it. A path
    /// order is no schema's, so it names no fingerprint.
    pub snapshot: norn_wire::Snapshot,
    /// What the page read.
    pub work: FindWork,
}

/// The documents among a set of paths one snapshot holds.
#[derive(Clone, Debug, PartialEq)]
pub struct Held {
    /// The documents, each once, in path byte order.
    pub candidates: Vec<Candidate>,
    /// The reading the paths were read at, as a cursor carries it. It names
    /// no fingerprint, as a path order's reading names none.
    pub snapshot: norn_wire::Snapshot,
}

/// How many rows each change feed holds past a generation, each counted to
/// the bound its question named.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FeedRows {
    /// The document rows standing at a generation past the documents feed's.
    pub documents: u64,
    /// The deaths recorded at a generation past the death feed's.
    pub tombstones: u64,
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
            snapshot: paged.found.snapshot,
            work: paged.found.work,
        })
    }

    /// The documents among `paths` this snapshot holds, each once, in path
    /// byte order, and the reading they were read at.
    ///
    /// A path is compared as the bytes it is stored under. One statement
    /// ([`SearchStatement::HeldPaths`]) answers every path, one seek of
    /// `documents_path` each, so the question costs the paths it names and
    /// never the vault; a question naming no path runs no statement.
    pub fn held_candidates(&self, paths: &[&str]) -> Result<Held, StoreError> {
        self.run_held(paths, &mut Lookups::default())
    }

    /// Every statement [`Snapshot::held_candidates`] runs for `paths`, each
    /// with the plan SQLite reported for the text and values it ran with.
    pub fn held_candidates_plans(&self, paths: &[&str]) -> Result<Vec<SearchPlan>, StoreError> {
        let mut lookups = Lookups::default();
        self.run_held(paths, &mut lookups)?;
        self.explained(lookups.ran, |statement, filters, plan| SearchPlan {
            statement,
            filters,
            plan,
        })
    }

    fn run_held(&self, paths: &[&str], lookups: &mut Lookups) -> Result<Held, StoreError> {
        let mut held: BTreeMap<String, i64> = BTreeMap::new();
        if !paths.is_empty() {
            let read = self
                .run_statement(
                    &mut lookups.ran,
                    Ran::new(SearchStatement::HeldPaths, compose_held_paths(paths)?),
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
                )
                .map_err(|problem| error::sql("reading which paths the snapshot holds", problem))?;
            held.extend(read.into_iter().map(|(document, path)| (path, document)));
        }
        Ok(Held {
            candidates: held
                .into_iter()
                .map(|(path, document)| Candidate {
                    key: FoundKey::unsorted(document, path),
                })
                .collect(),
            snapshot: self.reading_facts(None, lookups)?,
        })
    }

    /// How many document rows this snapshot holds at a generation past
    /// `documents_after`, and how many deaths at one past `tombstones_after`,
    /// each counted to at most `bound`.
    ///
    /// The two feeds a lane-2 engine drains, counted past the generations its
    /// watermarks record: what it has not yet drained, in rows, however many
    /// rows one write generation stamped. One statement
    /// ([`SearchStatement::FeedRowsAfter`]) counts both, each a range of its
    /// feed's index stopped at the bound, so it costs at most twice the bound.
    pub fn feed_rows_after(
        &self,
        documents_after: i64,
        tombstones_after: i64,
        bound: u64,
    ) -> Result<FeedRows, StoreError> {
        self.run_feed_rows(
            documents_after,
            tombstones_after,
            bound,
            &mut Lookups::default(),
        )
    }

    /// Every statement [`Snapshot::feed_rows_after`] runs, each with the plan
    /// SQLite reported for the text and values it ran with.
    pub fn feed_rows_after_plans(
        &self,
        documents_after: i64,
        tombstones_after: i64,
        bound: u64,
    ) -> Result<Vec<SearchPlan>, StoreError> {
        let mut lookups = Lookups::default();
        self.run_feed_rows(documents_after, tombstones_after, bound, &mut lookups)?;
        self.explained(lookups.ran, |statement, filters, plan| SearchPlan {
            statement,
            filters,
            plan,
        })
    }

    fn run_feed_rows(
        &self,
        documents_after: i64,
        tombstones_after: i64,
        bound: u64,
        lookups: &mut Lookups,
    ) -> Result<FeedRows, StoreError> {
        const OPERATION: &str = "counting the change-feed rows past a generation";
        let counted = self
            .run_statement(
                &mut lookups.ran,
                Ran::new(
                    SearchStatement::FeedRowsAfter,
                    compose_feed_rows_after(documents_after, tombstones_after, bound),
                ),
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .map_err(|problem| error::sql(OPERATION, problem))?;
        let (documents, tombstones) = counted.into_iter().next().unwrap_or((0, 0));
        let count = |counted: i64| {
            u64::try_from(counted).map_err(|_| StoreError::Damaged {
                what: format!("{OPERATION} answered {counted}"),
            })
        };
        Ok(FeedRows {
            documents: count(documents)?,
            tombstones: count(tombstones)?,
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

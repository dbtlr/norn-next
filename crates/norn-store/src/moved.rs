//! The bounded store reads that follow a broken link through an out-of-band
//! move.
//!
//! A document moved outside norn leaves a death at its old path and a live row
//! at its new one holding the same bytes. Following a broken link through that
//! move is two reads: the death the link's old target names — by its wikilink
//! suffix, or by its exact path for a Markdown or `vault://` link — and the
//! live documents holding the death's last content hash. **Each stops at two
//! rows**, because a follow decides only between one and more than one: one
//! death and one holder is a move, and two of either is ambiguous.
//!
//! - The suffix read probes the one key the root decides, raw or folded, as a
//!   document suffix probe does. The ambiguity-ignore set is not applied: it
//!   narrows which live documents a link resolves to, not which deaths it can
//!   follow.
//! - The exact-path read uses the root's case behaviour: bytewise on a root
//!   that tells spellings apart, or an equality seek under `NOCASE` of the
//!   partial `tombstones_path_nocase` on a root that folds ASCII case. Several
//!   deaths at one folded place remain several deaths, bounded to two.
//! - **A quarantine death is never read.** Its file is still on disk, so it
//!   never moved, and following its last good hash would point a link at a
//!   copy. The suffix indexes and `tombstones_path_nocase` hold no quarantine
//!   row, so the limit counts only deaths whose file is gone.
//! - These reads walk an index that runs on to `path`, so which two
//!   deaths or holders a caller names does not depend on row ids.
//!
//! Each shape is named by [`MovedStatement`] and runs through
//! [`Snapshot::run_statement`], so it is counted on the snapshot and explained
//! by its `*_plans` method as every read builder's statements are.
//!
//! **A dormant carrier.** The consuming layer is Layer 5C move-following in the
//! repair planner (NORN-399), which is not built yet, so no production caller
//! reaches these reads; their bars and the counter lane's size-independence
//! pair hold them at their seam until it lands.

use norn_db::EmittedPlan;
use norn_db::rusqlite::types::Value;
use norn_db::rusqlite::{self, Row};

use crate::error::{self, StoreError};
use crate::facts::Provenance;
use crate::path::{DocumentPath, SuffixKey, suffix_probe};
use crate::read::{Ran, ReadStatement};
use crate::request::{range_predicate_from, unreadable};
use crate::store::Snapshot;

/// Every statement shape the move-following reads run, named.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MovedStatement {
    /// Recorded deaths in the suffix ranges the root's key opens.
    TombstonesBySuffix,
    /// Recorded deaths at an exact place under the root's case behaviour.
    TombstoneAtPath,
    /// Live documents holding a content hash, in path order.
    LiveHolders,
}

/// How many statement shapes [`MovedStatement::all`] holds.
pub const MOVED_STATEMENTS: usize = 3;

impl MovedStatement {
    /// Every statement shape, in slot order.
    pub fn all() -> [Self; MOVED_STATEMENTS] {
        [
            Self::TombstonesBySuffix,
            Self::TombstoneAtPath,
            Self::LiveHolders,
        ]
    }

    /// Where this statement stands in [`Self::all`], exhaustively.
    pub fn slot(self) -> usize {
        match self {
            Self::TombstonesBySuffix => 0,
            Self::TombstoneAtPath => 1,
            Self::LiveHolders => 2,
        }
    }
}

impl From<MovedStatement> for ReadStatement {
    fn from(statement: MovedStatement) -> Self {
        ReadStatement::Moved(statement)
    }
}

/// A death a broken link's old target may name: its path, the hash its
/// document was last derived at, and how the death was learned — never
/// [`Provenance::Quarantine`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MovedTombstone {
    pub path: DocumentPath,
    /// `None` where the death was learned from a path already absent with no
    /// hash recorded before it, which a follow cannot take further.
    pub last_content_hash: Option<String>,
    pub provenance: Provenance,
}

/// A statement the move-following reads ran, with SQLite's plan for it.
#[derive(Clone, Debug)]
pub struct MovedPlan {
    pub statement: ReadStatement,
    pub plan: EmittedPlan,
}

impl Snapshot {
    /// At most two deaths named by a wikilink suffix with its anchor split off.
    /// Quarantine is excluded because the file remains on disk.
    /// The target takes [`suffix_probe`]'s refusals; ambiguity-ignore does not
    /// exclude deaths from this read. Deaths sharing a key come in path order,
    /// so the two named do not depend on row ids.
    pub fn tombstones_by_suffix(&self, target: &str) -> Result<Vec<MovedTombstone>, StoreError> {
        self.read_tombstones_by_suffix(target, &mut Vec::new())
    }

    /// Run [`Snapshot::tombstones_by_suffix`] on this snapshot and explain
    /// the very text and values its statement ran with.
    pub fn tombstones_by_suffix_plans(&self, target: &str) -> Result<Vec<MovedPlan>, StoreError> {
        let mut record = Vec::new();
        self.read_tombstones_by_suffix(target, &mut record)?;
        self.explained(record, |statement, _, plan| MovedPlan { statement, plan })
    }

    /// At most two deaths at an exact place, in bytewise path order.
    /// Quarantine is excluded because the file remains on disk.
    /// A root that tells spellings apart compares paths bytewise; a root
    /// that folds ASCII case reads deaths at the folded place.
    pub fn tombstone_at_path(
        &self,
        path: &DocumentPath,
    ) -> Result<Vec<MovedTombstone>, StoreError> {
        self.read_tombstone_at_path(path, &mut Vec::new())
    }

    /// Run [`Snapshot::tombstone_at_path`] on this snapshot and explain
    /// the very text and values its statement ran with.
    pub fn tombstone_at_path_plans(
        &self,
        path: &DocumentPath,
    ) -> Result<Vec<MovedPlan>, StoreError> {
        let mut record = Vec::new();
        self.read_tombstone_at_path(path, &mut record)?;
        self.explained(record, |statement, _, plan| MovedPlan { statement, plan })
    }

    /// At most two live documents holding `content_hash`, in bytewise path
    /// order, so the named holders do not depend on row ids.
    pub fn live_holders(&self, content_hash: &str) -> Result<Vec<DocumentPath>, StoreError> {
        self.read_live_holders(content_hash, &mut Vec::new())
    }

    /// Run [`Snapshot::live_holders`] on this snapshot and explain
    /// the very text and values its statement ran with.
    pub fn live_holders_plans(&self, content_hash: &str) -> Result<Vec<MovedPlan>, StoreError> {
        let mut record = Vec::new();
        self.read_live_holders(content_hash, &mut record)?;
        self.explained(record, |statement, _, plan| MovedPlan { statement, plan })
    }

    /// Read the hash index once in path order, recording the statement that ran.
    fn read_live_holders(
        &self,
        content_hash: &str,
        record: &mut Vec<Ran>,
    ) -> Result<Vec<DocumentPath>, StoreError> {
        let ran = Ran::new(
            MovedStatement::LiveHolders,
            (
                "SELECT path FROM documents WHERE content_hash = ?1 ORDER BY path LIMIT 2"
                    .to_string(),
                vec![Value::Text(content_hash.to_string())],
            ),
        );
        self.run_statement(record, ran, |row| {
            let written: String = row.get(0)?;
            Ok(DocumentPath::new(&written))
        })
        .map_err(|problem| error::sql("reading live holders of a content hash", problem))?
        .into_iter()
        .collect()
    }

    /// Read the exact place under the root's case behaviour, recording the
    /// statement that ran. The folded seek walks only deaths at the folded
    /// place and names at most two in path order.
    fn read_tombstone_at_path(
        &self,
        path: &DocumentPath,
        record: &mut Vec<Ran>,
    ) -> Result<Vec<MovedTombstone>, StoreError> {
        let query = match SuffixKey::under(self.path_order()) {
            SuffixKey::Raw => (
                "SELECT path, last_content_hash, provenance FROM tombstones \
                 WHERE path = ?1 AND provenance <> 'quarantine'"
                    .to_string(),
                vec![Value::Text(path.as_str().to_string())],
            ),
            SuffixKey::Folded => (
                "SELECT path, last_content_hash, provenance FROM tombstones \
                 WHERE path = ?1 COLLATE NOCASE AND provenance <> 'quarantine' \
                 ORDER BY path LIMIT 2"
                    .to_string(),
                vec![Value::Text(path.as_str().to_string())],
            ),
        };
        let ran = Ran::new(MovedStatement::TombstoneAtPath, query);
        self.run_statement(record, ran, moved_tombstone)
            .map_err(|problem| error::sql("reading a tombstone at a path", problem))?
            .into_iter()
            .collect()
    }

    /// Read the suffix ranges once, recording the statement that ran.
    fn read_tombstones_by_suffix(
        &self,
        target: &str,
        record: &mut Vec<Ran>,
    ) -> Result<Vec<MovedTombstone>, StoreError> {
        let probe = suffix_probe(target)?.in_space(SuffixKey::under(self.path_order()));
        // Each OR arm carries the partial indexes' predicate, so SQLite seeks
        // the partial index for both reductions instead of scanning it, and
        // the two-row limit counts only deaths whose file is gone.
        let predicate = (0..probe.range_count())
            .map(|at| {
                format!(
                    "({} AND provenance <> 'quarantine')",
                    range_predicate_from(probe.key().column(), 1, 1 + at * 2)
                )
            })
            .collect::<Vec<_>>()
            .join(" OR ");
        let ran = Ran::new(
            MovedStatement::TombstonesBySuffix,
            (
                format!(
                    "SELECT path, last_content_hash, provenance FROM tombstones WHERE {predicate} LIMIT 2"
                ),
                probe
                    .ranges()
                    .flat_map(|(lower, upper)| {
                        [
                            Value::Text(lower.to_string()),
                            Value::Text(upper.to_string()),
                        ]
                    })
                    .collect(),
            ),
        );
        self.run_statement(record, ran, moved_tombstone)
            .map_err(|problem| error::sql("reading tombstones by suffix", problem))?
            .into_iter()
            .collect()
    }
}

/// Decode the closed provenance vocabulary as store damage on an unknown value.
fn moved_tombstone(row: &Row<'_>) -> rusqlite::Result<Result<MovedTombstone, StoreError>> {
    let path: String = row.get(0)?;
    let last_content_hash = row.get(1)?;
    let written: String = row.get(2)?;
    let Some(provenance) = Provenance::from_str(&written) else {
        return Ok(Err(unreadable("tombstones.provenance", &written)));
    };
    Ok(DocumentPath::new(&path).map(|path| MovedTombstone {
        path,
        last_content_hash,
        provenance,
    }))
}

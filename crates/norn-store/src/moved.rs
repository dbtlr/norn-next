//! The bounded store reads that follow a broken link through an out-of-band
//! move.
//!
//! A document moved outside norn leaves a death at its old path and a live row
//! at its new one holding the same bytes. Following a broken link through that
//! move is two reads: the death the link's old target names — by its suffix
//! for a suffix-addressed link, or by its exact path for a `./`, `../`, `/` or
//! `vault://` link, in either family — and the
//! live documents holding the death's last content hash. **Each stops at two
//! rows**, because a follow decides only between one and more than one: one
//! death and one holder is a move, and two of either is ambiguous.
//!
//! - Suffix and exact-path reads seek normalized comparison keys on every root.
//!   Ambiguity-ignore narrows live resolution, not recorded deaths.
//! - Quarantine deaths are excluded by the partial suffix and path-key indexes:
//!   their files still stand on disk and cannot be followed as moves.
//! - These reads walk an index that runs on to `path`, so which two
//!   deaths or holders a caller names does not depend on row ids.
//!
//! Each shape is named by [`MovedStatement`] and runs through
//! [`Snapshot::run_statement`], so it is counted on the snapshot and explained
//! by its `*_plans` method as every read builder's statements are.
//!
//! # One routine turns a stored link into its reads
//!
//! Which of those reads a link runs follows from its address: a suffix
//! address reads its suffix, a path form reads each path it names from its
//! holder, pooled distinct by path in path order and stopping at two, and every
//! other address reads nothing. [`Snapshot::followed_tombstones`] is that
//! split, and the death prune ([ADR 0052]) runs the same function on the
//! store's writer, so the deaths a broken link keeps are the deaths repair
//! reads for it by construction rather than by agreement between two copies.
//!
//! [ADR 0052]: https://github.com/dbtlr/norn/blob/main/docs/decisions/0052-a-death-is-kept-only-while-lane-2-or-a-broken-link-needs-it.md

use std::collections::BTreeMap;

use norn_db::EmittedPlan;
use norn_db::rusqlite::types::Value;
use norn_db::rusqlite::{self, Row};
use norn_wire::{LinkAddress, LinkFamily};

use crate::error::{self, StoreError};
use crate::facts::Provenance;
use crate::link::{directory_of, path_names};
use crate::path::{DocumentPath, suffix_probe};
use crate::read::{Ran, ReadStatement};
use crate::request::{range_predicate_from, unreadable};
use crate::store::Snapshot;

/// Every statement shape the move-following reads run, named.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MovedStatement {
    /// Recorded deaths in the normalized suffix ranges.
    TombstonesBySuffix,
    /// Recorded deaths at a normalized complete path key.
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

    /// At most two deaths sharing the complete-path comparison key, in
    /// original bytewise path order on every root. Quarantine is excluded
    /// because the file remains on disk.
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

    /// The deaths the move-following reads return for one stored link: the
    /// link's `family`, `protocol` and `target` with its anchor split off, held
    /// by the document at `holder`.
    ///
    /// A suffix address reads [`Snapshot::tombstones_by_suffix`] over its
    /// target, and none where the target is no suffix
    /// [`suffix_probe`] admits. A path form — a `./`, `../`, `/` or `vault://`
    /// target — reads [`Snapshot::tombstone_at_path`] at each path it names
    /// from `holder`, pooling the deaths distinct by path, in path order, and
    /// stopping at two. Every other address — elsewhere, unnameable, or the
    /// holding document itself — reads nothing. **This is the one place the
    /// split is written**: the death prune runs it too, so the at most two
    /// deaths a broken link keeps are exactly the ones this returns.
    pub fn followed_tombstones(
        &self,
        family: LinkFamily,
        protocol: Option<&str>,
        target: &str,
        holder: &DocumentPath,
    ) -> Result<Vec<MovedTombstone>, StoreError> {
        self.read_followed_tombstones(family, protocol, target, holder, &mut Vec::new())
    }

    /// Run [`Snapshot::followed_tombstones`] on this snapshot and explain
    /// the very text and values each statement it ran with.
    pub fn followed_tombstones_plans(
        &self,
        family: LinkFamily,
        protocol: Option<&str>,
        target: &str,
        holder: &DocumentPath,
    ) -> Result<Vec<MovedPlan>, StoreError> {
        let mut record = Vec::new();
        self.read_followed_tombstones(family, protocol, target, holder, &mut record)?;
        self.explained(record, |statement, _, plan| MovedPlan { statement, plan })
    }

    fn read_followed_tombstones(
        &self,
        family: LinkFamily,
        protocol: Option<&str>,
        target: &str,
        holder: &DocumentPath,
        record: &mut Vec<Ran>,
    ) -> Result<Vec<MovedTombstone>, StoreError> {
        followed_tombstones_with(
            family,
            protocol,
            target,
            holder,
            |statement, (sql, values)| {
                self.run_statement(record, Ran::new(statement, (sql, values)), moved_tombstone)
                    .map_err(|problem| error::sql(statement.operation(), problem))?
                    .into_iter()
                    .collect()
            },
        )
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

    /// Read at most two deaths at the normalized complete path key, recording
    /// the indexed equality seek in path order.
    fn read_tombstone_at_path(
        &self,
        path: &DocumentPath,
        record: &mut Vec<Ran>,
    ) -> Result<Vec<MovedTombstone>, StoreError> {
        let ran = Ran::new(MovedStatement::TombstoneAtPath, at_path_query(path));
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
        let ran = Ran::new(MovedStatement::TombstonesBySuffix, suffix_query(target)?);
        self.run_statement(record, ran, moved_tombstone)
            .map_err(|problem| error::sql("reading tombstones by suffix", problem))?
            .into_iter()
            .collect()
    }
}

impl MovedStatement {
    /// What a refusal of this statement names the store as doing.
    pub(crate) fn operation(self) -> &'static str {
        match self {
            Self::TombstonesBySuffix => "reading tombstones by suffix",
            Self::TombstoneAtPath => "reading a tombstone at a path",
            Self::LiveHolders => "reading live holders of a content hash",
        }
    }
}

/// The text and values of [`Snapshot::tombstone_at_path`]'s statement, which
/// the death prune runs on the writer as well.
pub(crate) fn at_path_query(path: &DocumentPath) -> (String, Vec<Value>) {
    (
        "SELECT path, last_content_hash, provenance FROM tombstones \
         WHERE path_key = ?1 AND provenance <> 'quarantine' \
         ORDER BY path LIMIT 2"
            .to_string(),
        vec![Value::Text(path.path_key().to_string())],
    )
}

/// The text and values of [`Snapshot::tombstones_by_suffix`]'s statement, with
/// [`suffix_probe`]'s refusals.
pub(crate) fn suffix_query(target: &str) -> Result<(String, Vec<Value>), StoreError> {
    let probe = suffix_probe(target)?;
    // Each OR arm carries the partial indexes' predicate, so SQLite seeks
    // the partial index for both optional `.md` readings instead of scanning it, and
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
    Ok((
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
    ))
}

/// The most statements [`followed_tombstones_with`] runs for one link: a path
/// form names at most two paths, one where its leaf does not end in `.md`.
pub(crate) const MOST_FOLLOWED_READS: usize = 2;

/// The most deaths one move-following statement returns.
pub(crate) const MOST_FOLLOWED_DEATHS: usize = 2;

/// The deaths the move-following reads return for one stored link, each read
/// run through `run` — the one split [`Snapshot::followed_tombstones`] states.
pub(crate) fn followed_tombstones_with(
    family: LinkFamily,
    protocol: Option<&str>,
    target: &str,
    holder: &DocumentPath,
    mut run: impl FnMut(MovedStatement, (String, Vec<Value>)) -> Result<Vec<MovedTombstone>, StoreError>,
) -> Result<Vec<MovedTombstone>, StoreError> {
    let named = match LinkAddress::of(family, protocol, target) {
        LinkAddress::Suffix(target) => {
            let Ok(query) = suffix_query(&target) else {
                return Ok(Vec::new());
            };
            return run(MovedStatement::TombstonesBySuffix, query);
        }
        LinkAddress::Relative(path) => path_names(directory_of(holder), &path),
        LinkAddress::Rooted(path) | LinkAddress::RootedName(path) => path_names(Vec::new(), &path),
        LinkAddress::Elsewhere | LinkAddress::Unnameable(_) | LinkAddress::HoldingDocument => {
            return Ok(Vec::new());
        }
    };
    debug_assert!(named.len() <= MOST_FOLLOWED_READS);
    let mut pooled = BTreeMap::new();
    for named in named {
        let path = DocumentPath::new(&named)?;
        for tombstone in run(MovedStatement::TombstoneAtPath, at_path_query(&path))? {
            pooled.insert(tombstone.path.clone(), tombstone);
        }
    }
    Ok(pooled.into_values().take(MOST_FOLLOWED_DEATHS).collect())
}

/// Decode the closed provenance vocabulary as store damage on an unknown value.
pub(crate) fn moved_tombstone(
    row: &Row<'_>,
) -> rusqlite::Result<Result<MovedTombstone, StoreError>> {
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

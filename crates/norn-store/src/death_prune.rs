//! The death prune: the claim-bound unit that removes every death neither lane
//! 2 nor a broken link needs, and raises the death floor ([ADR 0052]).
//!
//! # What a death is kept for
//!
//! A death stands while either of two conditions holds, and a pass removes it
//! once neither does:
//!
//! 1. **Lane 2 has not read it, within the vault's bound.** Its generation is
//!    above the read-through generation the unit is handed, and it is one of
//!    the newest deaths the live-document count admits. Every unread death
//!    counts toward that bound, one a broken link also keeps and a quarantine
//!    death included. Where no read-through generation is handed — no reader
//!    of the death feed stands for the vault — this condition keeps nothing.
//! 2. **Move-following would read it for a broken link.** It is one of the at
//!    most two deaths [`crate::Snapshot::followed_tombstones`] returns for a
//!    link holding a current `link/broken` finding under the pinned
//!    fingerprint. That routine is the one repair reads through, so a broken
//!    link keeps exactly the deaths repair reads for it, and no link keeps a
//!    quarantine death, which those reads never return.
//!
//! # A pass, in phases, each continued by an exclusive keyset seek
//!
//! A [`DeathPrunePass`] is the caller's: it holds the pass's cursors, and the
//! **kept set** lives in a table in the temporary database of the store's
//! writer connection, as the rolling heal's inventory does. A pass fixes its
//! upper bound, the store's write generation, at its first unit, and then:
//!
//! - **marks** the broken findings in the order they were filed,
//!   `(generation, path, position)` off `findings_broken_filed`, recording the
//!   deaths each one's move-following reads return in the kept set;
//! - **counts** the live documents, `L`, in a unit that runs nothing else —
//!   the one read a row bound cannot cut;
//! - **walks** the deaths at or below its upper bound in descending feed order
//!   off `tombstones_change_feed`, counting every death it passes, until it
//!   has passed `L`, reached a death at or below the read-through generation,
//!   or reached the start of the feed. Where it passes `L` first, the next
//!   death, the `(L + 1)`th newest at or below the upper bound, is the
//!   **cutoff**, fixed for the rest of the pass;
//! - **removes**, reading the deaths in feed order up to its upper bound and
//!   deleting each the kept set does not hold and condition 1 does not keep:
//!   one above the unit's read-through generation and after the cutoff. Every
//!   removing unit first marks the broken findings filed since the marking
//!   cursor, so the kept set it deletes against covers every link broken in
//!   its own transaction. A unit whose catch-up fills its bound deletes
//!   nothing;
//! - **clears** its kept set, a bounded delete at a time.
//!
//! No page steps over rows an earlier page consumed, and none sorts; each
//! statement carries a plan bar ([`DeathPruneStatement`]).
//!
//! A pass that finds a pinned fingerprint other than the one it began under
//! clears its kept set and starts over. A kept set lost from under a pass — the
//! writer connection replaced, or the temporary database reset — is detected
//! by the token the pass recorded beside it, and the pass starts over; a pass
//! never deletes against a kept set it did not fill.
//!
//! # A unit is one transaction, bounded by the rows it reads, and takes no generation
//!
//! Findings, deaths, kept-set rows and index steps count toward the unit's row
//! bound together, so a page of kept deaths cannot hold the writer longer than
//! a page of deleted ones. The unit moves the death floor and never the write
//! generation: it changes nothing a lane-1 read answers, since move-following
//! answers the same for every link broken when it commits.
//!
//! # What is the host's
//!
//! When a pass is due, its clock, the leg it runs in, and the read-through
//! generation it hands each unit are the host's: the store accepts that
//! generation as a plain value and registers no consumer.
//!
//! [ADR 0052]: https://github.com/dbtlr/norn/blob/main/docs/decisions/0052-a-death-is-kept-only-while-lane-2-or-a-broken-link-needs-it.md

use std::sync::atomic::{AtomicU64, Ordering};

use norn_db::EmittedPlan;
use norn_db::rusqlite::types::Value;
use norn_db::rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};

use crate::ddl;
use crate::ddl::findings::BROKEN_FILED_KIND;
use crate::error::{self, StoreError};
use crate::facts::LinkFamily;
use crate::moved::{
    MOST_FOLLOWED_DEATHS, MOST_FOLLOWED_READS, followed_tombstones_with, moved_tombstone,
};
use crate::path::DocumentPath;
use crate::request::{FeedCursor, MAX_PAGE, ReadWork, Reading, Request};
use crate::store::StoreReading;

/// The rows one unit reads by default, sized so that a unit costs no more than
/// a rolling-heal slice, about 13 ms at 10,000 documents.
///
/// **Measured on a release build** (2026-11-10, shared host), over 10,000 live
/// documents, 1,000 of them holding a broken link, and 13,500 deaths, 1,500 of
/// them named by those links, with readers that had read nothing. Per logical
/// row, marking cost about 2.4–3.6 µs, the walk 1.1–2.5 µs and the removal
/// 0.8–1.8 µs. At 2,048 rows the slowest unit of a pass took 6.2 ms in one run
/// and 11.6 ms in another (a removing unit); at 4,096 rows marking alone took
/// 9.5 ms. The live-document count took 0.15–0.22 ms over the 10,000
/// documents, so one count fits a unit's allowance with two orders of
/// magnitude to spare.
pub const DEATH_PRUNE_UNIT_ROWS: usize = 2_048;

/// The most rows marking one broken finding reads: the finding, and for each
/// move-following statement its link runs, the statement's seek and the
/// deaths it returns.
const FINDING_MOST_ROWS: usize = 1 + MOST_FOLLOWED_READS * (1 + MOST_FOLLOWED_DEATHS);

/// The least row bound a unit takes: one broken finding's whole marking, so
/// every unit can make progress.
pub const DEATH_PRUNE_LEAST_ROWS: usize = FINDING_MOST_ROWS;

/// The rows one death a removing unit reads costs: the death off the feed
/// index, and its probe of the kept set.
const REMOVAL_ROWS: usize = 2;

/// How far lane 2 has read the death feed, as the host hands it to one unit.
///
/// The host hands `None` in its place where no reader of the death feed stands
/// for the vault — its engine is turned off — and condition 1 then keeps
/// nothing. A reader that stands always hands one of these.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ReadThrough {
    /// The readers stand but have read no death in the current store epoch:
    /// every death is unread.
    Nothing,
    /// Every death at or below this generation has been read or accounted for.
    Through(i64),
}

impl ReadThrough {
    /// The generation at or below which every death has been read: below every
    /// generation where nothing has.
    fn generation(self) -> i64 {
        match self {
            ReadThrough::Nothing => BELOW_EVERY_GENERATION,
            ReadThrough::Through(generation) => generation,
        }
    }
}

/// A generation below every one a write takes.
const BELOW_EVERY_GENERATION: i64 = 0;

/// Where a pass stands, and so what its next unit runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeathPrunePhase {
    /// Not begun: the next unit fixes the upper bound and starts marking.
    Idle,
    /// Clearing the kept set: a leftover one before a pass begins, one a
    /// fingerprint change made stale, or the pass's own at its end.
    Clearing,
    /// Marking the deaths broken links keep.
    Marking,
    /// Counting the live documents.
    Counting,
    /// Walking the deaths newest first for the cutoff.
    Walking,
    /// Removing the deaths nothing keeps.
    Removing,
    /// Ended, its kept set cleared. A unit run on a finished pass begins the
    /// next one.
    Finished,
}

/// One death-prune pass's cursors: what the attachment holds between units.
///
/// A host restart, a detach or a rebuild loses it, and the next pass starts
/// over. Its kept set lives in the temporary database of the writer it began
/// on; a pass handed to another store, or to one whose temporary database was
/// reset, finds its token missing and starts over.
#[derive(Clone, Debug)]
pub struct DeathPrunePass {
    token: Option<u64>,
    fingerprint: String,
    upper_bound: Option<i64>,
    marked: Option<Filed>,
    step: Step,
    cutoff: Option<FeedCursor>,
    lowest_read_through: Option<ReadThrough>,
    kept_for_lane_two_alone: bool,
    kept_rows: u64,
}

/// A broken finding's place in the order findings were filed.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Filed {
    generation: i64,
    path: String,
    position: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Step {
    Idle,
    Clearing {
        then: AfterClearing,
    },
    Marking,
    Counting,
    Walking {
        live: u64,
        after: Option<FeedCursor>,
        passed: u64,
    },
    Removing {
        after: Option<FeedCursor>,
    },
    Finished,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AfterClearing {
    /// Begin the pass once a kept set it did not fill is gone.
    Begin,
    /// End the pass.
    Finish,
}

impl Default for DeathPrunePass {
    fn default() -> Self {
        Self::new()
    }
}

impl DeathPrunePass {
    /// A pass not yet begun.
    pub fn new() -> Self {
        DeathPrunePass {
            token: None,
            fingerprint: String::new(),
            upper_bound: None,
            marked: None,
            step: Step::Idle,
            cutoff: None,
            lowest_read_through: None,
            kept_for_lane_two_alone: false,
            kept_rows: 0,
        }
    }

    /// Where the pass stands.
    pub fn phase(&self) -> DeathPrunePhase {
        match self.step {
            Step::Idle => DeathPrunePhase::Idle,
            Step::Clearing { .. } => DeathPrunePhase::Clearing,
            Step::Marking => DeathPrunePhase::Marking,
            Step::Counting => DeathPrunePhase::Counting,
            Step::Walking { .. } => DeathPrunePhase::Walking,
            Step::Removing { .. } => DeathPrunePhase::Removing,
            Step::Finished => DeathPrunePhase::Finished,
        }
    }

    /// Whether the pass has ended and cleared its kept set.
    pub fn is_finished(&self) -> bool {
        self.step == Step::Finished
    }

    /// The store's write generation at the pass's first unit: deaths recorded
    /// after it are left to a later pass. `None` before the pass begins.
    pub fn upper_bound(&self) -> Option<i64> {
        self.upper_bound
    }

    /// The `(L + 1)`th newest death at or below the upper bound, where the
    /// walk passed `L` unread deaths before it: it and every death before it
    /// in feed order are past condition 1's bound. Fixed for the pass once
    /// the walk ends.
    pub fn cutoff(&self) -> Option<&FeedCursor> {
        self.cutoff.as_ref()
    }

    /// The lowest read-through generation any of this pass's removing units
    /// was handed, `None` where every one was handed none. Every death a unit
    /// kept for lane 2 stands above the generation that unit was handed, so
    /// lane 2 can have read none of them until the read-through generation
    /// rises above this.
    pub fn lowest_read_through(&self) -> Option<ReadThrough> {
        self.lowest_read_through
    }

    /// Whether a removing unit of this pass kept a death for lane 2 alone:
    /// one condition 1 kept and no broken link did.
    pub fn kept_for_lane_two_alone(&self) -> bool {
        self.kept_for_lane_two_alone
    }

    /// The rows this pass's kept set holds.
    pub fn kept_rows(&self) -> u64 {
        self.kept_rows
    }

    /// Back to a pass not yet begun, keeping nothing of this one.
    fn restart(&mut self) {
        *self = DeathPrunePass::new();
    }
}

/// What one unit did, in logical rows, so a clock taken around it is paired
/// with what it read.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DeathPruneWork {
    /// Every logical row the unit read: findings, deaths, kept-set rows and
    /// move-following seeks together, which its row bound bounds — save the
    /// live-document count, the one read a row bound cannot cut, which counts
    /// each document it steps over here.
    pub rows_read: u64,
    /// Broken findings the marking phase read.
    pub findings_marked: u64,
    /// Broken findings a removing unit's catch-up read, filed since the
    /// marking cursor.
    pub catch_up_findings: u64,
    /// Broken findings whose link stood, and whose move-following reads ran.
    pub broken_links_marked: u64,
    /// Move-following statements run for those links.
    pub followed_reads: u64,
    /// Deaths added to the kept set.
    pub deaths_marked: u64,
    /// The live documents counted, where this unit counted them.
    pub live_documents: Option<u64>,
    /// Deaths the bound walk passed.
    pub deaths_walked: u64,
    /// Deaths a removing unit read and judged.
    pub deaths_examined: u64,
    /// Deaths removed.
    pub deaths_removed: u64,
    /// Deaths kept because a broken link's move-following reads return them.
    pub deaths_kept_by_link: u64,
    /// Deaths kept for lane 2 alone: condition 1 kept them and no broken link
    /// did.
    pub deaths_kept_for_lane_two: u64,
    /// Kept-set rows cleared.
    pub kept_rows_cleared: u64,
}

/// One unit's outcome: what it ran, what it changed, and where the pass now
/// stands.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeathPruneUnit {
    /// The phase the unit ran.
    pub ran: DeathPrunePhase,
    /// Whether the unit found the pass's kept set lost or its fingerprint
    /// moved, and started the pass over.
    pub restarted: bool,
    /// Whether the pass ended with this unit, its kept set cleared.
    pub finished: bool,
    /// The death floor this unit raised the store to, where it raised it.
    pub floor_raised: Option<i64>,
    /// The store reading as this unit committed it: the epoch, and the write
    /// generation and the death floor read in its transaction.
    pub reading: StoreReading,
    /// What the unit read and did.
    pub work: DeathPruneWork,
}

/// Every statement shape a death-prune unit runs beyond the move-following
/// reads, which [`crate::MovedStatement`] names and bars.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeathPruneStatement {
    /// A page of broken findings filed after a cursor, with each one's link.
    BrokenFiled,
    /// The live documents, counted whole.
    LiveDocuments,
    /// A page of deaths at or below a position, newest first.
    WalkDeaths,
    /// A page of deaths after a position and at or below the upper bound,
    /// each with whether the kept set holds it.
    RemovalPage,
    /// One death removed by its path.
    RemoveDeath,
    /// The death floor raised to the higher of it and a generation.
    RaiseFloor,
    /// One death added to the kept set.
    KeepDeath,
    /// A bounded page of the kept set cleared.
    ClearKept,
}

/// How many statement shapes [`DeathPruneStatement::all`] holds.
pub const DEATH_PRUNE_STATEMENTS: usize = 8;

impl DeathPruneStatement {
    /// Every statement shape, in slot order.
    pub fn all() -> [Self; DEATH_PRUNE_STATEMENTS] {
        [
            Self::BrokenFiled,
            Self::LiveDocuments,
            Self::WalkDeaths,
            Self::RemovalPage,
            Self::RemoveDeath,
            Self::RaiseFloor,
            Self::KeepDeath,
            Self::ClearKept,
        ]
    }

    /// Where this statement stands in [`Self::all`], exhaustively.
    pub fn slot(self) -> usize {
        match self {
            Self::BrokenFiled => 0,
            Self::LiveDocuments => 1,
            Self::WalkDeaths => 2,
            Self::RemovalPage => 3,
            Self::RemoveDeath => 4,
            Self::RaiseFloor => 5,
            Self::KeepDeath => 6,
            Self::ClearKept => 7,
        }
    }

    /// The statement's text, the one place it is spelled.
    fn sql(self) -> String {
        match self {
            // The kind is the literal the partial index is built from, so the
            // planner can prove the index covers the query.
            Self::BrokenFiled => format!(
                "SELECT f.generation, f.path, f.position, l.family, l.protocol, l.target
                 FROM findings AS f
                 LEFT JOIN documents AS d ON d.path = f.path
                 LEFT JOIN links AS l ON l.document = d.id AND l.ordinal = f.position
                 WHERE f.kind = '{}' AND f.vault_schema_fingerprint = ?1
                   AND (f.generation, f.path, f.position) > (?2, ?3, ?4)
                 ORDER BY f.generation, f.path, f.position
                 LIMIT ?5",
                BROKEN_FILED_KIND.as_str()
            ),
            Self::LiveDocuments => "SELECT count(*) FROM documents".to_string(),
            Self::WalkDeaths => "SELECT generation, path FROM tombstones
                 WHERE (generation, path) < (?1, ?2)
                 ORDER BY generation DESC, path DESC
                 LIMIT ?3"
                .to_string(),
            Self::RemovalPage => "SELECT t.generation, t.path,
                     EXISTS (SELECT 1 FROM temp.death_prune_kept AS k WHERE k.path = t.path)
                 FROM tombstones AS t
                 WHERE (t.generation, t.path) > (?1, ?2) AND t.generation <= ?3
                 ORDER BY t.generation, t.path
                 LIMIT ?4"
                .to_string(),
            Self::RemoveDeath => "DELETE FROM tombstones WHERE path = ?1".to_string(),
            Self::RaiseFloor => "INSERT INTO meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = max(value, excluded.value)"
                .to_string(),
            Self::KeepDeath => {
                "INSERT OR IGNORE INTO temp.death_prune_kept (path) VALUES (?1)".to_string()
            }
            Self::ClearKept => "DELETE FROM temp.death_prune_kept WHERE path IN (
                     SELECT path FROM temp.death_prune_kept ORDER BY path LIMIT ?1
                 )"
            .to_string(),
        }
    }

    /// The values a plan of this statement is taken under: bound as its
    /// execution site binds them, with each cursor inside its own range.
    fn explained_parameters(self) -> Vec<Value> {
        let text = |value: &str| Value::Text(value.to_string());
        let page = Value::Integer(MAX_PAGE as i64);
        match self {
            Self::BrokenFiled => vec![
                text("explained-schema"),
                Value::Integer(1),
                text("explained.md"),
                Value::Integer(0),
                page,
            ],
            Self::LiveDocuments => Vec::new(),
            Self::WalkDeaths => vec![Value::Integer(2), text("explained.md"), page],
            Self::RemovalPage => vec![
                Value::Integer(1),
                text("explained.md"),
                Value::Integer(2),
                page,
            ],
            Self::RemoveDeath | Self::KeepDeath => vec![text("explained.md")],
            Self::RaiseFloor => vec![text(ddl::meta::DEATH_FLOOR), Value::Integer(1)],
            Self::ClearKept => vec![page],
        }
    }
}

/// The pragmas a writer's temporary database is held under, shared with the
/// rolling inventory: SQLite drops every temporary table when `temp_store`
/// changes, so the two set the one value before either creates a table.
pub(crate) const TEMPORARY_STORAGE: &str = "PRAGMA temp_store = FILE;
    PRAGMA temp.cache_size = -256;";

/// The kept set and the token of the pass that fills it.
const KEPT_SET: &str = "CREATE TEMP TABLE IF NOT EXISTS death_prune_kept (
        path TEXT PRIMARY KEY NOT NULL
    ) WITHOUT ROWID;
    CREATE TEMP TABLE IF NOT EXISTS death_prune_pass (token INTEGER NOT NULL);";

/// The source of pass tokens: unique within the process, so a pass never
/// mistakes another's kept set for its own.
static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

/// What a unit may still read.
struct Budget {
    left: usize,
}

impl Budget {
    fn spend(&mut self, rows: usize, work: &mut DeathPruneWork) {
        debug_assert!(rows <= self.left, "a unit read past its row bound");
        self.left -= rows;
        work.rows_read += rows as u64;
    }
}

impl Request<'_> {
    /// Run one death-prune unit of `pass`, reading at most `row_bound` logical
    /// rows, with condition 1 judged against `read_through` — `None` where no
    /// reader of the death feed stands for the vault.
    ///
    /// The unit is one `IMMEDIATE` transaction on the writer. It takes no
    /// write generation; where it removes deaths, it raises the death floor to
    /// the highest generation it removed in the same transaction. `pass`
    /// moves only when the unit commits, so a unit that fails leaves it, and
    /// the kept set, as they were. A unit run on a finished pass begins the
    /// next pass. Refused while a from-zero
    /// build is in progress or link health is deferred, and for a row bound
    /// below [`DEATH_PRUNE_LEAST_ROWS`].
    pub fn death_prune_unit(
        &mut self,
        pass: &mut DeathPrunePass,
        read_through: Option<ReadThrough>,
        row_bound: usize,
    ) -> Result<DeathPruneUnit, StoreError> {
        if self.store.construction_step().is_some() || self.store.link_health_deferred {
            return Err(StoreError::Lifecycle {
                operation: "running a death-prune unit",
                path: self.store.path().to_path_buf(),
                message: "a death prune runs only over a store whose from-zero build has ended \
                          and whose link health is admitted"
                    .into(),
            });
        }
        if row_bound < DEATH_PRUNE_LEAST_ROWS {
            return Err(StoreError::Bound {
                what: "a death-prune unit's row bound, which must reach the least one broken \
                       finding's marking reads",
                limit: DEATH_PRUNE_LEAST_ROWS,
                given: row_bound,
            });
        }
        // The unit works on a copy, adopted only once its transaction commits:
        // a unit that fails rolls its kept-set rows back, and its cursors with
        // them.
        let committed = pass;
        let mut next = committed.clone();
        let pass = &mut next;
        if pass.is_finished() {
            pass.restart();
        }
        let epoch = self.store.epoch().to_string();
        self.store
            .connection()
            .execute_batch(TEMPORARY_STORAGE)
            .map_err(|problem| error::sql("preparing the death prune's storage", problem))?;
        let read_work = &self.read_work;
        let transaction = self
            .store
            .database
            .immediate_transaction("opening a death-prune unit")?;
        transaction
            .execute_batch(KEPT_SET)
            .map_err(|problem| error::sql("preparing the death prune's kept set", problem))?;
        let fingerprint: String =
            norn_db::meta::get_meta(&transaction, ddl::meta::VAULT_SCHEMA_FINGERPRINT)?
                .unwrap_or_default();
        let held: Option<i64> = transaction
            .query_row("SELECT token FROM temp.death_prune_pass", [], |row| {
                row.get(0)
            })
            .optional()
            .map_err(|problem| error::sql("reading the death prune's token", problem))?;
        let before = crate::store::death_floor(&transaction)?;
        let mut unit = Unit {
            connection: &transaction,
            read_work,
            budget: Budget { left: row_bound },
            work: DeathPruneWork::default(),
            highest_removed: None,
        };
        let mut restarted = false;

        // A pass whose kept set is not the one it filled starts over: it never
        // deletes against marks it did not make.
        if let Some(token) = pass.token
            && held != Some(token as i64)
        {
            pass.restart();
            restarted = true;
        }
        // A pass under a fingerprint no longer pinned marked from findings
        // that are gone.
        if pass.token.is_some()
            && pass.fingerprint != fingerprint
            && !matches!(
                pass.step,
                Step::Clearing {
                    then: AfterClearing::Begin
                }
            )
        {
            let (token, kept_rows) = (pass.token, pass.kept_rows);
            pass.restart();
            pass.token = token;
            pass.kept_rows = kept_rows;
            pass.step = Step::Clearing {
                then: AfterClearing::Begin,
            };
            restarted = true;
        }

        let ran = loop {
            match pass.step.clone() {
                Step::Idle => {
                    let leftover: bool = transaction
                        .query_row(
                            "SELECT EXISTS (SELECT 1 FROM temp.death_prune_kept)",
                            [],
                            |row| row.get(0),
                        )
                        .map_err(|problem| {
                            error::sql("reading the death prune's kept set", problem)
                        })?;
                    if leftover {
                        pass.step = Step::Clearing {
                            then: AfterClearing::Begin,
                        };
                        continue;
                    }
                    let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
                    transaction
                        .execute_batch("DELETE FROM temp.death_prune_pass")
                        .and_then(|_| {
                            transaction.execute(
                                "INSERT INTO temp.death_prune_pass (token) VALUES (?1)",
                                params![token as i64],
                            )
                        })
                        .map_err(|problem| {
                            error::sql("recording the death prune's token", problem)
                        })?;
                    pass.token = Some(token);
                    pass.fingerprint = fingerprint.clone();
                    pass.upper_bound = Some(crate::store::last_write_generation(&transaction)?);
                    pass.step = Step::Marking;
                }
                Step::Clearing { then } => {
                    let emptied = unit.clear(pass)?;
                    if emptied {
                        match then {
                            AfterClearing::Begin => {
                                transaction
                                    .execute_batch("DELETE FROM temp.death_prune_pass")
                                    .map_err(|problem| {
                                        error::sql("ending the death prune's token", problem)
                                    })?;
                                pass.restart();
                            }
                            AfterClearing::Finish => {
                                transaction
                                    .execute_batch("DELETE FROM temp.death_prune_pass")
                                    .map_err(|problem| {
                                        error::sql("ending the death prune's token", problem)
                                    })?;
                                pass.token = None;
                                pass.step = Step::Finished;
                            }
                        }
                    }
                    break DeathPrunePhase::Clearing;
                }
                Step::Marking => {
                    if unit.mark(pass, false)? {
                        pass.step = Step::Counting;
                    }
                    break DeathPrunePhase::Marking;
                }
                Step::Counting => {
                    let live = unit.count_live()?;
                    pass.step = Step::Walking {
                        live,
                        after: None,
                        passed: 0,
                    };
                    break DeathPrunePhase::Counting;
                }
                Step::Walking {
                    live,
                    after,
                    passed,
                } => {
                    let upper_bound = pass.upper_bound.expect("a begun pass has a bound");
                    pass.step = match unit.walk(upper_bound, live, after, passed, read_through)? {
                        Walked::Partway { after, passed } => Step::Walking {
                            live,
                            after,
                            passed,
                        },
                        Walked::Ended(cutoff) => {
                            pass.cutoff = cutoff;
                            Step::Removing { after: None }
                        }
                    };
                    break DeathPrunePhase::Walking;
                }
                Step::Removing { after } => {
                    if let Some(read_through) = read_through {
                        pass.lowest_read_through = Some(
                            pass.lowest_read_through
                                .map_or(read_through, |lowest| lowest.min(read_through)),
                        );
                    }
                    if !unit.mark(pass, true)? {
                        break DeathPrunePhase::Removing;
                    }
                    pass.step = match unit.remove(pass, after, read_through)? {
                        Removed::Partway(after) => Step::Removing { after },
                        Removed::Ended => Step::Clearing {
                            then: AfterClearing::Finish,
                        },
                    };
                    break DeathPrunePhase::Removing;
                }
                Step::Finished => unreachable!("a finished pass restarts before its unit"),
            }
        };

        let floor_raised = match unit.highest_removed {
            Some(highest) if before.is_none_or(|floor| highest > floor) => {
                let raise = DeathPruneStatement::RaiseFloor;
                transaction
                    .prepare_cached(&raise.sql())
                    .and_then(|mut statement| {
                        statement.execute(params![ddl::meta::DEATH_FLOOR, highest])
                    })
                    .map_err(|problem| error::sql("raising the death floor", problem))?;
                Some(highest)
            }
            _ => None,
        };
        let (write_generation, death_floor) = crate::store::generation_and_floor(&transaction)?;
        let work = unit.work;
        transaction
            .commit()
            .map_err(|problem| error::sql("committing a death-prune unit", problem))?;
        let finished = pass.is_finished();
        *committed = next;
        Ok(DeathPruneUnit {
            ran,
            restarted,
            finished,
            floor_raised,
            reading: StoreReading::of(epoch, write_generation).with_death_floor(death_floor),
            work,
        })
    }

    /// One death-prune statement as a unit runs it, with the query plan
    /// SQLite reported for it, explained under the values its execution site
    /// binds. The kept set is created first where it does not stand, so the
    /// statements that read it can be planned.
    pub fn death_prune_plan(
        &self,
        statement: DeathPruneStatement,
    ) -> Result<EmittedPlan, StoreError> {
        self.store
            .connection()
            .execute_batch(TEMPORARY_STORAGE)
            .and_then(|()| self.store.connection().execute_batch(KEPT_SET))
            .map_err(|problem| error::sql("preparing the death prune's kept set", problem))?;
        Ok(self.store.database.emitted_plan(
            &statement.sql(),
            params_from_iter(statement.explained_parameters()),
        )?)
    }
}

/// One unit's transaction and what it has read so far.
struct Unit<'a> {
    connection: &'a Connection,
    read_work: &'a ReadWork,
    budget: Budget,
    work: DeathPruneWork,
    highest_removed: Option<i64>,
}

/// Where a bound walk stopped.
enum Walked {
    /// Out of rows for this unit, having passed `passed` deaths up to `after`.
    Partway {
        after: Option<FeedCursor>,
        passed: u64,
    },
    /// The walk ended, with the cutoff where it passed `L` deaths first.
    Ended(Option<FeedCursor>),
}

/// Where a removing unit stopped.
enum Removed {
    /// Out of rows for this unit, past `after`.
    Partway(Option<FeedCursor>),
    /// Every death up to the pass's bound has been judged.
    Ended,
}

/// One broken finding off the filed order, with its link where it stands.
struct BrokenFinding {
    filed: Filed,
    link: Option<(LinkFamily, Option<String>, String)>,
}

impl Unit<'_> {
    /// Mark the broken findings filed after the pass's marking cursor, as far
    /// as the unit's rows reach; true where it reached the last of them.
    fn mark(&mut self, pass: &mut DeathPrunePass, catch_up: bool) -> Result<bool, StoreError> {
        loop {
            let limit = (self.budget.left / FINDING_MOST_ROWS).min(MAX_PAGE);
            if limit == 0 {
                return Ok(false);
            }
            let (generation, path, position) = pass
                .marked
                .as_ref()
                .map_or((BELOW_EVERY_GENERATION, String::new(), 0), |filed| {
                    (filed.generation, filed.path.clone(), filed.position)
                });
            let page = Request::read_all_on(
                self.connection,
                self.read_work,
                &DeathPruneStatement::BrokenFiled.sql(),
                params![pass.fingerprint, generation, path, position, limit as i64],
                broken_finding,
                "reading broken findings in the order they were filed",
            )?;
            let read = page.len();
            for finding in page {
                self.budget.spend(1, &mut self.work);
                if catch_up {
                    self.work.catch_up_findings += 1;
                } else {
                    self.work.findings_marked += 1;
                }
                if let Some((family, protocol, target)) = &finding.link {
                    self.work.broken_links_marked += 1;
                    let holder = DocumentPath::new(&finding.filed.path)?;
                    let (connection, read_work) = (self.connection, self.read_work);
                    // Each statement is one seek and the deaths it returns,
                    // counted before they are pooled.
                    let (mut followed, mut returned) = (0, 0);
                    let deaths = followed_tombstones_with(
                        family.wire(),
                        protocol.as_deref(),
                        target,
                        &holder,
                        |statement, (sql, values)| {
                            followed += 1;
                            let deaths = Request::read_all_on(
                                connection,
                                read_work,
                                &sql,
                                params_from_iter(values),
                                moved_tombstone,
                                statement.operation(),
                            )?;
                            returned += deaths.len();
                            Ok(deaths)
                        },
                    )?;
                    self.budget.spend(followed + returned, &mut self.work);
                    self.work.followed_reads += followed as u64;
                    let mut keep = self
                        .connection
                        .prepare_cached(&DeathPruneStatement::KeepDeath.sql())
                        .map_err(|problem| error::sql("keeping a death", problem))?;
                    for death in deaths {
                        let added = keep
                            .execute(params![death.path.as_str()])
                            .map_err(|problem| error::sql("keeping a death", problem))?;
                        self.work.deaths_marked += added as u64;
                        pass.kept_rows += added as u64;
                    }
                }
                pass.marked = Some(finding.filed);
            }
            if read < limit {
                return Ok(true);
            }
        }
    }

    /// Count the live documents, whole.
    fn count_live(&mut self) -> Result<u64, StoreError> {
        let live: i64 = self
            .connection
            .query_row(&DeathPruneStatement::LiveDocuments.sql(), [], |row| {
                row.get(0)
            })
            .map_err(|problem| error::sql("counting the live documents", problem))?;
        let live = live as u64;
        self.work.live_documents = Some(live);
        self.work.rows_read += live;
        Ok(live)
    }

    /// Walk the deaths at or below `upper_bound` newest first, from `after`,
    /// for the cutoff.
    fn walk(
        &mut self,
        upper_bound: i64,
        live: u64,
        mut after: Option<FeedCursor>,
        mut passed: u64,
        read_through: Option<ReadThrough>,
    ) -> Result<Walked, StoreError> {
        loop {
            let wanted = usize::try_from(live - passed + 1).unwrap_or(usize::MAX);
            let limit = self.budget.left.min(wanted).min(MAX_PAGE);
            if limit == 0 {
                return Ok(Walked::Partway { after, passed });
            }
            // Above every death at or below the bound: the walk starts at the
            // newest of them.
            let (generation, path) = after
                .as_ref()
                .map_or((upper_bound + 1, String::new()), |at| {
                    (at.generation(), at.path().as_str().to_string())
                });
            let page = Request::read_all_on(
                self.connection,
                self.read_work,
                &DeathPruneStatement::WalkDeaths.sql(),
                params![generation, path, limit as i64],
                fed_position,
                "walking the deaths for the cutoff",
            )?;
            let read = page.len();
            for death in page {
                self.budget.spend(1, &mut self.work);
                self.work.deaths_walked += 1;
                if read_through.is_some_and(|read| death.generation() <= read.generation()) {
                    // Fewer than `L` unread deaths: condition 1 keeps them all.
                    return Ok(Walked::Ended(None));
                }
                if passed == live {
                    return Ok(Walked::Ended(Some(death)));
                }
                passed += 1;
                after = Some(death);
            }
            if read < limit {
                // The start of the feed.
                return Ok(Walked::Ended(None));
            }
        }
    }

    /// Judge the deaths after `after` up to the pass's bound, removing every
    /// one nothing keeps.
    fn remove(
        &mut self,
        pass: &mut DeathPrunePass,
        mut after: Option<FeedCursor>,
        read_through: Option<ReadThrough>,
    ) -> Result<Removed, StoreError> {
        let upper_bound = pass.upper_bound.expect("a begun pass has a bound");
        loop {
            let limit = (self.budget.left / REMOVAL_ROWS).min(MAX_PAGE);
            if limit == 0 {
                return Ok(Removed::Partway(after));
            }
            let (generation, path) = after
                .as_ref()
                .map_or((BELOW_EVERY_GENERATION, String::new()), |at| {
                    (at.generation(), at.path().as_str().to_string())
                });
            let page = Request::read_all_on(
                self.connection,
                self.read_work,
                &DeathPruneStatement::RemovalPage.sql(),
                params![generation, path, upper_bound, limit as i64],
                judged_death,
                "reading the deaths a pass judges",
            )?;
            let read = page.len();
            let mut remove = self
                .connection
                .prepare_cached(&DeathPruneStatement::RemoveDeath.sql())
                .map_err(|problem| error::sql("removing a death", problem))?;
            for (death, kept_by_link) in page {
                self.budget.spend(REMOVAL_ROWS, &mut self.work);
                self.work.deaths_examined += 1;
                let unread = read_through.is_some_and(|read| {
                    death.generation() > read.generation()
                        && pass.cutoff.as_ref().is_none_or(|cutoff| {
                            (death.generation(), death.path().as_str())
                                > (cutoff.generation(), cutoff.path().as_str())
                        })
                });
                if kept_by_link {
                    self.work.deaths_kept_by_link += 1;
                } else if unread {
                    self.work.deaths_kept_for_lane_two += 1;
                    pass.kept_for_lane_two_alone = true;
                } else {
                    remove
                        .execute(params![death.path().as_str()])
                        .map_err(|problem| error::sql("removing a death", problem))?;
                    self.work.deaths_removed += 1;
                    self.highest_removed =
                        Some(self.highest_removed.map_or(death.generation(), |highest| {
                            highest.max(death.generation())
                        }));
                }
                after = Some(death);
            }
            if read < limit {
                return Ok(Removed::Ended);
            }
        }
    }

    /// Clear a bounded page of the kept set; true where it is now empty.
    fn clear(&mut self, pass: &mut DeathPrunePass) -> Result<bool, StoreError> {
        let limit = self.budget.left.min(MAX_PAGE);
        let cleared = self
            .connection
            .prepare_cached(&DeathPruneStatement::ClearKept.sql())
            .and_then(|mut statement| statement.execute(params![limit as i64]))
            .map_err(|problem| error::sql("clearing the death prune's kept set", problem))?;
        self.budget.spend(cleared, &mut self.work);
        self.work.kept_rows_cleared += cleared as u64;
        pass.kept_rows = pass.kept_rows.saturating_sub(cleared as u64);
        Ok(cleared < limit)
    }
}

/// A broken finding's place in the filed order, and its link's address where
/// a link stands at its position.
fn broken_finding(row: &Row<'_>) -> Reading<BrokenFinding> {
    let filed = Filed {
        generation: row.get(0)?,
        path: row.get(1)?,
        position: row.get(2)?,
    };
    let family: Option<String> = row.get(3)?;
    let link = match family {
        None => None,
        Some(written) => {
            let Some(family) = LinkFamily::from_str(&written) else {
                return Ok(Err(crate::request::unreadable("links.family", &written)));
            };
            Some((family, row.get(4)?, row.get(5)?))
        }
    };
    Ok(Ok(BrokenFinding { filed, link }))
}

/// A death's position in feed order.
fn fed_position(row: &Row<'_>) -> Reading<FeedCursor> {
    let generation: i64 = row.get(0)?;
    let path: String = row.get(1)?;
    Ok(DocumentPath::new(&path).map(|path| FeedCursor::at(generation, path)))
}

/// A death's position in feed order, and whether the kept set holds it.
fn judged_death(row: &Row<'_>) -> Reading<(FeedCursor, bool)> {
    let kept: bool = row.get(2)?;
    Ok(fed_position(row)?.map(|position| (position, kept)))
}

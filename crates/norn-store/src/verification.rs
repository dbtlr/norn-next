//! The store's verification of its own consistency, in two halves.
//!
//! A store has ten kinds of consistency to lose, and the verification asks
//! after each: the pages themselves, the foreign keys that carry cascade
//! deletion, the full-text index against the column it is an index of, the
//! frontmatter projection against being JSON at all, the closed vocabularies
//! against the values a reader will accept, the rule rows against the
//! findings whose key they copy, the rule sets against the findings citing
//! them (`Checks::rule_sets`: each set's spelling, the fingerprint it is held under,
//! its names against its citers' rule rows, and a set no finding cites), the
//! document and tombstone pillars against each other, each
//! content-quarantined place's hash against the finding it stands beside, and
//! each document's sub-fingerprints against the columns they are hashes of.
//!
//! The third is what an external-content FTS5 table can lose without anything
//! else noticing, which is exactly why the index is maintained by triggers —
//! and it is asked at **rank 1**, which checks the index against
//! `documents.body` rather than only against itself. The fourth is the
//! projection's own claim, checked by the JSON1 reader that will be asked to
//! query it. The fifth closes the gap between "the doctor says healthy" and a
//! read that fails: a value outside a closed vocabulary is damage the reader
//! reports, so the verification has to see it too. The sixth is the copy of a
//! finding's key each rule row carries, which the write takes from the
//! finding's own row and nothing structural keeps equal after it; a rule row
//! that disagrees would page its finding under another kind or place. The
//! seventh holds the names a finding reports, read off its set, equal to the
//! rule rows a validate selects it by. The eighth is the disjointness the
//! `tombstones_clear_on_derive` trigger maintains — nothing structural holds
//! it, so it is checked at rest rather than trusted, the same ruling the
//! vocabularies get. The ninth is the same ruling over the trigger
//! `content_quarantines_end_with_their_finding`: a content-quarantined
//! place's hash stands only beside a content finding at that place, where no
//! document row stands.
//!
//! The tenth is a **recompute at rest**, for the reason the stored suffix key
//! gets one: a sub-fingerprint is a derived column, and every read that would
//! notice one drifting from the column it hashes is a read that has already
//! trusted it. A change-feed consumer triages on these values and fetches
//! nothing where they match, so a body hash that stopped describing its body
//! is a document that stops being re-derived and reports nothing wrong. The
//! check reads one row at a time, so what it costs in memory is one document
//! rather than the table.
//!
//! **The full-text check is a write, and the periodic verification runs it on
//! a copy.** It is an `INSERT` into the index's command column, so it takes
//! the write lock on any connection that runs it. The whole verification a
//! from-zero build closes with ([`crate::Store::verify_integrity`]) runs every
//! check on the store's writer, which that build holds anyway. The periodic
//! verification runs through a [`VerificationSource`] and never holds the
//! writer: the read-only checks over one write-ahead-log snapshot of a
//! read-only connection of its own, then a `VACUUM INTO` through another
//! read-only connection that writes a copy of the store beside it, and the
//! full-text check on that copy. The copy is the store as one snapshot reads
//! it, so a verdict about the copy's index is a verdict about the store
//! lifetime the copy was taken from.
//!
//! **Every check classifies a failure the one way.** Only a driver error that
//! describes the file's own contents, or a check that finds the contents
//! disagreeing with themselves, is reported as damage; every other failure is
//! the refused operation it was. A verdict of damage is what authorizes
//! discarding the database, so the classification decides which failures
//! schedule a rebuild.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use norn_db::rusqlite::{self, Connection, InterruptHandle};
use norn_wire::{FindingKind, LinkAddressKind, Severity};

use crate::error::{self, StoreError};
use crate::facts::{LinkFamily, Provenance, TagSource};
use crate::hash;
use crate::store::Store;

/// How many virtual-machine steps a verification's statement takes between
/// two readings of its stop mark.
///
/// A stop that lands between the mark's last reading before a statement and
/// the interrupt that follows it would escape both, so the connection reads
/// the mark itself every this many steps. A step is a few nanoseconds of
/// work, so a statement stops within microseconds of the mark being set, and
/// the reading costs nothing a scan would notice.
const STOP_MARK_INTERVAL: i32 = 1_000;

/// What a [`VerificationSource`]'s verification came to.
#[derive(Debug)]
pub enum Verification {
    /// Every check passed: the read-only checks over one snapshot of the
    /// store, and the full-text check over a copy of it.
    Sound,
    /// The verification was stopped before it reached a verdict. It says
    /// nothing about the store, damage least of all.
    Interrupted,
    /// A check failed: [`StoreError::Damaged`] where the store's own contents
    /// are wrong, and the refused operation otherwise — the file gone, the
    /// environment refusing an open, another store lifetime at the path, the
    /// copy's directory refusing the copy or a disk too full to hold it.
    Failed(StoreError),
}

/// Where a verification is when it meets a point at which a stop is met, as
/// [`VerificationSource::verify`] hands it to its caller's hook.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VerificationPoint {
    /// Before a statement of the read-only checks. The snapshot they share is
    /// open from the first statement on.
    Check,
    /// Before a row the sub-fingerprint recompute reads.
    Row,
    /// Before the copy is written: the read-only checks have passed and
    /// their snapshot is closed.
    Copy,
    /// After the copy is written, before its full-text check.
    CopyCheck,
}

/// Why the checks stopped short of their end.
enum Halt {
    /// The stop mark was set.
    Stopped,
    /// A check failed.
    Failed(StoreError),
}

impl From<StoreError> for Halt {
    fn from(error: StoreError) -> Self {
        Halt::Failed(error)
    }
}

/// The checks of one verification over one connection, with the point a stop
/// is met at: before each statement, and before each row the recompute reads.
struct Checks<'a> {
    connection: &'a Connection,
    mark: Option<&'a StopMark>,
    between: &'a mut dyn FnMut(VerificationPoint),
}

/// A point between two steps of a verification: the caller's hook runs, then
/// a set stop mark ends the verification here.
fn point(
    mark: Option<&StopMark>,
    between: &mut dyn FnMut(VerificationPoint),
    at: VerificationPoint,
) -> Result<(), Halt> {
    between(at);
    match mark {
        Some(mark) if mark.stopped.load(Ordering::SeqCst) => Err(Halt::Stopped),
        _ => Ok(()),
    }
}

impl Checks<'_> {
    fn point(&mut self) -> Result<(), Halt> {
        point(self.mark, self.between, VerificationPoint::Check)
    }

    /// One `count(*)` the verification asks, behind a point.
    fn count(&mut self, sql: &str, operation: &'static str) -> Result<i64, Halt> {
        self.point()?;
        Ok(self
            .connection
            .query_row(sql, [], |row| row.get(0))
            .map_err(|error| error::sql(operation, error))?)
    }

    /// The pages and the foreign keys: the checks the whole verification asks
    /// ahead of the full-text check.
    fn pages_and_keys(&mut self) -> Result<(), Halt> {
        self.point()?;
        let report: String = self
            .connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .map_err(|error| error::sql("checking database integrity", error))?;
        if report != "ok" {
            return Err(StoreError::Damaged { what: report }.into());
        }

        let orphans = self.count(
            "SELECT count(*) FROM pragma_foreign_key_check",
            "checking foreign keys",
        )?;
        if orphans != 0 {
            return Err(StoreError::Damaged {
                what: format!("{orphans} rows reference a row that is not there"),
            }
            .into());
        }
        Ok(())
    }

    /// The read-only checks the whole verification asks after the full-text
    /// check.
    fn rows(&mut self) -> Result<(), Halt> {
        let unreadable = self.count(
            "SELECT count(*) FROM documents
             WHERE frontmatter IS NOT NULL AND json_valid(frontmatter) = 0",
            "checking the frontmatter projection",
        )?;
        if unreadable != 0 {
            return Err(StoreError::Damaged {
                what: format!(
                    "{unreadable} documents carry a frontmatter projection that is not JSON"
                ),
            }
            .into());
        }

        for (table, column, vocabulary) in [
            (
                "links",
                "family",
                quoted(LinkFamily::ALL.iter().map(|value| value.as_str())),
            ),
            (
                "links",
                "address",
                quoted(LinkAddressKind::ALL.iter().map(|value| value.as_str())),
            ),
            (
                "document_tags",
                "source",
                quoted(TagSource::ALL.iter().map(|value| value.as_str())),
            ),
            (
                "tombstones",
                "provenance",
                quoted(Provenance::ALL.iter().map(|value| value.as_str())),
            ),
            (
                "findings",
                "kind",
                quoted(FindingKind::ALL.iter().map(FindingKind::as_str)),
            ),
            (
                "findings",
                "severity",
                quoted(Severity::ALL.iter().map(Severity::as_str)),
            ),
            (
                "finding_rules",
                "kind",
                quoted(FindingKind::ALL.iter().map(FindingKind::as_str)),
            ),
            (
                "finding_rules",
                "severity",
                quoted(Severity::ALL.iter().map(Severity::as_str)),
            ),
        ] {
            let outside = self.count(
                &format!("SELECT count(*) FROM {table} WHERE {column} NOT IN ({vocabulary})"),
                "checking a closed vocabulary",
            )?;
            if outside != 0 {
                return Err(StoreError::Damaged {
                    what: format!(
                        "{outside} rows hold a `{table}.{column}` outside the values this schema \
                         writes"
                    ),
                }
                .into());
            }
        }

        // A rule row carries its finding's key so a validate by rule seeks it
        // in the findings' order; a copy that disagrees with its finding
        // would page that finding under another kind, severity or place.
        let strayed = self.count(
            "SELECT count(*) FROM finding_rules AS fr JOIN findings AS f ON f.id = fr.finding
             WHERE fr.vault_schema_fingerprint IS NOT f.vault_schema_fingerprint
                OR fr.kind IS NOT f.kind OR fr.severity IS NOT f.severity
                OR fr.path IS NOT f.path OR fr.position IS NOT f.position",
            "checking the rules findings cite",
        )?;
        if strayed != 0 {
            return Err(StoreError::Damaged {
                what: format!("{strayed} rule rows disagree with the finding they cite a rule of"),
            }
            .into());
        }
        self.rule_sets()?;

        let undead = self.count(
            "SELECT count(*) FROM tombstones JOIN documents USING (path)",
            "checking the pillars are disjoint",
        )?;
        if undead != 0 {
            return Err(StoreError::Damaged {
                what: format!("{undead} tombstones stand at a path that holds a document row"),
            }
            .into());
        }

        let unfounded = self.count(
            &format!(
                "SELECT count(*) FROM content_quarantines
                 WHERE EXISTS (SELECT 1 FROM documents WHERE documents.path = content_quarantines.path)
                    OR NOT EXISTS (
                        SELECT 1 FROM findings
                        WHERE findings.path = content_quarantines.path AND findings.kind = '{}'
                    )",
                crate::increment::CONTENT_FINDING_KIND.as_str()
            ),
            "checking each quarantined hash's finding",
        )?;
        if unfounded != 0 {
            return Err(StoreError::Damaged {
                what: format!(
                    "{unfounded} quarantined hashes stand without their content finding or beside \
                     a document row"
                ),
            }
            .into());
        }

        self.recompute_the_sub_fingerprints()
    }

    /// Hold every copy of a finding's rules equal to the one the wire reports.
    ///
    /// A finding's rules are the set its row cites, read back off the set's
    /// spelling; `finding_rules` holds them a second time, one row per rule,
    /// for a validate selecting by rule to seek. Nothing structural keeps the
    /// two equal after the write, so each way they can come apart is damage
    /// here: a spelling that is no set this crate wrote — not JSON, empty, out
    /// of order, named twice or spelled other than canonically — a set held
    /// under another fingerprint than a finding citing it, and a finding whose
    /// rule rows are not exactly the names of its set, which includes rule rows
    /// for a finding citing no set and a set's name with no row. And a set no
    /// finding cites, which `findings_collect_rule_set` deletes with its last
    /// citer, so only a write out of band leaves one standing.
    fn rule_sets(&mut self) -> Result<(), Halt> {
        let unreadable = self.count(
            "SELECT count(*) FROM rule_sets WHERE json_valid(rules) = 0",
            "checking the rule sets' spellings",
        )?;
        if unreadable != 0 {
            return Err(StoreError::Damaged {
                what: format!("{unreadable} rule sets are spelled as something other than JSON"),
            }
            .into());
        }
        self.point()?;
        let walked = self
            .connection
            .prepare(&format!(
                "SELECT rs.id, {} FROM rule_sets AS rs
                 LEFT JOIN json_each(rs.rules) AS j ORDER BY rs.id",
                crate::rule_set::walked_columns("rs", "j")
            ))
            .and_then(|mut read| {
                read.query_map([], |row| crate::rule_set::walked(row, 0, 1))?
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .map_err(|error| error::sql("reading the rule sets", error))?;
        crate::rule_set::owned_sets(walked)?;

        let elsewhere = self.count(
            "SELECT count(*) FROM findings AS f JOIN rule_sets AS rs ON rs.id = f.rule_set
             WHERE rs.vault_schema_fingerprint IS NOT f.vault_schema_fingerprint",
            "checking the fingerprint a rule set is held under",
        )?;
        if elsewhere != 0 {
            return Err(StoreError::Damaged {
                what: format!(
                    "{elsewhere} findings cite a rule set held under another fingerprint"
                ),
            }
            .into());
        }

        let unheld = self.count(
            "SELECT count(*) FROM finding_rules AS fr JOIN findings AS f ON f.id = fr.finding
             WHERE NOT EXISTS (
                 SELECT 1 FROM rule_sets AS rs, json_each(rs.rules) AS j
                 WHERE rs.id = f.rule_set AND j.value = fr.rule
             )",
            "checking the rule rows against their finding's set",
        )?;
        let unrowed = self.count(
            "SELECT count(*) FROM findings AS f JOIN rule_sets AS rs ON rs.id = f.rule_set,
                 json_each(rs.rules) AS j
             WHERE NOT EXISTS (
                 SELECT 1 FROM finding_rules AS fr WHERE fr.finding = f.id AND fr.rule = j.value
             )",
            "checking a finding's set against its rule rows",
        )?;
        if unheld + unrowed != 0 {
            return Err(StoreError::Damaged {
                what: format!(
                    "{unheld} rule rows name a rule their finding's rule set does not hold, and \
                     {unrowed} rules a finding's set holds have no rule row"
                ),
            }
            .into());
        }

        let uncited = self.count(
            "SELECT count(*) FROM rule_sets AS rs
             WHERE NOT EXISTS (SELECT 1 FROM findings AS f WHERE f.rule_set = rs.id)",
            "checking every rule set is cited",
        )?;
        if uncited != 0 {
            return Err(StoreError::Damaged {
                what: format!("{uncited} rule sets stand that no finding cites"),
            }
            .into());
        }
        Ok(())
    }

    /// Recompute every document's sub-fingerprints from the columns they are
    /// hashes of, and report the first row that disagrees.
    ///
    /// One row at a time, and nothing accumulates: a body is read, hashed and
    /// dropped before the next row is asked for, so the check costs the widest
    /// document rather than the table. A stop is met before each row, so a
    /// stopped recompute ends within one document's hash.
    fn recompute_the_sub_fingerprints(&mut self) -> Result<(), Halt> {
        let operation = "recomputing a document's sub-fingerprints";
        self.point()?;
        let mut statement = self
            .connection
            .prepare(
                "SELECT path, body, body_hash, frontmatter, frontmatter_projection_hash
                 FROM documents WHERE body IS NOT NULL",
            )
            .map_err(|error| error::sql(operation, error))?;
        let mut rows = statement
            .query([])
            .map_err(|error| error::sql(operation, error))?;
        let read = |row: &rusqlite::Row<'_>| -> rusqlite::Result<RecomputedRow> {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        };
        loop {
            point(self.mark, self.between, VerificationPoint::Row)?;
            let Some(row) = rows.next().map_err(|error| error::sql(operation, error))? else {
                return Ok(());
            };
            let (path, body, body_hash, frontmatter, projection_hash) =
                read(row).map_err(|error| error::sql(operation, error))?;
            if hash::sub_fingerprint(&body) != body_hash {
                return Err(StoreError::Damaged {
                    what: format!("`{path}` records a body hash its stored body does not produce"),
                }
                .into());
            }
            let agrees = match (&frontmatter, &projection_hash) {
                (None, None) => true,
                (Some(projection), Some(recorded)) => {
                    hash::sub_fingerprint(projection) == *recorded
                }
                // The `CHECK` on the table refuses this pair, so reaching it is
                // damage rather than a shape a write produces.
                (Some(_), None) | (None, Some(_)) => false,
            };
            if !agrees {
                return Err(StoreError::Damaged {
                    what: format!(
                        "`{path}` records a frontmatter projection hash its stored projection does \
                         not produce"
                    ),
                }
                .into());
            }
        }
    }
}

/// One row the sub-fingerprint recompute reads: the path a failure names, then
/// each hashed column beside the hash it has to produce.
type RecomputedRow = (String, String, String, Option<String>, Option<String>);

fn quoted<'a>(values: impl Iterator<Item = &'a str>) -> String {
    values
        .map(|value| format!("'{value}'"))
        .collect::<Vec<String>>()
        .join(", ")
}

/// The whole verification on the store's writer: the pages and the foreign
/// keys, the full-text check, then the rest of the read-only checks.
pub(crate) fn whole(connection: &Connection) -> Result<(), StoreError> {
    let mut nothing = |_| {};
    let mut checks = Checks {
        connection,
        mark: None,
        between: &mut nothing,
    };
    let checked = checks
        .pages_and_keys()
        .and_then(|()| full_text(connection).map_err(Halt::from))
        .and_then(|()| checks.rows());
    match checked {
        Ok(()) => Ok(()),
        Err(Halt::Failed(error)) => Err(error),
        Err(Halt::Stopped) => unreachable!("a verification with no stop mark was stopped"),
    }
}

/// The full-text check: the index against `documents.body`, at rank 1.
///
/// **It is a write**, so it fails where the database cannot be written to at
/// all. Only the driver saying the index disagrees with what it indexes is
/// damage; any other failure is the refused operation.
pub(crate) fn full_text(connection: &Connection) -> Result<(), StoreError> {
    connection
        .execute(
            "INSERT INTO documents_fts(documents_fts, rank) VALUES ('integrity-check', 1)",
            [],
        )
        .map(|_| ())
        .map_err(|error| {
            if norn_db::is_damaged(&error) {
                StoreError::Damaged {
                    what: format!(
                        "the full-text index disagrees with the documents it indexes: {error}"
                    ),
                }
            } else {
                error::sql("checking the full-text index", error)
            }
        })
}

impl Store {
    /// Mint the source this store lifetime's periodic verification runs
    /// through.
    ///
    /// **The mint opens no database and runs no SQL**, as the feed source's
    /// mint does, so it can run wherever the store is held. Its connections
    /// open when [`VerificationSource::verify`] runs, read-only, bound to this
    /// store's epoch, and close before it returns. The source holds nothing of
    /// the store itself, so the writer is never borrowed by it.
    pub fn verification_source(&self) -> VerificationSource {
        let path = self.database.path().to_path_buf();
        VerificationSource {
            copy: verification_copy(&path),
            path,
            epoch: self.epoch().to_string(),
            mark: Arc::new(StopMark::default()),
        }
    }
}

/// Where the verification of the store at `store` writes its copy: beside the
/// store, in the directory that holds it, so the copy is on the store's own
/// machine and filesystem and never in the vault.
pub(crate) fn verification_copy(store: &Path) -> PathBuf {
    store.with_extension("verification-copy")
}

/// One store lifetime's periodic verification: a source that opens its
/// read-only connections when it verifies, outside every read budget, and
/// closes them when the verification returns.
///
/// **An establishment against another store lifetime is refused**, as a feed
/// source's is, so a source that outlived the store it was minted from — the
/// file discarded and built again at the same path — reaches no verdict
/// about the new one.
///
/// **It can be stopped from another thread** through its
/// [`VerificationStop`]: a stopped verification reaches no verdict. The stop
/// is the store lifetime's retirement mark; a source is minted per lifetime,
/// so a stop is never undone.
///
/// The database file it reads is not reachable from it, for the reason
/// [`crate::SnapshotReader`] gives.
pub struct VerificationSource {
    path: PathBuf,
    copy: PathBuf,
    epoch: String,
    mark: Arc<StopMark>,
}

impl std::fmt::Debug for VerificationSource {
    /// The epoch and whether it is stopped; never the path.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VerificationSource")
            .field("epoch", &self.epoch)
            .field("stopped", &self.mark.stopped.load(Ordering::SeqCst))
            .finish_non_exhaustive()
    }
}

/// A verification's stop: the mark it reads, and the interrupt of the
/// connection it runs on while one is open.
#[derive(Default)]
struct StopMark {
    stopped: AtomicBool,
    /// The interrupt of the verification's open connection, set while one is
    /// open and taken back before it closes. A leaf lock: held only to set,
    /// take or fire the handle.
    interrupt: Mutex<Option<InterruptHandle>>,
}

impl StopMark {
    fn interrupt(&self) -> std::sync::MutexGuard<'_, Option<InterruptHandle>> {
        self.interrupt
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }
}

/// Make `connection` stoppable by `mark` until the guard drops: its progress
/// handler reads the mark, and a stop interrupts it. A mark already set is
/// met here, under the interrupt's lock, so a stop is either met here or
/// finds the handle.
fn arm<'a>(mark: &'a Arc<StopMark>, connection: &Connection) -> Result<Armed<'a>, Halt> {
    let reading = Arc::clone(mark);
    connection
        .progress_handler(STOP_MARK_INTERVAL, Some(move || reading.stopped()))
        .map_err(|error| error::sql("arming the verification's stop", error))?;
    let mut interrupt = mark.interrupt();
    if mark.stopped() {
        return Err(Halt::Stopped);
    }
    *interrupt = Some(connection.get_interrupt_handle());
    Ok(Armed(mark))
}

/// Takes a connection's interrupt back out of the stop mark before the
/// connection closes, on every way out.
struct Armed<'a>(&'a StopMark);

impl Drop for Armed<'_> {
    fn drop(&mut self) {
        *self.0.interrupt() = None;
    }
}

/// The verification's copy of the store, removed with its journal files on
/// every way out of the verification that wrote it: a verdict, a refusal, a
/// stop and an unwind alike.
struct ScratchCopy<'a>(&'a Path);

impl<'a> ScratchCopy<'a> {
    /// Clear `path` for a copy: a copy a crash left there is removed first,
    /// so the copy written is the one this verification takes. A removal the
    /// environment refuses is the refused operation.
    fn clear(path: &'a Path) -> Result<Self, Halt> {
        norn_db::remove_database(path).map_err(StoreError::from)?;
        Ok(ScratchCopy(path))
    }
}

impl Drop for ScratchCopy<'_> {
    fn drop(&mut self) {
        let _ = norn_db::remove_database(self.0);
    }
}

/// The handle that stops a [`VerificationSource`]'s verification from
/// another thread.
#[derive(Clone)]
pub struct VerificationStop(Arc<StopMark>);

impl std::fmt::Debug for VerificationStop {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VerificationStop")
            .field("stopped", &self.0.stopped())
            .finish()
    }
}

impl VerificationStop {
    /// Stop the verification, whatever it is doing, and every one after it.
    ///
    /// **The mark is set first, then the statement in flight is
    /// interrupted** — a read-only check, the copy or the copy's full-text
    /// check. A verification reads the mark at each of its points, and each
    /// of its connections reads it every few thousand steps of a statement,
    /// so a statement that starts between the verification's last reading and
    /// the interrupt is stopped too. The verification returns
    /// [`Verification::Interrupted`] within one such interval, or one
    /// document's recompute, of this call.
    pub fn stop(&self) {
        self.0.stopped.store(true, Ordering::SeqCst);
        if let Some(handle) = self.0.interrupt().as_ref() {
            handle.interrupt();
        }
    }

    /// Whether this verification has been stopped.
    pub fn is_stopped(&self) -> bool {
        self.0.stopped()
    }
}

impl VerificationSource {
    /// The store lifetime this source was minted from.
    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    /// The handle that stops this source's verification.
    pub fn stop_handle(&self) -> VerificationStop {
        VerificationStop(Arc::clone(&self.mark))
    }

    /// Remove a copy a verification of this store left behind, as a crash
    /// leaves one: the clearing an attach runs. A verification clears the
    /// copy's place itself before it writes, so this is cleanup and never a
    /// precondition.
    pub fn discard_copy(&self) -> Result<(), StoreError> {
        Ok(norn_db::remove_database(&self.copy)?)
    }

    /// Run every check of the verification without holding the store's
    /// writer, and report the first failure.
    ///
    /// 1. The read-only checks, in the whole verification's order, over one
    ///    snapshot of a read-only connection of the source's own.
    /// 2. A `VACUUM INTO` through a second read-only connection, which writes
    ///    a copy of the store, as one snapshot reads it, beside the store.
    /// 3. The full-text check on the copy, through a connection to the copy
    ///    alone.
    ///
    /// `between` runs at each point a stop is met, and says which: before
    /// each read-only check and each row the recompute reads, before the copy
    /// and before the copy's check.
    ///
    /// The connections open here and close before this returns, and the copy
    /// is removed on every way out, an unwind included. The file gone, the
    /// environment refusing an open, another store lifetime at the path and a
    /// copy its directory or disk refused are refusals, never damage. A
    /// `VACUUM INTO` the store's own contents fail is damage, and so is a
    /// copy's index that disagrees with the copy's documents: the copy is
    /// what the store held. A stop, wherever it lands, is
    /// [`Verification::Interrupted`], whatever the checks had found.
    pub fn verify(&self, mut between: impl FnMut(VerificationPoint)) -> Verification {
        if self.mark.stopped() {
            return Verification::Interrupted;
        }
        let checked = self
            .read_only_checks(&mut between)
            .and_then(|()| self.full_text_on_a_copy(&mut between));
        if self.mark.stopped() {
            return Verification::Interrupted;
        }
        match checked {
            Ok(()) => Verification::Sound,
            Err(Halt::Stopped) => Verification::Interrupted,
            Err(Halt::Failed(error)) => Verification::Failed(error),
        }
    }

    /// Refuse `database` where it is another store lifetime than the one the
    /// source was minted from.
    fn same_lifetime(&self, database: &norn_db::Database) -> Result<(), Halt> {
        if database.epoch() == self.epoch {
            return Ok(());
        }
        Err(StoreError::Sql {
            operation: "opening a verification source",
            condition: None,
            message: "the store at the source's path is another lifetime than the source was \
                      minted from"
                .to_string(),
        }
        .into())
    }

    /// The read-only checks over one snapshot of a connection of their own.
    fn read_only_checks(&self, between: &mut dyn FnMut(VerificationPoint)) -> Result<(), Halt> {
        let mut database =
            norn_db::open_read_only_to_verify(&self.path).map_err(StoreError::from)?;
        self.same_lifetime(&database)?;
        let armed = arm(&self.mark, database.connection())?;
        database.open_snapshot().map_err(StoreError::from)?;
        let mut checks = Checks {
            connection: database.connection(),
            mark: Some(&self.mark),
            between,
        };
        let checked = checks.pages_and_keys().and_then(|()| checks.rows());
        let _ = database.close_snapshot();
        drop(armed);
        checked
    }

    /// Copy the store beside itself through a read-only connection, and run
    /// the full-text check on the copy.
    fn full_text_on_a_copy(&self, between: &mut dyn FnMut(VerificationPoint)) -> Result<(), Halt> {
        point(Some(&self.mark), between, VerificationPoint::Copy)?;
        let copy = ScratchCopy::clear(&self.copy)?;
        {
            let mut source =
                norn_db::open_read_only_to_copy(&self.path, copy.0).map_err(StoreError::from)?;
            self.same_lifetime(&source)?;
            let armed = arm(&self.mark, source.connection())?;
            source.copy_into(copy.0).map_err(StoreError::from)?;
            drop(armed);
        }
        point(Some(&self.mark), between, VerificationPoint::CopyCheck)?;
        let connection = match norn_db::connect(copy.0).map_err(StoreError::from)? {
            norn_db::Attempt::Connected(connection) => connection,
            norn_db::Attempt::Unreadable { detail } => {
                // The copy was just written, so a copy that reads as no
                // database is the disk refusing it rather than the store's
                // contents: those were read whole to write it.
                return Err(StoreError::Sql {
                    operation: "opening the verification's copy",
                    condition: None,
                    message: detail,
                }
                .into());
            }
        };
        let armed = arm(&self.mark, &connection)?;
        let checked = full_text(&connection);
        drop(armed);
        drop(connection);
        drop(copy);
        Ok(checked?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The connection reads the mark itself.** A statement that starts
    /// after the mark is set, past every point the verification reads it at,
    /// is stopped by the connection's own reading of it before it ends.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: a connection with no file behind it.
    fn a_statement_begun_after_the_mark_is_set_is_stopped_by_the_connection() {
        let connection = Connection::open_in_memory().unwrap();
        let mark = Arc::new(StopMark::default());
        let armed = arm(&mark, &connection).unwrap_or_else(|_| panic!("a mark nothing set"));
        let counted = "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n \
                       WHERE i < 1000000) SELECT count(*) FROM n";
        let all: i64 = connection.query_row(counted, [], |row| row.get(0)).unwrap();
        assert_eq!(all, 1_000_000);

        // The mark alone, with no interrupt fired: what stops the statement
        // is the connection's own reading of it.
        mark.stopped.store(true, Ordering::SeqCst);
        let error = connection
            .query_row(counted, [], |row| row.get::<_, i64>(0))
            .expect_err("a statement begun after the stop");
        assert_eq!(
            error.sqlite_error_code(),
            Some(rusqlite::ErrorCode::OperationInterrupted),
            "{error:?}"
        );
        drop(armed);
    }
}

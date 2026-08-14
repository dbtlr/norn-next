//! The induced-failure rows a running host has to answer.
//!
//! Four conditions, each stated as a required outcome and a forbidden one, and
//! each reached through a production attachment rather than through a fake
//! operation. Two of them are **refusals** — a full disk and a revoked
//! permission — where the environment is broken and the stored state is not, so
//! the entry stays untrusted, everything already committed stands, and the
//! database is never discarded. Two are **tears**, where a process dies partway
//! through a heal and what it left at rest has to be something the next heal
//! converges from with no edit to the vault.
//!
//! **Every case names its forbidden outcome as an assertion.** A refusal that
//! reached rung 3 discarded a sound database to fix a broken environment; a
//! heal that pruned an unreadable path read an error as evidence of deletion; a
//! tear that left half a changeset broke the unit of atomicity. None of those
//! is visible in "the request failed", so each is read off the account the
//! host's own jobs write, off the rows at rest, or off both.
//!
//! **Recovery is the other half of each refusal, and of each tear.** A
//! transient environmental failure is resolved by refusing, so the case that
//! stops there has only shown the refusal: what says the work was not lost is
//! clearing the condition and demanding the vault again, and comparing what
//! converges against a derivation built from zero over the same tree.
//!
//! **Converging is not enough on its own.** A store that was thrown away and
//! derived again from the same tree converges too, and every recovery bar here
//! would pass over it — with the work the case was about silently redone rather
//! than healed. So each recovery attach is read as well as compared: the account
//! says no rung-3 rebuild ran, and the rows written and changesets committed are
//! bounded by what the refusal or the tear actually withheld, which a derivation
//! from zero exceeds. [`assert_healed_only`] is that pair.
//!
//! The tears run in a child process. An abort is the only thing that leaves a
//! database the way a killed process leaves one — no unwinding, no rollback
//! through a destructor — so the process that meets the condition cannot be the
//! process that judges it. Each child records which boundary it was torn at
//! before it dies, and the parent asserts on that record: state at rest cannot
//! tell a hook that fired from a hook that was deleted, and a heal with no tear
//! point in it at all leaves a converged store that satisfies every outcome
//! assertion by not having failed.
//!
//! This binary is its own because its arrangements are process-wide, and
//! because half its cases spawn processes.

#![cfg(feature = "induced-failure")]
#![allow(clippy::disallowed_methods)] // Acceptance fixture: arranging and judging a vault tree.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use norn_config::ConfigDirs;
use norn_config::registry::{Entry, VaultRoot};
use norn_host::{
    AttachMode, DemandLease, EvidenceReading, Host, JobEvidence, LifecyclePolicy,
    ProductionEntryOps, ProductionPolicy, RegistryRead,
};
use norn_store::induced_failure::{self, ARM_HITS, INCREMENT_SEAM};
use norn_store::{DocumentPath, Store, StoredDocument, StoredPathOrder};
use norn_testkit::attestation::{Attestation, SEAM};
use norn_testkit::equivalence::{StoreProjection, assert_operationally_valid, tombstones};
use norn_testkit::isolation::{self, Lease};
use norn_testkit::process::{Run, RunStatus, Sandbox};
use norn_testkit::wait::Budget;
use norn_wire::{ErrorEnvelope, ReasonCode, TrustState, VaultName};

/// The variable that puts a run in the child role, naming the tree it serves.
const CHILD_ROOT: &str = "NORN_HOST_LOCKDOWN_ROOT";

/// The variable naming which tear the child arms.
const CHILD_TEAR: &str = "NORN_HOST_LOCKDOWN_TEAR";

/// A runaway bound on a state converging, not a bar on how fast it does.
const WAIT_LIMIT: Duration = Duration::from_secs(120);

/// How long a child may run before the harness ends it and says so.
///
/// Above the child's own [`WAIT_LIMIT`], deliberately: a child that cannot
/// converge reaches its own bound first and reports what it last saw, which is
/// the better message. This is the backstop under a child that reaches no bound
/// at all — it is killed and reported as timed out, naming this bound, rather
/// than left to hang the lane.
const CHILD_DEADLINE: Duration = Duration::from_secs(180);

/// The signal an abort raises, which is how a torn child reports itself.
const SIGABRT: i32 = 6;

/// The vault every case here is served under.
const VAULT_NAME: &str = "notes";

/// The bytes a document that reads has.
fn readable(index: usize) -> String {
    format!("---\ntitle: Note {index}\n---\n\n# Note {index}\n\nA paragraph about [[note-000]].\n")
}

/// What holds the page cap away from the cases it is not for.
///
/// A cap is read by every connection opened after it, so it reaches any store a
/// case running beside it opens — including the derived store of a vault that
/// asked for no such condition. The case that arms one therefore takes this
/// exclusively and every other case takes it shared, which is the narrowest
/// arrangement that keeps a process-wide condition inside the case that wanted
/// it.
static ARMS_THE_PROCESS: std::sync::RwLock<()> = std::sync::RwLock::new(());

/// The guard a case takes while it opens stores nothing should be capping.
fn beside_the_arms() -> std::sync::RwLockReadGuard<'static, ()> {
    ARMS_THE_PROCESS
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The guard the case that caps the pages takes.
fn arming_the_process() -> std::sync::RwLockWriteGuard<'static, ()> {
    ARMS_THE_PROCESS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ---------------------------------------------------------------------------
// Rung 2, refused: a full disk
// ---------------------------------------------------------------------------

/// **A full disk refuses the heal, the entry stays untrusted, and everything
/// already committed stands.**
///
/// The condition is met by the engine: the derived store's connection is held
/// to the page count the database already has, so the heal's own increment
/// reports `SQLITE_FULL` — the code a disk with no room left produces. What the
/// host does with it is production all the way down: the store types it as a
/// refusal rather than as damage, the job reports an environmental failure, and
/// the entry goes untrusted saying so.
///
/// **The forbidden outcomes are the ones a passing request would hide.** Rung 3
/// must not run: discarding a sound database because a disk filled destroys
/// work to fix nothing, and the account the host's jobs write is where a
/// rebuild would show. And the rows the first attach committed must still be
/// there, each whole.
///
/// Clearing the condition and demanding the vault again is what says the work
/// was refused rather than lost: the entry reaches `Ready`, what it holds is
/// equal to a derivation built from zero over the same tree, and the account of
/// that attach says it got there by writing the rows the refusal withheld
/// rather than by deriving the vault over again.
#[test]
fn a_full_disk_refuses_the_heal_and_the_entry_stays_untrusted() {
    let _exclusive = arming_the_process();
    let vault = Vault::new("full-disk");
    for index in 0..4 {
        vault.write(&format!("note-{index:03}.md"), &readable(index));
    }

    let (before, held) = {
        let serving = vault.serving(ProductionPolicy::new(64, 64).unwrap());
        let lease = attach_and_wait(&serving, vault.name());
        drop(lease);
        let mut store = vault.store();
        assert_operationally_valid(&mut store, "the first attach");
        let projection = StoreProjection::read(&mut store).expect("projecting the first attach");
        let held = induced_failure::page_count(&mut store).expect("the page count at rest");
        (projection, held)
    };
    assert_eq!(before.documents().len(), 4);

    // Filesystem reality advances while the derived store is detached, so the
    // next heal has work that cannot fit in a database that may not grow.
    for index in 4..40 {
        vault.write(&format!("note-{index:03}.md"), &readable(index));
    }

    // The cap is process-wide, so the case that arms one puts it back through a
    // destructor: a panic between here and the uncap below would otherwise hold
    // every store this process opens afterwards to a page count it never asked
    // for.
    let _disarmed = Disarmed;
    let untrusted = {
        induced_failure::cap_the_pages(held);
        let serving = vault.serving(ProductionPolicy::new(64, 64).unwrap());
        let opening = serving.evidence();
        let _lease = serving
            .demand(vault.name(), AttachMode::Durable)
            .expect("request the attachment");
        let untrusted = wait_for_untrusted(&serving, vault.name());
        let spent = serving.evidence().since(opening);
        assert_eq!(
            spent.rebuilds_run, 0,
            "a full disk reached rung 3, which discards a sound database to fix a broken \
             environment"
        );
        untrusted
    };
    // The reason is the case's own arm attesting: a refusal that named anything
    // else is a refusal this arrangement did not cause, and the entry would then
    // be untrusted for a reason no assertion here is stated over.
    assert!(
        untrusted.contains("full"),
        "the entry is untrusted for a reason that does not name the disk: {untrusted}"
    );
    assert!(
        untrusted.contains("EnvironmentalRefusal"),
        "a full disk was published as something other than a broken environment, which is what \
         sends an entry to a rung that discards its database: {untrusted}"
    );

    induced_failure::uncap_the_pages();
    let mut refused = vault.store();
    let after = StoreProjection::read(&mut refused).expect("projecting the refused attach");
    assert_operationally_valid(&mut refused, "the store a full disk refused over");
    for document in before.documents() {
        let standing = after
            .document(document.path.as_str())
            .unwrap_or_else(|| panic!("the refusal took the committed row at {}", document.path));
        assert_eq!(
            standing, document,
            "the refusal left a row that is not the one that committed"
        );
    }
    drop(refused);

    // The recovery bar: with room again, an ordinary demand converges on what a
    // derivation from zero over this tree holds — and does it by healing the
    // work the refusal withheld rather than by starting over. The first attach
    // covered 4 documents and 36 more were written while the store was
    // detached, and the refusal committed none of them, so the heal owes 36
    // rows where a derivation from zero writes all 40.
    let serving = vault.serving(ProductionPolicy::new(64, 64).unwrap());
    let healed = heal_and_read(&serving, vault.name());
    drop(serving);
    assert_healed_only("a full disk that was cleared", healed, 36, 1);
    vault.assert_converged_from_zero("a full disk that was cleared");
}

// ---------------------------------------------------------------------------
// Rung 2, refused: a revoked permission
// ---------------------------------------------------------------------------

/// **An unreadable path is an error, never evidence of deletion.**
///
/// A subtree of the vault has its mode taken away between two attaches, so the
/// heal's walk meets `EACCES` where it expected documents. The convergent
/// answer for a path that is *gone* is to prune the row standing at it; the
/// answer for a path that cannot be read is the opposite, because the walk
/// learned nothing about whether a document is there. So the heal refuses and
/// the entry stays untrusted.
///
/// **The forbidden outcome is the prune.** A row under the locked subtree that
/// disappeared, or a tombstone recorded for it, would be the heal reading a
/// denied directory as a deletion — and the vault would then be serving a
/// smaller answer than the tree holds, with nothing saying so. Rung 3 is
/// forbidden for the same reason a full disk forbids it.
#[test]
fn an_unreadable_subtree_refuses_the_heal_rather_than_pruning_it() {
    let _beside = beside_the_arms();
    let vault = Vault::new("permission-loss");
    vault.write("open/kept.md", &readable(0));
    vault.write("locked/inside.md", &readable(1));
    let locked = vault.path().join("locked");

    let before = {
        let serving = vault.serving(ProductionPolicy::new(64, 64).unwrap());
        let lease = attach_and_wait(&serving, vault.name());
        drop(lease);
        let mut store = vault.store();
        StoreProjection::read(&mut store).expect("projecting the first attach")
    };
    assert!(before.document("locked/inside.md").is_some());

    let untrusted = {
        let _revoked = Revoked::over(&locked);
        let serving = vault.serving(ProductionPolicy::new(64, 64).unwrap());
        let opening = serving.evidence();
        let _lease = serving
            .demand(vault.name(), AttachMode::Durable)
            .expect("request the attachment");
        let untrusted = wait_for_untrusted(&serving, vault.name());
        let spent = serving.evidence().since(opening);
        assert_eq!(
            spent.rebuilds_run, 0,
            "a revoked permission reached rung 3, which discards a sound database to fix a \
             broken environment"
        );
        untrusted
    };
    // The reason is the case's own arm attesting: it has to name the subtree
    // whose mode was taken away and the denial that came of it, or the entry is
    // untrusted for something this arrangement did not cause.
    assert!(
        untrusted.contains("locked") && untrusted.contains("Permission denied"),
        "the entry is untrusted for a reason that is not the revoked subtree: {untrusted}"
    );
    assert!(
        untrusted.contains("EnvironmentalRefusal"),
        "a revoked permission was published as something other than a broken environment, which \
         is what sends an entry to a rung that discards its database: {untrusted}"
    );

    let mut refused = vault.store();
    let after = StoreProjection::read(&mut refused).expect("projecting the refused attach");
    assert!(
        after.document("locked/inside.md").is_some(),
        "the heal read a directory it could not open as evidence that the document under it was \
         deleted"
    );
    assert_eq!(
        tombstones(&mut refused).expect("reading the tombstones"),
        Vec::new(),
        "the heal recorded a death for a path nothing said was gone"
    );
    assert_eq!(
        after.documents(),
        before.documents(),
        "the refused heal changed what the store holds"
    );
    drop(refused);

    // The recovery bar: with the subtree readable again, an ordinary demand
    // converges on what a derivation from zero over this tree holds — and does
    // it by healing the work the refusal withheld rather than by starting over.
    // The refused heal changed nothing and no file moved under it, so the work
    // left over is nothing at all: the recovery writes no row and commits no
    // changeset, where a derivation from zero writes both documents in one.
    let serving = vault.serving(ProductionPolicy::new(64, 64).unwrap());
    let healed = heal_and_read(&serving, vault.name());
    drop(serving);
    assert_healed_only("a permission that was restored", healed, 0, 0);
    vault.assert_converged_from_zero("a permission that was restored");
}

// ---------------------------------------------------------------------------
// Rung 2, torn: a chunk boundary
// ---------------------------------------------------------------------------

/// **A heal torn between two chunks leaves every chunk that committed and no
/// part of the one that had not begun.**
///
/// A heal-scale increment is chunked into separately atomic changesets, and
/// this is the boundary between two of them: the chunk before it committed and
/// recorded its findings, and the process ends at the next chunk's first
/// statement. What that leaves is a store whose coverage of the vault is short
/// and whose every row is whole — each at one generation, each hashing to the
/// bytes at its path.
///
/// **The forbidden outcome is a partial chunk.** A row from the changeset that
/// never opened, or a generation belonging to it, would mean the changeset
/// stopped being the unit of atomicity.
///
/// Recovery is the ordinary attach heal and nothing else: no file is touched
/// between the tear and it, so what converges the store is the hash comparison
/// a heal always runs rather than anything a crash arranged.
#[test]
fn a_tear_at_a_chunk_boundary_leaves_every_committed_chunk_whole() {
    let _beside = beside_the_arms();
    let vault = Vault::new("chunk-tear");
    for index in 0..6 {
        vault.write(&format!("note-{index:03}.md"), &readable(index));
    }

    let torn = vault.run_child(Some("chunk"));
    assert_eq!(
        torn.status,
        RunStatus::Signaled(SIGABRT),
        "the chunk boundary did not end the process it was armed in\n{}",
        torn.stderr
    );
    torn.attestation.assert_reached(
        "a tear at a chunk boundary",
        &[
            (SEAM, INCREMENT_SEAM),
            ("boundary", "chunk"),
            ("changesets", "1"),
        ],
    );
    torn.attestation
        .assert_count("a tear at a chunk boundary", 1);

    let mut store = vault.store();
    let rows = every_row(&mut store);
    assert_eq!(
        rows.len(),
        2,
        "the tear left something other than the one chunk that committed"
    );
    let generation = rows[0].generation;
    assert!(
        rows.iter().all(|row| row.generation == generation),
        "the tear left rows from more than one changeset"
    );
    for row in &rows {
        let bytes = std::fs::read(vault.path().join(row.path.as_str())).expect("the document");
        assert_eq!(
            row.content_hash,
            norn_fs::ContentHash::of(&bytes).to_string(),
            "a committed chunk holds a row that is not the document at its path"
        );
    }
    assert_operationally_valid(&mut store, "the store a chunk-boundary tear left");
    drop(store);

    // The coverage is short and the vault is unchanged, so the ordinary attach
    // heal is what makes up the difference: it writes the 4 documents no row
    // stands at, 2 to a changeset. A derivation from zero over these 6
    // documents writes all of them and takes 3 changesets to do it, so the
    // bound is what tells this heal from a store silently rebuilt and walked
    // again.
    let serving = vault.serving(ProductionPolicy::new(8, 2).unwrap());
    let healed = heal_and_read(&serving, vault.name());
    drop(serving);
    assert_healed_only("a chunk boundary that was torn", healed, 4, 2);
    vault.assert_converged_from_zero("a chunk boundary that was torn");
}

// ---------------------------------------------------------------------------
// Rung 2, torn: between a flush's increment and its findings
// ---------------------------------------------------------------------------

/// **A tear between a flush's increment and its findings is healed by the rows
/// themselves.**
///
/// One flush is a changeset plus the findings recorded after it, each in its
/// own transaction. A process that dies between the two leaves the increment
/// landed with nothing beside it saying why — and no pending-work table stands
/// beside the store, because what the tear leaves is required to demand its own
/// re-derivation.
///
/// Two signals do that, and this case is stated over both:
///
/// - **A markdown place holding no row is re-derived unconditionally.** The
///   quarantined document's row died in the increment and a tombstone stands
///   where it was; a tombstone is not a row, so the next walk opens the path
///   again and files the finding the tear lost.
/// - **A degraded row standing without its finding is re-derived on an
///   unchanged content hash.** The degraded document's row asserts an absent
///   frontmatter projection beside a nonzero count of frontmatter diagnostics,
///   and nothing document-scoped stands at it, so the heal reads it again even
///   though its bytes did not move.
///
/// **No file is edited between the tear and the recovery.** That is the whole
/// claim: the pair converges from what is at rest in the rows, and a heal that
/// needed a filesystem event to notice would leave the vault under-reporting
/// until one arrived.
#[test]
fn a_tear_between_a_flush_and_its_findings_is_healed_by_the_rows_themselves() {
    let _beside = beside_the_arms();
    let vault = Vault::new("findings-tear");
    vault.write("steady.md", &readable(0));
    vault.write("quarantined.md", &readable(1));
    vault.write("degraded.md", &readable(2));

    {
        let serving = vault.serving(ProductionPolicy::new(64, 64).unwrap());
        let lease = attach_and_wait(&serving, vault.name());
        drop(lease);
    }
    let mut store = vault.store();
    assert!(
        findings_at(&mut store, "quarantined.md").is_empty()
            && findings_at(&mut store, "degraded.md").is_empty(),
        "the vault opened with findings, so this case cannot tell the tear's absence from them"
    );
    drop(store);

    // The two documents stop reading, each in the way its own signal is about.
    // Two of the vault's three documents change, and the child heals them under
    // `ProductionPolicy::new(8, 2)` — a chunk of 2 — so the changed set is
    // exactly one chunk: the first changeset to commit carries both of them,
    // which is the changeset `after-commit` tears the findings away from.
    // `steady.md` never enters the increment, so nothing about the tear depends
    // on where it would fall in the walk's ordering.
    vault.write_bytes("quarantined.md", UNDECODABLE);
    vault.write("degraded.md", "---\ntitle: : :\n---\n\n# Body\n");

    let torn = vault.run_child(Some("after-commit"));
    assert_eq!(
        torn.status,
        RunStatus::Signaled(SIGABRT),
        "the flush was not torn between its increment and its findings\n{}",
        torn.stderr
    );
    torn.attestation.assert_reached(
        "a tear between a flush's increment and its findings",
        &[
            (SEAM, INCREMENT_SEAM),
            ("boundary", "after-commit"),
            ("changesets", "1"),
        ],
    );
    torn.attestation
        .assert_count("a tear between a flush's increment and its findings", 1);

    // What the tear left: the increment landed, and nothing beside it.
    let mut store = vault.store();
    assert!(
        store
            .begin_request()
            .stored_document(&document_path("quarantined.md"))
            .expect("reading the quarantined path")
            .is_none(),
        "the quarantined path kept a row, so the tear did not reach the increment"
    );
    assert!(
        tombstones(&mut store)
            .expect("reading the tombstones")
            .iter()
            .any(|tombstone| tombstone.path.as_str() == "quarantined.md"),
        "the increment recorded no death for the quarantined path"
    );
    assert!(
        findings_at(&mut store, "quarantined.md").is_empty(),
        "the tear did not land between the increment and the recording"
    );

    let degraded = store
        .begin_request()
        .stored_document(&document_path("degraded.md"))
        .expect("reading the degraded path")
        .expect("the degraded row the increment wrote");
    assert!(
        degraded.frontmatter.is_none() && degraded.frontmatter_diagnostic_count > 0,
        "the degraded row does not assert a block nothing read: {degraded:?}"
    );
    assert!(
        findings_at(&mut store, "degraded.md").is_empty(),
        "the tear did not land between the increment and the recording"
    );
    let hash_before = degraded.content_hash.clone();
    drop(store);

    // The next heal, with no edit to the vault at all. The two documents the
    // tear left bare are the whole of its work, and only the degraded one has a
    // row to write — the quarantined path yields no facts — where a derivation
    // from zero over this tree writes that row and `steady.md` beside it.
    let serving = vault.serving(ProductionPolicy::new(64, 64).unwrap());
    let healed = heal_and_read(&serving, vault.name());
    drop(serving);
    assert_healed_only("a flush torn before its findings", healed, 1, 1);

    let mut store = vault.store();
    let refiled = findings_at(&mut store, "quarantined.md");
    assert_eq!(
        refiled.len(),
        1,
        "the walk did not re-derive a markdown place holding no row, so the finding the tear \
         lost is still lost"
    );
    let restored = findings_at(&mut store, "degraded.md");
    assert_eq!(
        restored.len(),
        1,
        "a degraded row standing without its finding did not route a re-derivation, so the \
         finding the tear lost is still lost"
    );
    assert_eq!(
        restored[0].kind, "document/frontmatter-unreadable",
        "the finding beside the degraded row states a different cause"
    );
    let degraded = store
        .begin_request()
        .stored_document(&document_path("degraded.md"))
        .expect("reading the degraded path")
        .expect("the degraded row");
    assert_eq!(
        degraded.content_hash, hash_before,
        "the recovery needed the document's bytes to move, which is the edit this case forbids"
    );
    assert_operationally_valid(&mut store, "the store a findings tear was healed in");
    drop(store);

    vault.assert_converged_from_zero("a flush torn before its findings");
}

/// Bytes no derivation reads facts out of, which is what quarantines a place.
const UNDECODABLE: &[u8] = &[0xff, 0xfe, 0x00, 0x9f, 0x92, 0x96];

// ---------------------------------------------------------------------------
// The child role
// ---------------------------------------------------------------------------

/// The attach a torn case runs, in the process that does not survive it.
///
/// The tear is armed before the demand, so the heal below is the ordinary
/// production one: nothing here knows a boundary was armed, and what ends the
/// process is the store's own check inside the increment the heal ran.
///
/// A child started with no tear named arms nothing and runs the same attach to
/// the end. That is the **control**, and it is what makes every record
/// assertion beside a tear mean something: same binary, same spawn, same live
/// record file, and the arm is the only difference between them.
#[test]
fn the_child_role_attaches_under_whatever_it_was_armed_at() {
    let Some(root) = std::env::var_os(CHILD_ROOT) else {
        the_control_attaches_with_nothing_armed();
        return;
    };
    let armed = match std::env::var(CHILD_TEAR).as_deref() {
        Ok("chunk") => {
            induced_failure::abort_at_the_chunk_boundary(1);
            true
        }
        Ok("after-commit") => {
            induced_failure::abort_after_committing_changesets(1);
            true
        }
        Err(std::env::VarError::NotPresent) => false,
        other => panic!("the child was given no tear it knows: {other:?}"),
    };
    attach_under_the_arm(Path::new(&root));
    assert!(!armed, "the armed boundary did not end this process");
}

/// The control: a child spawned exactly as a torn one is, with nothing armed.
///
/// **The record file is live in this run**, pointed at by the same variable the
/// store's arms read, which is the whole point of running it as a child. An arm
/// records itself only where a harness named a file to record into, so a control
/// that left the variable unset would satisfy "nothing was recorded" by having
/// no sink to record into — and the absence assertion the tears rest on would be
/// true of a protocol with no arms left in it at all.
fn the_control_attaches_with_nothing_armed() {
    let _beside = beside_the_arms();
    let vault = Vault::new("unarmed-child");
    vault.write("note.md", &readable(0));
    let ran = vault.run_child(None);
    assert_eq!(
        ran.status,
        RunStatus::Exited(0),
        "the child role did not reach the end of an attach with nothing armed\n{}",
        ran.stderr
    );
    let mut store = vault.store();
    assert_eq!(every_row(&mut store).len(), 1);
    ran.attestation
        .assert_never_reached("an attach with nothing armed", &[(SEAM, INCREMENT_SEAM)]);
    ran.attestation
        .assert_count("an attach with nothing armed", 0);
}

/// Attach the vault at `root` through a production host and wait for it.
fn attach_under_the_arm(root: &Path) {
    let vault = Vault::adopt(root);
    let serving = vault.serving(ProductionPolicy::new(8, 2).unwrap());
    let lease = attach_and_wait(&serving, vault.name());
    drop(lease);
}

// ---------------------------------------------------------------------------
// Waiting
// ---------------------------------------------------------------------------

/// Attach `name` through `serving` and hand back what that attach spent.
///
/// The reading is a difference over one account, taken around the demand, so it
/// is the recovery attach's own work and not the work of anything before it.
fn heal_and_read(serving: &Serving, name: &VaultName) -> EvidenceReading {
    let opening = serving.evidence();
    let lease = attach_and_wait(serving, name);
    drop(lease);
    serving.evidence().since(opening)
}

/// **What separates a recovery from a silent rebuild from zero.**
///
/// Every recovery leg here converges on a derivation built from zero over the
/// same tree — and so does a store that was discarded and walked again, which is
/// the outcome each of these cases is stated against. Convergence therefore
/// cannot be the whole assertion, and this is the other half: the recovery did
/// the work left over, and no more.
///
/// The two assertions divide the ground between them, and neither covers the
/// other's half:
///
/// - `rebuilds_run` is the host's rung-3 leg alone. It counts a job that
///   discarded derived state and built it again, and it counts nothing the
///   store's own open does — a database the open discarded for a schema it
///   could not read is a rebuild this counter never sees.
/// - The work bound is what closes that door. It is read off the counters that
///   move with the **changed set** rather than with the vault: an attach heal
///   opens every document it walks whatever it ends up writing, so `document_opens`
///   says the same thing after a recovery and after a rebuild — while the rows a
///   job wrote and the changesets it committed do not. A derivation from zero
///   writes every document the tree holds; a recovery that healed only what was
///   withheld writes what was withheld. `upserts` and `changesets` are that
///   work's shape, so a store that came to its answer by starting over fails
///   here whichever door it went through.
#[track_caller]
fn assert_healed_only(subject: &str, spent: EvidenceReading, upserts: u64, changesets: u64) {
    assert_eq!(
        spent.rebuilds_run, 0,
        "{subject}: the recovery ran the host's rung-3 rebuild, which discards derived state \
         rather than healing it: {spent:?}"
    );
    assert!(
        spent.documents_upserted <= upserts,
        "{subject}: the recovery wrote {} document rows where the work left over is {upserts}, so \
         it derived more of the vault than the tear or the refusal withheld: {spent:?}",
        spent.documents_upserted
    );
    assert!(
        spent.changesets_applied <= changesets,
        "{subject}: the recovery committed {} changesets where the work left over takes \
         {changesets}, so it derived more of the vault than the tear or the refusal withheld: \
         {spent:?}",
        spent.changesets_applied
    );
}

/// Demand `name`, wait for it to reach `Ready`, and hand back the demand.
fn attach_and_wait(
    host: &Host<ProductionEntryOps>,
    name: &VaultName,
) -> DemandLease<ProductionEntryOps> {
    let lease = host
        .demand(name, AttachMode::Durable)
        .expect("request the attachment");
    let deadline = Instant::now() + WAIT_LIMIT;
    loop {
        let observed = host.state(name);
        if observed == Ok(TrustState::Ready) {
            return lease;
        }
        assert!(
            !names_no_vault(&observed),
            "the host serves no vault under `{name}`: {observed:?}"
        );
        assert!(
            Instant::now() < deadline,
            "the attach did not converge inside {WAIT_LIMIT:?}; observed {observed:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Wait until the entry is untrusted, and hand back the reason it names.
///
/// A refusal is not instantaneous: the demand schedules a job, the job meets
/// the condition, and the entry is marked afterwards. And an untrusted entry
/// does not *answer* a status read with its state — it refuses the read, under
/// the code that says the derived state cannot be trusted, carrying the reason
/// as the refusal's detail. So both spellings end this wait, and a state that
/// reached `Ready` ends it too as a failure: a case about a refusal that served
/// the vault has shown the opposite of what it states.
fn wait_for_untrusted(host: &Host<ProductionEntryOps>, name: &VaultName) -> String {
    let deadline = Instant::now() + WAIT_LIMIT;
    loop {
        let observed = host.state(name);
        match &observed {
            Ok(TrustState::Untrusted { reason, .. }) => return format!("{reason:?}"),
            Err(envelope) if envelope.code() == &ReasonCode::HostEntryUntrusted => {
                return format!("{:?}", envelope.detail());
            }
            Ok(TrustState::Ready) => {
                panic!("the entry reached `Ready` under a condition it was required to refuse")
            }
            _ => {}
        }
        assert!(
            !names_no_vault(&observed),
            "the host serves no vault under `{name}`: {observed:?}"
        );
        assert!(
            Instant::now() < deadline,
            "the entry did not refuse inside {WAIT_LIMIT:?}; observed {observed:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn names_no_vault(observed: &Result<TrustState, ErrorEnvelope>) -> bool {
    matches!(observed, Err(envelope) if envelope.code() == &ReasonCode::HostUnknownVault)
}

// ---------------------------------------------------------------------------
// Reading a store
// ---------------------------------------------------------------------------

fn document_path(at: &str) -> DocumentPath {
    DocumentPath::new(at).expect("a document path")
}

fn findings_at(store: &mut Store, at: &str) -> Vec<norn_store::StoredFinding> {
    store
        .begin_request()
        .stored_findings(&document_path(at))
        .expect("reading the findings at a path")
}

/// Every derived row, a bounded page at a time.
fn every_row(store: &mut Store) -> Vec<StoredDocument> {
    const PAGE: usize = 64;
    let mut rows: Vec<StoredDocument> = Vec::new();
    loop {
        let after = rows.last().map(|row| row.path.clone());
        let page = store
            .begin_request()
            .stored_documents_after_ordered(after.as_ref(), PAGE, StoredPathOrder::Sensitive)
            .expect("reading a page of rows");
        let exhausted = page.len() < PAGE;
        rows.extend(page);
        if exhausted {
            return rows;
        }
    }
}

// ---------------------------------------------------------------------------
// The vault
// ---------------------------------------------------------------------------

/// A vault tree, and the machine-local directories a host serves it from.
struct Vault {
    root: PathBuf,
    name: VaultName,
    /// The sandbox the tree lives in, and what a child this vault runs is
    /// isolated by. A child adopts a tree its parent made and holds none, so
    /// nothing it drops takes the tree away.
    sandbox: Option<Sandbox>,
}

impl Vault {
    fn new(label: &str) -> Vault {
        let sandbox = Sandbox::new(
            Path::new(env!("CARGO_TARGET_TMPDIR")),
            &format!("norn-host-lockdown-{label}"),
        )
        .expect("a sandbox");
        let root = sandbox.work_dir();
        std::fs::create_dir_all(root.join("vault/.norn")).expect("a vault root");
        std::fs::write(root.join("vault/.norn/schema.yaml"), "version: 1\n")
            .expect("the vault schema");
        Vault {
            root,
            name: VaultName::new(VAULT_NAME).expect("a vault name"),
            sandbox: Some(sandbox),
        }
    }

    /// The vault a tree already at `root` names. The child's way in.
    fn adopt(root: &Path) -> Vault {
        Vault {
            root: root.to_path_buf(),
            name: VaultName::new(VAULT_NAME).expect("a vault name"),
            sandbox: None,
        }
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn name(&self) -> &VaultName {
        &self.name
    }

    fn path(&self) -> PathBuf {
        self.root.join("vault")
    }

    fn write(&self, at: &str, body: &str) {
        self.write_bytes(at, body.as_bytes());
    }

    fn write_bytes(&self, at: &str, body: &[u8]) {
        let path = self.path().join(at);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("a directory in the vault");
        }
        std::fs::write(path, body).expect("a document in the vault");
    }

    fn dirs(&self) -> ConfigDirs {
        ConfigDirs::new(self.root.join("config"), self.root.join("data"))
            .expect("config directories")
    }

    fn database(&self) -> PathBuf {
        self.dirs().derived_dir(&self.name).join("store.sqlite3")
    }

    fn store(&self) -> Store {
        Store::open(self.database()).expect("opening the derived store")
    }

    /// A host serving this vault, holding the real-watcher lease while it does.
    fn serving(&self, policy: ProductionPolicy) -> Serving {
        let entry = Entry::new(
            self.name.clone(),
            VaultRoot::new(self.path()).expect("a vault root"),
        );
        let registry = RegistryRead::from_entries([entry]);
        let lease = Lease::hold(
            isolation::REAL_WATCHER,
            isolation::acquisition_budget(Budget::new(WAIT_LIMIT, Duration::from_millis(250))),
        );
        let ops = ProductionEntryOps::new(self.dirs(), policy);
        let account = ops.account();
        let host = Host::new(
            registry,
            ops,
            LifecyclePolicy {
                idle_after: Duration::from_secs(3600),
                worker_slots: 1,
                watch_poll_interval: Duration::from_millis(20),
            },
        )
        .expect("a production host");
        Serving {
            host,
            account,
            _watcher_lease: lease,
        }
    }

    /// Run this binary again as a child armed at `tear`, and report what it
    /// left behind. A child given no tear is the control, and arms nothing.
    ///
    /// The run goes through the testkit's harness rather than through a bare
    /// command, so the child gets a cleared environment with the variables this
    /// case names and the isolation root its parent resolved — a child left to
    /// derive that root would take every lease uncontended and report nothing
    /// about having done so — and a deadline, so a child that wedges is killed
    /// and reported instead of hanging the lane.
    fn run_child(&self, tear: Option<&str>) -> Ran {
        let sandbox = self
            .sandbox
            .as_ref()
            .expect("a vault that owns its tree runs the children over it");
        let hits = self.root.join("arm-hits");
        let mut run = Run::new(sandbox, std::env::current_exe().expect("this binary"))
            .args([
                "--exact",
                "the_child_role_attaches_under_whatever_it_was_armed_at",
                "--nocapture",
            ])
            .deadline(CHILD_DEADLINE)
            .env(CHILD_ROOT, &self.root)
            .env(ARM_HITS, &hits);
        if let Some(tear) = tear {
            run = run.env(CHILD_TEAR, tear);
        }
        let outcome = run.wait().expect("running the lockdown child");
        Ran {
            status: outcome.status,
            stderr: outcome.stderr_text(),
            attestation: Attestation::read(&hits),
        }
    }

    /// Compare what this vault's store holds against a derivation built from
    /// zero over the same tree.
    ///
    /// The second derivation is served from a second set of machine-local
    /// directories, so it shares the documents and none of the derived state:
    /// it walks the tree from an empty database, which is what makes it an
    /// answer this one can be judged against rather than a copy of it.
    fn assert_converged_from_zero(&self, subject: &str) {
        let beside = Vault {
            root: self.root.join("from-zero"),
            name: self.name.clone(),
            sandbox: None,
        };
        std::fs::create_dir_all(beside.root()).expect("a second machine's directories");
        let entry = Entry::new(
            self.name.clone(),
            VaultRoot::new(self.path()).expect("a vault root"),
        );
        let registry = RegistryRead::from_entries([entry]);
        let lease = Lease::hold(
            isolation::REAL_WATCHER,
            isolation::acquisition_budget(Budget::new(WAIT_LIMIT, Duration::from_millis(250))),
        );
        let host = Host::new(
            registry,
            ProductionEntryOps::new(beside.dirs(), ProductionPolicy::new(64, 64).unwrap()),
            LifecyclePolicy {
                idle_after: Duration::from_secs(3600),
                worker_slots: 1,
                watch_poll_interval: Duration::from_millis(20),
            },
        )
        .expect("a production host");
        let demand = attach_and_wait(&host, &self.name);
        drop(demand);
        drop(host);
        drop(lease);

        let mut left = self.store();
        let mut right = beside.store();
        assert_operationally_valid(&mut left, subject);
        assert_operationally_valid(&mut right, "the derivation from zero");
        let left = StoreProjection::read(&mut left).expect("projecting the converged store");
        let right = StoreProjection::read(&mut right).expect("projecting the from-zero store");
        assert!(
            !right.population().is_empty(),
            "the derivation from zero holds nothing, so the comparison below compares nothing"
        );
        left.assert_equivalent(&right, subject);
    }
}

/// What a child left behind.
struct Ran {
    /// How the run ended: a signal for a child a tear reached, an exit for the
    /// control, and a timeout for a child the harness had to kill.
    status: RunStatus,
    stderr: String,
    attestation: Attestation,
}

/// Everything back to what a store opened in an ordinary process meets.
struct Disarmed;

impl Drop for Disarmed {
    fn drop(&mut self) {
        induced_failure::disarm();
    }
}

/// A host, and the real-watcher lease covering the watcher it installs.
struct Serving {
    host: Host<ProductionEntryOps>,
    account: std::sync::Arc<JobEvidence>,
    // Dropped after the host by declaration order, which is the order that
    // matters: the watcher goes with the host and the lease covers it.
    _watcher_lease: Lease,
}

impl Serving {
    /// What this host's jobs have spent and done, as it stands.
    fn evidence(&self) -> EvidenceReading {
        self.account.read()
    }
}

impl std::ops::Deref for Serving {
    type Target = Host<ProductionEntryOps>;

    fn deref(&self) -> &Self::Target {
        &self.host
    }
}

/// A directory nothing may read, put back when the case is done with it.
///
/// The restore is a destructor rather than a line at the end of the case: a
/// case that fails while the mode is taken away would otherwise leave a tree
/// nothing can remove.
struct Revoked {
    path: PathBuf,
}

impl Revoked {
    fn over(path: &Path) -> Revoked {
        set_mode(path, 0o000);
        assert!(
            std::fs::read_dir(path).is_err(),
            "this process reads {} whatever its mode says, so the case below would be arranging \
             nothing. A privileged process cannot carry the permission-loss row.",
            path.display()
        );
        Revoked {
            path: path.to_path_buf(),
        }
    }
}

impl Drop for Revoked {
    fn drop(&mut self) {
        set_mode(&self.path, 0o755);
    }
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .expect("setting a directory's mode");
}

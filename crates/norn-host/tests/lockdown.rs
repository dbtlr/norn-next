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
//! **The revoked permission is stated in two shapes, because a mode taken away
//! from a document and a mode taken away from a directory are not the same
//! condition to the machine.** A document is read by the heal alone, so every
//! platform reaches the refusal through the heal's own open. A directory is
//! read by the heal *and* by the watcher backend that has to cover it, so which
//! of the two meets the denial first is the backend's, and the case that
//! revokes a subtree is stated over both doors.
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
//! **Four more conditions are the watcher's, and they run in a child for a
//! different reason.** A registration the platform refuses, a backend stream
//! that ends, a backend that says it lost the path set, and a synchronization
//! boundary that never arrives are conditions nothing can arrange over a
//! temporary directory, so each is armed at `norn-fs`'s watcher seam through the
//! environment a process is started with — read once, and applied to every watch
//! that process establishes. The child attaches a real host over a real backend,
//! so what answers the arm is the production path from the registration call
//! through to the trust state a client reads, and the required outcomes are
//! different ones: an attach that acquires nothing and waits for a new demand,
//! trust withdrawn under the cause the failure carried and resumed only by a
//! recovery demand, that same published cause standing unchanged across the
//! ticks that follow it with nothing scheduled against the coverage it says is
//! gone, an overflow that widens work to a full-tree reconcile while the
//! coverage it was reported on stays installed, and an attach whose boundary is
//! withheld that goes untrusted under an expired synchronization holding no
//! derived state. The child asserts the outcome, the parent asserts the seam's
//! record of the boundary that produced it, and neither half is enough alone: a
//! seam whose check was removed leaves the child's expectations unmet *and* the
//! record missing.
//!
//! **Two more are the walk's own paging window, and they are one boundary read
//! twice.** Listing a directory and stating the names it listed are two
//! observations, and a foreign writer can act between them — a window one call
//! wide that nothing can arrange over a temporary directory, so each is armed at
//! `norn-fs`'s walk seam through the environment the child is started with. An
//! entry that vanishes there is the vault evolving: the walk states the name as
//! one it read nothing at, the heal completes, and every other document derives.
//! A stat the machine refuses is the environment breaking: the heal refuses, the
//! entry stays untrusted naming the path, and nothing is pruned. Neither is
//! stated without the other, because what the pair says is where the boundary
//! between them runs.
//!
//! **Two of them share one arm and state different things.** A stream that ends
//! is the condition behind both the recovery case and the case about the cause
//! that outlives the ticks after it: one is about the way back and the other
//! about the way the entry stays, and neither's assertions imply the other's. An
//! arm is a condition rather than a case, so a condition worth two required
//! outcomes gets two children.
//!
//! **The record file sits outside every watched edge.** Coverage over a vault is
//! the tree and the tree's own parent, and both fault seams append to the record
//! while that coverage is live — so the file lives in a directory of the
//! sandbox's own, which neither edge reaches. A record written under the vault's
//! parent is a filesystem change the backend reports, and it can be the very
//! delivery a stream arm is stated over.
//!
//! This binary is its own because its arrangements are process-wide, and
//! because half its cases spawn processes.

#![cfg(feature = "induced-failure")]
#![allow(clippy::disallowed_methods)] // Acceptance fixture: arranging and judging a vault tree.

// The production attachment's shared composition, for the one thing this suite
// takes from it: reading a withdrawal off whichever of the two spellings an
// untrusted entry answered with. The tree, the host and the lease are this
// suite's own, because its children adopt a tree the parent arranged rather
// than a generated corpus.
mod attach;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use norn_config::ConfigDirs;
use norn_config::registry::{Entry, VaultRoot};
use norn_host::{
    AttachMode, DemandLease, EvidenceReading, Host, JobEvidence, LifecyclePolicy,
    ProductionEntryOps, ProductionPolicy, RegistryRead, WATCH_SYNCHRONIZATION_DEADLINE,
};
use norn_store::induced_failure::{self, ARM_HITS, INCREMENT_SEAM};
use norn_store::{DocumentPath, Store, StoredDocument, StoredPathOrder};
use norn_testkit::attestation::{Attestation, SEAM};
use norn_testkit::equivalence::{StoreProjection, assert_operationally_valid, tombstones};
use norn_testkit::isolation::{self, Lease};
use norn_testkit::process::{Run, RunStatus, Sandbox};
use norn_testkit::wait::{Budget, Observed, wait_until};
use norn_wire::{
    ErrorEnvelope, ReasonCode, TrustState, UntrustedReason, VaultName, WatcherLossCause,
};

use attach::{
    assert_stands_across, state_budget, untrusted_reason,
    wait_for_withdrawn_trust as withdrawn_trust_inside,
};

/// The variable that puts a run in the child role, naming the tree it serves.
const CHILD_ROOT: &str = "NORN_HOST_LOCKDOWN_ROOT";

/// The variable naming which tear the child arms.
const CHILD_TEAR: &str = "NORN_HOST_LOCKDOWN_TEAR";

/// The variable naming which watcher condition the child meets.
const CHILD_WATCH: &str = "NORN_HOST_LOCKDOWN_WATCH";

/// The variable naming which paging-window condition the child meets.
const CHILD_WALK: &str = "NORN_HOST_LOCKDOWN_WALK";

/// The variable a harness arms `norn-fs`'s watcher seam through.
///
/// It is spelled here rather than imported because the seam it reaches is
/// fenced inside `norn-fs`: a harness arms it the way anything outside that
/// crate does, by putting the pair in the environment a process is started
/// with.
const WATCH_ARMED_STAGES: &str = "NORN_FS_WATCH_ARMED_STAGES";

/// The variable naming the file a fired `norn-fs` arm records itself in, which
/// is the filesystem crate's own spelling of [`ARM_HITS`].
///
/// A case points both at one file. The two seams write records that name
/// themselves, so which of them fired is read off a record rather than off
/// which file it landed in.
const FS_ARM_HITS: &str = "NORN_FS_ARM_HITS";

/// The variable a harness arms `norn-fs`'s walk seam through, spelled here for
/// the same reason the watcher's is: the seam it reaches is fenced inside
/// `norn-fs`, and a harness arms it the way anything outside that crate does.
const WALK_ARMED_STAGES: &str = "NORN_FS_WALK_ARMED_STAGES";

/// The seam a fired watcher arm records itself under.
const WATCH_SEAM: &str = "norn-fs/watch";

/// The seam a fired walk arm records itself under.
const WALK_SEAM: &str = "norn-fs/walk";

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
    // be untrusted for a reason no assertion here is stated over. The variant is
    // matched rather than rendered and searched, so a full disk published as
    // damage — which is what sends an entry to a rung that discards its database
    // — fails here rather than passing on a substring.
    let UntrustedReason::EnvironmentalRefusal { detail, .. } = &untrusted else {
        panic!(
            "a full disk was published as something other than a broken environment: {untrusted:?}"
        )
    };
    assert!(
        detail.contains("full"),
        "the entry is untrusted for a reason that does not name the disk: {detail}"
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

/// **An unreadable document is an error, never a document with nothing in it.**
///
/// One document of the vault has its mode taken away between two attaches, so
/// the heal's own open meets `EACCES` at a path the walk enumerated and stated
/// nothing else about. The answer for bytes norn *read* and could derive no
/// facts from is to quarantine the place, record a finding naming it, and keep
/// going; the answer for bytes norn could not read at all is the opposite,
/// because nothing was learned about the document. So the heal refuses and the
/// entry stays untrusted.
///
/// **The forbidden outcome is the quarantine.** An entry that reached `Ready`
/// with a finding standing at the locked path would be serving the vault while
/// silently answering as though one of its documents held nothing — a broken
/// environment turned into data loss, which is the hazard refusal exists to
/// prevent. The prune is forbidden here for the same reason it is under a locked
/// subtree, and rung 3 for the same reason a full disk forbids it.
///
/// **A document is read by the heal and by nothing else.** Change detection over
/// a vault is installed over its directories, so revoking a document's mode
/// leaves every watcher backend covering the tree exactly as before — which is
/// what makes the heal's door the one door this case has on every platform, and
/// why the subtree case below is the one stated over two.
#[test]
fn an_unreadable_document_refuses_the_heal_rather_than_quarantining_it() {
    let _beside = beside_the_arms();
    let vault = Vault::new("unreadable-document");
    vault.write("kept.md", &readable(0));
    vault.write("locked.md", &readable(1));
    let locked = vault.path().join("locked.md");

    let before = {
        let serving = vault.serving(ProductionPolicy::new(64, 64).unwrap());
        let lease = attach_and_wait(&serving, vault.name());
        drop(lease);
        let mut store = vault.store();
        StoreProjection::read(&mut store).expect("projecting the first attach")
    };
    assert!(before.document("locked.md").is_some());

    let untrusted = {
        let _revoked = Revoked::over_file(&locked);
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
    // The reason is the case's own arm attesting: it has to name the document
    // whose mode was taken away and the denial that came of it, or the entry is
    // untrusted for something this arrangement did not cause.
    let UntrustedReason::EnvironmentalRefusal { detail, .. } = &untrusted else {
        panic!(
            "an unreadable document was published as something other than a broken environment: \
             {untrusted:?}"
        )
    };
    assert!(
        detail.contains("locked.md") && detail.contains("Permission denied"),
        "the entry is untrusted for a reason that is not the revoked document: {detail}"
    );

    let mut refused = vault.store();
    let after = StoreProjection::read(&mut refused).expect("projecting the refused attach");
    assert!(
        after.document("locked.md").is_some(),
        "the heal read a document it could not open as evidence that the document was deleted"
    );
    assert!(
        findings_at(&mut refused, "locked.md").is_empty(),
        "the heal quarantined a document it never read, which turns a revoked permission into a \
         vault that answers as though the document held nothing"
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
    assert_operationally_valid(
        &mut refused,
        "the store an unreadable document refused over",
    );
    drop(refused);

    // The recovery bar: with the document readable again, an ordinary demand
    // converges on what a derivation from zero over this tree holds — and does
    // it by healing the work the refusal withheld rather than by starting over.
    // The refused heal committed nothing and no document's bytes moved, so the
    // work left over is nothing at all: the recovery writes no row and commits
    // no changeset, where a derivation from zero writes both documents in one.
    let serving = vault.serving(ProductionPolicy::new(64, 64).unwrap());
    let healed = heal_and_read(&serving, vault.name());
    drop(serving);
    assert_healed_only("a document that reads again", healed, 0, 0);
    vault.assert_converged_from_zero("a document that reads again");
}

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
///
/// **One revocation, two doors, and which one it reaches is the watcher
/// backend's.** A vault directory is read by the heal's walk and by the backend
/// installing change detection over the tree, so a mode taken away from a
/// directory is met by whichever of the two gets there first:
///
/// - A backend that covers a tree without reading each directory under it —
///   FSEvents — installs over the revoked subtree, and the heal's walk is what
///   meets `EACCES`. The entry goes untrusted under
///   [`UntrustedReason::EnvironmentalRefusal`].
/// - A backend that adds a watch per directory — inotify — cannot add the one
///   over the revoked subtree, so coverage fails before the heal walks. The
///   entry goes untrusted under [`UntrustedReason::WatcherLost`] with
///   [`WatcherLossCause::Backend`].
///
/// Both doors are the required outcome of the permission-loss row: the heal does
/// not run over a tree it cannot read, trust is withdrawn naming the subtree,
/// and nothing under it is pruned. So the reason is matched against exactly
/// those two — not admitted by a wildcard — and every other assertion here holds
/// whichever door the platform took. The one shape that reaches the heal's door
/// on every platform is a revoked *document*, which is its own case above,
/// because a document is read by nothing but the heal.
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
        let _revoked = Revoked::over_directory(&locked);
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
    // whose mode was taken away, or the entry is untrusted for something this
    // arrangement did not cause. The two arms are the two doors one revocation
    // reaches, and they are matched rather than admitted by a wildcard: a
    // revoked permission published as damage — which is what sends an entry to a
    // rung that discards its database — falls through to the panic below.
    match &untrusted {
        UntrustedReason::EnvironmentalRefusal { detail, .. } => assert!(
            detail.contains("locked") && detail.contains("Permission denied"),
            "the heal refused for something that is not the revoked subtree: {detail}"
        ),
        UntrustedReason::WatcherLost {
            cause: WatcherLossCause::Backend,
            detail,
            ..
        } => assert!(
            detail.contains("locked"),
            "coverage was lost over something that is not the revoked subtree: {detail}"
        ),
        other => panic!(
            "a revoked permission was published as neither of the two doors it reaches — a heal \
             that refused, or coverage that could not be installed over the subtree: {other:?}"
        ),
    }

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
    // Nothing under the subtree was written and no file moved, so the work left
    // over is nothing at all whichever door withdrew trust: the recovery writes
    // no row and commits no changeset, where a derivation from zero writes both
    // documents in one.
    //
    // One demand answers both doors. The refused host is dropped above, so this
    // is a fresh attach: it installs its own coverage over a subtree that reads
    // again and heals behind it, which is the same work a demand raised on the
    // untrusted entry itself would have owed under either reason.
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
// Trust transition: coverage that cannot be installed
// ---------------------------------------------------------------------------

/// **An attach whose registration refuses acquires nothing, schedules no
/// recovery, and waits for a demand that has not been made yet.**
///
/// The condition is the platform's own answer at the registration call: the
/// child is started armed at the watcher seam's install stage, so the watch
/// establishment inside its attach meets the typed refusal an operating system
/// that will not let this process observe a path produces. Everything after that
/// is production — the refusal travels the same conversion and the same teardown
/// a platform's own refusal travels, and the entry publishes it as coverage that
/// was lost to the backend.
///
/// **The forbidden outcomes are the ones a failed request would hide.** An
/// attach that refused at coverage never reached the store, so a derived
/// database standing afterwards would mean the entry acquired something the
/// refusal says it did not. And nothing may put coverage back on its own: the
/// child holds its demand lease across the whole failure, because dropping it
/// takes the demand out of the entry and with it the thing this rules out.
///
/// **Resumption is a second child, because the arm is process-wide.** Every
/// establishment in the armed process refuses, so clearing the condition means
/// leaving that process: the same vault is demanded again by a child spawned the
/// same way with nothing armed, and it reaches `Ready` and converges on a
/// derivation built from zero over the same tree.
///
/// That second child is also **an unarmed watcher control**. It runs the same
/// binary through the same spawn, pointed at a record file of its own that the
/// spawn creates before it starts; it installs a live subscription, reports a
/// change of its own through it, and writes no record into that file at all —
/// which is what makes the record the armed child left mean something rather
/// than being free. A file per condition is what keeps the two counts apart,
/// and the empty file the spawn leaves is what makes this one a count of
/// firings rather than of a sink that was never there. The expiry case's
/// resumption is the same control over a vault of its own, for the same reason:
/// each armed child's record is read against a control that ran the same way.
#[test]
fn an_attach_that_cannot_install_coverage_acquires_nothing() {
    let _beside = beside_the_arms();
    let vault = Vault::new("watch-install-refused");
    for index in 0..4 {
        vault.write(&format!("note-{index:03}.md"), &readable(index));
    }

    let refused = vault.run_watch_child("refused-attach", Some("install=refuses"));
    assert_eq!(
        refused.status,
        RunStatus::Exited(0),
        "the child did not answer a refused registration the way the entry is required to\n{}",
        refused.stderr
    );
    refused.attestation.assert_reached(
        "an attach whose registration refuses",
        &[
            (SEAM, WATCH_SEAM),
            ("stage", "install"),
            ("answer", "refuses"),
        ],
    );
    // One record and no more: a second is a second establishment, which is the
    // entry putting coverage back under a failure nothing addressed.
    refused
        .attestation
        .assert_count("an attach whose registration refuses", 1);
    assert!(
        !vault.database().exists(),
        "the refused attach left a derived store behind, so it acquired past the coverage it \
         could not install"
    );

    let resumed = vault.run_watch_child("resumed", None);
    assert_eq!(
        resumed.status,
        RunStatus::Exited(0),
        "the demand that followed the refusal was not served\n{}",
        resumed.stderr
    );
    resumed
        .attestation
        .assert_never_reached("a demand issued with nothing armed", &[(SEAM, WATCH_SEAM)]);
    resumed
        .attestation
        .assert_count("a demand issued with nothing armed", 0);

    let mut store = vault.store();
    assert_eq!(
        every_row(&mut store).len(),
        5,
        "the resumed attach did not derive the vault and the change the control made in it"
    );
    assert_operationally_valid(&mut store, "the store the resumed attach derived");
    drop(store);
    vault.assert_converged_from_zero("coverage installed by the demand that followed a refusal");
}

// ---------------------------------------------------------------------------
// Trust transition: a stream that ends under live coverage
// ---------------------------------------------------------------------------

/// **A backend failure after readiness withdraws trust under the cause it
/// carried, and the entry resumes only through a recovery demand.**
///
/// The child attaches, waits for `Ready`, and makes one change of its own. The
/// arm stands in place of the delivery that reports it: what reaches ingest is
/// the terminal error a backend that stopped for good produces, so the poll that
/// drained the subscription reports a watcher failure and the entry publishes
/// [`WatcherLossCause::Backend`] carrying the failure's own account of itself.
///
/// **Waiting for `Ready` is what makes it that delivery.** The arm is owed to
/// the first change a consumer meets over live coverage, and the attach's own
/// heal window has closed by the time the entry serves the vault — so what the
/// arm stands in place of is the edit below and not something the establishment
/// or the heal reported.
///
/// **The forbidden outcome is coverage coming back by itself.** The child holds
/// the demand it attached under across the failure and watches the entry stand
/// still on it — and reads the host's account across that same stretch, because
/// standing still is what a re-acquisition that met the arm again would look
/// like from outside. What installs coverage again is a recovery demand, and the
/// recovery rung the account records is how the child knows the entry came back
/// that way rather than by an attach or a rebuild.
///
/// **One change, and one only.** The arm is per establishment: the recovery
/// installs a second subscription, and that subscription gets a one-shot arm of
/// its own which fires on the first change a consumer meets over it — past its
/// boundary, past the heal the recovery runs, and folded into a batch. A second
/// change after the recovery would spend it, so the child makes none — and the
/// parent asserts exactly one record, because a second record is a delivery this
/// case leaked into a subscription it states nothing about.
#[test]
fn a_backend_failure_after_readiness_resumes_only_through_a_recovery_demand() {
    let _beside = beside_the_arms();
    let vault = Vault::new("watch-stream-failed");
    for index in 0..4 {
        vault.write(&format!("note-{index:03}.md"), &readable(index));
    }

    let failed = vault.run_watch_child("terminal-stream", Some("stream=fails"));
    assert_eq!(
        failed.status,
        RunStatus::Exited(0),
        "the child did not answer a stream that ended the way the entry is required to\n{}",
        failed.stderr
    );
    failed.attestation.assert_reached(
        "a backend stream that ended under live coverage",
        &[(SEAM, WATCH_SEAM), ("stage", "stream"), ("answer", "fails")],
    );
    failed
        .attestation
        .assert_count("a backend stream that ended under live coverage", 1);

    // The change the lost delivery would have reported is in the store: the
    // recovery re-heals the vault, so nothing the failure swallowed is lost.
    let mut store = vault.store();
    assert_eq!(
        every_row(&mut store).len(),
        5,
        "the recovery did not heal the change whose delivery the failure stood in place of"
    );
    assert_operationally_valid(&mut store, "the store a recovered attachment left");
    drop(store);
    vault.assert_converged_from_zero("coverage re-installed by a recovery demand");
}

// ---------------------------------------------------------------------------
// Trust transition: a published cause standing across the ticks after it
// ---------------------------------------------------------------------------

/// **A cause published for coverage that ended is the cause a client reads for
/// as long as nothing addresses it, and no work is scheduled against the
/// coverage it names.**
///
/// The condition is arranged exactly as
/// [`a_backend_failure_after_readiness_resumes_only_through_a_recovery_demand`]
/// arranges it — a child attaches, waits for `Ready`, makes one change, and the
/// arm stands in place of the delivery that would have reported it. **What the
/// two cases state is different, and neither implies the other.** That one is
/// about the way back: a recovery demand is what re-installs coverage, and the
/// account says the entry came back by that rung. This one is about the way the
/// entry stays: the loss is never demanded away, and what is required is that a
/// long stretch of the dispatcher's own ticks changes nothing about it.
///
/// **The stretch is ticks rather than time, and the ticks are counted.** The
/// child holds the entry across hundreds of watcher poll intervals and reads the
/// published reason at each look, so what a failure here reports is an entry
/// that moved rather than a deadline that passed — and the host's own account of
/// the polls it took is read across the same stretch, because every other
/// assertion here is one a dispatcher that stopped scanning would satisfy by
/// doing nothing. Two things are ruled out over that stretch, and a poll cadence
/// is what makes each of them reachable: a second reason minted on top of the
/// first, and work scheduled against coverage that is gone. The loss left
/// a vault-wide rescan standing in the entry's pending facts — an entry that
/// reconciled it would reread the whole vault against a subscription it no
/// longer holds — so the account across the stretch is required to show no
/// document opened, no row written and no changeset committed.
///
/// **The change the failure swallowed stays underived, and that is the state at
/// rest this case is read off.** The recovery case's store holds the swallowed
/// change because a recovery healed the vault; here nothing does, so the store
/// the child leaves holds exactly what the attach heal derived and no row at the
/// edited path. A store holding that row would say something put coverage back
/// while this case says nothing did.
#[test]
fn a_published_watcher_cause_outlives_the_production_ticks_after_it() {
    let _beside = beside_the_arms();
    let vault = Vault::new("watch-cause-outlives");
    for index in 0..4 {
        vault.write(&format!("note-{index:03}.md"), &readable(index));
    }

    let outlived = vault.run_watch_child("outlived-ticks", Some("stream=fails"));
    assert_eq!(
        outlived.status,
        RunStatus::Exited(0),
        "the child did not hold the cause across the ticks that followed it\n{}",
        outlived.stderr
    );
    outlived.attestation.assert_reached(
        "a backend stream that ended and was never demanded back",
        &[(SEAM, WATCH_SEAM), ("stage", "stream"), ("answer", "fails")],
    );
    outlived
        .attestation
        .assert_count("a backend stream that ended and was never demanded back", 1);

    let mut store = vault.store();
    let rows = every_row(&mut store);
    assert_eq!(
        rows.len(),
        4,
        "the store holds rows the attach heal did not derive, so something reached the vault \
         after coverage ended: {rows:?}"
    );
    assert!(
        !rows
            .iter()
            .any(|row| row.path == document_path(OUTLIVED_EDIT)),
        "the change the failure swallowed was derived anyway, so coverage came back over an \
         entry this case requires to stand still: {rows:?}"
    );
    assert_operationally_valid(&mut store, "the store a never-recovered attachment left");
}

// ---------------------------------------------------------------------------
// Trust transition: a synchronization boundary that never arrives
// ---------------------------------------------------------------------------

/// **An attach whose coverage never proves itself live withdraws trust under an
/// expired synchronization, acquires no derived state, and waits for a demand.**
///
/// The condition is the boundary the platform never reports: the child is
/// started armed at the watcher seam's barrier stage, so the subscription its
/// attach establishes withholds the `Live` publication on every platform path
/// and the wait for that boundary ends with the subscription still
/// synchronizing. Everything after that is production — the wait takes its
/// genuine expiry branch, marks the subscription terminal, and the typed
/// [`WatchError::SynchronizationExpired`] travels the attach's own failure route
/// to the trust state a client reads.
///
/// **The authored deadline is not what elapses, and the child measures that.**
/// An armed barrier makes the wait nothing, so the case reaches the expiry
/// branch in the time a syscall takes rather than in the deadline the production
/// attach authors — and the time from the demand to the published withdrawal is
/// held to a fraction of that deadline, so an arm that stopped shortening the
/// wait fails here instead of passing slowly. What says a caller really reached
/// the withheld boundary is the seam's record, which the barrier writes at the
/// wait rather than at the arming.
///
/// **What such an attach leaves behind is stated rather than assumed.** Coverage
/// is installed before the store is opened and the boundary is waited on after
/// it, so this attach — unlike a refused registration — does open a derived
/// database. It commits nothing into it: the parent reads the file between the
/// two children and requires it to stand with no derived row, which is what
/// separates "the store was opened" from "the entry acquired the vault".
///
/// **Resumption is a second child, because the arm is process-wide.** Every
/// establishment in the armed process withholds its boundary, so the recovery a
/// demand would ask for meets the same expiry — clearing the condition means
/// leaving that process. The same vault is demanded again by a child spawned the
/// same way with nothing armed, and it reaches `Ready`, reports a change of its
/// own through the live subscription it installed, and converges on a derivation
/// built from zero over the same tree. That child is an unarmed watcher control
/// of this suite: it writes no record into the record file the spawn creates for
/// it, which is what makes the armed child's single record a count of firings.
#[test]
fn an_attach_whose_boundary_never_arrives_acquires_nothing() {
    let _beside = beside_the_arms();
    let vault = Vault::new("watch-barrier-expired");
    for index in 0..4 {
        vault.write(&format!("note-{index:03}.md"), &readable(index));
    }

    let expired = vault.run_watch_child("expired-barrier", Some("barrier=expires"));
    assert_eq!(
        expired.status,
        RunStatus::Exited(0),
        "the child did not answer a boundary that never arrived the way the entry is required \
         to\n{}",
        expired.stderr
    );
    expired.attestation.assert_reached(
        "an attach that waited on a boundary that was withheld",
        &[
            (SEAM, WATCH_SEAM),
            ("stage", "barrier"),
            ("answer", "expires"),
        ],
    );
    // One record and no more: the barrier records the wait rather than the
    // arming, so a second is a second establishment — the entry putting
    // coverage back under a failure nothing addressed.
    expired
        .attestation
        .assert_count("an attach that waited on a boundary that was withheld", 1);

    // The database the attach opened before it waited, standing with nothing in
    // it. Read between the two children, because the resumption below derives
    // the vault into this same file.
    assert!(
        vault.database().exists(),
        "the expired attach left no derived store, so what the assertion below reads is a file \
         that was never opened rather than a store that acquired nothing"
    );
    let mut store = vault.store();
    let rows = every_row(&mut store);
    assert!(
        rows.is_empty(),
        "the expired attach derived the vault into the store it opened, past coverage it never \
         proved live: {rows:?}"
    );
    drop(store);

    let resumed = vault.run_watch_child("resumed", None);
    assert_eq!(
        resumed.status,
        RunStatus::Exited(0),
        "the demand that followed the expiry was not served\n{}",
        resumed.stderr
    );
    resumed
        .attestation
        .assert_never_reached("a demand issued with nothing armed", &[(SEAM, WATCH_SEAM)]);
    resumed
        .attestation
        .assert_count("a demand issued with nothing armed", 0);

    let mut store = vault.store();
    assert_eq!(
        every_row(&mut store).len(),
        5,
        "the resumed attach did not derive the vault and the change the control made in it"
    );
    assert_operationally_valid(&mut store, "the store the resumed attach derived");
    drop(store);
    vault.assert_converged_from_zero("coverage installed by the demand that followed an expiry");
}

// ---------------------------------------------------------------------------
// Trust transition: a backend that lost the path set
// ---------------------------------------------------------------------------

/// **A vault-wide overflow publishes the overflow, widens the work to a
/// full-tree reconcile, and keeps the coverage it was reported on.**
///
/// The child attaches, waits for `Ready`, and makes one change of its own. The
/// arm stands in place of the delivery that reports it with the message both
/// platform backends emit when they dropped events and the path set is no longer
/// known — inotify's queue overflow, FSEvents' must-scan-subdirectories flag —
/// which the watcher widens to a rescan of the vault and of the schema. Coverage
/// was never lost, so this is not a trust loss: the entry publishes
/// [`UntrustedReason::WatcherOverflow`] because what it knows about the vault is
/// unreliable until something rereads it, and the reconcile that rereads it is
/// scheduled in the same breath.
///
/// **Waiting for `Ready` is what makes it that delivery.** The arm is owed to
/// the first change a consumer meets over live coverage, and the attach's own
/// heal window has closed by the time the entry serves the vault — so what the
/// arm stands in place of is the edit below and not something the establishment
/// or the heal reported.
///
/// **The forbidden outcome is a recovery.** A recovery would tear coverage down
/// and install it again over a subscription that never stopped reporting, and
/// the entry would come back by a rung it does not owe. The child reads the
/// host's own account instead: no recovery ran, no rebuild ran, and the leg that
/// returned the entry to `Ready` opened every document in the vault rather than
/// the one that changed, which is the shape of a full-tree heal and not of the
/// scoped increment one dirty path earns.
///
/// **The overflow is read off the account rather than sampled.** The entry
/// publishes it only for the length of the reconcile that clears it, which is a
/// window a reader can be scheduled straight past — so what the child asserts is
/// the host's own cumulative count of the reports it took up, which stands after
/// the state carrying them is gone. The counted report and the published reason
/// are one fact: an entry holding coverage and owing no rung publishes the
/// overflow for exactly the facts that carry a rescan, and the two assertions
/// below that no rung ran are what say this entry was such an entry.
///
/// **Live coverage is proven by using it.** The arm is spent and nothing
/// re-established the watch, so a second change reaches the store through the
/// same subscription the overflow was reported on — reported by path, taking up
/// no second report of a lost path set — and the parent's single record is what
/// says no second arm stood in place of it.
#[test]
fn a_vault_wide_overflow_reconciles_under_coverage_that_stays_installed() {
    let _beside = beside_the_arms();
    let vault = Vault::new("watch-stream-rescans");
    for index in 0..OVERFLOWED_DOCUMENTS {
        vault.write(&format!("note-{index:03}.md"), &readable(index));
    }

    let overflowed = vault.run_watch_child("overflow", Some("stream=rescans"));
    assert_eq!(
        overflowed.status,
        RunStatus::Exited(0),
        "the child did not answer a lost path set the way the entry is required to\n{}",
        overflowed.stderr
    );
    overflowed.attestation.assert_reached(
        "a backend that reported its path set lost",
        &[
            (SEAM, WATCH_SEAM),
            ("stage", "stream"),
            ("answer", "rescans"),
        ],
    );
    overflowed
        .attestation
        .assert_count("a backend that reported its path set lost", 1);

    let mut store = vault.store();
    assert_eq!(
        every_row(&mut store).len(),
        OVERFLOWED_DOCUMENTS + 2,
        "the reconcile and the coverage that outlived it did not derive both changes"
    );
    assert_operationally_valid(&mut store, "the store an overflow was reconciled in");
    drop(store);
    vault.assert_converged_from_zero("an overflow reconciled under live coverage");
}

/// How many documents the overflow case's vault holds.
///
/// Wide enough that what the reconcile opened separates a reread of the vault
/// from a reread of the one path that changed by two orders of magnitude: the
/// count is the case's evidence that the overflow widened the work, and over a
/// four-document vault the two legs are a few opens apart. Every other case
/// here reads nothing off the size of the tree and holds four.
const OVERFLOWED_DOCUMENTS: usize = 200;

// ---------------------------------------------------------------------------
// Rung 2, converged: an entry that leaves a directory page
// ---------------------------------------------------------------------------

/// **An entry that vanishes between a directory's listing and its stat is
/// dropped from the page, and the heal completes.**
///
/// Paging a directory is two observations, and a foreign writer can unlink a
/// name between them. That is the vault evolving rather than the machine
/// breaking: a walk begun now lists no such entry either, so the walk states the
/// name as one it read nothing at and the merge answers exactly as it answers a
/// name the walk never yielded.
/// The condition is one call wide, so it is armed at `norn-fs`'s walk seam
/// through the environment the child is started with, and what answers the arm
/// is the production heal from the paging stat through to the trust state a
/// client reads.
///
/// **The forbidden outcome is the refusal.** A whole heal that stops because one
/// document was deleted while it ran leaves the entry untrusted waiting for a
/// demand, with every other document in the vault underived — one foreign
/// deletion turned into a vault nobody is serving. The child asserts the entry
/// serves the vault and reached it without rung 3; the parent asserts the rows
/// the heal left, and neither half is enough alone.
///
/// **The entry that left is not a quarantine and not a death.** Nothing was read
/// and nothing was there to prune, so a finding at that path or a tombstone
/// standing for it would be the heal concluding something about a document it
/// never saw.
///
/// Recovery is the ordinary attach heal: the arm is spent, the document is where
/// it always was, and the next walk derives it. The bound is what tells that
/// from a store rebuilt and walked again — one row, one changeset, where a
/// derivation from zero writes four.
#[test]
fn an_entry_that_vanishes_from_a_page_is_dropped_and_the_heal_completes() {
    let _beside = beside_the_arms();
    let vault = Vault::new("walk-page-vanishes");
    for index in 0..4 {
        vault.write(&format!("note-{index:03}.md"), &readable(index));
    }

    let converged = vault.run_walk_child("vanished", "page=vanishes");
    assert_eq!(
        converged.status,
        RunStatus::Exited(0),
        "the child did not answer an entry that left a page the way the entry is required to\n{}",
        converged.stderr
    );
    converged.attestation.assert_reached(
        "an entry that vanished from a page",
        &[(SEAM, WALK_SEAM), ("stage", "page"), ("answer", "vanishes")],
    );
    converged
        .attestation
        .assert_count("an entry that vanished from a page", 1);

    let mut store = vault.store();
    let paths: Vec<String> = every_row(&mut store)
        .iter()
        .map(|row| row.path.as_str().to_owned())
        .collect();
    assert_eq!(
        paths,
        vec![
            "note-001.md".to_owned(),
            "note-002.md".to_owned(),
            "note-003.md".to_owned()
        ],
        "the heal derived something other than every document beside the one that left the page"
    );
    assert!(
        findings_at(&mut store, "note-000.md").is_empty(),
        "the heal quarantined an entry that left the page, which states that norn read the \
         document and could derive nothing from it"
    );
    assert_eq!(
        tombstones(&mut store).expect("reading the tombstones"),
        Vec::new(),
        "the heal recorded a death for a path that never held a row"
    );
    assert_operationally_valid(&mut store, "the store an entry left a page in");
    drop(store);

    let serving = vault.serving(ProductionPolicy::new(64, 64).unwrap());
    let healed = heal_and_read(&serving, vault.name());
    drop(serving);
    assert_healed_only("an entry that left one page", healed, 1, 1);
    vault.assert_converged_from_zero("an entry that left one page");
}

/// **An entry the machine will not stat refuses the heal, at that same
/// window.**
///
/// The convergence above narrows the paging window to absence; it does not
/// remove the environmental boundary there. A denied stat says nothing about
/// whether an entry is at that name, so reading it as one that left would let a
/// revoked permission prune every row the page covers. The condition is armed at
/// the same seam and the same window as the vanishing above, which is what makes
/// the pair a boundary rather than two unrelated cases.
///
/// **The forbidden outcome is the prune.** A row that disappeared, or a
/// tombstone recorded for it, would be the walk reading a machine that would not
/// answer as a deletion. Rung 3 is forbidden for the same reason a full disk
/// forbids it: the environment is broken and the stored state is not.
#[test]
fn an_entry_the_machine_will_not_stat_refuses_the_heal_rather_than_dropping_it() {
    let _beside = beside_the_arms();
    let vault = Vault::new("walk-page-denied");
    for index in 0..4 {
        vault.write(&format!("note-{index:03}.md"), &readable(index));
    }

    let before = {
        let serving = vault.serving(ProductionPolicy::new(64, 64).unwrap());
        let lease = attach_and_wait(&serving, vault.name());
        drop(lease);
        let mut store = vault.store();
        StoreProjection::read(&mut store).expect("projecting the first attach")
    };
    assert_eq!(before.documents().len(), 4);

    let refused = vault.run_walk_child("denied", "page=denied");
    assert_eq!(
        refused.status,
        RunStatus::Exited(0),
        "the child did not answer a stat the machine refused the way the entry is required to\n{}",
        refused.stderr
    );
    refused.attestation.assert_reached(
        "a paging stat the machine refused",
        &[(SEAM, WALK_SEAM), ("stage", "page"), ("answer", "denied")],
    );
    refused
        .attestation
        .assert_count("a paging stat the machine refused", 1);

    let mut store = vault.store();
    let after = StoreProjection::read(&mut store).expect("projecting the refused attach");
    assert_eq!(
        after.documents(),
        before.documents(),
        "the refused heal changed what the store holds"
    );
    assert_eq!(
        tombstones(&mut store).expect("reading the tombstones"),
        Vec::new(),
        "the heal read a stat the machine refused as evidence that the entry was gone"
    );
    assert_operationally_valid(&mut store, "the store a refused paging stat stands in");
    drop(store);

    // The recovery bar: with nothing armed, an ordinary demand converges on what
    // a derivation from zero over this tree holds. The refused heal committed
    // nothing and no document's bytes moved, so the work left over is nothing at
    // all — where a derivation from zero writes all four documents in one
    // changeset.
    let serving = vault.serving(ProductionPolicy::new(64, 64).unwrap());
    let healed = heal_and_read(&serving, vault.name());
    drop(serving);
    assert_healed_only("a paging stat that answers again", healed, 0, 0);
    vault.assert_converged_from_zero("a paging stat that answers again");
}

// ---------------------------------------------------------------------------
// The child role
// ---------------------------------------------------------------------------

/// The attach a torn case runs, in the process that does not survive it — and
/// the attach a watcher case runs, in the process the arm was read by.
///
/// **Every child of this suite is this one test, told apart by what its
/// environment names.** A tear is armed here, before the demand, so the heal
/// below is the ordinary production one: nothing in it knows a boundary was
/// armed, and what ends the process is the store's own check inside the
/// increment the heal ran. A watcher condition is armed *outside* this process
/// entirely — the seam reads the variable once, at the first watch this process
/// establishes — so a child given one runs its own attach and asserts what the
/// entry owed, in [`watch_under_the_arm`].
///
/// A child started with no tear and no condition named arms nothing and runs the
/// same attach to the end. That is the **control**, and it is what makes every
/// record assertion beside a tear mean something: same binary, same spawn, a
/// record file the spawn creates empty and reads back, and the arm is the only
/// difference between them.
#[test]
fn the_child_role_attaches_under_whatever_it_was_armed_at() {
    let Some(root) = std::env::var_os(CHILD_ROOT) else {
        the_control_attaches_with_nothing_armed();
        return;
    };
    if let Some(condition) = std::env::var_os(CHILD_WATCH) {
        let condition = condition
            .to_str()
            .expect("the watcher condition a child is given is UTF-8")
            .to_owned();
        watch_under_the_arm(Path::new(&root), &condition);
        return;
    }
    if let Some(condition) = std::env::var_os(CHILD_WALK) {
        let condition = condition
            .to_str()
            .expect("the paging condition a child is given is UTF-8")
            .to_owned();
        walk_under_the_arm(Path::new(&root), &condition);
        return;
    }
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
/// store's arms read and standing empty before the run begins, which is the
/// whole point of running it as a child. An arm records itself only where a
/// harness named a file to record into, so a control that left the variable
/// unset would satisfy "nothing was recorded" by having no sink to record into —
/// and the absence assertion the tears rest on would be true of a protocol with
/// no arms left in it at all.
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
// The watcher conditions, met inside the child
// ---------------------------------------------------------------------------

/// Meet `condition` over the vault at `root`, and assert what the entry owes.
///
/// The arm is already in this process's environment, so every call below is the
/// ordinary production one: nothing here knows a boundary was armed, and what
/// the case reads is the trust state a client would read. Every expectation is
/// an assertion, so a child that saw something else ends nonzero and the parent
/// reports it beside the record the seam left.
fn watch_under_the_arm(root: &Path, condition: &str) {
    let vault = Vault::adopt(root);
    match condition {
        "refused-attach" => a_refused_registration_acquires_nothing(&vault),
        "resumed" => a_new_demand_is_served_and_reports_a_change(&vault),
        "terminal-stream" => a_stream_that_ended_resumes_through_a_recovery(&vault),
        "outlived-ticks" => a_published_cause_stands_across_the_ticks_after_it(&vault),
        "expired-barrier" => a_withheld_boundary_expires_and_acquires_nothing(&vault),
        "overflow" => an_overflow_reconciles_under_live_coverage(&vault),
        other => panic!("the child was given no watcher condition it knows: {other:?}"),
    }
}

/// The armed attach: the registration refuses, so the entry acquires nothing
/// and nothing puts coverage back while the demand that asked for it stands.
fn a_refused_registration_acquires_nothing(vault: &Vault) {
    let serving = vault.serving(ProductionPolicy::new(64, 64).unwrap());
    let opening = serving.evidence();
    // Held across all of it: a demand withdrawn before the entry settles takes
    // the standing ask out of the entry, and with it what this rules out.
    let lease = serving
        .demand(vault.name(), AttachMode::Durable)
        .expect("request the attachment");

    let untrusted = wait_for_untrusted(&serving, vault.name());
    assert_backend_loss(&untrusted, "refused this registration");
    assert_stands_on(
        &serving,
        vault.name(),
        &untrusted,
        "an attach whose registration refused",
    );

    let spent = serving.evidence().since(opening);
    assert_eq!(
        spent.recoveries_run, 0,
        "the refused attach ran a recovery, which re-installs coverage over an attachment that \
         still holds its resources — and this attach acquired none: {spent:?}"
    );
    assert_eq!(
        spent.rebuilds_run, 0,
        "the refused attach reached rung 3, which discards derived state to answer coverage that \
         could not be installed: {spent:?}"
    );
    assert_eq!(
        spent.changesets_applied, 0,
        "the refused attach committed derived state past the coverage it never installed: \
         {spent:?}"
    );
    assert!(
        !vault.database().exists(),
        "the refused attach opened a derived store, which stands past the registration it never \
         got"
    );
    drop(lease);
}

/// The control, and the resumption: a child with nothing armed reaches `Ready`
/// and the live subscription it installed reports a change of its own.
///
/// **The record file is live in this run** — created empty by the spawn and
/// still there when the parent reads it — so "no watcher record" is a fact about
/// a seam nothing armed rather than about a harness with no sink. And the change
/// is reported rather than healed: the wait below is on the account moving after
/// the entry was already serving, which is work only a delivery schedules.
fn a_new_demand_is_served_and_reports_a_change(vault: &Vault) {
    let serving = vault.serving(ProductionPolicy::new(64, 64).unwrap());
    let lease = attach_and_wait(&serving, vault.name());

    let serving_from = serving.evidence();
    vault.write("reported.md", &readable(REPORTED));
    wait_for_derived(
        &serving,
        vault.name(),
        serving_from,
        "a change under live coverage with nothing armed",
    );
    drop(lease);
}

/// The armed stream: the delivery that reports one change is a terminal backend
/// failure instead, and only a recovery demand brings the entry back.
fn a_stream_that_ended_resumes_through_a_recovery(vault: &Vault) {
    let serving = vault.serving(ProductionPolicy::new(64, 64).unwrap());
    let lease = attach_and_wait(&serving, vault.name());

    // One change, and no other for the rest of this process: the recovery below
    // establishes a second watch, and that watch is armed too.
    vault.write("edited.md", &readable(EDITED));
    let untrusted = wait_for_withdrawn_trust(&serving, vault.name());
    assert_backend_loss(&untrusted, "ended this event stream");
    let withdrawn_at = serving.evidence();
    assert_stands_on(
        &serving,
        vault.name(),
        &untrusted,
        "a stream that ended under a standing demand",
    );
    // The settle above reads the state repeatedly and this reads across it. The
    // two answer different questions: an entry that ran a recovery on its own
    // and met the same armed stream again is untrusted under the same reason at
    // every look, so standing still is exactly what a spontaneous
    // re-acquisition looks like from outside. The account is where it shows.
    let settled = serving.evidence().since(withdrawn_at);
    assert_eq!(
        settled.recoveries_run, 0,
        "the entry re-installed coverage while nothing had asked it to, so what stood still \
         across the settle was a second failure rather than the first: {settled:?}"
    );

    let lost_at = serving.evidence();
    let recovery = serving
        .demand(vault.name(), AttachMode::Durable)
        .expect("demand the recovery");
    wait_for_ready(&serving, vault.name());
    let spent = serving.evidence().since(lost_at);
    assert_eq!(
        spent.recoveries_run, 1,
        "the entry came back by something other than the recovery a demand asks for: {spent:?}"
    );
    assert_eq!(
        spent.rebuilds_run, 0,
        "a backend that stopped reporting sent the entry to the rung that discards derived state: \
         {spent:?}"
    );
    drop((recovery, lease));
}

/// The armed stream, held rather than demanded back: the cause the failure
/// published stands across every tick that follows it, and nothing is scheduled
/// against the coverage it says is gone.
fn a_published_cause_stands_across_the_ticks_after_it(vault: &Vault) {
    let serving = vault.serving(ProductionPolicy::new(64, 64).unwrap());
    // Held for the whole case: a demand withdrawn here would let the reaper
    // detach the entry, and an entry that was taken away is not an entry that
    // stood still.
    let lease = attach_and_wait(&serving, vault.name());

    // One change, and no other for the rest of this process: the arm is owed to
    // the first delivery a consumer meets over live coverage, and this is it.
    vault.write(OUTLIVED_EDIT, &readable(OUTLIVED));
    let untrusted = wait_for_withdrawn_trust(&serving, vault.name());
    assert_backend_loss(&untrusted, "ended this event stream");

    let withdrawn_at = serving.evidence();
    assert_stands_across(
        &serving,
        vault.name(),
        &untrusted,
        HELD_LOOKS,
        "a cause published for coverage that ended",
    );
    let held = serving.evidence().since(withdrawn_at);

    // The positive fact the zeroes below are read against. Every one of them is
    // satisfied by a dispatcher that stopped taking watcher passes at the
    // moment coverage ended — nothing is opened, written or recovered by a scan
    // that never runs — so the stretch is required to be ticks rather than
    // wall-clock: the account says the entry was polled across it.
    assert!(
        held.watcher_polls > HELD_POLLS_FLOOR,
        "the entry was polled {} times across the stretch it is required to stand still through, \
         so what the zeroes below record is a dispatcher that stopped rather than a cause that \
         outlived its ticks: {held:?}",
        held.watcher_polls
    );

    // The rescan the loss left standing is the thing a reconcile would act on,
    // so the opens are read first: an entry that reread the vault against
    // coverage it no longer holds is the defect this case is stated against,
    // and it is the one a bare trust read cannot see.
    assert_eq!(
        held.document_opens, 0,
        "the entry read the vault across the ticks after the loss, so a reconcile ran against \
         coverage that ended: {held:?}"
    );
    assert_eq!(
        (held.documents_upserted, held.changesets_applied),
        (0, 0),
        "the entry derived and committed after coverage ended, so work was scheduled against a \
         subscription it does not hold: {held:?}"
    );
    assert_eq!(
        held.recoveries_run, 0,
        "the entry re-installed coverage while nothing had asked it to, so what stood still \
         across the ticks was a second failure rather than the first: {held:?}"
    );
    assert_eq!(
        held.rebuilds_run, 0,
        "a backend that stopped reporting sent the entry to the rung that discards derived \
         state: {held:?}"
    );

    // No recovery demand is raised here. What a demand does to this entry is
    // `a_stream_that_ended_resumes_through_a_recovery`'s claim, and raising one
    // would leave two cases stating the same transition.
    drop(lease);
}

/// The armed barrier: the subscription never publishes `Live`, the attach's
/// wait for it expires, and the entry acquires nothing while its demand stands.
fn a_withheld_boundary_expires_and_acquires_nothing(vault: &Vault) {
    let serving = vault.serving(ProductionPolicy::new(64, 64).unwrap());
    let opening = serving.evidence();
    // Held across all of it: a demand withdrawn before the entry settles takes
    // the standing ask out of the entry, and with it what this rules out.
    let demanded_at = Instant::now();
    let lease = serving
        .demand(vault.name(), AttachMode::Durable)
        .expect("request the attachment");

    let untrusted = wait_for_untrusted(&serving, vault.name());
    // **What the arm withholds is the boundary, not the deadline.** The attach
    // authors [`WATCH_SYNCHRONIZATION_DEADLINE`] for a boundary that may still
    // arrive, and an arm that made the wait spend it would state the same
    // outcome over an entry that sat unserved for the whole of it. So the
    // withdrawal is required to be prompt as well as correct: a third of the
    // authored deadline is far past the syscall a zero-length wait costs and
    // far under the deadline itself, so the bound separates the expiry branch
    // an armed barrier reaches at once from the one a real timeout reaches.
    let elapsed = demanded_at.elapsed();
    assert!(
        elapsed < WATCH_SYNCHRONIZATION_DEADLINE / 3,
        "the withdrawal took {elapsed:?}, which is the authored deadline \
         ({WATCH_SYNCHRONIZATION_DEADLINE:?}) rather than the wait an armed barrier ends at once"
    );
    let UntrustedReason::WatcherLost {
        cause: WatcherLossCause::SynchronizationExpired,
        detail,
        ..
    } = &untrusted
    else {
        panic!("a boundary that never arrived was published as something else: {untrusted:?}")
    };
    assert_eq!(
        detail, "filesystem watcher synchronization expired",
        "the expiry published a detail the watch error does not render"
    );
    assert_stands_on(
        &serving,
        vault.name(),
        &untrusted,
        "an attach whose boundary was withheld",
    );

    let spent = serving.evidence().since(opening);
    assert_eq!(
        spent.recoveries_run, 0,
        "the expired attach ran a recovery, which re-installs coverage over an attachment that \
         still holds its resources — and this attach acquired none: {spent:?}"
    );
    assert_eq!(
        spent.rebuilds_run, 0,
        "the expired attach reached rung 3, which discards derived state to answer coverage that \
         never proved itself live: {spent:?}"
    );
    assert_eq!(
        spent.changesets_applied, 0,
        "the expired attach committed derived state past coverage it never proved live: {spent:?}"
    );
    assert_eq!(
        spent.document_opens, 0,
        "the expired attach healed the vault, so it ran past the boundary its wait is required to \
         have failed at: {spent:?}"
    );
    drop(lease);
}

/// The armed stream, the other answer: the delivery reports a lost path set, so
/// the entry publishes the overflow and rereads the vault under the coverage it
/// still holds.
fn an_overflow_reconciles_under_live_coverage(vault: &Vault) {
    let serving = vault.serving(ProductionPolicy::new(64, 64).unwrap());
    let lease = attach_and_wait(&serving, vault.name());

    let ready_at = serving.evidence();
    vault.write("overflowed.md", &readable(OVERFLOWED));
    wait_for_derived(
        &serving,
        vault.name(),
        ready_at,
        "the reconcile an overflow schedules",
    );
    let spent = serving.evidence().since(ready_at);

    // The work the overflow owes is read first, because it is what a failure
    // here should name: an entry that came back without rereading the vault is
    // the defect, and an entry that came back by the wrong rung is the other
    // one. What it published is asked after both.
    assert!(
        spent.document_opens >= OVERFLOWED_DOCUMENTS as u64,
        "the leg that cleared the overflow opened {} documents of {OVERFLOWED_DOCUMENTS}, so it \
         reread the path that changed rather than the vault whose path set was lost: {spent:?}",
        spent.document_opens
    );
    assert_eq!(
        spent.recoveries_run, 0,
        "the entry came back by a recovery, which tears down coverage the overflow never lost: \
         {spent:?}"
    );
    assert_eq!(
        spent.rebuilds_run, 0,
        "an overflow sent the entry to the rung that discards derived state: {spent:?}"
    );
    assert_eq!(
        spent.watcher_rescans_reported, 1,
        "the entry took up something other than one report of a lost path set, so what it \
         published while the reread ran was not the overflow this case armed: {spent:?}"
    );

    // Coverage was never lost, so the subscription that reported the overflow is
    // still the one covering the vault: a second change reaches the store
    // through it, reported by path rather than as a second lost path set.
    let reconciled_at = serving.evidence();
    vault.write("reported.md", &readable(REPORTED));
    wait_for_derived(
        &serving,
        vault.name(),
        reconciled_at,
        "a change reported under the coverage an overflow was reported on",
    );
    let after = serving.evidence().since(reconciled_at);
    assert_eq!(
        (after.watcher_rescans_reported, after.recoveries_run),
        (0, 0),
        "the change after the overflow arrived as another lost path set, so coverage was \
         established again and a second arm stood in place of the delivery: {after:?}"
    );
    drop(lease);
}

/// The document a child writes to have one delivery a stream arm can stand in
/// place of.
const OVERFLOWED: usize = 900;

/// The document a child writes to prove a subscription is live.
const REPORTED: usize = 901;

/// The document whose delivery a terminal stream failure swallowed.
const EDITED: usize = 902;

/// The document whose delivery a terminal stream failure swallowed and no
/// recovery ever healed.
const OUTLIVED: usize = 903;

/// Where that document sits, which is the path the store is required not to
/// hold a row at.
const OUTLIVED_EDIT: &str = "outlived.md";

/// Fail unless the entry is untrusted because its watcher backend failed,
/// naming the armed fault that produced it.
///
/// **The arm is what attests.** A watcher loss under any other cause, or under
/// any other detail, is a loss this arrangement did not cause — and the case
/// would then be stated over a transition something else produced. The variant
/// is matched rather than rendered and searched, so a backend failure published
/// as a lost root, an expired boundary or an environmental refusal fails here.
#[track_caller]
fn assert_backend_loss(untrusted: &UntrustedReason, names: &str) {
    let UntrustedReason::WatcherLost {
        cause: WatcherLossCause::Backend,
        detail,
        ..
    } = untrusted
    else {
        panic!("a backend failure was published as something else: {untrusted:?}")
    };
    assert!(
        detail.contains("armed watcher fault") && detail.contains(names),
        "coverage was lost for something other than the armed fault: {detail}"
    );
}

/// Fail where the entry moves off `published` while nothing has addressed it,
/// over this suite's ordinary settle.
///
/// The settle itself is the shared one, so a case here reads a standing failure
/// the same way every other suite over a production attachment reads it. What
/// this names is the length: [`SETTLE_LOOKS`] is what rules out a re-acquisition
/// that was already on its way, and a case about a longer stretch names its own.
#[track_caller]
fn assert_stands_on(
    host: &Host<ProductionEntryOps>,
    name: &VaultName,
    published: &UntrustedReason,
    subject: &str,
) {
    assert_stands_across(host, name, published, SETTLE_LOOKS, subject);
}

/// How many times a settle looks at an entry that is required to stand still.
const SETTLE_LOOKS: u32 = 40;

/// How many looks the case about a cause outliving the ticks after it takes.
///
/// The children here dispatch a watcher poll every 20ms and a look is 25ms, so
/// 240 looks a tick and a quarter apart hold the entry across roughly three
/// hundred of them — far past the one or two a re-acquisition or a scheduled
/// reconcile would need to appear in, and past the settle every other case here
/// takes. **How many ticks actually ran is read rather than derived from this
/// arithmetic**: the case asserts the polls the host's own account recorded
/// across the stretch, because a dispatcher that stopped would leave the entry
/// standing still for exactly the reason this case exists to rule out.
const HELD_LOOKS: u32 = 240;

/// The floor on the watcher polls a held stretch is required to have taken.
///
/// The arithmetic above puts a healthy run near three hundred. This is far
/// under it, because what it separates is a dispatcher that kept scanning from
/// one that stopped — not a fast host from a loaded one, and this suite runs on
/// hosts that are both.
const HELD_POLLS_FLOOR: u64 = 50;

// ---------------------------------------------------------------------------
// The paging-window conditions, met inside the child
// ---------------------------------------------------------------------------

/// Meet `condition` over the vault at `root`, and assert what the entry owes.
///
/// The arm is already in this process's environment, so the attach below is the
/// ordinary production one: nothing here knows a window was armed, and what the
/// case reads is the trust state a client would read. The rows the heal left are
/// the parent's half — it opens the store after this process is gone.
fn walk_under_the_arm(root: &Path, condition: &str) {
    let vault = Vault::adopt(root);
    match condition {
        "vanished" => an_entry_that_left_a_page_still_serves_the_vault(&vault),
        "denied" => a_paging_stat_the_machine_refused_withdraws_the_entry(&vault),
        other => panic!("the child was given no paging condition it knows: {other:?}"),
    }
}

/// The armed attach: one entry leaves the page, and the heal converges over the
/// rest of the vault rather than refusing the whole of it.
fn an_entry_that_left_a_page_still_serves_the_vault(vault: &Vault) {
    let serving = vault.serving_undrained(ProductionPolicy::new(64, 64).unwrap());
    let opening = serving.evidence();
    let lease = serve_or_report_the_refusal(&serving, vault.name());

    let spent = serving.evidence().since(opening);
    assert!(
        spent.changesets_applied > 0,
        "the entry serves a vault the heal committed nothing to, so what is read after this is a \
         store no attach wrote: {spent:?}"
    );
    assert_eq!(
        spent.rebuilds_run, 0,
        "an entry leaving one directory page reached rung 3, which discards derived state to \
         answer an edit another writer made: {spent:?}"
    );
    drop(lease);
}

/// The armed attach: the machine will not stat a name the listing named, so the
/// heal refuses and the entry stays untrusted naming it.
fn a_paging_stat_the_machine_refused_withdraws_the_entry(vault: &Vault) {
    let serving = vault.serving_undrained(ProductionPolicy::new(64, 64).unwrap());
    let opening = serving.evidence();
    let _lease = serving
        .demand(vault.name(), AttachMode::Durable)
        .expect("request the attachment");

    let untrusted = wait_for_untrusted(&serving, vault.name());
    let spent = serving.evidence().since(opening);
    assert_eq!(
        spent.rebuilds_run, 0,
        "a refused paging stat reached rung 3, which discards a sound database to fix a broken \
         environment: {spent:?}"
    );
    // The reason is the case's own arm attesting: it has to name the entry the
    // stat was refused over and the denial that came of it, or the entry is
    // untrusted for something this arrangement did not cause.
    let UntrustedReason::EnvironmentalRefusal { detail, .. } = &untrusted else {
        panic!(
            "a refused paging stat was published as something other than a broken environment: \
             {untrusted:?}"
        )
    };
    assert!(
        detail.contains("note-000.md") && detail.contains("Permission denied"),
        "the entry is untrusted for a reason that is not the refused stat: {detail}"
    );
}

/// Demand `name`, wait for it to serve the vault, and fail at once where it
/// refuses instead.
///
/// **A refusal is what the case around this is stated against**, so it ends the
/// wait rather than running it out: the reason the entry published says what
/// went wrong, where an elapsed deadline says only that something did.
fn serve_or_report_the_refusal(
    serving: &Serving,
    name: &VaultName,
) -> DemandLease<ProductionEntryOps> {
    let lease = serving
        .demand(name, AttachMode::Durable)
        .expect("request the attachment");
    wait_until(
        &format!("the entry under `{name}` to serve the vault"),
        state_budget(WAIT_LIMIT),
        || {
            let observed = serving.state(name);
            if observed == Ok(TrustState::Ready) {
                return Observed::Met(());
            }
            if let Some(reason) = untrusted_reason(&observed) {
                panic!(
                    "the heal refused over an edit another writer made inside one of its own \
                     windows: {reason:?}"
                );
            }
            assert!(
                !names_no_vault(&observed),
                "the host serves no vault under `{name}`: {observed:?}"
            );
            Observed::pending(format!("the state is {observed:?}"))
        },
    )
    .unwrap_or_else(|failure| panic!("{failure}"));
    lease
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
    wait_for_ready(host, name);
    lease
}

/// Wait until the entry serves the vault.
///
/// Separate from the demand that asks for it, because the demand a recovery is
/// asked for by is raised over an entry that is already attached: what is waited
/// on there is the same convergence, reached from a state a fresh demand never
/// stands in.
fn wait_for_ready(host: &Host<ProductionEntryOps>, name: &VaultName) {
    wait_until(
        &format!("the entry under `{name}` to serve the vault"),
        state_budget(WAIT_LIMIT),
        || {
            let observed = host.state(name);
            if observed == Ok(TrustState::Ready) {
                return Observed::Met(());
            }
            assert!(
                !names_no_vault(&observed),
                "the host serves no vault under `{name}`: {observed:?}"
            );
            Observed::pending(format!("the state is {observed:?}"))
        },
    )
    .unwrap_or_else(|failure| panic!("{failure}"));
}

/// Wait until the work that followed `opening` wrote a derived row and the
/// entry is serving again.
///
/// **This is how a case says a change was taken up rather than that time
/// passed.** A trust state read after an edit says nothing on its own: an entry
/// that never heard about the change is `Ready` too. The account is what
/// separates them — a job that upserted a row is a job that read the change —
/// and it is cumulative, so the reading is taken against one from before the
/// edit rather than sampled.
fn wait_for_derived(serving: &Serving, name: &VaultName, opening: EvidenceReading, subject: &str) {
    wait_until(
        &format!("{subject} to be derived and the entry to serve the vault again"),
        state_budget(WAIT_LIMIT),
        || {
            let spent = serving.evidence().since(opening);
            let observed = serving.state(name);
            if spent.documents_upserted > 0 && observed == Ok(TrustState::Ready) {
                return Observed::Met(());
            }
            assert!(
                !names_no_vault(&observed),
                "the host serves no vault under `{name}`: {observed:?}"
            );
            Observed::pending(format!("the state is {observed:?}, having spent {spent:?}"))
        },
    )
    .unwrap_or_else(|failure| panic!("{failure}"));
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
///
/// **The reason crosses typed rather than rendered.** Both spellings carry the
/// same [`UntrustedReason`], so a case reads the variant the host published and
/// matches it without a wildcard — a refusal minted under a reason no case here
/// states is a failure at the match rather than a substring that happened not to
/// appear.
fn wait_for_untrusted(host: &Host<ProductionEntryOps>, name: &VaultName) -> UntrustedReason {
    wait_until(
        &format!("the entry under `{name}` to refuse"),
        state_budget(WAIT_LIMIT),
        || {
            let observed = host.state(name);
            if let Some(reason) = untrusted_reason(&observed) {
                return Observed::Met(reason);
            }
            assert!(
                observed != Ok(TrustState::Ready),
                "the entry reached `Ready` under a condition it was required to refuse"
            );
            assert!(
                !names_no_vault(&observed),
                "the host serves no vault under `{name}`: {observed:?}"
            );
            Observed::pending(format!("the state is {observed:?}"))
        },
    )
    .unwrap_or_else(|failure| panic!("{failure}"))
}

/// Wait until trust is withdrawn from an entry that is already serving, under
/// this suite's own runaway bound.
///
/// **`Ready` is where this wait starts rather than a failure**, which is what
/// separates it from [`wait_for_untrusted`]: the condition is met by a delivery
/// under live coverage, so the entry serves the vault until the poll that drains
/// the subscription reports it. The wait itself is the shared one, so the two
/// spellings an untrusted entry answers with are read the same way here and in
/// every other suite that attaches a production host.
fn wait_for_withdrawn_trust(host: &Host<ProductionEntryOps>, name: &VaultName) -> UntrustedReason {
    withdrawn_trust_inside(host, name, WAIT_LIMIT)
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
        std::fs::create_dir_all(sandbox.root().join("records")).expect("a record directory");
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
        self.serving_polling_every(policy, Duration::from_millis(20))
    }

    /// A host whose dispatcher drains no delivery inside the life of the child
    /// that takes it.
    ///
    /// **What a paging case reads is the attach heal's own answer.** The interval
    /// is the dispatcher's whole tick, so one longer than the child's life turns
    /// off all three things that tick does, and two of them are what would
    /// otherwise answer for the heal the case is stated over:
    ///
    /// - *Draining a delivery*, which schedules a scoped heal, and a scoped heal
    ///   walks again with the process's one arm already spent — deriving the very
    ///   entry the case is stated over, late enough to read as a page that never
    ///   dropped it. macOS can deliver a change made before the watch existed
    ///   after the boundary that proves coverage, so that ordering is a real one
    ///   over a tree its parent wrote a moment before the spawn, rather than a
    ///   hypothetical.
    /// - *Retrying a pending dispatch*, which is the refusing case's determinism
    ///   condition: the arm is spent by the heal that refused, so a retry would
    ///   run unarmed, succeed, and carry the entry from untrusted to `Ready`
    ///   underneath a wait that is reading for the untrusted state.
    /// - *Reaping idle shared attachments*, which no paging case turns on.
    ///
    /// Nothing else moves: coverage is installed and synchronized the way every
    /// other case installs it, and the heal that answers the arm is the
    /// production one.
    fn serving_undrained(&self, policy: ProductionPolicy) -> Serving {
        self.serving_polling_every(policy, Duration::from_secs(3600))
    }

    fn serving_polling_every(
        &self,
        policy: ProductionPolicy,
        watch_poll_interval: Duration,
    ) -> Serving {
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
                watch_poll_interval,
            },
        )
        .expect("a production host");
        Serving {
            host,
            account,
            _watcher_lease: lease,
        }
    }

    /// Where the arms of a child this vault runs record themselves.
    ///
    /// **Outside every watched edge.** Coverage over a vault is the tree and the
    /// tree's own parent, and the tree here is a directory of the run's working
    /// directory — so a record file beside the tree is a filesystem change the
    /// backend reports back into the batches a case is judging, and it can be
    /// the very delivery a stream arm stands in place of. Both seams write while
    /// coverage is live, so both records go in a directory of the sandbox's own,
    /// which neither edge reaches.
    fn records(&self) -> PathBuf {
        self.sandbox
            .as_ref()
            .expect("a vault that owns its tree runs the children over it")
            .root()
            .join("records")
    }

    /// Run this binary again as a child armed at `tear`, and report what it
    /// left behind. A child given no tear is the control, and arms nothing.
    fn run_child(&self, tear: Option<&str>) -> Ran {
        let hits = self.records().join("arm-hits");
        match tear {
            Some(tear) => self.spawn(&hits, &[(CHILD_TEAR, tear)]),
            None => self.spawn(&hits, &[]),
        }
    }

    /// Run this binary again as a child meeting the watcher `condition`, armed
    /// at `stages`, and report what it left behind.
    ///
    /// **Each condition gets a record file of its own**, so what a case reads is
    /// what one child wrote: a count over a file two children appended to would
    /// say nothing about which of them fired an arm.
    ///
    /// The arm is the process's, so a child meets the condition at every watch
    /// it establishes. Each of these establishes one.
    fn run_watch_child(&self, condition: &str, stages: Option<&str>) -> Ran {
        let hits = self.records().join(format!("{condition}-arm-hits"));
        let mut arm = vec![(CHILD_WATCH, condition)];
        if let Some(stages) = stages {
            arm.push((WATCH_ARMED_STAGES, stages));
        }
        self.spawn(&hits, &arm)
    }

    /// Run this binary again as a child meeting the paging-window `condition`,
    /// armed at `stages`, and report what it left behind.
    ///
    /// The arm is the process's and it fires once, so a child here meets the
    /// condition at the first entry any of its walks pages — which is the attach
    /// heal's own walk of the vault root.
    fn run_walk_child(&self, condition: &str, stages: &str) -> Ran {
        let hits = self.records().join(format!("{condition}-arm-hits"));
        self.spawn(
            &hits,
            &[(CHILD_WALK, condition), (WALK_ARMED_STAGES, stages)],
        )
    }

    /// Run this binary again as a child, recording into `hits`, with `arm` in
    /// its environment beside the variables every child gets.
    ///
    /// The run goes through the testkit's harness rather than through a bare
    /// command, so the child gets a cleared environment with the variables this
    /// case names and the isolation root its parent resolved — a child left to
    /// derive that root would take every lease uncontended and report nothing
    /// about having done so — and a deadline, so a child that wedges is killed
    /// and reported instead of hanging the lane.
    ///
    /// **Both seams are pointed at one file.** A record names the seam that
    /// wrote it, so which of them fired is read off the record rather than off
    /// which file it landed in — and a case that asserts a count over one file
    /// asserts it over both seams at once.
    ///
    /// **The file is made here, empty, before the child starts.** An arm
    /// appends, and [`Attestation::read`] answers the same for a file nothing
    /// wrote and a file that is not there — so a case that asserts nothing was
    /// recorded would otherwise be asserting it over a path no arm could have
    /// used. The file exists before the run and is checked to exist after it,
    /// which is what makes the absence a fact about the seam.
    // The record file is harness scaffolding outside every vault, so the handle
    // that creates it is not a vault handle `norn-fs` owns. The allow sits here
    // rather than at the file, so a vault handle opened anywhere else in this
    // suite is still caught.
    #[allow(clippy::disallowed_types)]
    fn spawn(&self, hits: &Path, arm: &[(&str, &str)]) -> Ran {
        // Created, never truncated: an arm appends, and a file two children
        // shared would lose the first one's records to the second one's spawn.
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(hits)
            .expect("the record file the child appends to");
        let sandbox = self
            .sandbox
            .as_ref()
            .expect("a vault that owns its tree runs the children over it");
        let mut run = Run::new(sandbox, std::env::current_exe().expect("this binary"))
            .args([
                "--exact",
                "the_child_role_attaches_under_whatever_it_was_armed_at",
                "--nocapture",
            ])
            .deadline(CHILD_DEADLINE)
            .env(CHILD_ROOT, &self.root)
            .env(ARM_HITS, hits)
            .env(FS_ARM_HITS, hits);
        for (name, value) in arm {
            run = run.env(*name, value);
        }
        let outcome = run.wait().expect("running the lockdown child");
        // **A filter that names nothing is a run that passes having done
        // nothing.** The child is selected by an exact test name, and a name
        // that no longer exists leaves the harness reporting success over zero
        // tests — every outcome below would then be read off a process that
        // never attached. The harness prints its count before it runs anything,
        // so the count is here whichever way the child ends, including the
        // signal a tear child dies from.
        let stdout = outcome.stdout_text();
        assert!(
            stdout.contains("running 1 test\n"),
            "the child harness ran something other than the one case this spawn selects, so what \
             is asserted below is a process that may never have attached:\n{stdout}"
        );
        assert!(
            hits.exists(),
            "the record file this child was pointed at is gone, so what is read below is the \
             absence of a sink rather than the absence of a firing: {}",
            hits.display()
        );
        Ran {
            status: outcome.status,
            stderr: outcome.stderr_text(),
            attestation: Attestation::read(hits),
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

/// A vault path nothing may read, put back when the case is done with it.
///
/// The restore is a destructor rather than a line at the end of the case: a
/// case that fails while the mode is taken away would otherwise leave a tree
/// nothing can remove.
///
/// Each constructor proves the revocation took by reading the path the way the
/// production path under test reads it — a directory is listed, a document is
/// read — because a process that reads a mode-000 path whatever its mode says
/// arranges nothing, and the case over it would then pass on the absence of a
/// condition.
struct Revoked {
    path: PathBuf,
    /// The mode the path is given back, which is the one its kind carries: a
    /// directory is entered and listed, a document is only read.
    restored: u32,
}

impl Revoked {
    /// A directory nothing may list.
    fn over_directory(path: &Path) -> Revoked {
        set_mode(path, 0o000);
        assert!(
            std::fs::read_dir(path).is_err(),
            "this process lists {} whatever its mode says, so the case below would be arranging \
             nothing. A privileged process cannot carry the permission-loss row.",
            path.display()
        );
        Revoked {
            path: path.to_path_buf(),
            restored: 0o755,
        }
    }

    /// A document nothing may open.
    fn over_file(path: &Path) -> Revoked {
        set_mode(path, 0o000);
        assert!(
            std::fs::read(path).is_err(),
            "this process reads {} whatever its mode says, so the case below would be arranging \
             nothing. A privileged process cannot carry the permission-loss row.",
            path.display()
        );
        Revoked {
            path: path.to_path_buf(),
            restored: 0o644,
        }
    }
}

impl Drop for Revoked {
    fn drop(&mut self) {
        set_mode(&self.path, self.restored);
    }
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .expect("setting a vault path's mode");
}

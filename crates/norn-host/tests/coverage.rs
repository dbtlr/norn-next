//! **The vault root leaving coverage, met end to end by a real attachment.**
//!
//! One trust transition, and the only one of the watcher's that needs no seam to
//! reach. A registration the platform refuses, a stream that ends, a boundary
//! that never arrives — none of those can be arranged over a temporary
//! directory, so each is armed at `norn-fs`'s watcher fault seam and each case
//! that states it compiles behind the feature that widens the seam. **Removing a
//! directory is not such a condition.** A real backend covering a real vault
//! reports the root's own name vanishing on its own, so this case arranges the
//! condition with `rm -r` and asserts what a host does with the entry
//! afterwards — which is why it sits in a suite of its own, behind no feature,
//! and runs in every lane that runs this crate's suites.
//!
//! # The two halves this joins
//!
//! Each half is already carried and they were never met together. `norn-fs`'s
//! watcher suite proves a removed vault root terminates the subscription with
//! [`WatchError::CoverageLost`] naming the canonical root, off a real backend.
//! `norn-host`'s lifecycle proves that cause reaches the trust state and puts
//! the entry's root back under classification — against fake entry operations,
//! with the error handed to the lifecycle rather than produced by a watcher.
//! What is asserted here is the join: a production attachment over a real
//! platform watcher, a root that really goes away, the state a client reads
//! afterwards, and the way back — the tree put back and demanded again, which
//! is what says the withdrawal is a state an entry leaves rather than one a
//! vault is abandoned in.
//!
//! [`WatchError::CoverageLost`]: norn_fs::WatchError::CoverageLost
//!
//! # What this case does not carry
//!
//! **The alias reclassification stays with the fake carrier.** The same signal
//! that withdraws trust also re-reads the entry's registered root against every
//! other root the host serves, and where that read finds a second name reaching
//! the one directory it parks both names under a duplicate-root refusal. That
//! refusal is what a status read then answers with — it stands in front of the
//! trust state — so a case that arranged an alias would be unable to read the
//! withdrawal this one is stated over. A root that is simply gone resolves to
//! nothing, classifies against nothing, and raises no park, which is what leaves
//! the published cause readable. Joining the two would take a second attachment
//! and a second assertion surface, and the fake carrier already states the alias
//! half over both names at once.
//!
//! **What a host's own jobs spent is not read here.** That account is behind
//! `induced-failure` with the rest of the harness-reachable surface, and this
//! suite is behind nothing. It costs this case nothing it needs: an entry that
//! recovered on its own would publish a different cause, because a watch
//! re-established over a directory that is not there fails at registration
//! rather than at coverage; and an entry that pruned or rebuilt would leave a
//! store this case reads at rest and finds changed. Both are asserted as facts
//! rather than as counters.
//!
//! The tree sits in a testkit sandbox, which is a unix-only harness.
#![cfg(unix)]
#![allow(clippy::disallowed_methods)] // Acceptance fixture: arranging and judging a vault tree.

mod attach;

use std::path::Path;
use std::time::{Duration, Instant};

use norn_testkit::equivalence::{StoreProjection, assert_operationally_valid, tombstones};
use norn_testkit::process::Sandbox;
use norn_wire::{UntrustedReason, VaultName, WatcherLossCause};

use attach::{Vault, assert_stands_across, attach_and_wait, derived_documents, untrusted_reason};

/// How many documents the vault holds.
///
/// Small: nothing here is read off the size of the tree. What the count is for
/// is the state at rest — a store that pruned the vault when its root went away
/// differs from one that did not by every one of these rows.
const DOCUMENTS: usize = 4;

/// The runaway bound on the withdrawal arriving.
///
/// **Not a bar on how fast a backend reports.** The condition is delivered on
/// the platform's own schedule and taken up on the host's next watcher tick, and
/// how long that takes is a clock this suite does not read. A case that reaches
/// this bound is one where the root went away and nothing ever said so.
const WITHDRAWN_LIMIT: Duration = Duration::from_secs(120);

/// How many times the settle looks at an entry required to stand still.
///
/// The interval between two looks is the shared one, so what this suite states
/// is the length of its own settle and nothing else.
const SETTLE_LOOKS: u32 = 40;

/// The document the resumed attach finds that the lost one never saw.
///
/// The recreated tree is not a copy of the old one: a document that was never
/// in the vault the first attach derived is what separates an entry that read
/// the tree again from one that handed back the rows it already held.
const RESUMED_DOCUMENT: &str = "resumed.md";

/// **A vault root that stops being covered withdraws trust naming that root,
/// prunes nothing, and does not put coverage back on its own.**
///
/// The host attaches the vault through production entry operations over the
/// machine's one real platform watcher, derives it, and serves it. Then the root
/// directory is removed. Coverage over a vault is the tree *and the tree's own
/// parent*, so what the backend reports is the root's name disappearing from the
/// directory above it — and that is the one watcher fact about the registry
/// rather than about the vault's documents: the path the entry is registered at
/// is not the path coverage was installed over any more.
///
/// **The required outcome is a specific cause, matched rather than rendered.**
/// The entry goes untrusted under [`WatcherLossCause::CoverageLost`], carrying
/// the watch error's own account of itself, which names the canonical root the
/// coverage was installed over. A withdrawal published under any other cause is
/// a transition this arrangement did not produce, so the variant and the path
/// are both asserted.
///
/// **The forbidden outcome is the vault being read as emptied.** Every document
/// the root held is gone from the filesystem, and a host that took the removal
/// for ordinary editing would prune every row and record a tombstone at each of
/// them — converging, correctly by its own lights, on an empty vault. That is
/// the outcome this case exists to rule out: the store is read after the host
/// has let go of it and is required to hold every row the attach heal derived,
/// with no tombstone beside them.
///
/// **That is a real claim on a backend that reports each deletion, and two
/// different mechanisms carry it.** A tree removal is one event to FSEvents and
/// one per path to inotify, so on Linux the documents are reported gone before
/// the root's own name is — and a batch carrying those paths is exactly what a
/// prune would run off. What decides it is the watcher's own ordering rather
/// than a race this case wins. The coalescer takes a recorded terminal failure
/// ahead of whatever facts are pending and drops the pending with it, which is
/// what carries the bar wherever the loss is recorded before an accumulating
/// batch comes due — every macOS run, where the removal is one report and the
/// deletions never accumulate at all. **The coalescer's quiet window is the
/// Linux-side margin**: the per-path deletions land far inside it, so the batch
/// is still accumulating when the root's own deletion records the terminal that
/// outranks it. The entry is told coverage ended, not that the vault emptied.
///
/// **The bar fails toward failure.** Neither mechanism can make a broken host
/// look sound: an entry that read the removal as ordinary editing prunes the
/// rows this case then reads at rest, and an entry never told about the loss
/// never publishes the cause this case waits for and ends at the runaway bound.
/// What a scheduling accident costs here is a red run, never a green one.
///
/// **The second forbidden outcome is coverage coming back by itself.** The
/// demand the vault was attached under is held across the whole failure, and the
/// entry is looked at repeatedly rather than once: a re-establishment takes a
/// job, and a single read after the withdrawal would be taken before that job
/// could run. Standing on the same cause is what says nothing re-acquired —
/// a watch re-established over a directory that is not there refuses at
/// registration, which publishes a backend failure rather than this one.
///
/// **What ends the loss is stated too.** A withdrawal nothing can come back
/// from would be a vault a host abandons, so the case puts the tree back and
/// demands the vault again: the entry reaches `Ready` over the recreated root
/// and converges on what a derivation built from zero over that tree holds,
/// including a document that was never in the vault the first attach derived.
/// The demand is a later one over the same machine-local state rather than one
/// raised through the host that lost coverage, because the store at rest above
/// is read with nothing serving it — and a later demand is what the contract
/// requires resumption of.
#[test]
fn a_root_that_stops_being_covered_withdraws_trust_and_prunes_nothing() {
    let sandbox = Sandbox::new(
        Path::new(env!("CARGO_TARGET_TMPDIR")),
        "norn-host-coverage-loss",
    )
    .expect("a sandbox");
    let root = sandbox.work_dir();
    let vault = arrange(&root);
    let canonical = std::fs::canonicalize(vault.path()).expect("the canonical vault root");

    let host = vault.host();
    // Held across all of it: a demand withdrawn here takes the standing ask out
    // of the entry, and with it the thing "nothing re-acquired coverage" is a
    // claim about.
    let lease = attach_and_wait(&host, vault.name());
    let mut store = vault.store();
    assert_eq!(
        derived_documents(&mut store),
        DOCUMENTS,
        "the attach did not derive the vault, so what the removal below takes away is a tree \
         nothing was holding rows for"
    );
    drop(store);

    std::fs::remove_dir_all(vault.path()).expect("removing the watched vault root");

    let untrusted = wait_for_coverage_loss(&host, vault.name());
    let UntrustedReason::WatcherLost {
        cause: WatcherLossCause::CoverageLost,
        detail,
        ..
    } = &untrusted
    else {
        panic!("a root that stopped being covered was published as something else: {untrusted:?}")
    };
    assert!(
        detail.contains(&canonical.display().to_string()),
        "the loss names something other than the root coverage was installed over: {detail}"
    );
    assert_stands_across(
        &host,
        vault.name(),
        &untrusted,
        SETTLE_LOOKS,
        "a root that stopped being covered",
    );

    // The store is read after the host lets go of it, so what is judged is the
    // state at rest rather than a database another process is writing.
    drop((lease, host));
    let mut store = vault.store();
    assert_eq!(
        derived_documents(&mut store),
        DOCUMENTS,
        "the entry pruned the vault when its root left coverage, so the loss was read as every \
         document having been deleted"
    );
    assert_eq!(
        tombstones(&mut store)
            .expect("reading the tombstones")
            .len(),
        0,
        "the entry recorded deaths for a vault whose root stopped being covered, so a lost \
         watcher was read as a change inside the vault"
    );
    assert_operationally_valid(&mut store, "the store an entry that lost its root left");
    drop(store);

    // The way back. The tree returns with a document the lost attachment never
    // saw, so what the resumed one derives is read off the vault rather than
    // off the rows it already held.
    write_documents(vault.path());
    std::fs::write(
        vault.path().join(RESUMED_DOCUMENT),
        "# Resumed\n\nA document the lost attachment never saw.\n",
    )
    .expect("the document the resumed attach finds");

    let resumed = vault.host();
    let lease = attach_and_wait(&resumed, vault.name());
    drop((lease, resumed));

    let mut store = vault.store();
    assert_eq!(
        derived_documents(&mut store),
        DOCUMENTS + 1,
        "the demand that followed the loss did not derive the recreated vault"
    );
    assert_operationally_valid(&mut store, "the store a resumed attachment left");
    drop(store);
    assert_converged_from_zero(
        &vault,
        &root,
        "the vault a later demand resumed coverage over",
    );
}

/// Write the vault tree at `root`, and hand back the view a host serves it from.
///
/// The tree is written here rather than generated: this case reads nothing off
/// the corpus, and what it needs of the vault is that every document in it is a
/// row the store is required to still hold afterwards.
fn arrange(root: &Path) -> Vault {
    write_documents(&root.join("vault"));
    Vault::adopt(root)
}

/// Write the schema and every document of the tree at `vault`.
///
/// Called twice over one path: once to arrange the vault, and once to put it
/// back after the removal this case is about. Writing a document that is
/// already there with the bytes it already has is the same tree either way,
/// which is what makes the resumed derivation comparable to the lost one.
fn write_documents(vault: &Path) {
    std::fs::create_dir_all(vault.join(".norn")).expect("a vault root");
    std::fs::write(vault.join(".norn/schema.yaml"), attach::SCHEMA).expect("the vault schema");
    for index in 0..DOCUMENTS {
        std::fs::write(
            vault.join(format!("note-{index:03}.md")),
            format!("# Note {index}\n\nA readable document.\n"),
        )
        .expect("a document in the vault");
    }
}

/// Wait until the entry is untrusted **for the loss of coverage over its root**.
///
/// **The cause is what ends this wait, not the state.** A tree being removed is
/// a tree changing under a live watcher: a backend can report the documents
/// going before it reports the root's own name going, and enough such reports
/// at once are a lost path set — which the entry publishes as
/// [`UntrustedReason::WatcherOverflow`] while it rereads the vault. That is a
/// state on the way to this one rather than an outcome, so a wait that stopped
/// at the first untrusted reason it saw would read a scheduling gap as the
/// answer. The coverage loss is terminal, so it is where this settles.
///
/// Any other reason ends the wait where it is found. An entry untrusted for
/// something else is this case's subject answering wrongly rather than a state
/// on the way, and waiting out the bound over it would report a condition that
/// never arrived instead of the answer that did.
fn wait_for_coverage_loss(
    host: &norn_host::Host<norn_host::ProductionEntryOps>,
    name: &VaultName,
) -> UntrustedReason {
    let deadline = Instant::now() + WITHDRAWN_LIMIT;
    loop {
        let observed = host.state(name);
        match untrusted_reason(&observed) {
            Some(
                reason @ UntrustedReason::WatcherLost {
                    cause: WatcherLossCause::CoverageLost,
                    ..
                },
            ) => return reason,
            // The reread the overflow stands for runs under coverage the entry
            // still holds, so the loss is still ahead of it.
            Some(UntrustedReason::WatcherOverflow) => {}
            Some(other) => {
                panic!("the removed root withdrew trust under something else: {other:?}")
            }
            None => {}
        }
        assert!(
            Instant::now() < deadline,
            "trust was not withdrawn for the lost root inside {WITHDRAWN_LIMIT:?}; observed \
             {observed:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Compare what this vault's store holds against a derivation built from zero
/// over the same tree.
///
/// The second derivation is served from a second set of machine-local
/// directories, so it shares the documents and none of the derived state: it
/// walks the tree from an empty database, which is what makes it an answer the
/// first one can be judged against rather than a copy of it. The two are
/// attached one at a time, because a machine runs one platform watcher service
/// and this suite holds the lease over it for one attachment at a time.
fn assert_converged_from_zero(vault: &Vault, root: &Path, subject: &str) {
    let machine = root.join("from-zero");
    std::fs::create_dir_all(&machine).expect("a second machine's directories");
    let beside = vault.beside(&machine);
    let host = beside.host();
    let demand = attach_and_wait(&host, beside.name());
    drop((demand, host));

    let mut left = vault.store();
    let mut right = beside.store();
    assert_operationally_valid(&mut right, "the derivation from zero");
    let left = StoreProjection::read(&mut left).expect("projecting the resumed store");
    let right = StoreProjection::read(&mut right).expect("projecting the from-zero store");
    assert!(
        !right.population().is_empty(),
        "the derivation from zero holds nothing, so the comparison below compares nothing"
    );
    left.assert_equivalent(&right, subject);
}

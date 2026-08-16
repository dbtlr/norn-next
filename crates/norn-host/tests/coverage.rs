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
//! platform watcher, a root that really goes away, and the state a client reads
//! afterwards.
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
use std::time::Duration;

use norn_testkit::equivalence::{assert_operationally_valid, tombstones};
use norn_testkit::process::Sandbox;
use norn_wire::{UntrustedReason, WatcherLossCause};

use attach::{
    Vault, attach_and_wait, derived_documents, untrusted_reason, wait_for_withdrawn_trust,
};

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
const SETTLE_LOOKS: u32 = 40;

/// How long the settle waits between two looks.
const SETTLE_LOOK: Duration = Duration::from_millis(25);

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
/// **The second forbidden outcome is coverage coming back by itself.** The
/// demand the vault was attached under is held across the whole failure, and the
/// entry is looked at repeatedly rather than once: a re-establishment takes a
/// job, and a single read after the withdrawal would be taken before that job
/// could run. Standing on the same cause is what says nothing re-acquired —
/// a watch re-established over a directory that is not there refuses at
/// registration, which publishes a backend failure rather than this one.
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

    let untrusted = wait_for_withdrawn_trust(&host, vault.name(), WITHDRAWN_LIMIT);
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
    assert_stands_on(&host, vault.name(), &untrusted);

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
}

/// Write the vault tree at `root`, and hand back the view a host serves it from.
///
/// The tree is written here rather than generated: this case reads nothing off
/// the corpus, and what it needs of the vault is that every document in it is a
/// row the store is required to still hold afterwards.
fn arrange(root: &Path) -> Vault {
    let vault = root.join("vault");
    std::fs::create_dir_all(vault.join(".norn")).expect("a vault root");
    std::fs::write(vault.join(".norn/schema.yaml"), attach::SCHEMA).expect("the vault schema");
    for index in 0..DOCUMENTS {
        std::fs::write(
            vault.join(format!("note-{index:03}.md")),
            format!("# Note {index}\n\nA readable document.\n"),
        )
        .expect("a document in the vault");
    }
    Vault::adopt(root)
}

/// Fail where the entry moves off `published` while nothing has addressed it.
///
/// The look is repeated rather than taken once, because what is ruled out is an
/// entry that puts coverage back on its own: a re-acquisition takes a job, and a
/// single read after the failure would be taken before that job could run.
#[track_caller]
fn assert_stands_on(
    host: &norn_host::Host<norn_host::ProductionEntryOps>,
    name: &norn_wire::VaultName,
    published: &UntrustedReason,
) {
    for _ in 0..SETTLE_LOOKS {
        std::thread::sleep(SETTLE_LOOK);
        let observed = host.state(name);
        let standing = untrusted_reason(&observed).unwrap_or_else(|| {
            panic!("the entry moved off the loss nothing addressed: {observed:?}")
        });
        assert_eq!(
            &standing, published,
            "the entry published a second reason over the root it lost coverage of"
        );
    }
}

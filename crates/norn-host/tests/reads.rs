//! **The production read path, end to end**: a real vault, a real attachment,
//! and the handle that attachment mints.
//!
//! What the lifecycle suite pins against fakes is the adjudication — which
//! published demand answers a read and which refuses one. What is pinned here
//! is the join: the reader a `ProductionAttachment` mints is `norn-store`'s
//! over the database that attachment derived, the snapshot a hold establishes
//! is of that database at the generation the attach left it at, and what a
//! read runs while it holds the entry gate is one statement.
//!
//! The vault is the smallest generated profile, because none of these cases is
//! about scale: the subject is the seam, and a bigger tree would buy a slower
//! suite and nothing else.
#![cfg(unix)]
#![allow(clippy::disallowed_methods)] // Harness scaffolding: this suite's own generated tree.

mod attach;

use std::path::Path;

use norn_host::{Demand, ReadRefusal};
use norn_testkit::process::Sandbox;
use norn_wire::TrustState;

/// The generated profile every case here attaches.
const PROFILE: &str = "tiny";

/// A sandbox and a vault generated inside it, named for the case.
fn a_vault(label: &str) -> (Sandbox, attach::Vault) {
    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), label).expect("a sandbox");
    let vault = attach::Vault::generate(&sandbox.work_dir().join("attached"), PROFILE);
    (sandbox, vault)
}

/// A read over a real attachment answers under the demand the entry publishes
/// and from the database that attachment derived: the epoch is the store's
/// own, and the generation is the one the heal left it at.
#[test]
fn a_read_answers_under_the_store_the_attachment_derived() {
    let (_sandbox, vault) = a_vault("host-reads-reading");
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());

    let hold = host
        .begin_read(vault.name())
        .expect("an attached vault answers a read");
    assert_eq!(
        hold.reading().published(),
        &Demand::State(TrustState::Ready),
        "the hold reports a demand the entry does not publish"
    );

    let store = vault.store();
    assert_eq!(
        hold.reading().store().epoch(),
        store.epoch(),
        "the read answered from a database the attachment did not derive"
    );
    assert!(
        hold.reading().store().write_generation() > 0,
        "the attachment committed nothing the read could name a generation of"
    );
}

/// **A read is refused before the attach it asks for has finished**, and the
/// refusal is the entry's own published demand rather than a shape the read
/// path invented.
#[test]
fn a_read_over_an_entry_that_is_not_serving_refuses_with_its_published_demand() {
    let (_sandbox, vault) = a_vault("host-reads-refusal");
    let host = vault.host();

    let refusal = host
        .begin_read(vault.name())
        .expect_err("an entry that is not attached answered a read");
    match refusal {
        ReadRefusal::NotServing(Demand::State(TrustState::Warming { .. })) => {}
        other => panic!("a read over an unattached entry refused with {other:?}"),
    }

    // The read scheduled the attach it was refused for, so the vault serves
    // without anything else asking for it.
    let _lease = attach::attach_and_wait(&host, vault.name());
    assert!(
        host.begin_read(vault.name()).is_ok(),
        "the attach a read scheduled left the entry unreadable"
    );
}

/// **The per-read gate discipline, over a real attachment.** Each read here
/// finds the entry's handle standing, so each runs exactly one statement while
/// it holds the entry gate — the statement that establishes its snapshot,
/// with no mint beside it — and a read that found the entry's connection free
/// waited for nothing on its way to it.
///
/// **The contention half of the instrument is not asserted here, because it
/// cannot be asserted here without a race.** Saying "a read waited" requires
/// observing a read while it is waiting, and the host records a wait only once
/// that wait has ended: `reader_waits` moves when the read that waited
/// establishes, so a case cannot hold the connection and watch the account for
/// a waiter at the same time. No production signal reports a read that is
/// currently waiting, and this suite runs against the production attachment,
/// so it has no hook to synchronize on. Sleeping and hoping the other threads
/// reached the wait first is what that gap tempts a case into, and a reading
/// that only sometimes observes contention fails on a loaded machine while the
/// code is perfectly correct.
///
/// Where the contention reading is pinned instead is the lifecycle suite,
/// against a reader fake that counts a wait when the wait begins: a case there
/// blocks until a waiter has provably reached the occupied-connection path,
/// then releases the connection and asserts the wait was one. That is the same
/// claim, held where it can be held deterministically.
#[test]
fn reads_over_one_entry_each_run_one_statement_under_the_gate() {
    let (_sandbox, vault) = a_vault("host-reads-overlap");
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());

    let readers = 4;
    let before = host.read_evidence();
    for _ in 0..readers {
        drop(
            host.begin_read(vault.name())
                .expect("an attached vault answers a read"),
        );
    }
    let reading = host.read_evidence().since(before);

    assert_eq!(
        reading.reads_served, readers,
        "the account missed one of the reads"
    );
    assert_eq!(
        reading.statements_under_the_gate, readers,
        "a read ran something other than one statement under the gate"
    );
    // Every one of these reads found the entry's handle standing, so none of
    // them healed and none of them paid for a mint under the gate. Without
    // this the establishing reading above would stand for everything a read
    // ran there, which is the claim it cannot make on its own.
    assert_eq!(
        reading.mint_statements_under_the_gate, 0,
        "a read over an entry holding its handle ran a mint under the gate"
    );
    assert_eq!(
        host.read_evidence().widest_statements_under_the_gate,
        1,
        "one read ran more than the establishing statement under the gate"
    );
    // **The control on the contention reading.** Each read here gave the
    // connection back before the next one asked for it, so none of them waited.
    // A reading that reported a wait anyway would attest nothing about sharing
    // the handle, because it would stand whether or not two reads overlapped.
    assert_eq!(
        reading.reader_waits, 0,
        "reads that never overlapped waited for the handle anyway"
    );
}

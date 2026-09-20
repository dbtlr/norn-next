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
use std::thread;
use std::time::Duration;

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

/// **The read-concurrency instrument, over overlapping reads of one entry.**
/// Each read runs exactly one statement while it holds the entry gate — the
/// statement that establishes its snapshot — and the reads that queued behind
/// the one handle the entry holds report the wait that cost them.
#[test]
fn overlapping_reads_run_one_statement_each_under_the_gate_and_attest_contention() {
    let (_sandbox, vault) = a_vault("host-reads-overlap");
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());

    let readers = 4;
    let before = host.read_evidence();
    thread::scope(|scope| {
        for _ in 0..readers {
            scope.spawn(|| {
                let hold = host
                    .begin_read(vault.name())
                    .expect("an attached vault answers a read");
                // The handle is held while the other reads ask for it, which
                // is what makes the reads overlap rather than queue.
                thread::sleep(Duration::from_millis(20));
                drop(hold);
            });
        }
    });
    let overlapped = host.read_evidence().since(before);

    assert_eq!(
        overlapped.reads_served, readers,
        "the account missed one of the overlapping reads"
    );
    assert_eq!(
        overlapped.statements_under_the_gate, readers,
        "an overlapping read ran something other than one statement under the gate"
    );
    assert_eq!(
        overlapped.widest_statements_under_the_gate, 1,
        "one read ran more than the establishing statement under the gate"
    );
    assert!(
        overlapped.reader_waits >= 1,
        "four overlapping reads of one entry reported no wait for the one handle they share"
    );
    // **The control.** The same reads without the overlap wait for nothing:
    // a contention reading that stood whether or not the reads overlapped
    // would attest nothing about sharing the handle.
    let before = host.read_evidence();
    for _ in 0..readers {
        drop(
            host.begin_read(vault.name())
                .expect("an attached vault answers a read"),
        );
    }
    let sequential = host.read_evidence().since(before);
    assert_eq!(
        sequential.reads_served, readers,
        "the account missed one of the sequential reads"
    );
    assert_eq!(
        sequential.reader_waits, 0,
        "reads that never overlapped waited for the handle anyway"
    );
}

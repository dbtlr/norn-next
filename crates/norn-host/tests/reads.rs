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

use norn_host::{Demand, ReadRefusal, ReloadRefusal};
use norn_testkit::process::Sandbox;
use norn_testkit::wait::{Observed, wait_until};
use norn_wire::{
    AttachMode, CountParams, ErrorDetail, GroupKey, NotReady, ReasonCode, TrustState,
    UntrustedReason, VaultAddress, VaultName, VaultRoot,
};

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

/// **A read resolves under the case behaviour the vault root proved, and
/// detects none of its own.** The attach opened the store under the order its
/// coverage proved, and the snapshot a read answers from reads under the order
/// the store's rows were derived under.
#[test]
fn a_reads_snapshot_carries_the_case_behaviour_the_root_proved() {
    let (_sandbox, vault) = a_vault("host-reads-case-behaviour");
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());

    let root = std::fs::canonicalize(vault.path()).expect("the vault root");
    let proven = norn_host::stored_path_order(
        norn_fs::PathNormalizer::detect(&root)
            .expect("the root's case behaviour")
            .case_sensitivity(),
    );
    let hold = host
        .begin_read(vault.name())
        .expect("an attached vault answers a read");
    assert_eq!(hold.snapshot().path_order(), proven);
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
/// finds the entry's handle standing and is served, so each runs exactly one
/// statement while it holds the entry gate — the statement that establishes
/// its snapshot, with no mint and no refused attempt beside it — and a read
/// that found the entry's connection free waited for nothing on its way to
/// it.
///
/// **The contention half of the instrument is not asserted here, because it
/// cannot be asserted here without a race.** Saying "a read waited" requires
/// observing a read while it is waiting, and the host records a wait only once
/// that wait has ended — the reading moves when the waiter takes the
/// connection — so a case cannot hold the connection and watch the account for
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
    // Every one of these reads found the entry's handle standing and was
    // served, so none of them healed and none of them had an establishment
    // refuse. Without this the served-read reading above would stand for
    // everything a read ran under the gate, which is a claim it cannot make on
    // its own.
    assert_eq!(
        (
            reading.mint_statements_under_the_gate,
            reading.refused_establishment_statements_under_the_gate
        ),
        (0, 0),
        "a read over an entry holding its handle ran something else under the gate"
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

/// **A hold carries the declaration its snapshot pins, across a reload that
/// changes it.** The model is taken in the gate hold that establishes the
/// snapshot, so a builder handed both compiles against the schema the
/// snapshot's store pins — before the reload and after it — and is never
/// refused as compiled under another.
#[test]
fn a_hold_carries_the_declaration_its_snapshot_pins_across_a_schema_reload() {
    let (_sandbox, vault) = a_vault("host-reads-declaration");
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());

    let pinned = |store: &mut norn_store::Store| {
        store
            .begin_request()
            .vault_schema_pin()
            .expect("reading the pin")
            .expect("an attachment pins the vault schema")
            .fingerprint
    };
    let before = pinned(&mut vault.store());
    {
        let hold = host
            .begin_read(vault.name())
            .expect("an attached vault answers a read");
        assert_eq!(hold.content_model().schema(), Some(before.as_str()));
        hold.snapshot()
            .count(
                &CountParams::new(VaultAddress::name(vault.name().clone())),
                hold.content_model(),
            )
            .expect("a count compiled against the hold's declaration");
    }

    std::fs::write(
        vault.path().join(".norn/schema.yaml"),
        b"version: 1\nfields:\n  created:\n    type: date\n",
    )
    .expect("rewrite the vault schema");
    wait_until(
        "the reload to activate the rewritten schema",
        attach::state_budget(attach::READY_LIMIT),
        || match host.reload(vault.name()) {
            Ok(()) => Observed::Met(()),
            Err(ReloadRefusal::Unavailable(trust)) => {
                Observed::pending(format!("the entry is {trust:?}"))
            }
            Err(refused) => panic!("the reload was refused: {refused:?}"),
        },
    )
    .unwrap_or_else(|failure| panic!("{failure}"));

    let after = pinned(&mut vault.store());
    assert_ne!(after, before, "the reload pinned the schema it replaced");
    let hold = host
        .begin_read(vault.name())
        .expect("a reloaded vault answers a read");
    assert_eq!(
        hold.content_model().schema(),
        Some(after.as_str()),
        "the hold carries a declaration its snapshot does not pin"
    );
    hold.snapshot()
        .count(
            &CountParams::new(VaultAddress::name(vault.name().clone()))
                .with_by([GroupKey::field("created")]),
            hold.content_model(),
        )
        .expect("a count grouping by a typed key compiled against the hold's declaration");
}

/// A count over `name`, every document in one tally.
fn a_count(name: &VaultName) -> CountParams {
    CountParams::new(VaultAddress::name(name.clone()))
}

/// **A count answers under the reading of the snapshot it was read from.** The
/// trust is the `Ready` the entry published, the epoch is the database the
/// attachment derived, and the generation is the one its writes had reached —
/// nothing writes to the vault here, so the store's generation after the
/// answer is the one its snapshot was established at.
#[test]
fn a_count_answers_under_the_reading_of_its_snapshot() {
    let (_sandbox, vault) = a_vault("host-reads-count");
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());

    let answered = host
        .count(&a_count(vault.name()))
        .expect("an attached vault answers a count");

    let mut store = vault.store();
    let generation = store
        .begin_request()
        .write_generation()
        .expect("the store's generation");
    assert_eq!(answered.answer.reading.trust, TrustState::Ready);
    assert_eq!(answered.answer.reading.epoch, store.epoch());
    assert_eq!(
        answered.answer.reading.generation,
        u64::try_from(generation).expect("a generation at or above zero"),
        "the answer names another generation than its snapshot read"
    );
    assert_eq!(answered.answer.reading.ladder, None);
    assert!(answered.answer.is_complete());
    let [tally] = answered.answer.report.rows.as_slice() else {
        panic!(
            "an ungrouped count answered {} tallies",
            answered.answer.report.rows.len()
        );
    };
    assert!(tally.count > 0, "the count found no document in the vault");
    assert_eq!(
        answered.snapshot.snapshots_opened(),
        1,
        "the answer was read from other than the one snapshot its hold established"
    );
    assert_eq!(
        answered.snapshot.statements_executed(),
        1 + answered.work.statements,
        "the snapshot ran statements the count does not account for"
    );
}

/// **A count over an entry that holds nothing yet is refused with the warming
/// state of the attach it asked for**, filed under `host/entry-not-ready`
/// rather than answered as a state.
#[test]
fn a_count_over_an_unattached_vault_is_not_ready() {
    let (_sandbox, vault) = a_vault("host-reads-count-warming");
    let host = vault.host();

    let refused = host
        .count(&a_count(vault.name()))
        .expect_err("an unattached vault answered a count");
    assert_eq!(refused.code(), &ReasonCode::HostEntryNotReady);
    let ErrorDetail::EntryNotReady { state, .. } = refused.detail() else {
        panic!("a count over an unattached vault refused with {refused:?}");
    };
    assert!(
        matches!(state, NotReady::Warming { .. }),
        "the count did not ask for the attach it was refused for: {state:?}"
    );
}

/// **A count over a parked entry is refused with the park's own code.** Two
/// names over one root are a registry in conflict: the attach the first count
/// asks for classifies the root and parks the entry, and every count after it
/// is refused as that park rather than restated as a state — a read withdraws
/// no park.
#[test]
fn a_count_over_a_parked_vault_is_refused_with_the_parks_code() {
    let (_sandbox, vault) = a_vault("host-reads-count-parked");
    let alias = VaultName::new("alias").expect("a legal vault name");
    let host = vault.host_under([vault.name().clone(), alias]);

    let refused = wait_until(
        "the attach a count asked for to park the conflicting entry",
        attach::state_budget(attach::READY_LIMIT),
        || match host.count(&a_count(vault.name())) {
            Err(refused) if refused.code() == &ReasonCode::HostEntryNotReady => {
                Observed::pending(format!("the count was refused with {refused:?}"))
            }
            Err(refused) => Observed::Met(refused),
            Ok(answered) => panic!("a vault registered twice answered a count: {answered:?}"),
        },
    )
    .unwrap_or_else(|failure| panic!("{failure}"));
    assert_eq!(
        refused.code(),
        &ReasonCode::HostDuplicateRoot,
        "{refused:?}"
    );
    assert!(
        host.count(&a_count(vault.name()))
            .is_err_and(|again| again == refused),
        "a count withdrew the park it was refused with"
    );
}

/// A count naming a vault the registry does not hold is refused under the name
/// it asked for.
#[test]
fn a_count_over_an_unknown_name_is_an_unknown_vault() {
    let (_sandbox, vault) = a_vault("host-reads-count-unknown");
    let host = vault.host();
    let ledger = VaultName::new("ledger").expect("a legal vault name");

    let refused = host
        .count(&a_count(&ledger))
        .expect_err("an unknown name answered a count");
    assert_eq!(refused.detail(), &ErrorDetail::unknown_vault(ledger));
}

/// A count addressing its vault by root asks for a throwaway attach, which
/// the host refuses — even over the root of a vault it serves by name.
#[test]
fn a_count_by_root_is_an_unsupported_attach() {
    let (_sandbox, vault) = a_vault("host-reads-count-root");
    let host = vault.host();
    let root = VaultRoot::new(vault.path()).expect("an absolute root");

    let refused = host
        .count(&CountParams::new(VaultAddress::root(root)))
        .expect_err("a root answered a count");
    assert_eq!(
        refused.detail(),
        &ErrorDetail::unsupported_attach_mode(AttachMode::Throwaway)
    );
}

/// A count over `name` grouping by the typed key `created`, which compiles
/// only against a declaration that types it.
fn a_count_by_created(name: &VaultName) -> CountParams {
    a_count(name).with_by([GroupKey::field("created")])
}

/// **A recovery that pins a corrected schema hands a read the declaration it
/// pinned.** The attach reads a schema this build cannot declare, pins nothing
/// and publishes it as untrusted; the schema is corrected; the read's own
/// demand runs the recovery that pins it. The count that follows groups by a
/// key only that schema types, so it answers once the recovery has published —
/// and is never refused as compiled under another declaration than the one its
/// snapshot pins.
#[test]
fn a_read_after_a_recovery_compiles_against_the_schema_the_recovery_pinned() {
    let (_sandbox, vault) = a_vault("host-reads-recovered-declaration");
    let schema = vault.path().join(".norn/schema.yaml");
    std::fs::write(&schema, b"\tinvalid: yaml\n").expect("write an unreadable schema");
    let host = vault.host();
    let _lease = host
        .demand(vault.name(), AttachMode::Durable)
        .expect("request attachment");
    let reason = attach::wait_for_withdrawn_trust(&host, vault.name(), attach::READY_LIMIT);
    assert!(
        matches!(reason, UntrustedReason::SchemaUnreadable { .. }),
        "the attach withheld trust for another reason: {reason:?}"
    );

    std::fs::write(
        &schema,
        b"version: 1\nfields:\n  created:\n    type: date\n",
    )
    .expect("correct the vault schema");
    let answered = wait_until(
        "a count grouping by a typed key to answer after the recovery",
        attach::state_budget(attach::READY_LIMIT),
        || match host.count(&a_count_by_created(vault.name())) {
            Ok(answered) => Observed::Met(answered),
            Err(refused) if refused.code() == &ReasonCode::HostReadFailed => {
                panic!("the count was compiled against another declaration: {refused:?}")
            }
            Err(refused) => Observed::pending(format!("the count was refused with {refused:?}")),
        },
    )
    .unwrap_or_else(|failure| panic!("{failure}"));
    assert_eq!(answered.answer.reading.trust, TrustState::Ready);
}

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
    AnswerAdvisory, AttachMode, BodyText, CollectionPage, CollectionSelector, Column, ComparedBy,
    CountParams, DescribeParams, Direction, ErrorDetail, Facet, FacetKind, FieldType, FindParams,
    FindReport, FindingKind, GetParams, GetReport, GroupKey, Hint, NotReady, ReasonCode,
    ResolutionTarget, Sort, SortKey, TrustState, UntrustedReason, ValidateParams, ValidateReport,
    VaultAddress, VaultName, VaultRoot,
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

/// Make every page of the `documents` table and its indexes unreadable, the
/// way a corrupt file is: the write-ahead log is folded into the database
/// first, so the file holds every page, and then each page's type byte is
/// overwritten with one no b-tree page carries.
fn damage_the_documents_table(database: &Path) {
    let (pages, page_size) = match norn_db::connect(database).expect("connecting to the store") {
        norn_db::Attempt::Connected(connection) => {
            connection
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
                .expect("fold the log into the database");
            let page_size: u64 = connection
                .query_row("PRAGMA page_size", [], |row| row.get(0))
                .expect("the page size");
            let mut statement = connection
                .prepare(
                    "SELECT pageno FROM dbstat WHERE name IN \
                     (SELECT name FROM sqlite_schema WHERE tbl_name = 'documents')",
                )
                .expect("the pages of the documents table");
            let pages: Vec<u64> = statement
                .query_map([], |row| row.get(0))
                .expect("read the pages")
                .collect::<Result<_, _>>()
                .expect("a page number");
            (pages, page_size)
        }
        norn_db::Attempt::Unreadable { detail } => panic!("the store is unreadable: {detail}"),
    };
    assert!(
        !pages.is_empty() && !pages.contains(&1),
        "the documents table stands on pages {pages:?}"
    );
    let mut bytes = std::fs::read(database).expect("read the database file");
    for page in pages {
        let at = usize::try_from((page - 1) * page_size).expect("an offset in memory");
        bytes[at] = 0x00;
    }
    std::fs::write(database, bytes).expect("write the damaged database");
}

/// **A count whose store finds its derived data damaged is refused as an
/// untrusted entry, and the entry rebuilds.** The damage is the store's own
/// verdict on a corrupt page the count read, so the answer is
/// `host/entry-untrusted` under the store-damaged-rebuilding reason, never a
/// failed read; the entry publishes that and runs rung 3, which discards the
/// damaged database and derives the vault into a new one that answers.
#[test]
fn a_count_that_meets_damage_is_untrusted_and_the_entry_rebuilds() {
    let (_sandbox, vault) = a_vault("host-reads-count-damaged");
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let damaged_epoch = vault.store().epoch().to_string();

    damage_the_documents_table(&vault.database());

    let refused = wait_until(
        "a count to meet the damage over an entry nothing else holds",
        attach::state_budget(attach::READY_LIMIT),
        || match host.count(&a_count(vault.name())) {
            Err(refused) if refused.code() == &ReasonCode::HostReaderUnavailable => {
                Observed::pending(format!("the count was refused with {refused:?}"))
            }
            Err(refused) => Observed::Met(refused),
            Ok(answered) => panic!("a damaged store answered a count: {answered:?}"),
        },
    )
    .unwrap_or_else(|failure| panic!("{failure}"));
    assert_eq!(
        refused.detail(),
        &ErrorDetail::entry_untrusted(UntrustedReason::store_damaged_rebuilding(
            "the store is damaged"
        )),
        "{refused:?}"
    );

    let answered = wait_until(
        "the rebuild to put the entry back into service",
        attach::state_budget(attach::READY_LIMIT),
        || match host.count(&a_count(vault.name())) {
            Ok(answered) => Observed::Met(answered),
            Err(refused) if refused.code() == &ReasonCode::HostReadFailed => {
                panic!("a count over a rebuilding entry failed as a read: {refused:?}")
            }
            Err(refused) => Observed::pending(format!("the count was refused with {refused:?}")),
        },
    )
    .unwrap_or_else(|failure| panic!("{failure}"));
    assert_eq!(answered.answer.reading.trust, TrustState::Ready);
    assert_ne!(
        answered.answer.reading.epoch.to_string(),
        damaged_epoch,
        "the count answered from the database the damage was met in"
    );
}

// ---- the read verbs through the one seam ----
//
// Every verb here answers through the seam `count` answers through, so the
// refusals that seam owns — an entry not serving, a name the registry does not
// hold, a vault addressed by root — are the count cases above, and these cases
// hold what each verb's own builder answers through the host.

/// The schema the verb cases derive under: `created` is a date, so an order
/// over it compares dates.
const DATED_SCHEMA: &[u8] = b"version: 1\nfields:\n  created:\n    type: date\n";

/// A document carrying headings at three levels and two block definitions.
const HANDBOOK: &str = "---\ncreated: 2026-03-01T00:00:00Z\n---\n# Handbook\n\nintro para ^intro\n\n\
## Design Notes\n\ndesign body ^design\n\n### Deep\n\ndeep body\n\n## Last\n\nlast body\n";

/// The documents the verb cases add beside the generated tree: the handbook,
/// two documents one target names, and one whose frontmatter never closes.
const WRITTEN: &[(&str, &str)] = &[
    ("zz-guide/zz-handbook.md", HANDBOOK),
    (
        "zz-twins/one/zz-twin.md",
        "---\ncreated: 2026-03-02T00:00:00Z\n---\none\n",
    ),
    (
        "zz-twins/two/zz-twin.md",
        "---\ncreated: 2026-03-03T00:00:00Z\n---\ntwo\n",
    ),
    (
        "zz-broken/zz-unclosed.md",
        "---\ncreated: 2026-03-04\nbody\n",
    ),
];

/// A generated vault holding [`WRITTEN`] and `extra` beside its own tree,
/// derived under [`DATED_SCHEMA`], and a host serving it once it is `Ready`.
fn a_verb_vault(
    label: &str,
    extra: &[(&str, &str)],
) -> (Sandbox, attach::Vault, attach::ServingHost) {
    let (sandbox, vault) = a_vault(label);
    std::fs::write(vault.path().join(".norn/schema.yaml"), DATED_SCHEMA)
        .expect("write the dated schema");
    for (path, text) in WRITTEN.iter().chain(extra) {
        let at = vault.path().join(path);
        std::fs::create_dir_all(at.parent().expect("a document's folder"))
            .expect("create the document's folder");
        std::fs::write(at, text).expect("write the document");
    }
    let host = vault.host();
    (sandbox, vault, host)
}

/// Assert `reading` is the reading of the snapshot an answer was read from:
/// `Ready`, the database the attachment derived, and the generation its
/// writes had reached — nothing writes to the vault once it is attached here,
/// so the store's generation now is the one every snapshot was established
/// at.
fn assert_read_from_its_snapshot(reading: &norn_wire::AnswerReading, vault: &attach::Vault) {
    let mut store = vault.store();
    let generation = store
        .begin_request()
        .write_generation()
        .expect("the store's generation");
    assert_eq!(reading.trust, TrustState::Ready);
    assert_eq!(reading.epoch, store.epoch());
    assert_eq!(
        reading.generation,
        u64::try_from(generation).expect("a generation at or above zero"),
        "the answer names another generation than its snapshot read"
    );
}

fn address(name: &VaultName) -> VaultAddress {
    VaultAddress::name(name.clone())
}

fn a_target(text: &str) -> ResolutionTarget {
    ResolutionTarget::new(text).expect("a target")
}

/// The paths a find page answered.
fn paths_of(page: &FindReport) -> Vec<String> {
    page.rows
        .iter()
        .map(|row| row.path.as_str().to_string())
        .collect()
}

/// **A find answers under the reading of its snapshot, and its pages continue
/// exactly through the host**: each page continues the cursor the one before
/// it minted, moves nothing, and the pages together are the whole listing one
/// page answers. A cursor minted in the path order continues in no other, so a
/// continuation asking for another order is refused as an order changed.
#[test]
fn a_find_pages_exactly_and_refuses_a_cursor_under_another_order() {
    let (_sandbox, vault, host) = a_verb_vault("host-reads-find", &[]);
    let _lease = attach::attach_and_wait(&host, vault.name());

    let whole = host
        .find(&FindParams::new(address(vault.name())).with_limit(1000))
        .expect("an attached vault answers a find");
    assert_read_from_its_snapshot(&whole.answer.reading, &vault);
    assert!(whole.answer.is_complete());
    assert!(
        whole.answer.report.next.is_none(),
        "the whole listing paged"
    );
    assert_eq!(
        whole.snapshot.statements_executed(),
        1 + whole.work.statements,
        "the snapshot ran statements the find does not account for"
    );

    let paged = FindParams::new(address(vault.name())).with_limit(3);
    let mut listed = Vec::new();
    let mut after = None;
    let mut first_cursor = None;
    loop {
        let request = match after.take() {
            None => paged.clone(),
            Some(cursor) => paged.clone().with_after(cursor),
        };
        let answered = host.find(&request).expect("a page answers");
        assert_read_from_its_snapshot(&answered.answer.reading, &vault);
        let page = answered.answer.report;
        assert!(
            page.moved.is_empty(),
            "a continuation moved: {:?}",
            page.moved
        );
        listed.extend(paths_of(&page));
        match page.next {
            Some(next) => {
                first_cursor.get_or_insert_with(|| next.clone());
                after = Some(next);
            }
            None => break,
        }
    }
    assert_eq!(
        listed,
        paths_of(&whole.answer.report),
        "the pages are not the whole listing"
    );

    let cursor = first_cursor.expect("the listing spans more than one page");
    let refused = host
        .find(
            &FindParams::new(address(vault.name()))
                .with_sort(Sort::new(SortKey::field("created"), Direction::Descending))
                .with_limit(3)
                .with_after(cursor),
        )
        .expect_err("a cursor continued under another order");
    assert_eq!(
        refused.code(),
        &ReasonCode::VaultCursorOrderChanged,
        "{refused:?}"
    );
}

/// **A find ordered over dates of both offset spellings carries the
/// advisory**, and one over dates of one spelling carries none. Every
/// generated document and every written one but the unclosed states its
/// offset; the mixed vault adds one that states none.
#[test]
fn a_find_ordered_over_mixed_offsets_is_advised() {
    let by_created = |name: &VaultName| {
        FindParams::new(address(name))
            .with_sort(Sort::new(SortKey::field("created"), Direction::Ascending))
            .with_limit(5)
    };

    // One host at a time: each holds the real-watcher lease while it serves.
    {
        let (_sandbox, vault, host) = a_verb_vault("host-reads-find-stated", &[]);
        let _lease = attach::attach_and_wait(&host, vault.name());
        let stated = host
            .find(&by_created(vault.name()))
            .expect("a find ordered by a date answers");
        assert!(
            stated.answer.advisories.is_empty(),
            "one spelling was advised: {:?}",
            stated.answer.advisories
        );
    }

    let (_sandbox, vault, host) = a_verb_vault(
        "host-reads-find-mixed",
        &[(
            "zz-dated/zz-unstated.md",
            "---\ncreated: \"2026-03-05\"\n---\nunstated\n",
        )],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let mixed = host
        .find(&by_created(vault.name()))
        .expect("a find ordered by a date answers");
    assert_read_from_its_snapshot(&mixed.answer.reading, &vault);
    assert_eq!(
        mixed.answer.advisories,
        [AnswerAdvisory::mixed_offset("created", ComparedBy::Sort)]
    );
}

/// **A validate answers the findings standing over the vault** under the
/// reading of its snapshot: the unclosed frontmatter's finding among them.
#[test]
fn a_validate_answers_the_findings_its_snapshot_holds() {
    let (_sandbox, vault, host) = a_verb_vault("host-reads-validate", &[]);
    let _lease = attach::attach_and_wait(&host, vault.name());

    let answered = host
        .validate(
            &ValidateParams::new(address(vault.name()))
                .with_kinds([FindingKind::FrontmatterUnclosed]),
        )
        .expect("an attached vault answers a validate");
    assert_read_from_its_snapshot(&answered.answer.reading, &vault);
    assert!(answered.answer.is_complete());
    let ValidateReport::Findings { page, .. } = &answered.answer.report else {
        panic!("a validate answered {:?}", answered.answer.report);
    };
    let found: Vec<_> = page
        .rows
        .iter()
        .map(|row| (row.kind, row.path.as_str()))
        .collect();
    assert_eq!(
        found,
        [(FindingKind::FrontmatterUnclosed, "zz-broken/zz-unclosed.md")]
    );
    assert_eq!(
        answered.snapshot.statements_executed(),
        1 + answered.work.statements,
        "the snapshot ran statements the validate does not account for"
    );
}

/// **A describe answers the vault's facets** under the reading of its
/// snapshot: the field its schema declares, and the same field its documents
/// carry.
#[test]
fn a_describe_answers_the_facets_its_snapshot_holds() {
    let (_sandbox, vault, host) = a_verb_vault("host-reads-describe", &[]);
    let _lease = attach::attach_and_wait(&host, vault.name());

    let answered = host
        .describe(
            &DescribeParams::new(address(vault.name()))
                .with_facets([FacetKind::DeclaredField, FacetKind::ObservedField]),
        )
        .expect("an attached vault answers a describe");
    assert_read_from_its_snapshot(&answered.answer.reading, &vault);
    assert!(answered.answer.is_complete());
    let facets = &answered.answer.report.rows;
    assert!(
        facets.iter().any(|facet| matches!(
            facet,
            Facet::DeclaredField { key, field_type: FieldType::Date, .. } if key == "created"
        )),
        "the declared date is not described: {facets:?}"
    );
    assert!(
        facets.iter().any(|facet| matches!(
            facet,
            Facet::ObservedField { key, .. } if key == "created"
        )),
        "the carried field is not described: {facets:?}"
    );
}

/// What a get of `target` answered, under the reading of its snapshot.
fn got(host: &attach::ServingHost, vault: &attach::Vault, params: &GetParams) -> GetReport {
    let answered = host.get(params).expect("an attached vault answers a get");
    assert_read_from_its_snapshot(&answered.answer.reading, vault);
    assert!(
        answered.answer.is_complete(),
        "{:?}",
        answered.answer.unsatisfied
    );
    assert_eq!(
        answered.snapshot.statements_executed(),
        1 + answered.work.statements,
        "the snapshot ran statements the get does not account for"
    );
    answered.answer.report
}

/// **A get answers the one document its target names** — its record, the
/// section a heading anchor names, the block a block anchor names, and a page
/// of one of its collections — each under the reading of its snapshot, and a
/// collection page continues from the cursor it minted.
#[test]
fn a_get_answers_a_record_a_section_a_block_and_a_collection_page() {
    let (_sandbox, vault, host) = a_verb_vault("host-reads-get", &[]);
    let _lease = attach::attach_and_wait(&host, vault.name());
    let getting = |text: &str| GetParams::new(address(vault.name()), a_target(text));

    let GetReport::Record { document, .. } = got(
        &host,
        &vault,
        &getting("zz-handbook").with_columns([Column::path()]),
    ) else {
        panic!("a target with no anchor answered no record");
    };
    assert_eq!(document.path.as_str(), "zz-guide/zz-handbook.md");

    let GetReport::Section {
        path,
        heading,
        body,
        ..
    } = got(&host, &vault, &getting("zz-handbook#design notes"))
    else {
        panic!("a heading anchor answered no section");
    };
    assert_eq!(path.as_str(), "zz-guide/zz-handbook.md");
    assert_eq!((heading.level, heading.text.as_str()), (2, "Design Notes"));
    assert_eq!(
        body.text(),
        "\ndesign body ^design\n\n### Deep\n\ndeep body\n\n",
        "the section is not its body to the next heading at its level"
    );

    let GetReport::Block { block, body, .. } = got(&host, &vault, &getting("zz-handbook#^design"))
    else {
        panic!("a block anchor answered no block");
    };
    assert_eq!(block.id, "design");
    assert_eq!(body.text(), "design body ^design");

    let headings = getting("zz-handbook")
        .with_collection(CollectionSelector::Headings)
        .with_limit(2);
    let heading_texts = |report: GetReport| match report {
        GetReport::Collection {
            page: CollectionPage::Headings { page, .. },
            ..
        } => (
            page.rows
                .into_iter()
                .map(|row| row.text)
                .collect::<Vec<_>>(),
            page.next,
        ),
        other => panic!("a headings page answered {other:?}"),
    };
    let (first, next) = heading_texts(got(&host, &vault, &headings));
    assert_eq!(first, ["Handbook", "Design Notes"]);
    let (second, last) = heading_texts(got(
        &host,
        &vault,
        &headings
            .clone()
            .with_after(next.expect("a second page of headings")),
    ));
    assert_eq!(second, ["Deep", "Last"]);
    assert!(last.is_none(), "the last page of headings minted a cursor");
}

/// **A get whose target names two documents is refused as ambiguous**, with
/// both in the head, how many there were, and the hint naming the target
/// whose `find` resolves them.
#[test]
fn a_get_of_an_ambiguous_target_refuses_with_its_head_and_hint() {
    let (_sandbox, vault, host) = a_verb_vault("host-reads-get-ambiguous", &[]);
    let _lease = attach::attach_and_wait(&host, vault.name());

    let refused = host
        .get(&GetParams::new(
            address(vault.name()),
            a_target("zz-twin#Heading"),
        ))
        .expect_err("an ambiguous target answered a get");
    assert_eq!(refused.code(), &ReasonCode::VaultAmbiguousTarget);
    let ErrorDetail::AmbiguousTarget {
        target, head, hint, ..
    } = refused.detail()
    else {
        panic!("an ambiguous get refused with {refused:?}");
    };
    assert_eq!(target, &a_target("zz-twin#Heading"));
    assert_eq!(head.total(), 2);
    let candidates: Vec<_> = head
        .candidates()
        .iter()
        .map(|candidate| (candidate.path.as_str(), candidate.suffix.as_str()))
        .collect();
    assert_eq!(
        candidates,
        [
            ("zz-twins/one/zz-twin.md", "one/zz-twin"),
            ("zz-twins/two/zz-twin.md", "two/zz-twin"),
        ]
    );
    assert_eq!(hint, &Hint::resolves(a_target("zz-twin")));
}

/// A get whose target names no document is refused as an unknown target,
/// echoing the target as the request named it.
#[test]
fn a_get_of_an_unknown_target_refuses_as_unknown() {
    let (_sandbox, vault, host) = a_verb_vault("host-reads-get-unknown", &[]);
    let _lease = attach::attach_and_wait(&host, vault.name());

    let refused = host
        .get(&GetParams::new(
            address(vault.name()),
            a_target("zz-nowhere#^gone"),
        ))
        .expect_err("an unknown target answered a get");
    assert_eq!(
        refused.detail(),
        &ErrorDetail::unknown_target(a_target("zz-nowhere#^gone"))
    );
}

/// **A get cuts every section and every block as `norn-text` reads it out of
/// the document**, over every document the attachment derived: a heading's
/// section is the span `resolve_section` answers for the heading's text over
/// the document's own parse, and a block is the extent `block_extent` answers
/// at its first definition's marker. What the host reads through is the
/// store's rows of the document, so this holds that those rows and the reader
/// the host hands a get give back what the text layer reads.
#[test]
fn a_gets_sections_and_blocks_are_the_text_layers_reading() {
    let (_sandbox, vault, host) = a_verb_vault("host-reads-get-text", &[]);
    let _lease = attach::attach_and_wait(&host, vault.name());

    let mut paths = Vec::new();
    attach::for_each_derived_path(&mut vault.store(), |path| {
        paths.push(path.as_str().to_string());
    });
    let (mut sections, mut blocks) = (0, 0);
    for path in paths {
        let Ok(source) = std::fs::read_to_string(vault.path().join(&path)) else {
            continue;
        };
        let document = norn_text::Document::parse(&source);
        let body = document.body();
        let scan = document.scan_body();
        let getting = |anchor: &str| {
            GetParams::new(address(vault.name()), a_target(&format!("{path}#{anchor}")))
        };

        for heading in scan.headings() {
            if heading.text.is_empty() || heading.text.starts_with('^') {
                continue;
            }
            let span = norn_text::resolve_section(
                scan.headings(),
                body,
                norn_text::SectionAddress::first(&heading.text),
            )
            .expect("a heading's own text names a section");
            let GetReport::Section {
                heading: answered,
                body: text,
                ..
            } = got(&host, &vault, &getting(&heading.text))
            else {
                panic!("`{path}#{}` answered no section", heading.text);
            };
            assert_eq!(answered.text, scan.headings()[span.heading].text, "{path}");
            assert_body(&text, &body[span.body_start..span.end], &path);
            sections += 1;
        }

        let mut seen = std::collections::BTreeSet::new();
        for definition in scan.block_ids() {
            if !seen.insert(definition.id.clone()) {
                continue;
            }
            let extent = scan.block_extent(definition.span.byte_offset);
            let GetReport::Block { body: text, .. } =
                got(&host, &vault, &getting(&format!("^{}", definition.id)))
            else {
                panic!("`{path}#^{}` answered no block", definition.id);
            };
            assert_body(&text, &body[extent], &path);
            blocks += 1;
        }
    }
    assert!(
        sections > 0 && blocks > 0,
        "the vault read {sections} sections and {blocks} blocks"
    );
}

/// Assert an answered body is `expected`, whole or cut at the row ceiling.
fn assert_body(answered: &BodyText, expected: &str, path: &str) {
    assert_eq!(
        answered.byte_length(),
        expected.len() as u64,
        "{path}: the whole is another length"
    );
    assert!(
        expected.starts_with(answered.text()),
        "{path}: the body answered is not the text layer's"
    );
    assert!(
        answered.is_truncated() || answered.text() == expected,
        "{path}: a whole body was cut"
    );
}

/// A document whose heading and block sit beside multibyte text: the
/// heading's body starts at byte 15 and the block's marker at byte 26, and
/// bytes 6 and 20 are each the second byte of an `é`.
const CAFE: (&str, &str) = ("zz-damage/zz-cafe.md", "# Café ☕ ##\n\nCafé ☕ ^cafe\n");

/// Rewrite one of [`CAFE`]'s derived rows in the store at `database` with
/// `set`, a `table` row the document owns.
fn rewrite_a_cafe_row(database: &Path, table: &str, set: &str) {
    match norn_db::connect(database).expect("connecting to the store") {
        norn_db::Attempt::Connected(connection) => {
            let changed = connection
                .execute(
                    &format!(
                        "UPDATE {table} SET {set} WHERE document = \
                         (SELECT id FROM documents WHERE path = ?1)"
                    ),
                    [CAFE.0],
                )
                .expect("rewrite a derived row");
            assert_eq!(
                changed, 1,
                "`{table}` holds another count of rows for the document"
            );
        }
        norn_db::Attempt::Unreadable { detail } => panic!("the store is unreadable: {detail}"),
    }
}

/// **A get that meets a stored offset its body cannot hold is refused as an
/// untrusted entry, and the entry rebuilds.** A heading's body offset or a
/// block's marker that falls inside a character, below zero or past the end
/// of the body is a derived row that disagrees with the body it was derived
/// from: store damage, answered as `host/entry-untrusted` under the
/// store-damaged-rebuilding reason, never a panic or a failed read. The
/// rebuild derives the rows again, so the same get answers after it.
#[test]
fn a_get_that_meets_an_offset_its_body_cannot_hold_is_untrusted_and_the_entry_rebuilds() {
    let (_sandbox, vault, host) = a_verb_vault("host-reads-get-offset-damage", &[CAFE]);
    let _lease = attach::attach_and_wait(&host, vault.name());
    let section = GetParams::new(address(vault.name()), a_target("zz-cafe#Café ☕"));
    let block = GetParams::new(address(vault.name()), a_target("zz-cafe#^cafe"));
    assert!(matches!(
        got(&host, &vault, &section),
        GetReport::Section { .. }
    ));
    assert!(matches!(
        got(&host, &vault, &block),
        GetReport::Block { .. }
    ));

    let cases = [
        ("headings", "body_offset = 6", &section, "inside the `é`"),
        ("headings", "body_offset = -1", &section, "below zero"),
        (
            "headings",
            "span_offset = -1",
            &section,
            "a heading below zero",
        ),
        ("blocks", "span_offset = 20", &block, "inside the `é`"),
        ("blocks", "span_offset = -1", &block, "below zero"),
        ("blocks", "span_offset = 4096", &block, "past the body"),
    ];
    for (table, set, params, what) in cases {
        rewrite_a_cafe_row(&vault.database(), table, set);
        let refused = host
            .get(params)
            .expect_err("a get answered over an offset its body cannot hold");
        assert_eq!(
            refused.detail(),
            &ErrorDetail::entry_untrusted(UntrustedReason::store_damaged_rebuilding(
                "the store is damaged"
            )),
            "{table} {what}: {refused:?}"
        );
        wait_until(
            "the rebuild to put the entry back into service",
            attach::state_budget(attach::READY_LIMIT),
            || match host.get(params) {
                Ok(answered) => Observed::Met(answered),
                Err(refused) if refused.code() == &ReasonCode::HostReadFailed => {
                    panic!("a get over a rebuilding entry failed as a read: {refused:?}")
                }
                Err(refused) => Observed::pending(format!("the get was refused with {refused:?}")),
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));
    }
}

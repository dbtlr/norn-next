//! The per-PR counter gate: what a request costs over a vault a real host
//! attached, counted rather than timed.
//!
//! Counters are the per-PR lane's currency. Unlike a clock they say the same
//! thing on a loaded machine, and unlike a store-level unit test they say it
//! about the path a request really takes: a `norn-fixtures` tree on disk, a
//! production attachment that walked it, and the derived store that attachment
//! left behind.
//!
//! Five bars, all counts:
//!
//! - **Zero on warm.** A request that only reads derives nothing, over the
//!   ~2k-document `realistic` profile — the scale the gates assert against. It
//!   is asserted twice over, because an attachment that is gone and one that is
//!   still serving are different subjects: once over the rows an attachment left
//!   behind, and once with the entry still attached under a held demand, across
//!   two passes so a cost paid on first touch is separated from the steady state.
//!   **The derivation counters answer for derivation, not for reading.** They
//!   count rows written and facts discarded, and none counts rows read, so a
//!   reader whose row count grew with the tree still reads zero on them: what
//!   this bar holds is that a warm read derives nothing. A read's own cost is
//!   held where it is counted. The store's pillars suite holds the pillar
//!   reads': a work bar drains the heal page, the four pillar enumerations and
//!   the two change feeds a row at a time through `Request::read_steps` and
//!   states each cost as a line in the rows drained, and plan bars over the ten
//!   keyed point reads assert an equality seek on the key each was given —
//!   through a named index for eight of them, and through a primary key for the
//!   field rows and the pinned-schema read. The store's read suites hold each
//!   read statement's plan, and the two bars below hold each read shape's
//!   work.
//! - **A read through a live hold reads no vault document.** A find run on a
//!   production hold's snapshot, under a live attachment, reads nothing through
//!   `norn-fs` on its own thread and moves nothing in the host's account of what
//!   its jobs derived and read off the vault, and each page runs a pinned number
//!   of statements. So does every other read shape, each asked through the host
//!   verb a client's request reaches and each read in a window of its own: the
//!   acceptance contract's count-by-field, suffix / stem resolve, links-to,
//!   findings-for-path and full-text match, and a get's page of a document's
//!   links and a describe beside them. Each zero is measured rather than
//!   structural: a document read and a walk of the root on the same thread move
//!   the first, and a document written and then removed under the same
//!   attachment moves the second. The window is the calling thread's, where a
//!   read verb runs its read; a read on a thread the verb spawned is outside it.
//! - **Size independence.** One bounded write costs the same at 300 documents
//!   and at 2000, and so does one unfiltered find paged newest first, and so
//!   does every other read shape above, each in the bounded form its verb's
//!   contract makes flat and each reading above zero on its statements and on
//!   its rows or its steps. A read shape's cost is its builder's work and the
//!   steps SQLite took over every statement run on its snapshot: statement
//!   counters, not a clock, and they do not see work a virtual table does
//!   inside a statement, so a full-text match's posting-list walk is not
//!   among them. A ceiling passes anything under it; a pair fails the moment
//!   the two scales stop moving together. Every read pair has a control that
//!   grows with the vault, run at the same two attachments, and each control
//!   must read more at the larger scale on the counts it names. A plan's
//!   links are held the same way: judging the links a hub's delete reaches,
//!   and generating the cascade a hub's move plans, each cost the hub's
//!   in-links at both scales. A hub's write and the judgment of its delete
//!   are held again beside namesakes of the hub that the vault's
//!   ambiguity-ignore set keeps out of its class, a subtree that grows with
//!   the vault, so a head read stepping past them would count differently.
//! - **Reads contend on one entry and run only their establishment under its
//!   gate.** Under the `overlapping reads on one entry` workload, eight reads
//!   start while a ninth holds the entry's one connection, and the hold is let
//!   go only once the host's reader-wait reading names all eight as waiting.
//!   Each read served runs exactly its snapshot's establishment under the
//!   entry gate, the deferred `BEGIN` and the one statement that establishes
//!   the snapshot, as the gate holder reads SQLite's own count of what it
//!   began on the holding thread on both sides of its hold rather than as the
//!   establishment reports it, and the gate's own take count reads no retake
//!   between those two readings, so the hold they bound is one hold; the
//!   reader-wait reading is eight; and each contended acquisition took exactly
//!   one round of the entry gate after its first, no one of them more than the
//!   ceiling authored in this crate's baselines. The control runs the same
//!   reads one after another and must fail on contention alone.
//!
//! - **An apply writes through its touched set.** A `set` by path, a hub's
//!   move, two deletes, a `set --where` and a mass delete are applied through
//!   the host's verbs at both per-PR scales, each read off the host's account
//!   of the apply job: it reads each file it touches the authored number of
//!   times for what it does to that file and no other file, commits one
//!   changeset whose work is its touched set's, opens at most one snapshot
//!   and steps no table or index end to end on it, and costs the same in every
//!   count at 300 documents as at 2000. A mass delete's re-decision is held to
//!   the Layer 3 limit per key the store's own bar holds.
//!
//! **Every reading is recorded, zero included.** A gate that passes says only
//! that nothing moved; which counters were asked and what each read is the
//! evidence behind it, and the write's non-zero reading beside them is what says
//! the instrument moves at all.
//!
//! **Every case here is `#[ignore]`d into the `counter-lane` lane**, and the CI
//! `counter gates` job is the only thing that runs them. Attaching thousands of
//! documents beside every other test would put a vault walk in "build and
//! test", which is a suite that stays free of measurement.
//!
//! Each generated tree sits in a testkit sandbox, which is a unix-only harness,
//! and the lane that runs these cases is a Linux one.
//!
//! **The read-through-a-hold bars read the host's account**, which a build
//! reads only behind `induced-failure`, so the lane step names the feature. A
//! build without it compiles every case and refuses the two that read the
//! account when they run, rather than passing them having read nothing.
#![cfg(unix)]
#![allow(clippy::disallowed_methods)] // Harness scaffolding: this suite's own generated tree.

mod attach;
mod baselines;

use std::path::Path;

use std::time::Duration;

use attach::read::{FIND_LIMIT, Pages, bounded_find, pages, the_pinned_declaration};
use norn_fs::reads::{ReadTally, ReadWindow};
use norn_host::Demand;
use norn_store::{
    Change, ContentModel, DocumentFacts, DocumentPath, ExplainedStatement, IncrementProvenance,
    MAX_PAGE, SnapshotCounters, Store, StoredDocument, StoredPathOrder,
};
use norn_testkit::counters::CounterSnapshot;
use norn_testkit::process::Sandbox;
use norn_testkit::scale::{ScaleObservation, SizeIndependencePair};
use norn_testkit::wait::{Observed, wait_until};
use norn_wire::{
    CollectionPage, CollectionSelector, Column, CountParams, DescribeParams, Direction, FacetKind,
    FindParams, GetParams, GetReport, GroupKey, Predicate, ResolutionTarget, RungSelection,
    RungSet, SearchParams, Sort, SortKey, TrustState, ValidateParams, ValidateReport, VaultAddress,
    VaultName,
};

/// The document the size-independence pair writes at both scales.
///
/// Named where no generated tree places one: the pair compares what one write
/// costs, and a path a profile already derived would cost the discard of its
/// fact rows at one scale and not the other.
const PROBE_PATH: &str = "counter-gate/probe.md";

/// **The zero-on-warm bar, over the host's own path.**
///
/// The vault is generated, attached, and detached; what the request reads is
/// what the attachment derived. Every reader the store offers is exercised
/// against content that is really there — the paths and stems come off rows the
/// attach wrote — so the reading is of readers that found something rather than
/// of lookups that missed.
#[test]
#[ignore = "counter-lane case: runs in the ci counter gates job, not the workspace suite"]
fn a_warm_request_over_an_attached_vault_finishes_at_zero() {
    let profile = norn_fixtures::Profile::by_name("realistic").expect("the gate profile");
    let (_sandbox, vault) = attached("counter-gate-warm", profile.name);

    let mut store = vault.store();
    assert_the_attachment_derived_the_profile(&mut store, &profile);
    let subject = a_derived_document(&mut store);

    let snapshot = a_warm_pass(&mut store, &subject);
    record_the_counters("a warm request over a detached vault", &snapshot);
    snapshot.assert_all_zero("a warm request over an attached vault");
}

/// **The zero-on-warm bar with the host still serving**, and again on a second
/// pass over the same store; and **a find through the live hold reads no vault
/// document**.
///
/// The case above reads what an attachment left behind: its host is gone by the
/// time a counter is read, so what it says is that the rows on disk answer
/// without deriving. This one says the other half — the entry is still
/// attached, its demand lease is still held, and its watcher is still
/// subscribed while the request runs. A read path that derived under a live
/// attachment, or that warmed something on first touch and paid for it, would
/// move a counter here and nowhere above.
///
/// Two passes over the store, and both are judged. A first pass over a store
/// nothing has read yet is where a lazily-built index or a cache filled on
/// demand would be paid for; the second is the steady state the claim is
/// about, and the pair is what separates them.
///
/// **The find runs through the hold, and reads nothing through `norn-fs`.**
/// `norn-fs` is the only file reader the host uses, and what it reads is
/// counted on the thread that asked, so two readings answer for the find. A
/// read window stands on this thread across the whole workload — both pages
/// and both passes — and reads no document opened, no stat and no directory
/// entry: that is the find itself reading nothing through `norn-fs`. The
/// host's account of its jobs reads what a read through a hold could cost the
/// vault on another thread — a job the host ran meanwhile — and reads zero
/// documents derived, files opened for their content, and changesets landed
/// and what they upserted, deleted and tombstoned. A raw `std::fs` read is
/// outside both readings: this is a claim about the reader the host has.
///
/// **The find's cost is pinned as well as its zero.** Each page runs
/// [`STATEMENTS_PER_PAGE`] statements and hydrates a whole page, so a find
/// that ran a statement per row it hydrated fails here at any scale.
///
/// Each zero is measured rather than structural, and its control moves every
/// count it asserts. On this thread, the same window around one document read
/// through `norn-fs` moves the opens and the stats, and a walk of the
/// registration root through the walker the host attaches with moves the
/// directory entries. In the host's account, a document
/// written into the vault under the same attachment moves the documents
/// derived, the files opened, the changesets and the upserts; removing it
/// again moves the deletes and the tombstones.
#[test]
#[ignore = "counter-lane case: runs in the ci counter gates job, not the workspace suite"]
fn warm_requests_under_a_live_attachment_finish_at_zero() {
    the_hosts_account_is_readable();
    let profile = norn_fixtures::Profile::by_name("realistic").expect("the gate profile");
    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), "counter-gate-live")
        .expect("a sandbox");
    let vault = attach::Vault::generate(&sandbox.work_dir().join("attached"), profile.name);

    // Host and lease are held for the whole case: demand is what keeps the idle
    // reaper away from the entry, so an entry that is still attached when the
    // last read finishes is one that was demanded throughout.
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());

    let mut store = vault.store();
    assert_the_attachment_derived_the_profile(&mut store, &profile);
    let subject = a_derived_document(&mut store);
    let declared = the_pinned_declaration(&mut store);

    let before = vault_work(&host);
    let window = ReadWindow::open();
    let hold = host
        .begin_read(vault.name())
        .expect("a live attachment answers a read");
    assert_eq!(
        hold.reading().published(),
        &Demand::State(TrustState::Ready),
        "the read ran under a demand the entry does not publish"
    );
    assert_eq!(
        hold.reading().store().epoch(),
        store.epoch(),
        "the read answered from a database this attachment did not derive"
    );

    // The find a client asks: a predicate, an order and a bound, then the
    // page its cursor continues, each row carrying its fields and its tags.
    let tasks = bounded_find(vault.name())
        .with_predicates([Predicate::equal_to("type", "task")])
        .with_columns([Column::fields(), Column::tags()]);
    let read = pages(hold.snapshot(), &tasks, &declared, 2);

    // The passes over the store run beside the hold rather than through it: a
    // request is opened from `&mut Store`, and that is where a derivation
    // counter is. They confirm the property over the strictly more
    // derivation-capable subject.
    let first = a_warm_pass(&mut store, &subject);
    let second = a_warm_pass(&mut store, &subject);
    drop(hold);
    let read_on_this_thread = thread_reads(window.finish());
    let read_off_the_vault = before
        .delta(&vault_work(&host))
        .expect("two readings of one account");

    record_the_counters("a warm request under a live attachment, first pass", &first);
    record_the_counters(
        "a warm request under a live attachment, second pass",
        &second,
    );
    record_the_counters(
        "a find through a live hold, read through norn-fs on its thread",
        &read_on_this_thread,
    );
    record_the_counters(
        "a find through a live hold, the host's account",
        &read_off_the_vault,
    );
    record_the_counters("a find through a live hold, its work", &read.readings());

    // The entry is still the one the reads ran against, rather than one the
    // host tore down part-way: a bar over a detached entry is the case above
    // wearing this one's name.
    assert_eq!(
        host.state(vault.name()),
        Ok(TrustState::Ready),
        "the reads above were meant to run against a live attachment, and the entry is not ready"
    );
    first.assert_all_zero("the first warm request under a live attachment");
    second.assert_all_zero("the second warm request under a live attachment");

    // A zero is only a statement about a find that read something, and what
    // it read is the shape's own cost.
    assert_eq!(
        read.pages.len(),
        2,
        "the find was meant to read a first page and the one its cursor continues"
    );
    for (at, page) in read.pages.iter().enumerate() {
        assert!(
            page.unsatisfied.is_empty(),
            "page {at} of the find could not apply {:?}",
            page.unsatisfied
        );
        assert_eq!(
            (page.work.statements, page.work.documents_hydrated),
            (STATEMENTS_PER_PAGE, u64::from(FIND_LIMIT)),
            "page {at} of the find was meant to run {STATEMENTS_PER_PAGE} statements and hydrate \
             a page of {FIND_LIMIT} rows, since `realistic` holds more tasks than two pages: {:?}",
            page.work
        );
    }
    for row in read.pages.iter().flat_map(|page| &page.rows) {
        let fields = row.fields.as_ref().expect("the find projected the fields");
        assert!(
            fields.contains_key("type") && row.tags.is_some(),
            "{} came back without the columns the find projected",
            row.path.as_str()
        );
    }
    read_on_this_thread.assert_all_zero("a find through a live hold, on its own thread");
    read_off_the_vault.assert_all_zero("a find through a live hold, in the host's account");

    the_thread_tally_moves(&vault, subject.path.as_str());
    the_hosts_account_moves(&host, &vault);
}

/// **The other half of the thread's zero.** A read window on this thread
/// around one read of `document` and one walk of the registration root, both
/// through `norn-fs`, moves every count a thread's zero asserts.
fn the_thread_tally_moves(vault: &attach::Vault, document: &str) {
    let window = ReadWindow::open();
    norn_fs::read_and_hash(vault.path(), Path::new(document))
        .expect("reading a document the attachment derived");
    for fact in norn_fs::walk(vault.path(), &[]).expect("walking the registration root") {
        fact.expect("a walk of the registration root");
    }
    let reached = thread_reads(window.finish());
    record_the_counters(
        "one document read and one walk of the root through norn-fs",
        &reached,
    );
    for count in ["document_opens", "stats", "walk_dirents"] {
        assert!(
            reached.get(count) > 0,
            "a document read and a walk through norn-fs on this thread did not move `{count}`: \
             {reached:?}"
        );
    }
}

/// **The other half of the account's zero.** A document written into the
/// vault under a live attachment is derived by the host, and removing it is
/// deleted and tombstoned by the host; each moves its counts in the host's
/// account.
fn the_hosts_account_moves(host: &attach::ServingHost, vault: &attach::Vault) {
    let written = vault.path().join("counter-gate-derived.md");
    let derived = the_host_spends(
        host,
        || {
            std::fs::write(&written, "---\ntitle: derived\n---\n\na body\n")
                .expect("writing a document into the vault");
        },
        "derive the document written under its attachment",
        &[
            "documents_derived",
            "document_opens",
            "changesets_applied",
            "documents_upserted",
        ],
    );
    record_the_counters("a document written under a live attachment", &derived);

    let deleted = the_host_spends(
        host,
        || std::fs::remove_file(&written).expect("removing the written document"),
        "delete the document removed under its attachment",
        &[
            "changesets_applied",
            "documents_deleted",
            "tombstones_recorded",
        ],
    );
    record_the_counters("a document removed under a live attachment", &deleted);
}

/// How many statements each page of the live-hold find runs, on the first
/// page and on the page its cursor continues alike.
///
/// Six, each once per page and none per row: the pinned schema's fingerprint
/// the find is judged under; whether a document carries `type`, and whether
/// one carries `created`, one existence seek each; the page of `created`
/// marker rows in descending order that the `type` filter narrows; the
/// document rows the page found, by row id; and the head of each found
/// document's tags. The cursor the second page continues is judged against
/// the order it was minted in and becomes the page statement's bound, with no
/// statement of its own. A statement run per hydrated row would put this at
/// more than a page's rows.
const STATEMENTS_PER_PAGE: u64 = 6;

/// What the host's account moved from just before `act` until every one of
/// `counts` has moved, waiting for the job the host runs on its own in answer
/// to `act`.
///
/// The baseline is read before `act` runs. The host folds a job's counts into
/// its account when the job ends, so a baseline read after the act can already
/// hold the job, and the wait would then never see its counts move.
///
/// The wait is for the whole set rather than for any one of it: the host folds
/// a job's counts into its account one at a time, so a reading taken part-way
/// through that fold shows some of a job's counts and not yet the rest. A
/// count that never moves exhausts the wait, and the failure names it.
fn the_host_spends(
    host: &attach::ServingHost,
    act: impl FnOnce(),
    what: &str,
    counts: &[&str],
) -> CounterSnapshot {
    let before = vault_work(host);
    act();
    wait_until(
        &format!("the host to {what}"),
        attach::state_budget(DERIVATION_LIMIT),
        || {
            let spent = before
                .delta(&vault_work(host))
                .expect("two readings of one account");
            let unmoved: Vec<&str> = counts
                .iter()
                .copied()
                .filter(|count| spent.get(count) == 0)
                .collect();
            if unmoved.is_empty() {
                Observed::Met(spent)
            } else {
                Observed::pending(format!(
                    "{unmoved:?} have not moved, and the host's account reads {spent:?}"
                ))
            }
        },
    )
    .unwrap_or_else(|failure| panic!("{failure}"))
}

/// What one thread read through `norn-fs` while a window stood over it, by
/// name.
fn thread_reads(tally: ReadTally) -> CounterSnapshot {
    [
        ("document_opens", tally.document_opens),
        ("stats", tally.stats),
        ("walk_dirents", tally.walk_dirents),
        ("write_dirents", tally.write_dirents),
    ]
    .into_iter()
    .collect()
}

/// How long the host may take to derive a document written under its
/// attachment. A runaway bound: the watcher polls far more often than this.
const DERIVATION_LIMIT: Duration = Duration::from_secs(60);

/// What of the host's account a read that reached the vault through the host
/// would move: the documents its jobs derived from the vault, the files they
/// opened for their content, and what the changesets they landed upserted,
/// deleted and tombstoned.
///
/// Watcher polls, stats and directory entries are left out: a live attachment
/// polls its watcher whatever anybody reads, and a poll reads no document.
/// Findings discarded are left out too: no control here moves them, and a
/// zero no control moves is not a measurement.
///
/// Each value is a running total over the host's life, so what a stretch of
/// work moved is the delta between two readings.
#[cfg(feature = "induced-failure")]
fn vault_work(host: &attach::ServingHost) -> CounterSnapshot {
    let account = host.evidence();
    [
        ("documents_derived", account.documents_derived),
        ("document_opens", account.document_opens),
        ("changesets_applied", account.changesets_applied),
        ("documents_upserted", account.documents_upserted),
        ("documents_deleted", account.documents_deleted),
        ("tombstones_recorded", account.tombstones_recorded),
    ]
    .into_iter()
    .collect()
}

/// A build without `induced-failure` carries no reader of the host's account.
#[cfg(not(feature = "induced-failure"))]
fn vault_work(_: &attach::ServingHost) -> CounterSnapshot {
    unreachable!("a case that reads the host's account refuses to run without `induced-failure`")
}

/// A build with `induced-failure` reads the host's account.
#[cfg(feature = "induced-failure")]
fn the_hosts_account_is_readable() {}

/// Refuse to run a case that reads the host's account in a build that cannot
/// read it, before the case generates anything, rather than pass it having
/// read nothing.
#[cfg(not(feature = "induced-failure"))]
fn the_hosts_account_is_readable() {
    panic!(
        "this case reads the host's account of what its jobs derived and read off the vault, \
         which a build reads only behind `induced-failure`: run the lane with \
         `LANE_FEATURES=induced-failure`"
    )
}

/// One warm read-only pass over `store`, and what it derived.
///
/// Every reader the store offers is exercised against content that is really
/// there — the path and the stem come off a row the attach wrote — so the
/// reading is of readers that found something rather than of lookups that
/// missed. A reader that returned nothing would count nothing whatever the read
/// path did.
fn a_warm_pass(store: &mut Store, subject: &StoredDocument) -> CounterSnapshot {
    let stem = subject.path.stem().to_string();
    let mut warm = store.begin_request();
    let probe = warm
        .class_probe(&stem)
        .expect("a class stem off a derived path");
    let class = warm
        .target_class(&stem, &norn_store::AmbiguityIgnore::none())
        .expect("a suffix target off a derived path");
    assert!(
        warm.stored_document(&subject.path)
            .expect("reading a document")
            .is_some(),
        "the attachment derived {} and a warm read did not find it",
        subject.path.as_str()
    );
    let _ = warm.stored_facts(&subject.path).expect("reading facts");
    let _ = warm
        .stored_tombstone(&subject.path)
        .expect("reading a tombstone");
    let _ = warm
        .stored_findings(&subject.path)
        .expect("reading findings");
    let _ = warm.findings_in_class(&probe).expect("reading a class");
    let _ = warm.suffix_candidates(&class).expect("reading candidates");
    let _ = warm
        .emitted_plan(ExplainedStatement::SuffixCandidates(&class))
        .expect("a query plan");
    assert!(
        warm.vault_schema_pin().expect("reading the pin").is_some(),
        "an attachment pins the vault schema, and this store carries no pin"
    );
    let _ = warm.pillars().expect("a pillar report");

    warm.finish().readings().collect()
}

/// Record a counter reading where a person will find it.
///
/// **A gate that passes says only that nothing moved.** Which counters were
/// asked and what each of them read is the evidence behind that, and a zero
/// nobody can see is indistinguishable from an instrument that was never
/// wired — so every counter in the vocabulary is written out by name, at the
/// value this request finished on.
fn record_the_counters(heading: &str, snapshot: &CounterSnapshot) {
    let readings: Vec<(&str, String)> = snapshot
        .names()
        .map(|name| (name, snapshot.get(name).to_string()))
        .collect();
    norn_testkit::readings::record(heading, &readings);
}

/// **The size-independence bar.** The vault around a bounded write is not part
/// of what the write costs.
///
/// Both profiles are attached the same way, and the same single-document
/// changeset is applied to each derived store. `ambiguous` holds 300 documents
/// and `realistic` 2000, so a write whose cost were the store's contents would
/// count differently across the pair.
#[test]
#[ignore = "counter-lane case: runs in the ci counter gates job, not the workspace suite"]
fn a_bounded_write_costs_the_same_at_both_scales() {
    let small = norn_fixtures::Profile::by_name("ambiguous").expect("the ambiguity profile");
    let large = norn_fixtures::Profile::by_name("realistic").expect("the gate profile");

    let small_counters = one_probe_write("counter-gate-pair-ambiguous", &small);
    let large_counters = one_probe_write("counter-gate-pair-realistic", &large);

    SizeIndependencePair::new(
        "upserting one document",
        ScaleObservation::new(&small, small_counters),
        ScaleObservation::new(&large, large_counters),
    )
    .assert_size_independent();
}

/// Attach `profile`, then count what upserting one document into its derived
/// store costs.
fn one_probe_write(label: &str, profile: &norn_fixtures::Profile) -> CounterSnapshot {
    let (_sandbox, vault) = attached(label, profile.name);

    let mut store = vault.store();
    assert_the_attachment_derived_the_profile(&mut store, profile);
    assert_the_probes_stem_is_the_probes_alone(&mut store);
    let mut request = store.begin_request();
    // The probe holds no link and its stem is its own, so the link health the
    // write re-decides reads no ambiguity-ignore glob: the pinned schema's
    // fingerprint is the whole of the declaration it is judged under.
    let declared = request
        .vault_schema_pin()
        .expect("reading the pinned schema")
        .map_or_else(norn_store::ContentModel::none, |pin| {
            norn_store::ContentModel::under(pin.fingerprint)
        });
    request
        .apply_increment(
            IncrementProvenance::Derived,
            [Change::Upsert(DocumentFacts::new(
                DocumentPath::new(PROBE_PATH).expect("a document path"),
                "counter-gate-probe",
                "a body\n",
                7,
            ))],
            &[],
            &declared,
        )
        .expect("applying a document upsert");
    let reading = request.finish();
    assert!(
        !reading.is_all_zero(),
        "a write that counted nothing is an instrument that never reached the store"
    );
    let snapshot: CounterSnapshot = reading.readings().collect();
    // **The other half of a zero.** The warm bars above read every counter at
    // zero over the same store type and the same instrument; what says that is
    // a read path deriving nothing rather than a counter set nothing ever
    // moves is a derivation, recorded beside them.
    record_the_counters(
        &format!("one document upserted over `{}`", profile.name),
        &snapshot,
    );
    snapshot
}

/// The stem the write-work bar's hub write targets. No generated document
/// shares it, asserted below rather than assumed: a fixture whose word list
/// ever drew it would put the hub in a populated class at one scale and not
/// the other, which is a violation of the word list rather than of ADR 0027.
const HUB_STEM: &str = "hub-gate-hub";

/// The document the write-work bar's hub write derives.
fn hub_path() -> String {
    format!("hub-gate/{HUB_STEM}.md")
}

/// How many documents already hold a bare-stem link to the hub, fixed at both
/// per-PR scales: the claim under test is that a hub's write costs its own
/// in-links, never the vault around it (ADR 0027's write-work bar).
const HUB_IN_LINKS: usize = 20;

/// Plant [`HUB_IN_LINKS`] documents linking the hub by its bare stem into
/// `vault`'s tree, before anything attaches it — so the attach heal derives
/// them as broken links, the way any other document a vault already held
/// would be.
fn plant_hub_in_links(vault: &attach::Vault) {
    for at in 0..HUB_IN_LINKS {
        let path = vault.path().join(format!("hub-gate/in-links/{at:04}.md"));
        std::fs::create_dir_all(path.parent().expect("a planted document's folder"))
            .expect("creating a planted document's folder");
        std::fs::write(&path, format!("See [[{HUB_STEM}]].\n"))
            .expect("writing a planted hub in-link");
    }
}

/// The folder the ignored-subtree bars plant the hub's ignored namesakes
/// under. Its name sorts ahead of `hub-gate` as a suffix key's second
/// segment, so every member stands ahead of the hub in the resolution
/// ladder's order: where a head read stepped past the members its class
/// keeps out, it would step past all of them before it reached the hub.
const IGNORED_HUB_FOLDER: &str = "hub-gate-archive/";

/// The ambiguity-ignore glob the ignored-subtree bars declare.
const IGNORED_HUB_PLACE: &str = "hub-gate-archive/**";

/// How many namesakes of the hub the ignored-subtree bars plant beside
/// `profile`: a quarter of its documents, so the subtree grows with the vault
/// — 75 at `ambiguous`, 500 at `realistic` — and a cost that followed it would
/// count differently across the pair.
fn ignored_hub_members(profile: &norn_fixtures::Profile) -> usize {
    profile.docs / 4
}

/// Declare [`IGNORED_HUB_PLACE`] in `vault`'s schema and plant `count`
/// documents at the hub's stem under it, before anything attaches the vault.
/// The glob keeps every one of them out of the class the bare stem opens, so
/// the in-links name the hub alone once it is written, and nothing while it
/// is not.
fn plant_ignored_hub_members(vault: &attach::Vault, count: usize) {
    std::fs::write(
        vault.path().join(".norn/schema.yaml"),
        format!("version: 1\npaths:\n  ambiguity_ignore: [\"{IGNORED_HUB_PLACE}\"]\n"),
    )
    .expect("declaring the ignored subtree");
    for at in 0..count {
        let path = vault
            .path()
            .join(format!("{IGNORED_HUB_FOLDER}{at:04}/{HUB_STEM}.md"));
        std::fs::create_dir_all(path.parent().expect("a planted document's folder"))
            .expect("creating a planted document's folder");
        std::fs::write(&path, "an archived hub\n").expect("writing an ignored namesake");
    }
}

/// The declaration the store pins over a hub vault, as a write or a judgment
/// is judged under it: the pinned fingerprint, and [`IGNORED_HUB_PLACE`]
/// where the vault declares it.
fn hub_declaration(store: &mut Store, ignoring: bool) -> ContentModel {
    let declared = the_pinned_declaration(store);
    if ignoring {
        declared.declare_ambiguity_ignore(
            norn_wire::Pattern::parse(IGNORED_HUB_PLACE).expect("the ignored subtree's glob"),
        )
    } else {
        declared
    }
}

/// **The hub's stem is the planted neighborhood's alone.** No document the
/// attachment derived beside the planted in-links shares it, but for the
/// ignored members [`plant_ignored_hub_members`] plants under
/// [`IGNORED_HUB_PLACE`].
fn assert_the_hub_stem_is_the_neighborhoods_alone(store: &mut Store) {
    let hub = DocumentPath::new(&hub_path()).expect("a document path");
    let mut sharing = Vec::new();
    attach::for_each_derived_path(store, |path| {
        if path.stem() == hub.stem() && !path.as_str().starts_with(IGNORED_HUB_FOLDER) {
            sharing.push(path.as_str().to_string());
        }
    });
    assert!(
        sharing.is_empty(),
        "the write-work bar's hub targets stem `{}`, and the attachment already derived \
         documents at it: {sharing:?}",
        hub.stem()
    );
}

/// **The write-work bar over a hub's in-links (ADR 0027).** Writing the
/// document a fixed number of other documents already link by its bare stem
/// re-decides exactly their links, at both per-PR scales alike: a hub's write
/// costs its own in-links, never the vault around it.
///
/// [`HUB_IN_LINKS`] documents linking `[[hub-gate-hub]]` are planted beside
/// each profile's generated tree and derived by the same attach heal that
/// derives the rest of it — each holding a broken link until the hub itself
/// is written. That write is then counted directly through the store, the
/// same seam [`one_probe_write`] counts through: it re-decides exactly the
/// planted in-links, resolves the one key their class names once, reads the
/// hub as the one candidate that key names once, discards every one of the
/// broken findings the in-links held, and files none in their place, at
/// `ambiguous` (300 documents) exactly as at `realistic` (2000).
#[test]
#[ignore = "counter-lane case: runs in the ci counter gates job, not the workspace suite"]
fn a_hub_writes_link_health_work_follows_its_in_links_at_both_scales() {
    let small = norn_fixtures::Profile::by_name("ambiguous").expect("the ambiguity profile");
    let large = norn_fixtures::Profile::by_name("realistic").expect("the gate profile");

    let small_counters = one_hub_write("counter-gate-hub-ambiguous", &small, 0);
    let large_counters = one_hub_write("counter-gate-hub-realistic", &large, 0);

    assert_the_hub_write_redecided_its_in_links(&small, &small_counters, &large, &large_counters);
    SizeIndependencePair::new(
        "writing a hub with a fixed number of in-links",
        ScaleObservation::new(&small, small_counters),
        ScaleObservation::new(&large, large_counters),
    )
    .assert_size_independent();
}

/// **The write-work bar beside an ignored subtree (NORN-320).** The bar
/// above, over a vault that also declares [`IGNORED_HUB_PLACE`] and holds
/// [`ignored_hub_members`] namesakes of the hub under it — 75 at `ambiguous`
/// and 500 at `realistic`, every one ahead of the hub in the ladder's order
/// and kept out of the class `[[hub-gate-hub]]` opens. Writing the hub
/// re-decides the same in-links, reads the hub as the one candidate, and
/// takes the same read steps at both scales: the head the re-decision reads
/// seeks the members the class admits and never steps past the ones it keeps
/// out.
#[test]
#[ignore = "counter-lane case: runs in the ci counter gates job, not the workspace suite"]
fn a_hub_writes_link_health_work_follows_its_in_links_beside_an_ignored_subtree() {
    let small = norn_fixtures::Profile::by_name("ambiguous").expect("the ambiguity profile");
    let large = norn_fixtures::Profile::by_name("realistic").expect("the gate profile");

    let small_counters = one_hub_write(
        "counter-gate-hub-ignored-ambiguous",
        &small,
        ignored_hub_members(&small),
    );
    let large_counters = one_hub_write(
        "counter-gate-hub-ignored-realistic",
        &large,
        ignored_hub_members(&large),
    );

    assert_the_hub_write_redecided_its_in_links(&small, &small_counters, &large, &large_counters);
    SizeIndependencePair::new(
        "writing a hub with a fixed number of in-links beside an ignored subtree",
        ScaleObservation::new(&small, small_counters),
        ScaleObservation::new(&large, large_counters),
    )
    .assert_size_independent();
}

/// **The hub's write re-decided exactly its in-links** at both scales: each
/// planted in-link once, under the one key their class names, read once,
/// with the hub its one candidate, every broken finding they held discarded
/// and none filed in their place.
fn assert_the_hub_write_redecided_its_in_links(
    small: &norn_fixtures::Profile,
    small_counters: &CounterSnapshot,
    large: &norn_fixtures::Profile,
    large_counters: &CounterSnapshot,
) {
    for (profile, counters) in [(small, small_counters), (large, large_counters)] {
        for (name, expected) in [
            ("links_redecided", HUB_IN_LINKS as u64),
            ("link_health_keys_resolved", 1),
            ("link_health_candidates_read", 1),
            ("findings_discarded", HUB_IN_LINKS as u64),
            ("findings_written", 0),
        ] {
            assert_eq!(
                counters.get(name),
                expected,
                "writing the hub over `{}` did not read `{name}` as its {HUB_IN_LINKS} planted \
                 in-links name",
                profile.name
            );
        }
        assert!(
            counters.get("read_steps") > 0,
            "writing the hub over `{}` took no read step, so the bar reads no head",
            profile.name
        );
    }
}

/// Attach `profile` with [`HUB_IN_LINKS`] planted beside it, and `ignored`
/// namesakes of the hub under [`IGNORED_HUB_PLACE`] where `ignored` is not
/// zero, write the hub they all name directly through the store, and hand
/// back what the write derived and the read steps it took.
fn one_hub_write(label: &str, profile: &norn_fixtures::Profile, ignored: usize) -> CounterSnapshot {
    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), label).expect("a sandbox");
    let vault = attach::Vault::generate(&sandbox.work_dir().join("attached"), profile.name);
    plant_hub_in_links(&vault);
    if ignored > 0 {
        plant_ignored_hub_members(&vault, ignored);
    }
    {
        let host = vault.host();
        attach::attach_and_wait(&host, vault.name());
    }

    let mut store = vault.store();
    let derived = attach::derived_documents(&mut store);
    assert_eq!(
        derived,
        profile.docs + HUB_IN_LINKS + ignored,
        "`{}` emits {} documents and {HUB_IN_LINKS} in-links and {ignored} ignored namesakes \
         were planted beside them, and the attachment derived {derived}",
        profile.name,
        profile.docs
    );
    assert_the_hub_stem_is_the_neighborhoods_alone(&mut store);

    let declared = hub_declaration(&mut store, ignored > 0);
    let mut request = store.begin_request();
    request
        .apply_increment(
            IncrementProvenance::Derived,
            [Change::Upsert(DocumentFacts::new(
                DocumentPath::new(&hub_path()).expect("a document path"),
                HUB_STEM,
                "the hub\n",
                8,
            ))],
            &[],
            &declared,
        )
        .expect("writing the hub");
    let read_steps = request.read_steps();
    let reading = request.finish();
    assert!(
        !reading.is_all_zero(),
        "writing the hub counted nothing, so the bar over it holds no reading"
    );
    let snapshot: CounterSnapshot = reading
        .readings()
        .chain([("read_steps", read_steps)])
        .collect();
    record_the_counters(
        &format!(
            "writing a hub with {HUB_IN_LINKS} in-links and {ignored} ignored namesakes over `{}`",
            profile.name
        ),
        &snapshot,
    );
    snapshot
}

/// **The resolution change set's bar over a hub's in-links (NORN-297).**
/// Deleting the document [`HUB_IN_LINKS`] others link by its bare stem,
/// leaving their links broken, records exactly those links, and judging them costs the same at both
/// per-PR scales: a plan's change set costs the links it reaches and the
/// candidates they resolve against, never the vault around them. Its twin
/// below holds the same bar beside an ignored subtree that grows with the
/// vault.
///
/// The hub and its planted in-links are derived by the attach heal beside
/// each profile's generated tree. A preview of the delete through the host,
/// saying the links may be left broken, records one entry per in-link. The
/// judgment the preview's planning and an apply's check each run — the
/// store's resolution door, on a snapshot of the attached store, with the
/// hub overlaid as removed — is then counted directly: it judges the twenty
/// in-links, resolves the one key they share once, reads the hub as the one
/// row that key's head holds, runs the same three statements by name, and
/// steps no table or index end to end, at `ambiguous` (300 documents)
/// exactly as at `realistic` (2000).
#[test]
#[ignore = "counter-lane case: runs in the ci counter gates job, not the workspace suite"]
fn a_hub_deletes_resolution_change_set_follows_its_in_links_at_both_scales() {
    let small = norn_fixtures::Profile::by_name("ambiguous").expect("the ambiguity profile");
    let large = norn_fixtures::Profile::by_name("realistic").expect("the gate profile");

    let small_counters = one_hub_delete("counter-gate-hub-delete-ambiguous", &small, 0);
    let large_counters = one_hub_delete("counter-gate-hub-delete-realistic", &large, 0);

    assert_the_hub_delete_judged_its_in_links(&small, &small_counters, &large, &large_counters);
    SizeIndependencePair::new(
        "judging the links a hub's delete reaches",
        ScaleObservation::new(&small, small_counters),
        ScaleObservation::new(&large, large_counters),
    )
    .assert_size_independent();
}

/// **The resolution change set's bar beside an ignored subtree (NORN-320).**
/// The bar above, over a vault that also declares [`IGNORED_HUB_PLACE`] and
/// holds [`ignored_hub_members`] namesakes of the hub under it — 75 at
/// `ambiguous` and 500 at `realistic`, every one ahead of the hub in the
/// ladder's order and kept out of the class `[[hub-gate-hub]]` opens. The
/// hub's delete records the same entries, and judging them reads the hub as
/// the one head row in the same statements and steps at both scales: the
/// head seeks the members the class admits and never steps past the ones it
/// keeps out.
#[test]
#[ignore = "counter-lane case: runs in the ci counter gates job, not the workspace suite"]
fn a_hub_deletes_resolution_change_set_follows_its_in_links_beside_an_ignored_subtree() {
    let small = norn_fixtures::Profile::by_name("ambiguous").expect("the ambiguity profile");
    let large = norn_fixtures::Profile::by_name("realistic").expect("the gate profile");

    let small_counters = one_hub_delete(
        "counter-gate-hub-delete-ignored-ambiguous",
        &small,
        ignored_hub_members(&small),
    );
    let large_counters = one_hub_delete(
        "counter-gate-hub-delete-ignored-realistic",
        &large,
        ignored_hub_members(&large),
    );

    assert_the_hub_delete_judged_its_in_links(&small, &small_counters, &large, &large_counters);
    SizeIndependencePair::new(
        "judging the links a hub's delete reaches beside an ignored subtree",
        ScaleObservation::new(&small, small_counters),
        ScaleObservation::new(&large, large_counters),
    )
    .assert_size_independent();
}

/// **The hub's delete judged exactly its in-links** at both scales: an entry
/// for each, one key resolved once, the hub its one head row, and no table or
/// index stepped end to end.
fn assert_the_hub_delete_judged_its_in_links(
    small: &norn_fixtures::Profile,
    small_counters: &CounterSnapshot,
    large: &norn_fixtures::Profile,
    large_counters: &CounterSnapshot,
) {
    for (profile, counters) in [(small, small_counters), (large, large_counters)] {
        for (name, expected) in [
            ("entries_recorded", HUB_IN_LINKS as u64),
            ("links_evaluated", HUB_IN_LINKS as u64),
            ("keys_resolved", 1),
            ("head_rows", 1),
            ("full_scan_steps", 0),
        ] {
            assert_eq!(
                counters.get(name),
                expected,
                "deleting the hub over `{}` did not read `{name}` as its {HUB_IN_LINKS} planted \
                 in-links name",
                profile.name
            );
        }
    }
}

/// Attach `profile` with the hub and [`HUB_IN_LINKS`] in-links planted beside
/// it, and `ignored` namesakes of the hub under [`IGNORED_HUB_PLACE`] where
/// `ignored` is not zero, preview the hub's delete through the host, and
/// count the store's judgment of the links it reaches.
fn one_hub_delete(
    label: &str,
    profile: &norn_fixtures::Profile,
    ignored: usize,
) -> CounterSnapshot {
    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), label).expect("a sandbox");
    let vault = attach::Vault::generate(&sandbox.work_dir().join("attached"), profile.name);
    plant_hub_in_links(&vault);
    if ignored > 0 {
        plant_ignored_hub_members(&vault, ignored);
    }
    std::fs::write(vault.path().join(hub_path()), "the hub\n").expect("writing the hub");
    let hub = norn_wire::DocumentPath::new(hub_path()).expect("a document path");
    {
        let host = vault.host();
        let _lease = attach::attach_and_wait(&host, vault.name());
        let previewed = host
            .apply(norn_wire::ApplyParams::new(
                norn_wire::ApplyMode::Preview,
                norn_wire::PlanDocument::operations(norn_wire::AuthoredPlan::new(
                    VaultAddress::name(vault.name().clone()),
                    vec![norn_wire::Operation::new(
                        norn_wire::OperationKind::delete_document_breaking_links(hub.clone()),
                    )],
                )),
            ))
            .expect("a preview is answered")
            .wait()
            .expect("the hub's delete previews");
        let norn_wire::ApplyReport::Previewed { plan, .. } = previewed.report else {
            panic!("a preview answered {:?}", previewed.report);
        };
        let entries = plan
            .conditions
            .iter()
            .filter(|condition| {
                matches!(condition, norn_wire::PlanCondition::LinkResolution { .. })
            })
            .count();
        assert_eq!(
            entries, HUB_IN_LINKS,
            "the hub's delete over `{}` recorded {entries} entries",
            profile.name
        );
    }

    let mut store = vault.store();
    assert_eq!(
        attach::derived_documents(&mut store),
        profile.docs + HUB_IN_LINKS + 1 + ignored,
        "`{}` derived other than its documents, the hub, its in-links and {ignored} ignored \
         namesakes",
        profile.name
    );
    let declared = hub_declaration(&mut store, ignored > 0);
    let snapshot = std::sync::Arc::new(store.open_reader().reader.expect("a reader"))
        .try_take()
        .expect("a handle nothing is reading holds its connection")
        .establish()
        .expect("a snapshot");
    let overlay = norn_store::PathOverlay::new().with(
        DocumentPath::new(&hub_path()).expect("a document path"),
        true,
        false,
    );
    let before = snapshot.counters();
    let mut entries = 0u64;
    let work = snapshot
        .resolution_changes(&overlay, &[], &declared, |change| {
            if change.before != change.after {
                entries += 1;
            }
        })
        .expect("judging the hub's in-links");
    let after = snapshot.counters();
    assert_eq!(
        work.ran,
        [
            norn_store::ResolutionStatement::Occupied,
            norn_store::ResolutionStatement::KeyLinks,
            norn_store::ResolutionStatement::Heads,
        ],
        "deleting the hub over `{}` ran other than one occupancy read, one page of the hub \
         key's links and one head read",
        profile.name
    );
    let counters: CounterSnapshot = [
        ("entries_recorded", entries),
        ("links_evaluated", work.links_evaluated),
        ("keys_resolved", work.keys_resolved),
        ("head_rows", work.head_rows),
        (
            "statements_executed",
            after.statements_executed() - before.statements_executed(),
        ),
        ("vm_steps", after.vm_steps() - before.vm_steps()),
        (
            "full_scan_steps",
            after.full_scan_steps() - before.full_scan_steps(),
        ),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_string(), value))
    .collect();
    record_the_counters(
        &format!(
            "judging the links a hub's delete reaches, {HUB_IN_LINKS} in-links and {ignored} \
             ignored namesakes over `{}`",
            profile.name
        ),
        &counters,
    );
    counters
}

/// The stem the move-cascade bar moves the hub to, in a folder of its own,
/// so every planted in-link stops naming it and needs a rewrite. No generated
/// document shares it, asserted rather than assumed.
const MOVED_HUB_STEM: &str = "hub-gate-moved-hub";

/// Where the move-cascade bar moves the hub.
fn moved_hub_path() -> String {
    format!("hub-gate-moved/{MOVED_HUB_STEM}.md")
}

/// **The move cascade's bar over a hub's in-links (NORN-297).** Moving the
/// document [`HUB_IN_LINKS`] others link by its bare stem to a new stem in a
/// new folder plans one rewrite per in-link, and generating that cascade
/// costs the same at both per-PR scales: a move's cascade costs the links it
/// reaches, the spellings it probes and the documents it rewrites, never the
/// vault around them. The profiles declare no ignore set: the head read
/// seeks past no member of its class the ignore set keeps out (the hub write
/// and delete bars' ignored-subtree twins hold that), but the cascade's
/// backlink pass and the naming of its targets still read a class through the
/// ignore filter row by row (NORN-339).
///
/// A preview of the move through the host plans a cascade of one rewrite
/// per in-link and records one written entry per rewritten link, and the
/// calling thread, where a preview plans and judges its plan, reads the same
/// documents through `norn-fs` at both scales: two counted opens per planned
/// file, the hub and its twenty holders each opened once while planning and
/// once while the applier judges the plan. The kernel's staging look, which
/// the preview runs as an apply's staging would, reads each target that
/// already stands once more, counted apart as a target read: the twenty
/// holders it replaces and the hub's old name it removes, and not the new
/// name, which holds no file to read.
/// What the preview's judgments on the store's resolution door cost is
/// read off the host's read account, as the preview really ran them — the
/// cascade's backlink pass and spelling probe, the planning's change set and
/// the applier's computation of it again: the same judgments, links
/// evaluated, keys resolved, head rows, statements and steps, and no table or
/// index stepped end to end, at `ambiguous` (300 documents) exactly as at
/// `realistic` (2000).
#[test]
#[ignore = "counter-lane case: runs in the ci counter gates job, not the workspace suite"]
fn a_hub_moves_cascade_follows_its_in_links_at_both_scales() {
    let small = norn_fixtures::Profile::by_name("ambiguous").expect("the ambiguity profile");
    let large = norn_fixtures::Profile::by_name("realistic").expect("the gate profile");

    let small_counters = one_hub_move("counter-gate-hub-move-ambiguous", &small);
    let large_counters = one_hub_move("counter-gate-hub-move-realistic", &large);

    for (profile, counters) in [(&small, &small_counters), (&large, &large_counters)] {
        for (name, expected) in [
            ("rewrites_planned", HUB_IN_LINKS as u64),
            ("entries_written", HUB_IN_LINKS as u64),
            ("document_opens", 2 * (HUB_IN_LINKS as u64 + 1)),
            ("target_reads", HUB_IN_LINKS as u64 + 1),
            ("judgments", 4),
            ("full_scan_steps", 0),
        ] {
            assert_eq!(
                counters.get(name),
                expected,
                "moving the hub over `{}` did not read `{name}` as its {HUB_IN_LINKS} planted \
                 in-links name: {counters:?}",
                profile.name
            );
        }
    }

    SizeIndependencePair::new(
        "generating the cascade a hub's move plans",
        ScaleObservation::new(&small, small_counters),
        ScaleObservation::new(&large, large_counters),
    )
    .assert_size_independent();
}

/// Attach `profile` with the hub and [`HUB_IN_LINKS`] in-links planted beside
/// it, preview the hub's move through the host in a read window on this
/// thread, and read what the preview's link judgments cost off the host's
/// read account.
fn one_hub_move(label: &str, profile: &norn_fixtures::Profile) -> CounterSnapshot {
    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), label).expect("a sandbox");
    let vault = attach::Vault::generate(&sandbox.work_dir().join("attached"), profile.name);
    plant_hub_in_links(&vault);
    std::fs::write(vault.path().join(hub_path()), "the hub\n").expect("writing the hub");
    let hub = norn_wire::DocumentPath::new(hub_path()).expect("a document path");
    let moved = norn_wire::DocumentPath::new(moved_hub_path()).expect("a document path");
    let (rewrites, entries, read, judged) = {
        let host = vault.host();
        let _lease = attach::attach_and_wait(&host, vault.name());
        let account = host.read_evidence();
        let window = ReadWindow::open();
        let previewed = host
            .move_path(norn_wire::MoveParams::new(
                VaultAddress::name(vault.name().clone()),
                norn_wire::ApplyMode::Preview,
                norn_wire::MoveSubject::document(hub.clone(), moved.clone()),
            ))
            .expect("a preview is answered")
            .wait()
            .expect("the hub's move previews");
        let read = window.finish();
        let judged = host.read_evidence().since(account).preview_link_judgments;
        let norn_wire::ApplyReport::Previewed { plan, .. } = previewed.report else {
            panic!("a preview answered {:?}", previewed.report);
        };
        let rewrites = plan.operations[0].cascade.len() as u64;
        let entries = plan
            .conditions
            .iter()
            .filter(|condition| {
                matches!(
                    condition,
                    norn_wire::PlanCondition::LinkResolution { link, after, .. }
                        if link.address == MOVED_HUB_STEM
                            && *after == norn_wire::Resolves::one(moved.clone())
                )
            })
            .count() as u64;
        (rewrites, entries, read, judged)
    };

    let mut store = vault.store();
    let mut sharing = Vec::new();
    attach::for_each_derived_path(&mut store, |path| {
        if path.stem() == MOVED_HUB_STEM {
            sharing.push(path.as_str().to_string());
        }
    });
    assert!(
        sharing.is_empty(),
        "the move-cascade bar moves the hub to stem `{MOVED_HUB_STEM}`, and the attachment \
         already derived documents at it: {sharing:?}"
    );
    let counters: CounterSnapshot = [
        ("rewrites_planned", rewrites),
        ("entries_written", entries),
        ("document_opens", read.document_opens),
        ("target_reads", read.target_reads),
        ("stats", read.stats),
        ("walk_dirents", read.walk_dirents),
        ("judgments", judged.judgments),
        ("links_evaluated", judged.links_evaluated),
        ("keys_resolved", judged.keys_resolved),
        ("head_rows", judged.head_rows),
        ("statements_executed", judged.statements_executed),
        ("vm_steps", judged.vm_steps),
        ("full_scan_steps", judged.full_scan_steps),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_string(), value))
    .collect();
    record_the_counters(
        &format!(
            "generating the cascade a hub's move plans, {HUB_IN_LINKS} in-links over `{}`",
            profile.name
        ),
        &counters,
    );
    counters
}

/// The stem of a document no link names, beside the hub: a delete of it has
/// no backlink to judge. No generated document shares it, asserted rather
/// than assumed.
const LONELY_STEM: &str = "hub-gate-lonely";

/// **A delete's backlink judgment over a hub's in-links (NORN-297).** The
/// Layer 3 mass-delete cost limit binds a delete as it binds a move: what a
/// delete costs to plan and judge is the links naming its document, never the
/// vault around them. Three previews through [`norn_host::Host::delete`] run
/// at both per-PR scales, each read off the host's read account as the
/// preview really ran it:
///
/// - **the hub's delete saying neither flag** is refused, naming each of the
///   [`HUB_IN_LINKS`] holders once and the as many links, after one judgment
///   — the backlink pass that leaves it unresolved; nothing is left to judge
///   after it;
/// - **the hub's delete leaving its links broken** records an entry and
///   advises left broken for each in-link, judged twice — planning's change
///   set and the applier's computation of it again — with no backlink pass,
///   since nothing is forbidden or rewritten;
/// - **a delete of a document no link names** judges no link, three times —
///   the backlink pass, the change set and the applier's check.
///
/// Each costs the same judgments, links evaluated, keys resolved, head rows,
/// statements and steps, and steps no table or index end to end, at
/// `ambiguous` (300 documents) exactly as at `realistic` (2000). The
/// profiles declare no ignore set: the head read seeks past no member of its
/// class the ignore set keeps out (the hub write and delete bars'
/// ignored-subtree twins hold that), but the backlink pass still reads a
/// class through the ignore filter row by row (NORN-339).
#[test]
#[ignore = "counter-lane case: runs in the ci counter gates job, not the workspace suite"]
fn a_hub_deletes_backlink_judgment_follows_its_in_links_at_both_scales() {
    let small = norn_fixtures::Profile::by_name("ambiguous").expect("the ambiguity profile");
    let large = norn_fixtures::Profile::by_name("realistic").expect("the gate profile");

    let small_counters = hub_deletes("counter-gate-hub-deletes-ambiguous", &small);
    let large_counters = hub_deletes("counter-gate-hub-deletes-realistic", &large);

    for (profile, counters) in [(&small, &small_counters), (&large, &large_counters)] {
        for (name, expected) in [
            ("refused_holders", HUB_IN_LINKS as u64),
            ("refused_total", HUB_IN_LINKS as u64),
            ("refused_judgments", 1),
            ("refused_links_evaluated", HUB_IN_LINKS as u64),
            ("refused_full_scan_steps", 0),
            ("broken_entries", HUB_IN_LINKS as u64),
            ("broken_advised", HUB_IN_LINKS as u64),
            ("broken_judgments", 2),
            ("broken_links_evaluated", 2 * HUB_IN_LINKS as u64),
            ("broken_full_scan_steps", 0),
            ("lonely_entries", 0),
            ("lonely_judgments", 3),
            ("lonely_links_evaluated", 0),
            ("lonely_full_scan_steps", 0),
        ] {
            assert_eq!(
                counters.get(name),
                expected,
                "the hub's deletes over `{}` did not read `{name}` as its {HUB_IN_LINKS} planted \
                 in-links name: {counters:?}",
                profile.name
            );
        }
    }

    SizeIndependencePair::new(
        "judging the backlinks of a hub's delete",
        ScaleObservation::new(&small, small_counters),
        ScaleObservation::new(&large, large_counters),
    )
    .assert_size_independent();
}

/// Attach `profile` with the hub, [`HUB_IN_LINKS`] in-links and a document
/// no link names planted beside it, preview the three deletes through the
/// host, and read what each preview's link judgments cost off the host's
/// read account, each counter named for its preview.
fn hub_deletes(label: &str, profile: &norn_fixtures::Profile) -> CounterSnapshot {
    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), label).expect("a sandbox");
    let vault = attach::Vault::generate(&sandbox.work_dir().join("attached"), profile.name);
    plant_hub_in_links(&vault);
    std::fs::write(vault.path().join(hub_path()), "the hub\n").expect("writing the hub");
    let lonely_path = format!("hub-gate/{LONELY_STEM}.md");
    std::fs::write(vault.path().join(&lonely_path), "no link names me\n")
        .expect("writing the lonely document");
    let hub = norn_wire::DocumentPath::new(hub_path()).expect("a document path");
    let lonely = norn_wire::DocumentPath::new(&lonely_path).expect("a document path");
    let address = VaultAddress::name(vault.name().clone());
    let mut counters: Vec<(String, u64)> = Vec::new();
    {
        let host = vault.host();
        let _lease = attach::attach_and_wait(&host, vault.name());
        let preview = |params: norn_wire::DeleteParams| {
            let account = host.read_evidence();
            let answered = host.delete(params).expect("a preview is answered").wait();
            let judged = host.read_evidence().since(account).preview_link_judgments;
            (answered, judged)
        };

        let (refused, judged) = preview(norn_wire::DeleteParams::new(
            address.clone(),
            norn_wire::ApplyMode::Preview,
            hub.clone(),
        ));
        let refused = refused.expect_err("the hub's plain delete is refused");
        let norn_wire::ErrorDetail::PlanRefused { unresolved, .. } = refused.detail() else {
            panic!("the hub's plain delete answered {refused:?}");
        };
        let [left] = unresolved.as_slice() else {
            panic!("one operation is unresolved: {unresolved:?}");
        };
        let norn_wire::UnresolvedReason::HasBacklinks { holders, total, .. } = &left.reason else {
            panic!("the hub's plain delete is unresolved for {:?}", left.reason);
        };
        judged_as(&mut counters, "refused", judged);
        counters.push(("refused_holders".to_string(), holders.len() as u64));
        counters.push(("refused_total".to_string(), *total));

        let (broken, judged) = preview(
            norn_wire::DeleteParams::new(address.clone(), norn_wire::ApplyMode::Preview, hub)
                .breaking_links(),
        );
        let broken = broken.expect("the hub's delete leaving its links broken previews");
        let norn_wire::ApplyReport::Previewed { plan, forecast, .. } = broken.report else {
            panic!("a preview answered {:?}", broken.report);
        };
        judged_as(&mut counters, "broken", judged);
        counters.push(("broken_entries".to_string(), link_entries(&plan)));
        counters.push(("broken_advised".to_string(), forecast.links.len() as u64));

        let (unlinked, judged) = preview(norn_wire::DeleteParams::new(
            address,
            norn_wire::ApplyMode::Preview,
            lonely,
        ));
        let unlinked = unlinked.expect("a delete no link refuses previews");
        let norn_wire::ApplyReport::Previewed { plan, .. } = unlinked.report else {
            panic!("a preview answered {:?}", unlinked.report);
        };
        judged_as(&mut counters, "lonely", judged);
        counters.push(("lonely_entries".to_string(), link_entries(&plan)));
    }

    // The hub and the lonely document are each the one document of their
    // stem, so the hub's in-links name it alone and nothing names the other.
    let mut store = vault.store();
    for at in [hub_path(), lonely_path] {
        let stem = DocumentPath::new(&at)
            .expect("a document path")
            .stem()
            .to_string();
        let mut sharing = Vec::new();
        attach::for_each_derived_path(&mut store, |path| {
            if path.stem() == stem && path.as_str() != at {
                sharing.push(path.as_str().to_string());
            }
        });
        assert!(
            sharing.is_empty(),
            "the delete bar's `{at}` has stem `{stem}`, and the attachment derived other \
             documents at it: {sharing:?}"
        );
    }
    let counters: CounterSnapshot = counters.into_iter().collect();
    record_the_counters(
        &format!(
            "judging the backlinks of a hub's delete, {HUB_IN_LINKS} in-links over `{}`",
            profile.name
        ),
        &counters,
    );
    counters
}

/// `judged`'s counts added to `counters`, each named for the preview
/// `prefix` names, or by its own name alone where `prefix` is empty.
fn judged_as(counters: &mut Vec<(String, u64)>, prefix: &str, judged: norn_host::LinkJudgmentCost) {
    for (name, value) in [
        ("judgments", judged.judgments),
        ("links_evaluated", judged.links_evaluated),
        ("keys_resolved", judged.keys_resolved),
        ("head_rows", judged.head_rows),
        ("statements_executed", judged.statements_executed),
        ("vm_steps", judged.vm_steps),
        ("full_scan_steps", judged.full_scan_steps),
    ] {
        counters.push((
            if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{prefix}_{name}")
            },
            value,
        ));
    }
}

/// How many link-resolution entries `plan` records.
fn link_entries(plan: &norn_wire::ResolvedPlan) -> u64 {
    plan.conditions
        .iter()
        .filter(|condition| matches!(condition, norn_wire::PlanCondition::LinkResolution { .. }))
        .count() as u64
}

/// The stem of the document the wikilink-rewrite bar retargets the hub's
/// in-links to. No generated document shares it, asserted rather than
/// assumed.
const RETARGET_STEM: &str = "hub-gate-retarget";

/// **A wikilink rewrite's cascade over a hub's in-links (NORN-297).** The
/// Layer 3 mass-delete cost limit binds a vault-wide rewrite as it binds a
/// move and a delete: retargeting every wikilink naming the hub costs the
/// links naming it, never the vault around them. A preview of the rewrite
/// through [`norn_host::Host::rewrite_wikilink`] plans one rewrite per
/// in-link, records one written entry per rewritten link and advises
/// nothing, and what its judgments on the store's resolution door cost is
/// read off the host's read account as the preview really ran them — ten
/// judgments: naming `old` and `new`, the cascade's pass over the links
/// naming the hub and its spelling probe, then the planning's change set and
/// the applier's computation of it again, each naming both ends again: the
/// same judgments, links evaluated, keys resolved, head rows, statements and
/// steps, and no table or index stepped end to end, at `ambiguous` (300
/// documents) exactly as at `realistic` (2000). The profiles declare no
/// ignore set: the head read seeks past no member of its class the ignore set
/// keeps out (the hub write and delete bars' ignored-subtree twins hold
/// that), but naming `old` and `new` and the cascade's backlink pass still
/// read a class through the ignore filter row by row (NORN-339).
#[test]
#[ignore = "counter-lane case: runs in the ci counter gates job, not the workspace suite"]
fn a_hubs_wikilink_rewrite_follows_its_in_links_at_both_scales() {
    let small = norn_fixtures::Profile::by_name("ambiguous").expect("the ambiguity profile");
    let large = norn_fixtures::Profile::by_name("realistic").expect("the gate profile");

    let small_counters = hub_rewrite("counter-gate-hub-rewrite-ambiguous", &small);
    let large_counters = hub_rewrite("counter-gate-hub-rewrite-realistic", &large);

    for (profile, counters) in [(&small, &small_counters), (&large, &large_counters)] {
        for (name, expected) in [
            ("rewrites", HUB_IN_LINKS as u64),
            ("entries", HUB_IN_LINKS as u64),
            ("advised", 0),
            ("judgments", 10),
            ("full_scan_steps", 0),
        ] {
            assert_eq!(
                counters.get(name),
                expected,
                "the hub's wikilink rewrite over `{}` did not read `{name}` as its \
                 {HUB_IN_LINKS} planted in-links name: {counters:?}",
                profile.name
            );
        }
    }

    SizeIndependencePair::new(
        "retargeting the wikilinks naming a hub",
        ScaleObservation::new(&small, small_counters),
        ScaleObservation::new(&large, large_counters),
    )
    .assert_size_independent();
}

/// Attach `profile` with the hub, [`HUB_IN_LINKS`] in-links and the
/// document they are retargeted to planted beside it, preview the rewrite
/// through the host, and read what the preview's link judgments cost off the
/// host's read account.
fn hub_rewrite(label: &str, profile: &norn_fixtures::Profile) -> CounterSnapshot {
    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), label).expect("a sandbox");
    let vault = attach::Vault::generate(&sandbox.work_dir().join("attached"), profile.name);
    plant_hub_in_links(&vault);
    std::fs::write(vault.path().join(hub_path()), "the hub\n").expect("writing the hub");
    let retarget_path = format!("hub-gate/{RETARGET_STEM}.md");
    std::fs::write(vault.path().join(&retarget_path), "the retarget\n")
        .expect("writing the retarget");
    let target = |text: &str| norn_wire::ResolutionTarget::new(text).expect("a target");
    let mut counters: Vec<(String, u64)> = Vec::new();
    {
        let host = vault.host();
        let _lease = attach::attach_and_wait(&host, vault.name());
        let account = host.read_evidence();
        let previewed = host
            .rewrite_wikilink(norn_wire::RewriteWikilinkParams::new(
                VaultAddress::name(vault.name().clone()),
                norn_wire::ApplyMode::Preview,
                target(HUB_STEM),
                target(RETARGET_STEM),
            ))
            .expect("a preview is answered")
            .wait()
            .expect("the hub's wikilink rewrite previews");
        let judged = host.read_evidence().since(account).preview_link_judgments;
        let norn_wire::ApplyReport::Previewed { plan, forecast, .. } = previewed.report else {
            panic!("a preview answered {:?}", previewed.report);
        };
        judged_as(&mut counters, "", judged);
        counters.push((
            "rewrites".to_string(),
            plan.operations
                .iter()
                .map(|operation| operation.cascade.len() as u64)
                .sum(),
        ));
        counters.push(("entries".to_string(), link_entries(&plan)));
        counters.push(("advised".to_string(), forecast.links.len() as u64));
    }

    // The hub and the retarget are each the one document of their stem, so
    // the hub's in-links name it alone and the retarget's name it alone.
    let mut store = vault.store();
    for at in [hub_path(), retarget_path] {
        let stem = DocumentPath::new(&at)
            .expect("a document path")
            .stem()
            .to_string();
        let mut sharing = Vec::new();
        attach::for_each_derived_path(&mut store, |path| {
            if path.stem() == stem && path.as_str() != at {
                sharing.push(path.as_str().to_string());
            }
        });
        assert!(
            sharing.is_empty(),
            "the rewrite bar's `{at}` has stem `{stem}`, and the attachment derived other \
             documents at it: {sharing:?}"
        );
    }
    let counters: CounterSnapshot = counters.into_iter().collect();
    record_the_counters(
        &format!(
            "retargeting the wikilinks naming a hub, {HUB_IN_LINKS} in-links over `{}`",
            profile.name
        ),
        &counters,
    );
    counters
}

/// The document the write-through bar's `set` writes, beside the hub
/// neighborhood: a frontmatter field and a heading, and no link. No
/// generated document shares its stem, asserted rather than assumed.
const SET_SUBJECT_PATH: &str = "hub-gate/apply-gate-subject.md";

/// Where the write-through bar's hub move takes the hub: a new stem, so every
/// in-link stops naming it and the cascade rewrites all of them, in the
/// folder the hub already stands in, so the move makes and empties no folder
/// and what the watcher reports back is the applier's own writes alone.
fn moved_in_place_hub_path() -> String {
    format!("hub-gate/{MOVED_HUB_STEM}.md")
}

/// **Write-through over the composed post-state, through the host's own
/// verbs.** Four applies run one after another over one attachment at each
/// per-PR scale, each through the verb a client's request reaches and each
/// read off the host's account around it, the apply job's own work on its
/// worker thread:
///
/// - **a `set` by path** replaces one document's frontmatter;
/// - **the hub's move** to a new stem replaces each of its [`HUB_IN_LINKS`]
///   holders with its link rewritten, creates the hub at its new name and
///   removes it from its old one — a move carrying the hub byte for byte,
///   whose create the write kernel stages as a copy of the hub's old file,
///   so that file's fate is copied away
///   ([`baselines::APPLY_TARGET_READS_PER_COPIED_AWAY_TARGET`]): one target
///   read more than a removed file's, for the copy, and no read of the hub's
///   content before the changeset's read-back of its new name;
/// - **a delete of a document no link names** removes one document;
/// - **the moved hub's delete leaving its links broken** removes one
///   document, and its changeset re-decides the twenty links that named it.
///
/// Each apply reads every file it touches exactly the authored number of
/// times for what it does to that file, by each protocol that reads it —
/// document reads ([`baselines::APPLY_DOCUMENT_READS_PER_REPLACED_TARGET`]
/// and its siblings), the write kernel's target reads
/// ([`baselines::APPLY_TARGET_READS_PER_REPLACED_TARGET`] and its siblings)
/// — each staged shadow once
/// ([`baselines::APPLY_SHADOW_READS_PER_WRITTEN_TARGET`]), and no other
/// file: each protocol's count is its budget summed over the plan's own
/// transitions, and the recording of which file each read was of names only
/// the plan's files, each at its own budget. Every read is counted by the
/// act that hashes what it read, so a file hashed twice is two reads and
/// the hash clause is held by the same counts. Each apply commits one
/// changeset whose work is its touched set's — a document derived and
/// upserted per written target, a death and a tombstone per removed one,
/// the links reaching them re-decided and nothing else — opens at most one
/// snapshot, steps no table or index end to end on it, and costs the same
/// in every count at `ambiguous` (300 documents) as at `realistic` (2000).
#[test]
#[ignore = "counter-lane case: runs in the ci counter gates job, not the workspace suite"]
fn an_apply_reads_each_file_it_touches_a_fixed_number_of_times_at_both_scales() {
    the_hosts_account_is_readable();
    let small = norn_fixtures::Profile::by_name("ambiguous").expect("the ambiguity profile");
    let large = norn_fixtures::Profile::by_name("realistic").expect("the gate profile");

    let small_counters = applies_through_the_host("counter-gate-applies-ambiguous", &small);
    let large_counters = applies_through_the_host("counter-gate-applies-realistic", &large);

    for (profile, counters) in [(&small, &small_counters), (&large, &large_counters)] {
        for (name, expected) in [
            ("set_replaced", 1),
            ("set_links_redecided", 0),
            ("set_snapshots_opened", 0),
            ("move_replaced", HUB_IN_LINKS as u64),
            ("move_created", 1),
            ("move_removed", 1),
            ("move_links_redecided", HUB_IN_LINKS as u64),
            ("move_link_health_keys_resolved", 1),
            ("move_link_health_candidates_read", 1),
            ("move_findings_written", 0),
            ("lonely_removed", 1),
            ("lonely_links_redecided", 0),
            ("hub_removed", 1),
            ("hub_links_redecided", HUB_IN_LINKS as u64),
            ("hub_findings_written", HUB_IN_LINKS as u64),
        ] {
            assert_eq!(
                counters.get(name),
                expected,
                "the applies over `{}` did not read `{name}` as their planted neighborhood \
                 names: {counters:?}",
                profile.name
            );
        }
    }

    SizeIndependencePair::new(
        "applying a set, a hub's move and two deletes through the host",
        ScaleObservation::new(&small, small_counters),
        ScaleObservation::new(&large, large_counters),
    )
    .assert_size_independent();
}

/// Attach `profile` with the hub, its [`HUB_IN_LINKS`] in-links, a document
/// no link names and [`SET_SUBJECT_PATH`] planted beside it, apply the
/// write-through bar's four writes through the host one after another, hold
/// each to the write-through bar, and hand back every count each spent,
/// named for its apply. The planted paths sort among the generated vault's at
/// both scales ([`the_planted_keys_sort_among_the_generated`]), asserted
/// rather than assumed.
fn applies_through_the_host(label: &str, profile: &norn_fixtures::Profile) -> CounterSnapshot {
    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), label).expect("a sandbox");
    let vault = attach::Vault::generate(&sandbox.work_dir().join("attached"), profile.name);
    plant_hub_in_links(&vault);
    std::fs::write(vault.path().join(hub_path()), "the hub\n").expect("writing the hub");
    let lonely_path = format!("hub-gate/{LONELY_STEM}.md");
    std::fs::write(vault.path().join(&lonely_path), "no link names me\n")
        .expect("writing the lonely document");
    std::fs::write(
        vault.path().join(SET_SUBJECT_PATH),
        "---\nstatus: draft\n---\n# Subject\n",
    )
    .expect("writing the set's subject");
    let document = |at: &str| norn_wire::DocumentPath::new(at).expect("a document path");
    let address = VaultAddress::name(vault.name().clone());
    let mut counters: Vec<(String, u64)> = Vec::new();
    {
        let host = vault.host();
        let _lease = attach::attach_and_wait(&host, vault.name());

        let set = one_apply(profile, "a set by path", &host, vault.path(), || {
            host.set(norn_wire::SetParams::new(
                address.clone(),
                norn_wire::ApplyMode::Apply,
                norn_wire::WriteTarget::path(document(SET_SUBJECT_PATH)),
                vec![norn_wire::FieldChange::set(
                    "status",
                    norn_wire::AuthoredValue::string("done"),
                )],
            ))
        });
        let moved = one_apply(profile, "the hub's move", &host, vault.path(), || {
            host.move_path(norn_wire::MoveParams::new(
                address.clone(),
                norn_wire::ApplyMode::Apply,
                norn_wire::MoveSubject::document(
                    document(&hub_path()),
                    document(&moved_in_place_hub_path()),
                ),
            ))
        });
        let lonely = one_apply(
            profile,
            "a delete no link refuses",
            &host,
            vault.path(),
            || {
                host.delete(norn_wire::DeleteParams::new(
                    address.clone(),
                    norn_wire::ApplyMode::Apply,
                    document(&lonely_path),
                ))
            },
        );
        let hub = one_apply(
            profile,
            "the moved hub's delete",
            &host,
            vault.path(),
            || {
                host.delete(
                    norn_wire::DeleteParams::new(
                        address.clone(),
                        norn_wire::ApplyMode::Apply,
                        document(&moved_in_place_hub_path()),
                    )
                    .breaking_links(),
                )
            },
        );
        for (prefix, spent) in [
            ("set", set),
            ("move", moved),
            ("lonely", lonely),
            ("hub", hub),
        ] {
            counters.extend(
                spent
                    .names()
                    .map(|name| (format!("{prefix}_{name}"), spent.get(name))),
            );
        }
    }

    // Every stem the applies wrote, moved or removed is the planted
    // neighborhood's alone: the hub went to its new stem and both it and the
    // lonely document were deleted, so no document the attachment derived
    // may stand at any of the three, and the subject stands alone at its own.
    let mut store = vault.store();
    let mut sharing = Vec::new();
    let subject = DocumentPath::new(SET_SUBJECT_PATH).expect("a document path");
    attach::for_each_derived_path(&mut store, |path| {
        let stem = path.stem();
        if [HUB_STEM, MOVED_HUB_STEM, LONELY_STEM].contains(&stem)
            || (stem == subject.stem() && path != &subject)
        {
            sharing.push(path.as_str().to_string());
        }
    });
    assert!(
        sharing.is_empty(),
        "the write-through bar's stems are the planted neighborhood's, and the attachment \
         derived other documents at them: {sharing:?}"
    );
    let planted: Vec<String> = (0..HUB_IN_LINKS)
        .map(|at| format!("hub-gate/in-links/{at:04}.md"))
        .chain([
            hub_path(),
            moved_in_place_hub_path(),
            lonely_path,
            SET_SUBJECT_PATH.to_owned(),
        ])
        .collect();
    the_planted_keys_sort_among_the_generated(
        &mut store,
        profile,
        "the write-through bar's hub neighborhood",
        &planted,
    );
    let counters: CounterSnapshot = counters.into_iter().collect();
    record_the_counters(
        &format!(
            "applying a set, a hub's move and two deletes through the host over `{}`",
            profile.name
        ),
        &counters,
    );
    counters
}

/// The documents a `set --where` writes at both scales: as many at 300
/// documents as at 2000, so what its selector and its writes cost is the
/// matches' own.
const WHERE_MATCHES: usize = 10;

/// The frontmatter key the `set --where` pair's documents carry, and no
/// generated document does, asserted rather than assumed.
const WHERE_KEY: &str = "apply_gate_where";

/// The stem every `set --where` document opens with, so the classes its
/// changeset names are the planted documents' alone: a generated document
/// at one of them would put a match in a populated class at one scale and
/// not the other. Asserted rather than assumed.
const WHERE_STEM: &str = "apply-gate-where";

/// The folder the `set --where` documents stand in. It sorts, as their
/// stems do, among the generated vault's own names at both scales rather than
/// past all of them at one: a seek for a key past every key an index holds
/// ends at the index's end in fewer steps than one that lands on a row it
/// then passes over, so a name sorting last at one scale and not at the other
/// would read a step apart per key for no difference in work. Asserted rather
/// than assumed, at both scales
/// ([`the_planted_keys_sort_among_the_generated`]).
const WHERE_FOLDER: &str = "apply-gate";

/// **A `set --where` costs its matches, never the vault around them.** The
/// same [`WHERE_MATCHES`] documents carry [`WHERE_KEY`] beside each per-PR
/// profile, with one more in their folder carrying another value of it, and
/// a `set` whose `where` names the value they share is applied through the
/// host. Its selector is matched on the apply job's one snapshot, through
/// the find builder: what the snapshot ran — the selector's statements and
/// the steps they took, beside the applier's judgment of the plan's links on
/// the same snapshot — is the same at `ambiguous` (300 documents) as at
/// `realistic` (2000), and none of it stepped a table or an index end to end.
/// The apply writes through exactly the matches: the reads it took are a
/// replaced target's budgets per match
/// ([`baselines::APPLY_DOCUMENT_READS_PER_REPLACED_TARGET`],
/// [`baselines::APPLY_TARGET_READS_PER_REPLACED_TARGET`]) and a shadow read
/// each, of the matches' files alone — planning's reads of the documents the
/// selector matched among them — in one changeset upserting the matches
/// alone.
#[test]
#[ignore = "counter-lane case: runs in the ci counter gates job, not the workspace suite"]
fn a_where_apply_costs_the_same_at_both_scales() {
    the_hosts_account_is_readable();
    let small = norn_fixtures::Profile::by_name("ambiguous").expect("the ambiguity profile");
    let large = norn_fixtures::Profile::by_name("realistic").expect("the gate profile");

    let small_counters = one_set_where("counter-gate-set-where-ambiguous", &small);
    let large_counters = one_set_where("counter-gate-set-where-realistic", &large);

    for (profile, counters) in [(&small, &small_counters), (&large, &large_counters)] {
        for (name, expected) in [
            ("replaced", WHERE_MATCHES as u64),
            ("snapshots_opened", 1),
            ("links_redecided", 0),
        ] {
            assert_eq!(
                counters.get(name),
                expected,
                "the set --where over `{}` did not read `{name}` as its {WHERE_MATCHES} matches \
                 name: {counters:?}",
                profile.name
            );
        }
        // The statement that establishes the snapshot is one of its
        // statements, so a selector that ran is a second.
        assert!(
            counters.get("statements") > 1 && counters.get("vm_steps") > 0,
            "the set --where over `{}` ran nothing on its snapshot past the statement that \
             establishes it, so the bar reads no selector: {counters:?}",
            profile.name
        );
    }

    SizeIndependencePair::new(
        "applying a set --where matching the same documents",
        ScaleObservation::new(&small, small_counters),
        ScaleObservation::new(&large, large_counters),
    )
    .assert_size_independent();
}

/// Attach `profile` with [`WHERE_MATCHES`] documents carrying [`WHERE_KEY`]
/// planted beside it, apply the `set --where` naming them through the host,
/// hold it to the write-through bar, and hand back what it spent.
fn one_set_where(label: &str, profile: &norn_fixtures::Profile) -> CounterSnapshot {
    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), label).expect("a sandbox");
    let vault = attach::Vault::generate(&sandbox.work_dir().join("attached"), profile.name);
    let folder = vault.path().join(WHERE_FOLDER);
    std::fs::create_dir_all(&folder).expect("creating the matches' folder");
    for at in 0..WHERE_MATCHES {
        std::fs::write(
            folder.join(format!("{WHERE_STEM}-{at:02}.md")),
            format!("---\n{WHERE_KEY}: flip\n---\n# Match {at}\n"),
        )
        .expect("writing a match");
    }
    std::fs::write(
        folder.join(format!("{WHERE_STEM}-kept.md")),
        format!("---\n{WHERE_KEY}: keep\n---\n# Kept\n"),
    )
    .expect("writing the document the predicate passes over");
    let spent = {
        let host = vault.host();
        let _lease = attach::attach_and_wait(&host, vault.name());
        one_apply(profile, "a set --where", &host, vault.path(), || {
            host.set(norn_wire::SetParams::new(
                VaultAddress::name(vault.name().clone()),
                norn_wire::ApplyMode::Apply,
                norn_wire::WriteTarget::matching([Predicate::equal_to(WHERE_KEY, "flip")]),
                vec![norn_wire::FieldChange::set(
                    WHERE_KEY,
                    norn_wire::AuthoredValue::string("flipped"),
                )],
            ))
        })
    };

    // The matches are the planted ones alone: the store holds the key, and
    // the stems, on exactly the planted documents — the matches, now
    // flipped, and the one kept.
    let mut store = vault.store();
    let mut carrying = Vec::new();
    attach::for_each_derived_document(&mut store, |document| {
        let keyed = document
            .frontmatter
            .as_ref()
            .is_some_and(|projection| projection.contains(&format!("\"{WHERE_KEY}\"")));
        if keyed || document.path.stem().starts_with(WHERE_STEM) {
            carrying.push(document.path.as_str().to_string());
        }
    });
    assert_eq!(
        carrying.len(),
        WHERE_MATCHES + 1,
        "the set --where's key and stems are the planted documents' alone, and the attachment \
         derived them at {carrying:?}"
    );
    let planted: Vec<String> = (0..WHERE_MATCHES)
        .map(|at| format!("{WHERE_FOLDER}/{WHERE_STEM}-{at:02}.md"))
        .chain([format!("{WHERE_FOLDER}/{WHERE_STEM}-kept.md")])
        .collect();
    the_planted_keys_sort_among_the_generated(
        &mut store,
        profile,
        "the set --where's planted matches",
        &planted,
    );
    record_the_counters(
        &format!(
            "applying a set --where matching {WHERE_MATCHES} documents over `{}`",
            profile.name
        ),
        &spent,
    );
    spent
}

/// How many documents the mass-delete bar removes in one plan: enough that
/// a cost per key the Layer 3 limit does not admit is far past it, and the
/// store-level bar's order of magnitude.
const MASS_DELETES: usize = 200;

/// The stem every document the mass-delete bar removes opens with, and the
/// folder they stand in. No generated document shares one, and some generated
/// stem and path sort after every one of them at both scales
/// ([`the_planted_keys_sort_among_the_generated`]), both asserted rather than
/// assumed.
const MASS_DELETE_STEM: &str = "mass-gate";

/// **The Layer 3 mass-delete cost limit binds `delete`.** [`MASS_DELETES`]
/// documents no link names are planted beside each per-PR profile, each at a
/// stem of its own, with one more beside them in their folder so the plan
/// empties no folder. One plan carrying, for each, the operation
/// [`norn_host::Host::delete`] compiles its request to is applied through the
/// host: it removes every one of them in one changeset, re-decides no link,
/// and that changeset's re-decision of the class and the path each removal
/// names steps the store at most [`norn_testkit::work::STEPS_PER_EMPTY_KEY`]
/// per key — the limit the store's own mass-delete bar holds a bare
/// changeset to — at `ambiguous` (300 documents) as at `realistic` (2000),
/// in the same counts at both. Its reads are a removed target's budgets
/// ([`baselines::APPLY_DOCUMENT_READS_PER_REMOVED_TARGET`],
/// [`baselines::APPLY_TARGET_READS_PER_REMOVED_TARGET`]) per document it
/// removes and none of any other file.
#[test]
#[ignore = "counter-lane case: runs in the ci counter gates job, not the workspace suite"]
fn a_mass_delete_through_the_host_costs_a_constant_per_key_at_both_scales() {
    the_hosts_account_is_readable();
    let small = norn_fixtures::Profile::by_name("ambiguous").expect("the ambiguity profile");
    let large = norn_fixtures::Profile::by_name("realistic").expect("the gate profile");

    let small_counters = one_mass_delete("counter-gate-mass-delete-ambiguous", &small);
    let large_counters = one_mass_delete("counter-gate-mass-delete-realistic", &large);

    // Each removal names a class, its stem, and a path, its own.
    let keys = 2 * MASS_DELETES as u64;
    let ceiling = norn_testkit::work::STEPS_PER_EMPTY_KEY * keys;
    for (profile, counters) in [(&small, &small_counters), (&large, &large_counters)] {
        for (name, expected) in [
            ("removed", MASS_DELETES as u64),
            ("links_redecided", 0),
            ("findings_written", 0),
        ] {
            assert_eq!(
                counters.get(name),
                expected,
                "the mass delete over `{}` did not read `{name}` as its {MASS_DELETES} unlinked \
                 documents name: {counters:?}",
                profile.name
            );
        }
        let steps = counters.get("changeset_read_steps");
        assert!(
            steps > 0 && baselines::fits(steps, ceiling),
            "the mass delete's changeset over `{}` took {steps} steps re-deciding the {keys} \
             classes and paths its {MASS_DELETES} removals name, past the Layer 3 limit of {} \
             per key ({ceiling}), or none at all",
            profile.name,
            norn_testkit::work::STEPS_PER_EMPTY_KEY
        );
    }

    SizeIndependencePair::new(
        "deleting documents no link names in one plan",
        ScaleObservation::new(&small, small_counters),
        ScaleObservation::new(&large, large_counters),
    )
    .assert_size_independent();
}

/// Attach `profile` with [`MASS_DELETES`] unlinked documents planted beside
/// it, apply their deletes in one plan through the host, hold it to the
/// write-through bar, and hand back what it spent.
fn one_mass_delete(label: &str, profile: &norn_fixtures::Profile) -> CounterSnapshot {
    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), label).expect("a sandbox");
    let vault = attach::Vault::generate(&sandbox.work_dir().join("attached"), profile.name);
    let folder = format!("{MASS_DELETE_STEM}/");
    std::fs::create_dir_all(vault.path().join(&folder)).expect("creating the doomed folder");
    let doomed: Vec<String> = (0..MASS_DELETES)
        .map(|at| format!("{folder}{MASS_DELETE_STEM}-{at:04}.md"))
        .collect();
    for (at, path) in doomed.iter().enumerate() {
        std::fs::write(vault.path().join(path), format!("doomed {at}\n"))
            .expect("writing a doomed document");
    }
    std::fs::write(vault.path().join(format!("{folder}kept.md")), "kept\n")
        .expect("writing the document that keeps the folder");
    let address = VaultAddress::name(vault.name().clone());
    let spent = {
        let host = vault.host();
        let _lease = attach::attach_and_wait(&host, vault.name());
        let operations: Vec<norn_wire::Operation> = doomed
            .iter()
            .flat_map(|path| {
                norn_wire::DeleteParams::new(
                    address.clone(),
                    norn_wire::ApplyMode::Apply,
                    norn_wire::DocumentPath::new(path).expect("a document path"),
                )
                .plan()
                .operations
            })
            .collect();
        one_apply(profile, "a mass delete", &host, vault.path(), || {
            host.apply(norn_wire::ApplyParams::new(
                norn_wire::ApplyMode::Apply,
                norn_wire::PlanDocument::operations(norn_wire::AuthoredPlan::new(
                    address.clone(),
                    operations,
                )),
            ))
        })
    };

    let mut store = vault.store();
    let mut sharing = Vec::new();
    attach::for_each_derived_path(&mut store, |path| {
        if path.stem().starts_with(MASS_DELETE_STEM) {
            sharing.push(path.as_str().to_string());
        }
    });
    assert!(
        sharing.is_empty(),
        "the mass delete removed every document at its stems, and the attachment derived \
         others at them: {sharing:?}"
    );
    let planted: Vec<String> = doomed
        .iter()
        .cloned()
        .chain([format!("{folder}kept.md")])
        .collect();
    the_planted_keys_sort_among_the_generated(
        &mut store,
        profile,
        "the mass delete's planted documents",
        &planted,
    );
    record_the_counters(
        &format!(
            "deleting {MASS_DELETES} documents no link names in one plan over `{}`",
            profile.name
        ),
        &spent,
    );
    spent
}

/// **A planted neighborhood's keys sort among the generated vault's, at the
/// scale `store` holds — asserted rather than assumed.** A seek for a key past
/// every key an index holds ends at the index's end in fewer steps than one
/// that lands on a row it then passes over, so a planted stem or path sorting
/// past every generated one at one scale and not at the other would read a
/// step apart per key for no difference in work, and a size-independence
/// equality over the pair would fail on the fixture's vocabulary rather than
/// on what the write did. That equality stands on this condition: **some
/// generated stem, and some generated path, sorts after every `planted` one**,
/// in the order a sensitive key keeps and in the ASCII-folded order a folded
/// key keeps. A vocabulary change that breaks it fails here, by name.
fn the_planted_keys_sort_among_the_generated(
    store: &mut Store,
    profile: &norn_fixtures::Profile,
    what: &str,
    planted: &[String],
) {
    type Order = fn(&str) -> String;
    let orders: [(&str, Order); 2] = [
        ("sensitive", |key| key.to_owned()),
        ("folded", |key| key.to_ascii_lowercase()),
    ];
    let planted: Vec<DocumentPath> = planted
        .iter()
        .map(|path| DocumentPath::new(path).expect("a planted document path"))
        .collect();
    let mut generated = Vec::new();
    attach::for_each_derived_path(store, |path| {
        if !planted.contains(path) {
            generated.push(path.clone());
        }
    });
    for (order, key) in orders {
        for (part, of) in [
            ("stem", DocumentPath::stem as fn(&DocumentPath) -> &str),
            ("path", DocumentPath::as_str),
        ] {
            let last = |paths: &[DocumentPath]| paths.iter().map(|path| key(of(path))).max();
            let last_planted = last(&planted).expect("a planted neighborhood");
            let last_generated = last(&generated).unwrap_or_default();
            assert!(
                last_generated > last_planted,
                "{what}: over `{}`, no generated {part} sorts after the planted `{last_planted}` \
                 in the {order} order (the last generated is `{last_generated}`), so the planted \
                 keys sort past the vault's own and a seek for them reads a different number of \
                 steps at each scale; plant them under names that sort among the generated ones",
                profile.name
            );
        }
    }
}

/// What an applied plan does to one file it touches.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg(feature = "induced-failure")]
enum Fate {
    /// Standing before and after, its content replaced.
    Replaced,
    /// Absent before and standing after.
    Created,
    /// Standing before and absent after.
    Removed,
    /// Standing before and absent after, a move having carried its bytes
    /// unchanged to another target: the write kernel stages that target as
    /// a copy of this file, which reads it once more.
    CopiedAway,
}

#[cfg(feature = "induced-failure")]
impl Fate {
    /// The document reads the authored budgets allow a file of this fate.
    fn document_reads(self) -> u64 {
        match self {
            Fate::Replaced => baselines::APPLY_DOCUMENT_READS_PER_REPLACED_TARGET,
            Fate::Created => baselines::APPLY_DOCUMENT_READS_PER_CREATED_TARGET,
            Fate::Removed => baselines::APPLY_DOCUMENT_READS_PER_REMOVED_TARGET,
            Fate::CopiedAway => baselines::APPLY_DOCUMENT_READS_PER_COPIED_AWAY_TARGET,
        }
    }

    /// The write kernel's target reads the authored budgets allow a file of
    /// this fate.
    fn target_reads(self) -> u64 {
        match self {
            Fate::Replaced => baselines::APPLY_TARGET_READS_PER_REPLACED_TARGET,
            Fate::Created => baselines::APPLY_TARGET_READS_PER_CREATED_TARGET,
            Fate::Removed => baselines::APPLY_TARGET_READS_PER_REMOVED_TARGET,
            Fate::CopiedAway => baselines::APPLY_TARGET_READS_PER_COPIED_AWAY_TARGET,
        }
    }

    /// Whether the file is written, through a shadow of its own.
    fn written(self) -> bool {
        matches!(self, Fate::Replaced | Fate::Created)
    }
}

/// The files an applied plan touches, by their vault-relative paths, each
/// with what the plan does to it. Read only where the host's account is,
/// behind `induced-failure`.
#[derive(Clone, Debug, Default)]
#[cfg(feature = "induced-failure")]
struct Touched {
    fates: std::collections::BTreeMap<std::path::PathBuf, Fate>,
}

#[cfg(feature = "induced-failure")]
impl Touched {
    /// What `plan`'s transitions do, file by file. A removed file the
    /// applier copies another target from is copied away, by the applier's
    /// own rule ([`norn_host::copied_sources`]) — the lane's names differ by
    /// more than case, so they are read under a sensitive root. A replaced
    /// file copied from — a chain's middle — has no budget authored, so a
    /// plan holding one fails here until one is.
    fn of(plan: &norn_wire::ResolvedPlan) -> Touched {
        let copied_sources = norn_host::copied_sources(plan, norn_fs::CaseSensitivity::Sensitive);
        let copied_away: std::collections::BTreeSet<&str> = copied_sources
            .iter()
            .map(norn_wire::DocumentPath::as_str)
            .collect();
        let mut touched = Touched::default();
        for transition in &plan.transitions {
            let present = |state: &norn_wire::FileState| {
                matches!(state, norn_wire::FileState::Present { .. })
            };
            let fate = match (present(&transition.before), present(&transition.after)) {
                (true, true) if copied_away.contains(transition.path.as_str()) => panic!(
                    "`{}` is replaced and copied from, a fate no budget is authored for",
                    transition.path.as_str()
                ),
                (true, true) => Fate::Replaced,
                (false, true) => Fate::Created,
                (true, false) if copied_away.contains(transition.path.as_str()) => Fate::CopiedAway,
                (true, false) => Fate::Removed,
                (false, false) => panic!(
                    "a transition at `{}` names no file before or after it",
                    transition.path.as_str()
                ),
            };
            let earlier = touched.fates.insert(transition.path.as_str().into(), fate);
            assert!(
                earlier.is_none(),
                "the plan carries two transitions at `{}`",
                transition.path.as_str()
            );
        }
        touched
    }

    /// How many files the plan does `fate` to.
    fn count(&self, fate: Fate) -> u64 {
        self.fates.values().filter(|&&each| each == fate).count() as u64
    }

    /// How many files the plan removes, copied away or not.
    fn removed(&self) -> u64 {
        self.count(Fate::Removed) + self.count(Fate::CopiedAway)
    }

    /// The files written, each through a shadow of its own.
    fn written(&self) -> u64 {
        self.fates.values().filter(|fate| fate.written()).count() as u64
    }

    /// The document reads the authored budgets allow this plan's files.
    fn document_reads(&self) -> u64 {
        self.fates.values().map(|fate| fate.document_reads()).sum()
    }

    /// The write kernel's target reads the authored budgets allow this
    /// plan's files.
    fn target_reads(&self) -> u64 {
        self.fates.values().map(|fate| fate.target_reads()).sum()
    }

    /// The plan's files counted by fate, for a message.
    fn summary(&self) -> String {
        format!(
            "{} replaced, {} created, {} removed and {} copied away",
            self.count(Fate::Replaced),
            self.count(Fate::Created),
            self.count(Fate::Removed),
            self.count(Fate::CopiedAway)
        )
    }
}

/// Apply what `act` asks through the host serving the vault at `root`, read
/// what the apply job spent off the host's account around it, hold it to the
/// write-through bar, and hand back every count it spent with the transitions
/// its plan carried.
///
/// **The account is the apply job's own.** The job runs on a worker thread
/// under the attribution window every job opens, so what it read through
/// `norn-fs` and what its changeset did are folded into the account when it
/// ends, before its answer is published; the snapshot it read the store on is
/// counted where its planning and applying are done with it. Nothing else
/// runs between the two readings but the watcher's echo of the applier's own
/// writes, which the own-write ledger suppresses on the watcher's thread
/// without a job: every apply here makes and empties no folder, so the echo
/// names no path a job would read.
///
/// **What the account cannot see.** The read tally is per thread, so it holds
/// the reads the job's own thread took and nothing the job moved onto another
/// — a limit every counter-lane read bar shares since Layer 3. And the
/// watcher's echo check (`norn-fs`'s `matches_expected`) opens and hashes
/// each written file once more on the watcher's thread, outside every job's
/// account: that read is excluded from the budgets, not counted in them.
///
/// **The bar**, at every apply:
///
/// - each read protocol's count is exactly its authored budget summed over
///   the plan's transitions — the document reads
///   ([`baselines::APPLY_DOCUMENT_READS_PER_REPLACED_TARGET`] and its
///   siblings), the write kernel's target reads
///   ([`baselines::APPLY_TARGET_READS_PER_REPLACED_TARGET`] and its
///   siblings) and the shadow reads
///   ([`baselines::APPLY_SHADOW_READS_PER_WRITTEN_TARGET`]) — floor and
///   ceiling, field by field, so no read moves from one protocol to another;
/// - **every read names a file the plan touches**: under a recording of the
///   files each counted read read, every document and target read is of a
///   path one of the plan's transitions names — a `where` target's planning
///   reads of its matches among them — each touched path is read exactly its
///   fate's budget by each protocol, and each staged shadow is read once,
///   under a name of its own;
/// - one changeset, deriving and upserting exactly the written targets and
///   killing and tombstoning exactly the removed ones;
/// - at most one read handle minted, with no table or index stepped end to
///   end on the snapshot it holds. One snapshot on each handle minted holds
///   by the type — a minted handle is one established snapshot — so the
///   equality of the two counts says the account was read off the snapshots
///   the job minted, not that a second snapshot was refused.
#[cfg(feature = "induced-failure")]
fn one_apply(
    profile: &norn_fixtures::Profile,
    what: &str,
    host: &attach::ServingHost,
    root: &Path,
    act: impl FnOnce() -> Result<norn_host::PendingApply, norn_wire::ErrorEnvelope>,
) -> CounterSnapshot {
    let _recording = norn_fs::reads::record_files();
    let mark = host.files_read().len();
    let before = host.evidence();
    let answered = act()
        .expect("an apply is admitted")
        .wait()
        .unwrap_or_else(|refused| {
            panic!("{what} over `{}` was refused: {refused:?}", profile.name)
        });
    let spent = host.evidence().since(before);
    let files = host.files_read().split_off(mark);
    let norn_wire::ApplyReport::Applied {
        plan, changeset, ..
    } = answered.report
    else {
        panic!("{what} answered {:?}", answered.report);
    };
    assert_eq!(
        changeset,
        norn_wire::ChangesetOutcome::Committed,
        "{what} over `{}` left its changeset to a heal",
        profile.name
    );
    let touched = Touched::of(&plan);
    for (name, reading, budget) in [
        (
            "document_opens",
            spent.document_opens,
            touched.document_reads(),
        ),
        ("target_reads", spent.target_reads, touched.target_reads()),
        (
            "shadow_reads",
            spent.shadow_reads,
            touched.written() * baselines::APPLY_SHADOW_READS_PER_WRITTEN_TARGET,
        ),
    ] {
        assert!(
            baselines::fits(reading, budget) && baselines::fits(budget, reading),
            "{what} over `{}` read `{name}` {reading} times, and the authored budgets over its \
             {} files allow exactly {budget}",
            profile.name,
            touched.summary()
        );
    }
    the_reads_name_the_touched_files(profile, what, root, &touched, &spent, &files);
    for (name, reading, expected) in [
        ("changesets_applied", spent.changesets_applied, 1),
        (
            "documents_derived",
            spent.documents_derived,
            touched.written(),
        ),
        (
            "documents_upserted",
            spent.documents_upserted,
            touched.written(),
        ),
        (
            "documents_deleted",
            spent.documents_deleted,
            touched.removed(),
        ),
        (
            "tombstones_recorded",
            spent.tombstones_recorded,
            touched.removed(),
        ),
        ("apply_full_scan_steps", spent.apply_full_scan_steps, 0),
    ] {
        assert_eq!(
            reading,
            expected,
            "{what} over `{}` read `{name}` as {reading}, and its {} files name {expected}",
            profile.name,
            touched.summary()
        );
    }
    assert!(
        spent.apply_mints <= 1
            && spent.apply_snapshots_opened == spent.apply_mints
            && (spent.apply_mints == 0) == (spent.apply_mint_statements == 0),
        "{what} over `{}` minted {} read handles running {} statements and opened {} snapshots \
         on them: an apply opens at most one, on the one handle it mints",
        profile.name,
        spent.apply_mints,
        spent.apply_mint_statements,
        spent.apply_snapshots_opened
    );
    [
        ("replaced", touched.count(Fate::Replaced)),
        ("created", touched.count(Fate::Created)),
        ("removed", touched.removed()),
        ("document_opens", spent.document_opens),
        ("target_reads", spent.target_reads),
        ("shadow_reads", spent.shadow_reads),
        ("stats", spent.stats),
        ("walk_dirents", spent.walk_dirents),
        ("documents_derived", spent.documents_derived),
        ("changesets_applied", spent.changesets_applied),
        ("documents_upserted", spent.documents_upserted),
        ("documents_deleted", spent.documents_deleted),
        ("tombstones_recorded", spent.tombstones_recorded),
        ("findings_discarded", spent.findings_discarded),
        ("findings_written", spent.findings_written),
        ("links_redecided", spent.links_redecided),
        ("link_health_keys_resolved", spent.link_health_keys_resolved),
        (
            "link_health_candidates_read",
            spent.link_health_candidates_read,
        ),
        ("changeset_read_steps", spent.changeset_read_steps),
        ("mints", spent.apply_mints),
        ("mint_statements", spent.apply_mint_statements),
        ("snapshots_opened", spent.apply_snapshots_opened),
        ("statements", spent.apply_statements),
        ("vm_steps", spent.apply_vm_steps),
        ("full_scan_steps", spent.apply_full_scan_steps),
    ]
    .into_iter()
    .collect()
}

/// **Which file each read was of.** `files` is the recording of the apply
/// job's counted reads, one per read: it must name exactly as many reads as
/// the account counted, every document and target read must be of a file
/// below `root` that the plan touches, each touched file must be read
/// exactly its fate's budget by each protocol, and each staged shadow must be
/// read once, under a name of its own: an absolute path whose file name is a
/// shadow name, outside `root` or in the shadow home's fallback below it, so
/// a shadow read counted against a document fails. A read of a file the
/// plan does not touch fails here even where it leaves every count at its
/// budget. The recording is in the order the reads ran, so each file the
/// plan writes must be read as a document exactly once after the kernel's
/// last read of it — the commit's read-back, which carries the written state
/// to the store without the applier holding its bytes — and a step that
/// reused bytes it had read before while the commit read twice fails here
/// with every count at its budget.
#[cfg(feature = "induced-failure")]
fn the_reads_name_the_touched_files(
    profile: &norn_fixtures::Profile,
    what: &str,
    root: &Path,
    touched: &Touched,
    spent: &norn_host::EvidenceReading,
    files: &[norn_fs::reads::FileRead],
) {
    use norn_fs::reads::ReadAct;

    assert_eq!(
        files.len() as u64,
        spent.document_opens + spent.target_reads + spent.shadow_reads,
        "{what} over `{}` counted {} document, {} target and {} shadow reads, and the recording \
         names {} files: every counted read names the file it read",
        profile.name,
        spent.document_opens,
        spent.target_reads,
        spent.shadow_reads,
        files.len()
    );
    let mut per_file: std::collections::BTreeMap<(&Path, ReadAct), u64> =
        std::collections::BTreeMap::new();
    let mut untouched = Vec::new();
    let mut shadows = std::collections::BTreeSet::new();
    let mut not_shadows = Vec::new();
    let shadow_fallback = root.join(norn_fs::shadow::FALLBACK);
    for read in files {
        if read.act == ReadAct::Shadow {
            let named_a_shadow = read
                .path
                .file_name()
                .is_some_and(norn_fs::shadow::is_shadow_name);
            let outside_the_documents =
                !read.path.starts_with(root) || read.path.starts_with(&shadow_fallback);
            if read.path.is_absolute() && named_a_shadow && outside_the_documents {
                shadows.insert(read.path.as_path());
            } else {
                not_shadows.push(read.path.display().to_string());
            }
            continue;
        }
        match read.path.strip_prefix(root) {
            Ok(relative) if touched.fates.contains_key(relative) => {
                *per_file.entry((relative, read.act)).or_default() += 1;
            }
            _ => untouched.push(format!("{:?} {}", read.act, read.path.display())),
        }
    }
    assert!(
        untouched.is_empty(),
        "{what} over `{}` read files its plan does not touch {} times, its {} files under `{}` \
         being the only ones it may read; the first: {:?}",
        profile.name,
        untouched.len(),
        touched.summary(),
        root.display(),
        &untouched[..untouched.len().min(5)]
    );
    assert!(
        not_shadows.is_empty(),
        "{what} over `{}` counted {} shadow reads of files that are not staged shadows, a \
         shadow being a file under a shadow name in a shadow home outside the vault's \
         documents; the first: {:?}",
        profile.name,
        not_shadows.len(),
        &not_shadows[..not_shadows.len().min(5)]
    );
    let mut off_budget = Vec::new();
    for (path, fate) in &touched.fates {
        for (act, budget) in [
            (ReadAct::Document, fate.document_reads()),
            (ReadAct::Target, fate.target_reads()),
        ] {
            let read = per_file.get(&(path.as_path(), act)).copied().unwrap_or(0);
            if read != budget {
                off_budget.push(format!(
                    "{} ({fate:?}): {read} {act:?} reads, budget {budget}",
                    path.display()
                ));
            }
        }
    }
    assert!(
        off_budget.is_empty(),
        "{what} over `{}` read {} touched files other than their fates' budgets; the first: \
         {:?}",
        profile.name,
        off_budget.len(),
        &off_budget[..off_budget.len().min(5)]
    );
    assert_eq!(
        shadows.len() as u64,
        spent.shadow_reads,
        "{what} over `{}` read {} staged shadows under {} names: each shadow is read once",
        profile.name,
        spent.shadow_reads,
        shadows.len()
    );
    let mut not_read_back_once = Vec::new();
    for (path, fate) in &touched.fates {
        if !fate.written() {
            continue;
        }
        let of_this = |read: &&norn_fs::reads::FileRead| {
            read.act != ReadAct::Shadow && read.path.strip_prefix(root).ok() == Some(path.as_path())
        };
        let after_the_kernel = files
            .iter()
            .rposition(|read| of_this(&read) && read.act == ReadAct::Target)
            .map_or(0, |last| last + 1);
        let read_back = files[after_the_kernel..]
            .iter()
            .filter(of_this)
            .filter(|read| read.act == ReadAct::Document)
            .count();
        if read_back != 1 {
            not_read_back_once.push(format!(
                "{} ({fate:?}): {read_back} document reads after the kernel's last",
                path.display()
            ));
        }
    }
    assert!(
        not_read_back_once.is_empty(),
        "{what} over `{}` did not read {} written files back exactly once after the kernel \
         published them; the first: {:?}",
        profile.name,
        not_read_back_once.len(),
        &not_read_back_once[..not_read_back_once.len().min(5)]
    );
}

/// A build without `induced-failure` carries no reader of the host's account.
#[cfg(not(feature = "induced-failure"))]
fn one_apply(
    _: &norn_fixtures::Profile,
    _: &str,
    _: &attach::ServingHost,
    _: &Path,
    _: impl FnOnce() -> Result<norn_host::PendingApply, norn_wire::ErrorEnvelope>,
) -> CounterSnapshot {
    unreachable!("a case that reads the host's account refuses to run without `induced-failure`")
}

/// **The size-independence bar over a read.** The vault around a bounded find
/// is not part of what the find costs.
///
/// The same find runs through a live hold at `ambiguous` (300 documents) and at
/// `realistic` (2000): newest `created` first, a page of [`FIND_LIMIT`] rows
/// carrying every field, then the page its cursor continues. What is compared
/// is every count the find's work carries — the statements, the keys its page
/// statements handed back, what SQLite counted stepping them, and the rows it
/// hydrated. The snapshot's own statement count is held equal to the
/// statements the pages report as they are read, a consistency check on the
/// report rather than a second reading of it. The order matches
/// every document at both scales, far more than a page, so what bounds the
/// hydration is the page and never the match count.
///
/// **The shape is unfiltered because a filter is not size-independent.** A
/// page a filter narrows drives from the filter's seek and sorts what it
/// matched, so its cost is the match count, and a predicate matching more than
/// a page at both scales matches more at the larger one.
///
/// Two controls run beside the bar, at the same two attachments, and each must
/// fail it by the count that grows. A page bounded at the most a page may hold
/// hydrates the whole vault at 300 documents and a thousand of them at 2000.
/// And the same order ascending reads the key's missing section first, which
/// walks every document carrying the key to reach the few that do not.
#[test]
#[ignore = "counter-lane case: runs in the ci counter gates job, not the workspace suite"]
fn a_bounded_find_costs_the_same_at_both_scales() {
    let small = norn_fixtures::Profile::by_name("ambiguous").expect("the ambiguity profile");
    let large = norn_fixtures::Profile::by_name("realistic").expect("the gate profile");

    let at_small = find_shapes("counter-gate-find-ambiguous", &small);
    let at_large = find_shapes("counter-gate-find-realistic", &large);

    for (profile, shapes) in [(&small, &at_small), (&large, &at_large)] {
        assert_eq!(
            shapes.bounded.pages.len(),
            2,
            "`{}` holds more than a page, so the bounded find reads two pages",
            profile.name
        );
        assert!(
            shapes
                .bounded
                .pages
                .iter()
                .all(|page| page.work.documents_hydrated == u64::from(FIND_LIMIT)),
            "a bounded page over `{}` hydrated other than a page of rows",
            profile.name
        );
        record_the_counters(
            &format!("a bounded find over `{}`", profile.name),
            &shapes.bounded.readings(),
        );
    }

    SizeIndependencePair::new(
        "a bounded find, two pages",
        ScaleObservation::new(&small, at_small.bounded.readings()),
        ScaleObservation::new(&large, at_large.bounded.readings()),
    )
    .assert_size_independent();

    let whole = SizeIndependencePair::new(
        "a find whose page holds the whole vault",
        ScaleObservation::new(&small, at_small.whole.readings()),
        ScaleObservation::new(&large, at_large.whole.readings()),
    )
    .violations();
    assert!(
        whole
            .iter()
            .any(|violation| violation.contains("`find_documents_hydrated`")),
        "a page that hydrates the whole vault passed the pair: {whole:?}"
    );
    let walked = SizeIndependencePair::new(
        "a find that walks the documents carrying its key",
        ScaleObservation::new(&small, at_small.walked.readings()),
        ScaleObservation::new(&large, at_large.walked.readings()),
    )
    .violations();
    assert!(
        walked
            .iter()
            .any(|violation| violation.contains("`find_page_vm_steps`")),
        "a page that walks the vault passed the pair: {walked:?}"
    );
    norn_testkit::readings::record(
        "the size-independence controls",
        &[
            ("a page holding the whole vault", whole.join("; ")),
            ("a page walking the vault", walked.join("; ")),
        ],
    );
}

/// The finds the size-independence pair reads at one scale.
struct FindShapes {
    /// The bar's shape: two bounded pages, newest first.
    bounded: Pages,
    /// One page bounded at [`MAX_PAGE`].
    whole: Pages,
    /// One bounded page, oldest first.
    walked: Pages,
}

/// Attach `profile` under a live host and read each [`FindShapes`] shape
/// through one hold.
fn find_shapes(label: &str, profile: &norn_fixtures::Profile) -> FindShapes {
    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), label).expect("a sandbox");
    let vault = attach::Vault::generate(&sandbox.work_dir().join("attached"), profile.name);
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());

    let mut store = vault.store();
    assert_the_attachment_derived_the_profile(&mut store, profile);
    let declared = the_pinned_declaration(&mut store);

    let hold = host
        .begin_read(vault.name())
        .expect("a live attachment answers a read");
    let bounded = bounded_find(vault.name()).with_columns([Column::fields()]);
    let whole = bounded
        .clone()
        .with_limit(u32::try_from(MAX_PAGE).expect("a page bound fits a wire limit"));
    let walked = bounded
        .clone()
        .with_sort(Sort::new(SortKey::field("created"), Direction::Ascending));
    FindShapes {
        bounded: pages(hold.snapshot(), &bounded, &declared, 2),
        whole: pages(hold.snapshot(), &whole, &declared, 1),
        walked: pages(hold.snapshot(), &walked, &declared, 1),
    }
}

/// **Every read shape through a live hold reads no vault document, and costs
/// the same at both scales.** Five shapes the acceptance contract names in
/// `docs/architecture.md` (count-by-field, suffix / stem resolve, links-to,
/// findings-for-path and full-text match; its predicate + sort + page is the
/// find [`a_bounded_find_costs_the_same_at_both_scales`] holds), and two read
/// verbs beside them, are each asked through the host verb a client's request
/// reaches, over `ambiguous` (300 documents) and `realistic` (2000), each with
/// the same planted neighborhood beside the generated tree ([`plant`]).
///
/// Each shape is held in the form its verb's contract makes flat, and bounded
/// by construction:
///
/// - **count-by-field**, a count grouped by `type` and narrowed by a path part
///   to the planted neighborhood. A part that seeks what it keeps drives the
///   tallies, so the work is the documents it admits. An unnarrowed ungrouped
///   count is not held: it is the b-tree's own count of `documents` and steps
///   no row, so its readings stand still while the pages it counts grow.
/// - **suffix / stem resolve**, a get of the whole record of a document named
///   by its bare stem, one segment of suffix: the class seek, the one document
///   and its collections' heads.
/// - **links page**, a get's page of the links one document carries, each
///   resolved at the read to the documents it names, and each naming a class
///   of one.
/// - **links-to**, a find narrowed by a `links_to` part: a single target's
///   three backlinks, paged at [`FIND_LIMIT`].
/// - **findings-for-path**, a validate summary narrowed by a path part to the
///   planted neighborhood. A page bounds itself; a summary tallies every
///   finding it admits, so the narrowing part is what bounds it.
/// - **full-text match**, a search on the lexical rung alone, for a word four
///   planted documents carry and nothing else does. A search costs the
///   documents its query matches, so the query is what bounds it.
/// - **describe**, a page of two observed fields. A page's work follows the
///   distinct keys it pages, so the page bound is what bounds it.
///
/// **The zeros are per shape.** Each shape runs inside its own read window on
/// this thread and its own two readings of the host's account, and reads no
/// document opened, no stat and no directory entry, and moves no document
/// derived, file opened or changeset landed. The zeros are measured: at each
/// scale, once the shapes have run, a document read and a walk through
/// `norn-fs` move the thread's counts, and a document written and removed
/// under the attachment moves the account's. The window is the calling
/// thread's, which is where a read verb runs its read; a file a verb read on
/// a thread of its own would be in neither reading.
///
/// **The work is read where the verb reports it**: the builder's own work
/// counts, and the snapshot's. The snapshot counts every statement run on it,
/// and the virtual-machine and full-scan steps SQLite took over each of them
/// whichever part of the read it answered. Statement counters do not see work
/// a virtual table does inside a statement, so a full-text match's
/// posting-list walk is not among them; a search's page cost is read off the
/// matches the module hands back instead. Each shape runs a pinned number of
/// statements, reads above zero on its statements, on its rows or its steps,
/// and on the snapshot's statements and steps, at both scales; and every
/// count is compared name by name across the pair.
///
/// **Each shape has a control that grows with the vault**, read at the same
/// two attachments, and each must read more at the larger scale on the counts
/// its shape names, the snapshot's steps among them. The
/// planted crowd is what grows: a tenth of the profile's documents, each
/// sharing one stem, carrying a key of its own and a word of its own and
/// linking one hub, beside as many documents whose frontmatter never closes.
/// So an unnarrowed grouped count walks every document; a get of the crowd's
/// stem is refused as ambiguous and counts the class it names; a links page
/// holding a link to that stem counts the same class; the hub's backlinks are
/// the crowd; an unnarrowed summary tallies the crowd's findings; a search for
/// the crowd's word matches the crowd; and a describe page at the most a page
/// may hold reads the crowd's keys.
///
/// Every failure names its shape and its scale, and every failure is
/// reported at once.
#[test]
#[ignore = "counter-lane case: runs in the ci counter gates job, not the workspace suite"]
fn every_read_shape_costs_the_same_at_both_scales_and_reads_no_vault_document() {
    the_hosts_account_is_readable();
    let small = norn_fixtures::Profile::by_name("ambiguous").expect("the ambiguity profile");
    let large = norn_fixtures::Profile::by_name("realistic").expect("the gate profile");

    let mut failures = Vec::new();
    let at_small = read_every_shape("counter-gate-shapes-ambiguous", &small, &mut failures);
    let at_large = read_every_shape("counter-gate-shapes-realistic", &large, &mut failures);
    let documents = (at_small.documents, at_large.documents);

    let mut controls = Vec::new();
    for (shape, (small_reading, large_reading)) in READ_SHAPES
        .iter()
        .zip(at_small.shapes.iter().zip(&at_large.shapes))
    {
        for (profile, reading) in [(&small, small_reading), (&large, large_reading)] {
            record_the_counters(
                &format!(
                    "`{}` through a live hold over `{}`",
                    shape.name, profile.name
                ),
                &reading.bounded,
            );
            for count in shape.working.iter().chain(SNAPSHOT_WORKING) {
                if reading.bounded.get(count) == 0 {
                    failures.push(format!(
                        "`{}` over `{}` read nothing on `{count}`, so its zeros say nothing: {:?}",
                        shape.name, profile.name, reading.bounded
                    ));
                }
            }
            let ran = reading.bounded.get("statements_executed");
            if ran != shape.statements {
                failures.push(format!(
                    "`{}` over `{}` ran {ran} statements on its snapshot, and its shape runs {}",
                    shape.name, profile.name, shape.statements
                ));
            }
        }
        failures.extend(
            SizeIndependencePair::new(
                shape.name,
                ScaleObservation::new(&small, small_reading.bounded.clone()),
                ScaleObservation::new(&large, large_reading.bounded.clone()),
            )
            .violations(),
        );

        let operation = format!("{}, its control", shape.name);
        let control = SizeIndependencePair::new(
            &operation,
            ScaleObservation::new(&small, small_reading.control.clone()),
            ScaleObservation::new(&large, large_reading.control.clone()),
        )
        .violations();
        for grows in shape.grows {
            let grown = (
                small_reading.control.get(grows),
                large_reading.control.get(grows),
            );
            if grown.1 <= grown.0 {
                failures.push(format!(
                    "`{}`'s control did not grow with the vault on `{grows}`, reading {} over \
                     {} documents and {} over {}",
                    shape.name, grown.0, documents.0, grown.1, documents.1
                ));
            }
        }
        controls.push((shape.name, control.join("; ")));
    }
    norn_testkit::readings::record("the read shapes' controls", &controls);
    assert!(
        failures.is_empty(),
        "the read shapes failed the lane:\n{}",
        failures.join("\n")
    );
}

/// The directory the planted documents sit in, beside the generated tree.
///
/// Every planted stem opens `cg-`, which no stem the generator draws does, so
/// no generated link names a planted document and no planted class holds a
/// generated one.
const PLANTED: &str = "counter-gate";

/// The word the neighborhood's search reads: carried by the lodestar and its
/// three linkers, and by no other document.
const NEIGHBORHOOD_WORD: &str = "counterquill";

/// The word every crowd document carries, and no other document does.
const CROWD_WORD: &str = "countercrowd";

/// The planted neighborhood, the same at every scale: a lodestar with two
/// links and three backlinks, a hub the crowd links to, a pointer holding a
/// link to the crowd's stem, and one document whose frontmatter never closes.
/// Every path is under `counter-gate/fixed/`.
const NEIGHBORHOOD: &[(&str, &str)] = &[
    (
        "cg-lodestar.md",
        "---\ntitle: Lodestar\ntype: note\ncreated: 2026-01-01T00:00:00Z\ntags: [gate]\n---\n\n\
         # Lodestar\n\nThe counterquill lodestar. ^anchor\n\n## Bearings\n\n\
         See [[cg-linker-one]] and [[cg-linker-two]].\n",
    ),
    (
        "cg-linker-one.md",
        "---\ntitle: Linker One\ntype: note\ncreated: 2026-01-02T00:00:00Z\n---\n\n\
         One counterquill points at [[cg-lodestar]].\n",
    ),
    (
        "cg-linker-two.md",
        "---\ntitle: Linker Two\ntype: task\ncreated: 2026-01-03T00:00:00Z\n---\n\n\
         Two counterquill points at [[cg-lodestar]].\n",
    ),
    (
        "cg-linker-three.md",
        "---\ntitle: Linker Three\ntype: task\ncreated: 2026-01-04T00:00:00Z\n---\n\n\
         Three counterquill points at [[cg-lodestar]].\n",
    ),
    (
        "cg-hub.md",
        "---\ntitle: Hub\ntype: note\ncreated: 2026-01-05T00:00:00Z\n---\n\n\
         The crowd links here.\n",
    ),
    (
        "cg-pointer.md",
        "---\ntitle: Pointer\ntype: note\ncreated: 2026-01-06T00:00:00Z\n---\n\n\
         A link naming the crowd's stem: [[cg-twin]].\n",
    ),
    (
        "cg-unclosed.md",
        "---\ncreated: 2026-01-07T00:00:00Z\nthe frontmatter never closes\n",
    ),
];

/// How many crowd documents stand beside `profile`'s tree, and as many whose
/// frontmatter never closes: a tenth of its documents, so the crowd grows
/// with the vault.
fn crowd(profile: &norn_fixtures::Profile) -> usize {
    profile.docs / 10
}

/// Write the planted documents into `vault`'s tree before anything attaches
/// it, and hand back how many were written.
///
/// **The neighborhood is what each shape reads, and it is the same at every
/// scale**, so a shape whose work follows what it reads counts the same over
/// both trees. **The crowd is what each control reads, and it grows with the
/// vault**: each crowd document stands in a directory of its own under one
/// stem, `cg-twin`, so the class that stem names is the crowd; each carries a
/// key of its own, the crowd's word, and a link to `cg-hub`. Beside it stand
/// as many documents whose frontmatter never closes, each a finding.
fn plant(vault: &attach::Vault, profile: &norn_fixtures::Profile) -> usize {
    let mut written = 0;
    let mut write = |path: String, text: &str| {
        let at = vault.path().join(path);
        std::fs::create_dir_all(at.parent().expect("a planted document's folder"))
            .expect("creating a planted document's folder");
        std::fs::write(at, text).expect("writing a planted document");
        written += 1;
    };
    for (path, text) in NEIGHBORHOOD {
        write(format!("{PLANTED}/fixed/{path}"), text);
    }
    for at in 0..crowd(profile) {
        write(
            format!("{PLANTED}/crowd/{at:04}/cg-twin.md"),
            &format!(
                "---\ntype: crowd\ncreated: 2026-02-01T00:00:00Z\nzz-cg-{at:04}: x\n---\n\n\
                 {CROWD_WORD} reaches [[cg-hub]].\n"
            ),
        );
        write(
            format!("{PLANTED}/broken/cg-broken-{at:04}.md"),
            "---\ncreated: 2026-02-01T00:00:00Z\nthe frontmatter never closes\n",
        );
    }
    written
}

/// What a read shape reads through: the live host, the vault it serves, and
/// the declaration the vault's store pins.
struct Reader<'a> {
    host: &'a attach::ServingHost,
    name: VaultName,
    declared: ContentModel,
}

impl Reader<'_> {
    fn vault(&self) -> VaultAddress {
        VaultAddress::name(self.name.clone())
    }

    fn target(text: &str) -> ResolutionTarget {
        ResolutionTarget::new(text).expect("a planted target")
    }
}

/// One read shape the lane holds, and the control that shows its pair can
/// fail.
struct ReadShape {
    /// The shape: the acceptance contract's name for it where the contract
    /// names it, and the verb's where it does not.
    name: &'static str,
    /// The bounded form: the verb as a client asks it, and what it read.
    bounded: fn(&Reader<'_>) -> CounterSnapshot,
    /// The counts of the bounded form's work that must read above zero.
    working: &'static [&'static str],
    /// The statements the bounded form runs on its snapshot, the
    /// establishing statement included, at every scale.
    statements: u64,
    /// The control: the same verb over what the crowd grows.
    control: fn(&Reader<'_>) -> CounterSnapshot,
    /// The counts the control must grow across the pair: each reads more at
    /// the larger scale.
    grows: &'static [&'static str],
}

/// The shapes [`every_read_shape_costs_the_same_at_both_scales_and_reads_no_vault_document`]
/// holds, beside the find its siblings hold.
const READ_SHAPES: &[ReadShape] = &[
    ReadShape {
        name: "count-by-field",
        bounded: |reader| count(reader, [Predicate::path(format!("{PLANTED}/fixed/**"))]),
        working: &["count_statements", "count_tallies_read", "count_vm_steps"],
        statements: 5,
        control: |reader| count(reader, []),
        grows: &["count_full_scan_steps", "vm_steps"],
    },
    ReadShape {
        name: "suffix / stem resolve",
        bounded: get_the_lodestar,
        working: &["get_statements", "get_vm_steps"],
        statements: 11,
        control: get_the_crowds_stem,
        grows: &["get_vm_steps", "vm_steps"],
    },
    ReadShape {
        name: "links page",
        bounded: |reader| links_page(reader, "cg-lodestar", 2),
        working: &["get_statements", "get_vm_steps"],
        statements: 6,
        control: |reader| links_page(reader, "cg-pointer", 1),
        grows: &["get_vm_steps", "vm_steps"],
    },
    ReadShape {
        name: "links-to",
        bounded: |reader| backlinks(reader, "cg-lodestar", Some(3)),
        working: &[
            "find_statements",
            "find_documents_hydrated",
            "find_page_vm_steps",
        ],
        statements: 5,
        control: |reader| backlinks(reader, "cg-hub", None),
        grows: &["find_page_vm_steps", "vm_steps"],
    },
    ReadShape {
        name: "findings-for-path",
        bounded: |reader| {
            validate_summary(reader, [Predicate::path(format!("{PLANTED}/fixed/**"))])
        },
        working: &[
            "validate_statements",
            "validate_rows_read",
            "validate_vm_steps",
        ],
        statements: 3,
        control: |reader| validate_summary(reader, []),
        grows: &["validate_vm_steps", "vm_steps"],
    },
    ReadShape {
        name: "full-text match",
        bounded: |reader| lexical_search(reader, NEIGHBORHOOD_WORD, Some(4)),
        working: &[
            "search_statements",
            "search_documents_hydrated",
            "search_page_vm_steps",
        ],
        statements: 4,
        control: |reader| lexical_search(reader, CROWD_WORD, None),
        grows: &["search_page_vm_steps", "vm_steps"],
    },
    ReadShape {
        name: "describe",
        bounded: |reader| observed_fields(reader, 2),
        working: &[
            "describe_statements",
            "describe_facets_read",
            "describe_vm_steps",
        ],
        statements: 3,
        control: |reader| {
            observed_fields(
                reader,
                u32::try_from(MAX_PAGE).expect("a page bound fits a wire limit"),
            )
        },
        grows: &["describe_facets_read", "vm_steps"],
    },
];

/// The snapshot's counts every shape's bounded form must read above zero,
/// beside the counts of its builder's work its shape names: the statements
/// it ran and the steps SQLite took stepping them.
const SNAPSHOT_WORKING: &[&str] = &["statements_executed", "vm_steps"];

/// What a shape's answer cost: the builder's work, then the snapshot's.
fn cost(
    work: impl Iterator<Item = (&'static str, u64)>,
    snapshot: &SnapshotCounters,
) -> CounterSnapshot {
    work.chain(snapshot.readings()).collect()
}

/// A count of the documents `predicates` admits, grouped by `type`.
fn count(reader: &Reader<'_>, predicates: impl IntoIterator<Item = Predicate>) -> CounterSnapshot {
    let answered = reader
        .host
        .count(
            &CountParams::new(reader.vault())
                .with_by([GroupKey::field("type")])
                .with_predicates(predicates),
        )
        .unwrap_or_else(|refusal| panic!("a count was refused: {refusal:?}"));
    assert!(
        answered.answer.is_complete() && !answered.answer.report.rows.is_empty(),
        "a count answered no tally, or could not apply its parts: {:?}",
        answered.answer
    );
    cost(answered.work.readings(), &answered.snapshot)
}

/// The whole record of the lodestar, named by its bare stem.
fn get_the_lodestar(reader: &Reader<'_>) -> CounterSnapshot {
    let answered = reader
        .host
        .get(&GetParams::new(
            reader.vault(),
            Reader::target("cg-lodestar"),
        ))
        .unwrap_or_else(|refusal| panic!("a get of the lodestar was refused: {refusal:?}"));
    let GetReport::Record { document, .. } = &answered.answer.report else {
        panic!("a get with no anchor answered {:?}", answered.answer.report);
    };
    assert_eq!(
        document.path.as_str(),
        format!("{PLANTED}/fixed/cg-lodestar.md"),
        "the lodestar's stem resolved to another document"
    );
    cost(answered.work.readings(), &answered.snapshot)
}

/// What a get of the crowd's stem ran before it was refused as ambiguous,
/// read off the statements its plans ran on a live hold's snapshot: a refusal
/// answers no work of its own.
fn get_the_crowds_stem(reader: &Reader<'_>) -> CounterSnapshot {
    let hold = reader
        .host
        .begin_read(&reader.name)
        .expect("a live attachment answers a read");
    let plans = hold
        .snapshot()
        .get_plans(
            &GetParams::new(reader.vault(), Reader::target("cg-twin")),
            &reader.declared,
            &NoText,
        )
        .unwrap_or_else(|refusal| panic!("a get of the crowd's stem was refused: {refusal:?}"));
    let mut work = CounterSnapshot::new();
    for plan in &plans {
        for (name, value) in plan.work.readings() {
            work.set(name, work.get(name) + value);
        }
    }
    for (name, value) in hold.snapshot().counters().readings() {
        work.set(name, value);
    }
    work
}

/// A get's text layer that is never asked: a get of a whole record cuts no
/// section and no block.
struct NoText;

impl norn_store::DocumentText for NoText {
    fn section(
        &self,
        _: &[norn_store::HeadingFact],
        _: &str,
        _: &str,
    ) -> Option<norn_store::SectionAt> {
        unreachable!("a get of a whole record cuts no section")
    }

    fn block(&self, _: &str, _: usize) -> std::ops::Range<usize> {
        unreachable!("a get of a whole record cuts no block")
    }
}

/// A get's page of the links the document `stem` names carries, which must
/// hold `links` of them.
fn links_page(reader: &Reader<'_>, stem: &str, links: usize) -> CounterSnapshot {
    let answered = reader
        .host
        .get(
            &GetParams::new(reader.vault(), Reader::target(stem))
                .with_collection(CollectionSelector::Links)
                .with_limit(FIND_LIMIT),
        )
        .unwrap_or_else(|refusal| panic!("a links page of `{stem}` was refused: {refusal:?}"));
    let GetReport::Collection {
        page: CollectionPage::Links { page, .. },
        ..
    } = &answered.answer.report
    else {
        panic!("a links page answered {:?}", answered.answer.report);
    };
    assert_eq!(
        page.rows.len(),
        links,
        "`{stem}` carries other links: {page:?}"
    );
    cost(answered.work.readings(), &answered.snapshot)
}

/// A page of the documents linking to `stem`, each carrying its fields, which
/// must hold `backlinks` of them where it names a number.
fn backlinks(reader: &Reader<'_>, stem: &str, backlinks: Option<usize>) -> CounterSnapshot {
    let answered = reader
        .host
        .find(
            &FindParams::new(reader.vault())
                .with_predicates([Predicate::links_to(Reader::target(stem))])
                .with_columns([Column::fields()])
                .with_limit(FIND_LIMIT),
        )
        .unwrap_or_else(|refusal| panic!("the backlinks of `{stem}` were refused: {refusal:?}"));
    assert!(
        answered.answer.is_complete(),
        "the backlinks of `{stem}` could not apply {:?}",
        answered.answer.unsatisfied
    );
    if let Some(backlinks) = backlinks {
        assert_eq!(
            answered.answer.report.page.rows.len(),
            backlinks,
            "`{stem}` has other backlinks"
        );
    }
    cost(answered.work.readings(), &answered.snapshot)
}

/// A summary of the findings standing over the documents `predicates`
/// admits.
fn validate_summary(
    reader: &Reader<'_>,
    predicates: impl IntoIterator<Item = Predicate>,
) -> CounterSnapshot {
    let answered = reader
        .host
        .validate(
            &ValidateParams::new(reader.vault())
                .with_predicates(predicates)
                .summarized(),
        )
        .unwrap_or_else(|refusal| panic!("a validate was refused: {refusal:?}"));
    let ValidateReport::Summary { by_kind, .. } = &answered.answer.report else {
        panic!("a summary answered {:?}", answered.answer.report);
    };
    assert!(
        by_kind.iter().any(|tally| tally.count > 0),
        "a summary over the planted findings tallied none"
    );
    cost(answered.work.readings(), &answered.snapshot)
}

/// A page of the lexical rung's hits for `word`, each carrying its document's
/// fields, which must hold `hits` of them where it names a number.
fn lexical_search(reader: &Reader<'_>, word: &str, hits: Option<usize>) -> CounterSnapshot {
    let answered = reader
        .host
        .search(
            &SearchParams::new(reader.vault(), word)
                .with_rungs(RungSelection::exactly(RungSet::lexical()))
                .with_columns([Column::fields()])
                .with_limit(FIND_LIMIT),
        )
        .unwrap_or_else(|refusal| panic!("a search for `{word}` was refused: {refusal:?}"));
    if let Some(hits) = hits {
        assert_eq!(
            answered.answer.report.page.rows.len(),
            hits,
            "`{word}` matched other documents"
        );
    }
    let searched = &answered.work;
    let lexical = searched
        .lexical
        .expect("a lexical search ran the lexical rung");
    assert!(
        searched.vector.is_none(),
        "a lexical search ran the vector rung"
    );
    // The search's own counts are the vector rung's and its restriction's,
    // which a lexical search runs none of: they are in the pair so that a
    // lexical path that starts spending them is read.
    let own = [
        ("search_candidate_pages", searched.candidate_pages),
        ("search_candidates", searched.candidates),
        ("search_margin", searched.margin),
        ("search_paths_checked", searched.paths_checked),
    ];
    cost(lexical.readings().chain(own), &answered.snapshot)
}

/// A page of `limit` observed fields.
fn observed_fields(reader: &Reader<'_>, limit: u32) -> CounterSnapshot {
    let answered = reader
        .host
        .describe(
            &DescribeParams::new(reader.vault())
                .with_facets([FacetKind::ObservedField])
                .with_limit(limit),
        )
        .unwrap_or_else(|refusal| panic!("a describe was refused: {refusal:?}"));
    assert!(
        !answered.answer.report.rows.is_empty(),
        "a describe answered no observed field"
    );
    cost(answered.work.readings(), &answered.snapshot)
}

/// One shape's readings at one scale: its bounded form's and its control's.
struct ShapeReading {
    bounded: CounterSnapshot,
    control: CounterSnapshot,
}

/// Every shape's readings at one scale, and how many documents the
/// attachment derived there.
struct ScaleReading {
    documents: usize,
    shapes: Vec<ShapeReading>,
}

/// Attach `profile` with the planted documents beside it under a live host,
/// and read every [`READ_SHAPES`] shape through it.
///
/// Each bounded form runs inside a read window of its own and two readings
/// of the host's account of its own, and a count either moves is a failure
/// naming the shape and the scale. The two instruments' controls run after
/// the shapes, under the same attachment.
fn read_every_shape(
    label: &str,
    profile: &norn_fixtures::Profile,
    failures: &mut Vec<String>,
) -> ScaleReading {
    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), label).expect("a sandbox");
    let vault = attach::Vault::generate(&sandbox.work_dir().join("attached"), profile.name);
    let planted = plant(&vault, profile);
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());

    let mut store = vault.store();
    let derived = attach::derived_documents(&mut store);
    assert_eq!(
        derived,
        profile.docs + planted,
        "`{}` emits {} documents and {planted} were planted beside them, and the attachment \
         derived {derived}",
        profile.name,
        profile.docs
    );
    let reader = Reader {
        host: &host,
        name: vault.name().clone(),
        declared: the_pinned_declaration(&mut store),
    };

    let mut readings = Vec::new();
    for shape in READ_SHAPES {
        let before = vault_work(&host);
        let window = ReadWindow::open();
        let bounded = (shape.bounded)(&reader);
        let on_this_thread = thread_reads(window.finish());
        let off_the_vault = before
            .delta(&vault_work(&host))
            .expect("two readings of one account");
        for (what, moved) in [
            ("read through norn-fs on its own thread", &on_this_thread),
            ("moved the host's account", &off_the_vault),
        ] {
            record_the_counters(
                &format!("`{}` over `{}`, what it {what}", shape.name, profile.name),
                moved,
            );
            let nonzero = moved.nonzero();
            if !nonzero.is_empty() {
                failures.push(format!(
                    "`{}` over `{}` {what}: {nonzero:?}",
                    shape.name, profile.name
                ));
            }
        }
        let control = (shape.control)(&reader);
        readings.push(ShapeReading { bounded, control });
    }

    assert_eq!(
        host.state(vault.name()),
        Ok(TrustState::Ready),
        "the shapes were meant to read a live attachment, and the entry is not ready"
    );
    the_thread_tally_moves(&vault, &format!("{PLANTED}/fixed/cg-lodestar.md"));
    the_hosts_account_moves(&host, &vault);
    ScaleReading {
        documents: derived,
        shapes: readings,
    }
}

/// Generate `profile`'s tree in a sandbox of its own, attach it, and hand back
/// the pair.
///
/// The sandbox comes back with the vault because it is what removes the tree:
/// the derived store the bars read sits inside it, and dropping it takes both.
///
/// The host is attached and dropped inside this call, so what the bars read is
/// what an attachment left behind rather than a host still working. The demand
/// that made it ready goes with it — the host is what an attachment belongs to,
/// and this one is gone before a bar reads anything.
fn attached(label: &str, profile: &str) -> (Sandbox, attach::Vault) {
    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), label).expect("a sandbox");
    let vault = attach::Vault::generate(&sandbox.work_dir().join("attached"), profile);
    {
        let host = vault.host();
        attach::attach_and_wait(&host, vault.name());
    }
    (sandbox, vault)
}

/// **A bar is only a statement about an attachment that happened.** The store
/// holds one derived document per document the profile emits.
///
/// The count comes off the profile rather than a number written here, so a
/// profile that grows moves the expectation with it; what this forbids is an
/// attach that converged over a fraction of the tree and left bars reading a
/// vault nobody walked.
fn assert_the_attachment_derived_the_profile(store: &mut Store, profile: &norn_fixtures::Profile) {
    let derived = attach::derived_documents(store);
    assert_eq!(
        derived, profile.docs,
        "`{}` emits {} documents and the attachment derived {derived}",
        profile.name, profile.docs
    );
}

/// **The probe's stem is the probe's alone.** No document the attachment
/// derived carries it.
///
/// What a bounded write costs includes the findings maintenance it implies, and
/// that is keyed by ambiguity class — the document stem. A generated document
/// sharing the probe's stem would put the probe in a populated class at one
/// scale and an empty one at the other, and the pair would read a violation
/// that is the word list's rather than the store's. Asserting it here is what
/// makes a future word-list edit fail saying so.
fn assert_the_probes_stem_is_the_probes_alone(store: &mut Store) {
    let probe = DocumentPath::new(PROBE_PATH).expect("a document path");
    let mut sharing = Vec::new();
    attach::for_each_derived_path(store, |path| {
        if path.stem() == probe.stem() {
            sharing.push(path.as_str().to_string());
        }
    });
    assert!(
        sharing.is_empty(),
        "the probe writes `{PROBE_PATH}`, whose stem `{}` decides the ambiguity class the write \
         maintains, and the attachment derived documents sharing it: {sharing:?}",
        probe.stem()
    );
}

/// One document the attachment derived, which is what the warm readers are
/// asked about.
fn a_derived_document(store: &mut Store) -> StoredDocument {
    let request = store.begin_request();
    let page = request
        .stored_documents_after_ordered(None, 1, StoredPathOrder::Sensitive)
        .expect("reading a page of derived documents");
    page.into_iter()
        .next()
        .expect("an attachment over a generated tree derives documents")
}

/// How many reads overlap on one entry in the read-concurrency workload, beside
/// the one read whose hold they overlap.
const OVERLAPPING_READS: u64 = 8;

/// Whether the read-concurrency workload overlaps its reads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Overlap {
    /// One read holds the entry's connection while the others start, and lets
    /// it go only once every one of them is waiting for it.
    Held,
    /// The same reads, one after another: each ends before the next begins.
    Removed,
}

/// What one run of the read-concurrency workload read off the host's read
/// account, and what the reads' own snapshots reported.
#[derive(Clone, Copy, Debug)]
struct ConcurrencyReadings {
    /// The host's read account over the workload's window.
    window: norn_host::ReadsSince,
    /// The most statements any one acquisition ran under the gate, over the
    /// host's life. The host is the workload's own, so its life is the window.
    widest_statements_under_the_gate: u64,
    /// The most rounds of the entry gate any one acquisition took after its
    /// first, over the host's life.
    widest_gate_rounds_after_the_first: u64,
    /// What the establishments reported about themselves: the statements each
    /// served read's snapshot had run when its hold was handed over, summed.
    /// A snapshot counts its query work, so this is the establishing
    /// statement alone, one per read.
    established_statements: u64,
    /// Every statement SQLite began for the served reads, on the thread each
    /// read ran on, from before its acquisition to after its hold was dropped:
    /// its establishment, the count it ran on its snapshot, and the
    /// `ROLLBACK` that ended the snapshot.
    connection_statements: u64,
}

/// **The read-concurrency workload**, `overlapping reads on one entry`: one
/// read takes the entry's hold, [`OVERLAPPING_READS`] more start on threads of
/// their own, and each served read runs one count on its snapshot before it
/// ends.
///
/// Under [`Overlap::Held`] the first hold is let go only once the host's own
/// contention reading names every overlapping read as waiting. The reading
/// moves where an acquisition finds the connection taken and gives the gate
/// back, and the wait it then begins returns only with the connection, so a
/// reading of [`OVERLAPPING_READS`] with the hold still held is every one of
/// them contended, observed rather than slept for. The wait for it is bounded,
/// and a read that never contends exhausts it naming the reading it stopped at.
///
/// The count each read runs after its hold is handed over is outside the gate,
/// and so is the `ROLLBACK` that ends its snapshot, so for every read SQLite
/// runs more statements than the gate held: the reading of what ran under the
/// gate is told apart from what the connection ran, off the same count.
fn overlapping_reads(
    host: &attach::ServingHost,
    name: &VaultName,
    overlap: Overlap,
) -> ConcurrencyReadings {
    let before = host.read_evidence();
    let run = norn_store::SnapshotReader::statements_run_on_this_thread;
    // Take a read's hold on the calling thread, reading SQLite's count of
    // that thread first.
    let acquire = |refused: &str| {
        let started = run();
        (started, host.begin_read(name).expect(refused))
    };
    // What a served read's snapshot reported when its hold was handed over,
    // and every statement SQLite began for the read on its thread once the
    // read's own statement ran and its hold was dropped. The hold is read on
    // the thread that acquired it.
    let read_once = |(started, hold): (u64, norn_host::ReadHold<norn_host::ProductionEntryOps>)| {
        let established = hold.snapshot().counters().statements_executed();
        hold.snapshot()
            .count(
                &CountParams::new(VaultAddress::name(name.clone())),
                hold.content_model(),
            )
            .expect("a count over the hold's own declaration");
        drop(hold);
        (established, run() - started)
    };
    let answered: Vec<(u64, u64)> = match overlap {
        Overlap::Removed => (0..=OVERLAPPING_READS)
            .map(|_| read_once(acquire("an attached vault answers a read")))
            .collect(),
        Overlap::Held => {
            let first = acquire("an attached vault answers a read");
            std::thread::scope(|scope| {
                let overlapping: Vec<_> = (0..OVERLAPPING_READS)
                    .map(|_| {
                        scope.spawn(|| {
                            read_once(acquire(
                                "a read waiting for the entry's connection was refused",
                            ))
                        })
                    })
                    .collect();
                wait_until(
                    "every overlapping read to be waiting for the entry's connection",
                    attach::state_budget(READS_CONTEND_LIMIT),
                    || match host.read_evidence().since(before).reader_waits {
                        waits if waits >= OVERLAPPING_READS => Observed::Met(()),
                        waits => Observed::pending(format!(
                            "{waits} of {OVERLAPPING_READS} reads are waiting"
                        )),
                    },
                )
                .unwrap_or_else(|failure| panic!("{failure}"));
                let mut answered = vec![read_once(first)];
                answered.extend(
                    overlapping
                        .into_iter()
                        .map(|read| read.join().expect("an overlapping read finished")),
                );
                answered
            })
        }
    };
    let life = host.read_evidence();
    ConcurrencyReadings {
        window: life.since(before),
        widest_statements_under_the_gate: life.widest_statements_under_the_gate,
        widest_gate_rounds_after_the_first: life.widest_gate_rounds_after_the_first,
        established_statements: answered.iter().map(|(established, _)| established).sum(),
        connection_statements: answered.iter().map(|(_, ran)| ran).sum(),
    }
}

/// How long the overlapping reads may take to reach the wait for the entry's
/// connection. A runaway bound: each one takes the gate once and finds the
/// connection taken.
const READS_CONTEND_LIMIT: Duration = Duration::from_secs(60);

/// What the read-concurrency bar finds wrong with one run of the workload, one
/// line per failed term; empty where the bar holds.
///
/// - **Each read served runs its establishment under the gate and nothing
///   else.** The host's reading of what SQLite ran under the gate is
///   [`norn_store::SNAPSHOT_ESTABLISHMENT_STATEMENTS`] per read served, the
///   deferred `BEGIN` and the establishing statement, nothing was minted and
///   no establishment refused, and no one acquisition ran other than that.
///   The reading is the gate holder's, so it is checked apart from what the
///   establishments reported about themselves, which is the establishing
///   statement alone, one each, and from what SQLite ran for the reads, which
///   is more.
/// - **Under one continuous hold.** The entry gate's own take count reads no
///   retake inside any establishing hold, so the two readings the statement
///   term is read between bound one hold of the gate.
/// - **Contention, measured.** Every overlapping read is in the reader-wait
///   reading: exactly [`OVERLAPPING_READS`], and so nonzero.
/// - **Each contended acquisition took exactly one round after its first.**
///   No one acquisition took more than the authored ceiling,
///   [`baselines::READ_GATE_ROUNDS_AFTER_THE_FIRST_PER_ACQUISITION`], and the
///   window's rounds after the first equal its waits, so a contended
///   acquisition that answered under the demand it read before it waited
///   fails as surely as one that took two rounds after its first.
fn the_read_concurrency_bar_fails(readings: &ConcurrencyReadings) -> Vec<String> {
    let window = readings.window;
    let served = OVERLAPPING_READS + 1;
    let per_read = norn_store::SNAPSHOT_ESTABLISHMENT_STATEMENTS;
    let mut failed = Vec::new();
    if window.reads_served != served {
        failed.push(format!(
            "the host served {} reads of the {served} the workload asked for",
            window.reads_served
        ));
    }
    if window.statements_under_the_gate != window.reads_served * per_read
        || (
            window.mint_statements_under_the_gate,
            window.refused_establishment_statements_under_the_gate,
        ) != (0, 0)
        || readings.widest_statements_under_the_gate != per_read
    {
        failed.push(format!(
            "the gate holder read {} statements under the gate for {} reads served (minted {}, \
             refused {}, widest acquisition {}), where each read runs exactly its \
             establishment's {per_read}",
            window.statements_under_the_gate,
            window.reads_served,
            window.mint_statements_under_the_gate,
            window.refused_establishment_statements_under_the_gate,
            readings.widest_statements_under_the_gate
        ));
    }
    if window.gate_retakes_within_establishing_holds != 0 {
        failed.push(format!(
            "the entry gate was taken {} times inside establishing holds, where each is one \
             continuous hold",
            window.gate_retakes_within_establishing_holds
        ));
    }
    if readings.established_statements != window.reads_served {
        failed.push(format!(
            "the establishments reported {} statements for {} reads served",
            readings.established_statements, window.reads_served
        ));
    }
    if readings.connection_statements <= window.statements_under_the_gate {
        failed.push(format!(
            "the connections ran {} statements for the reads, no more than the {} under the \
             gate, so the reading under the gate is not told apart from the connection's",
            readings.connection_statements, window.statements_under_the_gate
        ));
    }
    if window.reader_waits != OVERLAPPING_READS {
        failed.push(format!(
            "the reader-wait reading is {} where {OVERLAPPING_READS} reads contended for the \
             entry's connection",
            window.reader_waits
        ));
    }
    let ceiling = baselines::READ_GATE_ROUNDS_AFTER_THE_FIRST_PER_ACQUISITION;
    if !baselines::fits(readings.widest_gate_rounds_after_the_first, ceiling)
        || window.gate_rounds_after_the_first != window.reader_waits
    {
        failed.push(format!(
            "{} contended acquisitions took {} rounds of the gate after their first, one of \
             them {}, where each takes exactly one and none more than the ceiling of {ceiling}",
            window.reader_waits,
            window.gate_rounds_after_the_first,
            readings.widest_gate_rounds_after_the_first
        ));
    }
    failed
}

/// Record one run of the read-concurrency workload where a person will find
/// it.
fn record_the_concurrency_readings(heading: &str, readings: &ConcurrencyReadings) {
    let window = readings.window;
    let rows: Vec<(&str, String)> = [
        ("reads_served", window.reads_served),
        (
            "statements_under_the_gate",
            window.statements_under_the_gate,
        ),
        (
            "mint_statements_under_the_gate",
            window.mint_statements_under_the_gate,
        ),
        (
            "refused_establishment_statements_under_the_gate",
            window.refused_establishment_statements_under_the_gate,
        ),
        (
            "widest_statements_under_the_gate",
            readings.widest_statements_under_the_gate,
        ),
        ("established_statements", readings.established_statements),
        ("connection_statements", readings.connection_statements),
        (
            "gate_retakes_within_establishing_holds",
            window.gate_retakes_within_establishing_holds,
        ),
        ("reader_waits", window.reader_waits),
        (
            "gate_rounds_after_the_first",
            window.gate_rounds_after_the_first,
        ),
        (
            "widest_gate_rounds_after_the_first",
            readings.widest_gate_rounds_after_the_first,
        ),
    ]
    .into_iter()
    .map(|(name, value)| (name, value.to_string()))
    .collect();
    norn_testkit::readings::record(heading, &rows);
}

/// **The read-concurrency bar**, under the `overlapping reads on one entry`
/// workload over a production attachment: [`OVERLAPPING_READS`] reads start
/// while another holds the entry's one connection, every one of them is
/// observed waiting before that hold is let go, and then each is served.
///
/// Each read runs exactly its snapshot's establishment under the entry gate,
/// the deferred `BEGIN` and the one statement that establishes the snapshot,
/// and the count is the gate holder's: the host reads SQLite's count of what
/// it began on the holding thread on both sides of the hold, so a statement
/// SQLite runs on the connection under the gate is counted however it was
/// composed, and an establishing statement moved before or after the hold is
/// not, while the establishment's own report still reads one. The gate's own
/// take count is read at the same two points, so an establishment run while
/// the hold let the gate go and took it back reads a retake. Reader contention
/// is the reader-wait reading, and it names every overlapping read. Each
/// contended acquisition took exactly one round of the entry gate after its
/// first, none more than
/// [`baselines::READ_GATE_ROUNDS_AFTER_THE_FIRST_PER_ACQUISITION`].
///
/// **The control removes the overlap.** The same reads run one after another
/// on the same entry, and the bar must then fail on contention and on nothing
/// else: a contention reading that stood without overlapping reads would
/// attest nothing about sharing the connection, and a control that failed on
/// another term would say nothing about this one.
#[test]
#[ignore = "counter-lane case: runs in the ci counter gates job, not the workspace suite"]
fn overlapping_reads_on_one_entry_each_run_only_their_establishment_under_the_gate() {
    let sandbox = Sandbox::new(
        Path::new(env!("CARGO_TARGET_TMPDIR")),
        "counter-gate-read-concurrency",
    )
    .expect("a sandbox");
    let vault = attach::Vault::generate(&sandbox.work_dir().join("attached"), "tiny");

    let overlapped = {
        let host = vault.host();
        let _lease = attach::attach_and_wait(&host, vault.name());
        overlapping_reads(&host, vault.name(), Overlap::Held)
    };
    record_the_concurrency_readings("overlapping reads on one entry", &overlapped);
    let failed = the_read_concurrency_bar_fails(&overlapped);
    assert!(
        failed.is_empty(),
        "the read-concurrency bar failed under overlapping reads: {failed:#?}"
    );

    let sequential = {
        let host = vault.host();
        let _lease = attach::attach_and_wait(&host, vault.name());
        overlapping_reads(&host, vault.name(), Overlap::Removed)
    };
    record_the_concurrency_readings("the same reads with the overlap removed", &sequential);
    let failed = the_read_concurrency_bar_fails(&sequential);
    assert!(
        failed.len() == 1 && failed[0].contains("reader-wait"),
        "with the overlap removed the bar must fail on contention alone, and it found: \
         {failed:#?}"
    );
}

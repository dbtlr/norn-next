//! The per-PR counter gate: what a request costs over a vault a real host
//! attached, counted rather than timed.
//!
//! Counters are the per-PR lane's currency. Unlike a clock they say the same
//! thing on a loaded machine, and unlike a store-level unit test they say it
//! about the path a request really takes: a `norn-fixtures` tree on disk, a
//! production attachment that walked it, and the derived store that attachment
//! left behind.
//!
//! Three bars, all counts:
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
//!   field rows and the pinned-schema read. The store's find suites hold each
//!   find statement's plan, and the two bars below hold a find's work.
//! - **A read through a live hold reads no vault document.** A find run on a
//!   production hold's snapshot, under a live attachment, reads nothing through
//!   `norn-fs` on its own thread and moves nothing in the host's account of what
//!   its jobs derived and read off the vault, and each page runs a pinned number
//!   of statements. Each zero is measured rather than structural: a document
//!   read and a walk of the root on the same thread move the first, and a
//!   document written and then removed under the same attachment moves the
//!   second.
//! - **Size independence.** One bounded write costs the same at 300 documents
//!   and at 2000, and so does one unfiltered find paged newest first. A ceiling
//!   passes anything under it; a pair fails the moment the two scales stop
//!   moving together.
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
//! **The read-through-a-hold bar reads the host's account**, which a build
//! reads only behind `induced-failure`, so the lane step names the feature. A
//! build without it compiles every case and refuses that one when it runs,
//! rather than passing it having read nothing.
#![cfg(unix)]
#![allow(clippy::disallowed_methods)] // Harness scaffolding: this suite's own generated tree.

mod attach;

use std::path::Path;

use std::time::Duration;

use attach::read::{FIND_LIMIT, Pages, bounded_find, pages, the_pinned_declaration};
use norn_fs::reads::{ReadTally, ReadWindow};
use norn_host::Demand;
use norn_store::{
    Change, DocumentFacts, DocumentPath, ExplainedStatement, IncrementProvenance, MAX_PAGE, Store,
    StoredDocument, StoredPathOrder, class_probe,
};
use norn_testkit::counters::CounterSnapshot;
use norn_testkit::process::Sandbox;
use norn_testkit::scale::{ScaleObservation, SizeIndependencePair};
use norn_testkit::wait::{Observed, wait_until};
use norn_wire::{Column, Direction, Predicate, Sort, SortKey, TrustState};

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

    // **The other half of the thread's zero.** The same window around one
    // document read and one walk of the registration root, both through
    // `norn-fs`, moves every count the zero asserts.
    let window = ReadWindow::open();
    norn_fs::read_and_hash(vault.path(), Path::new(subject.path.as_str()))
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

    // **The other half of the account's zero.** A document written into the
    // vault under the same attachment is derived by the host, and removing it
    // is deleted and tombstoned by the host; each moves its counts.
    let written = vault.path().join("counter-gate-derived.md");
    let derived = the_host_spends(
        &host,
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
        &host,
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
    let probe = class_probe(&stem).expect("a class stem off a derived path");

    let mut warm = store.begin_request();
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
    let _ = warm
        .suffix_candidates(&probe.clone().into())
        .expect("reading candidates");
    let _ = warm
        .full_text_matches(&phrase(&stem))
        .expect("reading matches");
    let _ = warm
        .emitted_plan(ExplainedStatement::SuffixCandidates(&probe.clone().into()))
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

/// `stem` as a full-text phrase.
///
/// A generated stem carries spaces and non-ASCII characters, which are operators
/// and separators to the match grammar rather than text; quoting is what makes
/// the argument the phrase it reads as.
fn phrase(stem: &str) -> String {
    format!("\"{}\"", stem.replace('"', ""))
}

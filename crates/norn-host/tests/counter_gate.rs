//! The per-PR counter gate: what a request costs over a vault a real host
//! attached, counted rather than timed.
//!
//! Counters are the per-PR lane's currency. Unlike a clock they say the same
//! thing on a loaded machine, and unlike a store-level unit test they say it
//! about the path a request really takes: a `norn-fixtures` tree on disk, a
//! production attachment that walked it, and the derived store that attachment
//! left behind.
//!
//! Two bars, both counts:
//!
//! - **Zero on warm.** A request that only reads derives nothing, over the
//!   ~2k-document `realistic` profile — the scale the gates assert against. It
//!   is asserted twice over, because an attachment that is gone and one that is
//!   still serving are different subjects: once over the rows an attachment left
//!   behind, and once with the entry still attached under a held demand, across
//!   two passes so a cost paid on first touch is separated from the steady state.
//!   **The counters answer for derivation, not for reading.** The vocabulary
//!   counts rows written and facts discarded, and no member counts rows read, so
//!   a reader whose row count grew with the tree still reads zero here: what this
//!   bar holds is that a warm read derives nothing. The store's pillars suite
//!   holds the read's own cost shape, in two carriers. A work bar brackets each
//!   paged reader's drain with `Request::read_steps` and states the cost as a
//!   line in the rows drained. Plan bars over the keyed point readers take the
//!   plan off the executing statement and name the index each one seeks.
//! - **Size independence.** One bounded write costs the same at 300 documents
//!   and at 2000. A ceiling passes anything under it; a pair fails the moment
//!   the two scales stop moving together.
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
#![cfg(unix)]
#![allow(clippy::disallowed_methods)] // Harness scaffolding: this suite's own generated tree.

mod attach;

use std::path::Path;

use norn_store::{
    Change, DocumentFacts, DocumentPath, ExplainedStatement, IncrementProvenance, Store,
    StoredDocument, StoredPathOrder, class_probe,
};
use norn_testkit::counters::CounterSnapshot;
use norn_testkit::process::Sandbox;
use norn_testkit::scale::{ScaleObservation, SizeIndependencePair};
use norn_wire::TrustState;

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
/// pass over the same store.
///
/// The case above reads what an attachment left behind: its host is gone by the
/// time a counter is read, so what it says is that the rows on disk answer
/// without deriving. This one says the other half — the entry is still
/// attached, its demand lease is still held, and its watcher is still
/// subscribed while the request runs. A read path that derived under a live
/// attachment, or that warmed something on first touch and paid for it, would
/// move a counter here and nowhere above.
///
/// Two passes, and both are judged. A first pass over a store nothing has read
/// yet is where a lazily-built index or a cache filled on demand would be paid
/// for; the second is the steady state the claim is about, and the pair is what
/// separates them.
#[test]
#[ignore = "counter-lane case: runs in the ci counter gates job, not the workspace suite"]
fn warm_requests_under_a_live_attachment_finish_at_zero() {
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

    let first = a_warm_pass(&mut store, &subject);
    let second = a_warm_pass(&mut store, &subject);
    record_the_counters("a warm request under a live attachment, first pass", &first);
    record_the_counters(
        "a warm request under a live attachment, second pass",
        &second,
    );

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
    let _ = warm.suffix_candidates(&probe).expect("reading candidates");
    let _ = warm
        .full_text_matches(&phrase(&stem))
        .expect("reading matches");
    let _ = warm
        .emitted_plan(ExplainedStatement::SuffixCandidates(&probe))
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

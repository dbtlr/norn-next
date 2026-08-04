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
//!   ~2k-document `realistic` profile — the scale the gates assert against.
//! - **Size independence.** One bounded write costs the same at 300 documents
//!   and at 2000. A ceiling passes anything under it; a pair fails the moment
//!   the two scales stop moving together.
//!
//! **Every case here is `#[ignore]`d into the `counter-lane` lane**, and the CI
//! `counter gates` job is the only thing that runs them. Attaching thousands of
//! documents beside every other test would put a vault walk in "build and
//! test", which is a suite that stays free of measurement.
#![allow(clippy::disallowed_methods)] // Harness scaffolding: this suite's own generated tree.

mod attach;

use norn_store::{
    Change, DocumentFacts, DocumentPath, ExplainedStatement, IncrementProvenance, Store,
    StoredDocument, class_probe,
};
use norn_testkit::counters::CounterSnapshot;
use norn_testkit::scale::{ScaleObservation, SizeIndependencePair};

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
    let scratch = attach::Scratch::new("counter-gate-warm");
    let vault = attach::Vault::generate(scratch.path(), "realistic");
    {
        let host = vault.host();
        attach::attach_and_wait(&host, vault.name());
    }

    let mut store = vault.store();
    let subject = a_derived_document(&mut store);
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

    let reading = warm.finish();
    let snapshot: CounterSnapshot = reading.readings().collect();
    assert!(reading.is_all_zero(), "{:?}", snapshot.nonzero());
    snapshot.assert_all_zero("a warm request over an attached vault");
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

    let small_counters = one_probe_write("counter-gate-pair-ambiguous", small.name);
    let large_counters = one_probe_write("counter-gate-pair-realistic", large.name);

    SizeIndependencePair::new(
        "upserting one document",
        ScaleObservation::new(&small, small_counters),
        ScaleObservation::new(&large, large_counters),
    )
    .assert_size_independent();
}

/// Attach `profile`, then count what upserting one document into its derived
/// store costs.
fn one_probe_write(label: &str, profile: &str) -> CounterSnapshot {
    let scratch = attach::Scratch::new(label);
    let vault = attach::Vault::generate(scratch.path(), profile);
    {
        let host = vault.host();
        attach::attach_and_wait(&host, vault.name());
    }

    let mut store = vault.store();
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
    reading.readings().collect()
}

/// One document the attachment derived, which is what the warm readers are
/// asked about.
fn a_derived_document(store: &mut Store) -> StoredDocument {
    let request = store.begin_request();
    let page = request
        .stored_documents_after(None, 1)
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

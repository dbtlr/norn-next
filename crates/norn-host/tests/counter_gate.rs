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
    reading.readings().collect()
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

//! Derivation counters, and what they are attributable to.
//!
//! Two bars are built on counters — a warm request derives nothing, and the same
//! operation costs the same whatever the vault around it weighs — and both are
//! statements about **one request**. These cases pin the attribution that makes
//! them mean anything: a counter set belongs to the request that opened it, so a
//! second request cannot be charged for the first one's work.
//!
//! The readings are handed to the harness's own comparison primitives, which is
//! the shape the future bars read them in: the store owns the counters and
//! `norn-testkit` owns the assertions over them.

mod common;

use common::{Scratch, document, document_with_every_fact, path, vector, violation};
use norn_store::{DerivationCounters, Provenance};
use norn_testkit::counters::CounterSnapshot;

/// A snapshot of a request's reading, in the shape the harness compares.
fn snapshot(counters: &DerivationCounters) -> CounterSnapshot {
    counters.readings().collect()
}

/// Every counter is present in every reading, whatever its value. A counter that
/// appears in one reading and not another is a difference rather than a zero,
/// and reading a missing one as zero is how a renamed counter compares equal to
/// the one it replaced.
#[test]
fn every_reading_carries_every_counter() {
    let scratch = Scratch::new("names");
    let mut store = scratch.open();

    let empty = snapshot(&store.begin_request().finish());
    let mut request = store.begin_request();
    request
        .upsert_document(&document("notes.md", "hash-1", "a body\n"))
        .expect("writing a document");
    let worked = snapshot(&request.finish());

    let names: Vec<&str> = DerivationCounters::names().collect();
    assert!(!names.is_empty());
    for name in &names {
        assert!(
            empty.names().any(|carried| carried == *name),
            "an empty reading does not carry `{name}`"
        );
    }
    assert_eq!(
        empty.names().count(),
        names.len(),
        "a reading carries a counter the vocabulary does not"
    );
    // The two readings are of the same counters, which is what lets them be
    // compared at all.
    assert_eq!(
        empty.differences(&worked).len(),
        1,
        "only the counter that moved should differ: {:?}",
        empty.differences(&worked)
    );
}

/// What a document write derives, counted by what it wrote.
#[test]
fn a_document_write_counts_what_it_wrote() {
    let scratch = Scratch::new("write");
    let mut store = scratch.open();
    let facts = document_with_every_fact("docs/norn/glossary.md", "hash-1");

    let mut request = store.begin_request();
    request.upsert_document(&facts).expect("writing a document");
    let reading = snapshot(&request.finish());

    assert_eq!(
        reading.nonzero(),
        vec![
            ("documents_upserted", 1),
            ("frontmatter_projections", 1),
            ("heading_rows_written", 2),
            ("link_rows_written", 2),
            ("tag_rows_written", 2),
            ("block_rows_written", 2),
        ]
        .into_iter()
        .collect::<std::collections::BTreeMap<_, _>>()
        .into_iter()
        .collect::<Vec<_>>()
    );
}

/// **Counters are per request, never process-global.** A second request starts at
/// zero, so what one request derived is readable without subtracting two readings
/// of a shared number.
#[test]
fn a_second_request_is_not_charged_for_the_first_ones_work() {
    let scratch = Scratch::new("attribution");
    let mut store = scratch.open();

    let mut first = store.begin_request();
    first
        .upsert_document(&document_with_every_fact("one.md", "hash-1"))
        .expect("writing a document");
    let first = first.finish();
    assert_eq!(first.get("documents_upserted"), 1);
    assert_eq!(first.get("link_rows_written"), 2);

    let mut second = store.begin_request();
    second
        .upsert_document(&document("two.md", "hash-2", "a body\n"))
        .expect("writing a document");
    let second = second.finish();
    assert_eq!(second.get("documents_upserted"), 1);
    assert_eq!(
        second.get("link_rows_written"),
        0,
        "the second request was charged for the first one's links"
    );
    assert_eq!(second.get("fact_rows_discarded"), 0);
}

/// **A request that only reads derives nothing.** Every reader is outside the
/// counted set by construction rather than by a rule the read paths have to
/// keep, which is what the zero-on-warm bar rests on.
#[test]
fn a_request_that_only_reads_finishes_at_zero() {
    let scratch = Scratch::new("warm");
    let mut store = scratch.open();
    let subject = path("docs/norn/glossary.md");

    let mut warming = store.begin_request();
    warming
        .upsert_document(&document_with_every_fact(subject.as_str(), "hash-1"))
        .expect("writing a document");
    warming
        .record_finding(&violation(subject.as_str()))
        .expect("recording a finding");
    warming
        .store_vector(&vector(subject.as_str(), "hash-1"))
        .expect("storing a vector");
    assert!(!warming.finish().is_all_zero());

    let warm = store.begin_request();
    let _ = warm.stored_document(&subject).expect("reading a document");
    let _ = warm.stored_facts(&subject).expect("reading a document");
    let _ = warm
        .stored_tombstone(&subject)
        .expect("reading a tombstone");
    let _ = warm.stored_findings(&subject).expect("reading findings");
    let _ = warm
        .findings_in_class(&norn_store::class_probe("glossary"))
        .expect("reading findings");
    let _ = warm
        .suffix_candidates(&norn_store::class_probe("glossary"))
        .expect("reading candidates");
    let _ = warm.vault_schema_pin().expect("reading the pin");
    let _ = warm.pillars().expect("a pillar report");

    let reading = warm.finish();
    assert!(reading.is_all_zero(), "{:?}", reading.nonzero());
    snapshot(&reading).assert_all_zero("a request that only read");
}

/// A re-derivation counts the rows it discarded as well as the ones it wrote, so
/// the cost of replacing a document is visible rather than netted out.
#[test]
fn a_re_derivation_counts_what_it_discarded() {
    let scratch = Scratch::new("re-derive");
    let mut store = scratch.open();
    let facts = document_with_every_fact("docs/norn/glossary.md", "hash-1");

    let mut request = store.begin_request();
    request.upsert_document(&facts).expect("writing a document");
    request.upsert_document(&facts).expect("re-deriving");
    let reading = request.finish();

    assert_eq!(reading.get("documents_upserted"), 2);
    assert_eq!(reading.get("fact_rows_discarded"), 8);
    assert_eq!(reading.get("link_rows_written"), 4);
}

/// The same operation over two documents of the same shape reads the same
/// counters, which is the comparison a size-independence pair is made of.
#[test]
fn the_same_operation_reads_the_same_counters_whatever_else_is_stored() {
    let scratch = Scratch::new("independence");
    let mut store = scratch.open();

    let mut first = store.begin_request();
    first
        .upsert_document(&document_with_every_fact("small/one.md", "hash-1"))
        .expect("writing a document");
    let small = snapshot(&first.finish());

    // Fill the store, then perform the same operation again.
    let mut filling = store.begin_request();
    for index in 0..50 {
        filling
            .upsert_document(&document(
                &format!("bulk/note-{index}.md"),
                &format!("hash-{index}"),
                "a body\n",
            ))
            .expect("writing a document");
    }
    filling.finish();

    let mut second = store.begin_request();
    second
        .upsert_document(&document_with_every_fact("large/one.md", "hash-1"))
        .expect("writing a document");
    let large = snapshot(&second.finish());

    small.assert_equal_counts(&large, "writing one document of the same shape");
}

/// A delete counts the death and the tombstone separately, because a death
/// learned for a path nothing had derived still records one.
#[test]
fn a_delete_counts_the_death_and_the_tombstone() {
    let scratch = Scratch::new("delete-counts");
    let mut store = scratch.open();
    let subject = path("glossary.md");

    let mut request = store.begin_request();
    request
        .upsert_document(&document(subject.as_str(), "hash-1", "a body\n"))
        .expect("writing a document");
    request
        .delete_document(&subject, Provenance::HealPrune)
        .expect("deleting a document");
    request
        .delete_document(&path("never/derived.md"), Provenance::WatcherRemoval)
        .expect("recording a death");
    let reading = request.finish();

    assert_eq!(reading.get("documents_deleted"), 1);
    assert_eq!(reading.get("tombstones_recorded"), 2);
}

/// The schema-keyed discard counts what it discarded, and pinning counts as its
/// own act.
#[test]
fn a_schema_change_counts_the_pin_and_the_discard() {
    let scratch = Scratch::new("schema-counts");
    let mut store = scratch.open();

    let mut request = store.begin_request();
    request
        .pin_vault_schema(b"one\n", "schema-1")
        .expect("pinning a vault schema");
    request
        .record_finding(&violation("docs/index.md"))
        .expect("recording a finding");
    request
        .pin_vault_schema(b"two\n", "schema-2")
        .expect("re-pinning a vault schema");
    request.discard_schema_dependent().expect("an invalidation");
    let reading = request.finish();

    assert_eq!(reading.get("vault_schema_pins"), 2);
    assert_eq!(reading.get("findings_written"), 1);
    assert_eq!(reading.get("findings_discarded"), 1);
}

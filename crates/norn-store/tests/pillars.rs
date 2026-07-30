//! The four pillars: full text, vectors, findings, migrations.
//!
//! Each is exercised against a real database, because a pillar's DDL is only
//! worth what a write and a read of it prove. The findings cases carry the most
//! weight: the bounded head at rest, the full enumeration as a range, the
//! ambiguity-class scoping that maintenance depends on, and the vault-schema
//! fingerprint that a schema edit invalidates by.

mod common;

use common::{Scratch, ambiguity, document, document_with_every_fact, path, vector, violation};
use norn_store::{
    CANDIDATE_HEAD, CandidateFact, Provenance, StoreError, class_probe, suffix_probe,
};

/// The full-text index is maintained by triggers, so it is consistent with the
/// column it indexes after an insert, an update and a delete alike. The store's
/// own verification is what asks FTS5 whether it agrees with `documents.body`.
#[test]
fn the_full_text_index_stays_consistent_across_every_write() {
    let scratch = Scratch::new("full-text");
    let mut store = scratch.open();
    let subject = path("docs/norn/glossary.md");

    let mut request = store.begin_request();
    request
        .upsert_document(&document(subject.as_str(), "hash-1", "the first body\n"))
        .expect("writing a document");
    request.finish();
    store.verify_integrity().expect("after an insert");

    let mut request = store.begin_request();
    request
        .upsert_document(&document(
            subject.as_str(),
            "hash-2",
            "an entirely new body\n",
        ))
        .expect("re-deriving with a new body");
    request.finish();
    store.verify_integrity().expect("after an update");

    // A re-derivation that did not change the body does no index work, and the
    // index still agrees.
    let mut request = store.begin_request();
    request
        .upsert_document(&document(
            subject.as_str(),
            "hash-3",
            "an entirely new body\n",
        ))
        .expect("re-deriving with the same body");
    request.finish();
    store.verify_integrity().expect("after an unchanged body");

    let mut request = store.begin_request();
    request
        .delete_document(&subject, Provenance::PlanDelete)
        .expect("deleting a document");
    request.finish();
    store.verify_integrity().expect("after a delete");
}

/// A vector is identified by its document and its model, replaced when it is
/// recomputed, and taken by the cascade when its document dies.
#[test]
fn a_vector_belongs_to_a_document_and_a_model() {
    let scratch = Scratch::new("vectors");
    let mut store = scratch.open();
    let subject = path("docs/norn/glossary.md");

    let mut request = store.begin_request();
    request
        .upsert_document(&document(subject.as_str(), "hash-1", "a body\n"))
        .expect("writing a document");
    request
        .store_vector(&vector(subject.as_str(), "hash-1"))
        .expect("storing a vector");
    assert_eq!(request.pillars().expect("a pillar report").vectors, 1);

    // The same model and version for the same document is the same vector
    // recomputed, not a second one.
    request
        .store_vector(&vector(subject.as_str(), "hash-2"))
        .expect("recomputing a vector");
    assert_eq!(request.pillars().expect("a pillar report").vectors, 1);

    // Another version of the same model is a different vector.
    let mut upgraded = vector(subject.as_str(), "hash-2");
    upgraded.model_version = "2".to_string();
    request.store_vector(&upgraded).expect("storing a vector");
    assert_eq!(request.pillars().expect("a pillar report").vectors, 2);
    assert_eq!(request.counters().get("vectors_written"), 3);

    request
        .delete_document(&subject, Provenance::PlanDelete)
        .expect("deleting a document");
    assert_eq!(request.pillars().expect("a pillar report").vectors, 0);

    request.finish();
    store.verify_integrity().expect("after the cascade");
}

/// A vector for a document the store does not hold is refused. Nothing is
/// embedded that nothing has derived.
#[test]
fn a_vector_for_an_unknown_document_is_refused() {
    let scratch = Scratch::new("vector-unknown");
    let mut store = scratch.open();
    let error = store
        .begin_request()
        .store_vector(&vector("never/derived.md", "hash-1"))
        .expect_err("a vector for nothing");
    assert!(
        matches!(error, StoreError::UnknownDocument { .. }),
        "{error:?}"
    );
}

/// A finding keeps the head of its candidates and the number there really were.
/// The bound holds at rest exactly as it holds on the wire.
#[test]
fn a_finding_keeps_a_bounded_head_and_the_total() {
    let scratch = Scratch::new("findings");
    let mut store = scratch.open();
    let citing = path("docs/index.md");

    let mut request = store.begin_request();
    request
        .upsert_document(&document(citing.as_str(), "hash-1", "a body\n"))
        .expect("writing a document");
    let finding = ambiguity(
        citing.as_str(),
        "glossary",
        "glossary/",
        &[
            "a/glossary.md",
            "b/glossary.md",
            "c/glossary.md",
            "d/glossary.md",
            "e/glossary.md",
        ],
        400,
    );
    request
        .record_finding(&finding)
        .expect("recording a finding");

    let stored = request
        .stored_findings(&citing)
        .expect("reading findings")
        .pop()
        .expect("a finding");
    assert_eq!(stored.kind, finding.kind);
    assert_eq!(stored.severity, finding.severity);
    assert_eq!(stored.target.as_deref(), Some("glossary"));
    assert_eq!(stored.class_key.as_deref(), Some("glossary/"));
    assert_eq!(stored.span, finding.span);
    assert_eq!(stored.candidates, finding.candidates);
    assert_eq!(stored.candidates.len(), CANDIDATE_HEAD);
    assert_eq!(stored.candidates_total, 400);
    assert_eq!(stored.message, finding.message);
    assert!(stored.generation > 0);
}

/// A head handed more than it holds is refused rather than truncated. A silently
/// truncated payload is indistinguishable from a complete one.
#[test]
fn a_candidate_head_beyond_the_bound_is_refused() {
    let scratch = Scratch::new("bound");
    let mut store = scratch.open();
    let mut finding = ambiguity("docs/index.md", "glossary", "glossary/", &[], 6);
    finding.candidates = (0..6)
        .map(|index| CandidateFact {
            path: path(&format!("dir{index}/glossary.md")),
            suffix: format!("dir{index}/glossary"),
        })
        .collect();

    let error = store
        .begin_request()
        .record_finding(&finding)
        .expect_err("a head beyond the bound");
    let StoreError::Bound { limit, given, .. } = error else {
        panic!("the bound was not what refused: {error:?}");
    };
    assert_eq!((limit, given), (CANDIDATE_HEAD, 6));
}

/// **The full candidate enumeration is a range, not a table.** The head is a head
/// because the whole class stays reachable through the index the class key opens.
#[test]
fn the_full_candidate_enumeration_is_a_range_over_the_suffix_key() {
    let scratch = Scratch::new("enumeration");
    let mut store = scratch.open();
    let mut request = store.begin_request();

    for (index, at) in [
        "glossary.md",
        "docs/glossary.md",
        "docs/norn/glossary.md",
        "archive/docs/norn/glossary.md",
        "docs/norntest/glossary.md",
        "docs/norn/index.md",
        "docs/other/glossary-old.md",
    ]
    .iter()
    .enumerate()
    {
        request
            .upsert_document(&document(at, &format!("hash-{index}"), "a body\n"))
            .expect("writing a document");
    }

    let paths = |probe| {
        request
            .suffix_candidates(&probe)
            .expect("reading candidates")
            .iter()
            .map(|path| path.as_str().to_string())
            .collect::<Vec<String>>()
    };

    // The whole class of the stem: every `**/glossary.md`, and nothing whose
    // leaf merely starts the same way.
    let mut whole_class = paths(class_probe("glossary"));
    whole_class.sort();
    assert_eq!(
        whole_class,
        vec![
            "archive/docs/norn/glossary.md",
            "docs/glossary.md",
            "docs/norn/glossary.md",
            "docs/norntest/glossary.md",
            "glossary.md",
        ]
    );

    // A longer suffix narrows the same range, and segment alignment is what
    // keeps `norntest` out of it.
    let mut narrowed = paths(suffix_probe("norn/glossary").expect("a suffix target"));
    narrowed.sort();
    assert_eq!(
        narrowed,
        vec!["archive/docs/norn/glossary.md", "docs/norn/glossary.md"]
    );

    // A target that names its extension is the same probe.
    assert_eq!(
        paths(suffix_probe("norn/glossary.md").expect("a suffix target")).len(),
        2
    );

    // A suffix that identifies one document is not an ambiguity class at all.
    assert_eq!(
        paths(suffix_probe("docs/norn/index").expect("a suffix target")),
        vec!["docs/norn/index.md"]
    );
    assert_eq!(
        paths(suffix_probe("nothing").expect("a suffix target")).len(),
        0
    );
}

/// **Findings maintenance is scoped by affected ambiguity class.** A changed path
/// names a class, and the findings a change to it can invalidate are exactly the
/// ones in that class's range — not the ones written in the document that
/// changed.
#[test]
fn findings_are_reachable_by_the_ambiguity_class_a_change_affects() {
    let scratch = Scratch::new("class-scope");
    let mut store = scratch.open();
    let mut request = store.begin_request();

    for at in ["one.md", "two.md", "three.md"] {
        request
            .upsert_document(&document(at, "hash-1", "a body\n"))
            .expect("writing a document");
    }
    // Two findings about the same class, written in different documents, plus
    // one about a longer suffix in the same class and one about another class.
    request
        .record_finding(&ambiguity("one.md", "glossary", "glossary/", &[], 3))
        .expect("recording a finding");
    request
        .record_finding(&ambiguity("two.md", "glossary", "glossary/", &[], 3))
        .expect("recording a finding");
    request
        .record_finding(&ambiguity(
            "two.md",
            "norn/glossary",
            "glossary/norn/",
            &[],
            2,
        ))
        .expect("recording a finding");
    request
        .record_finding(&ambiguity("three.md", "index", "index/", &[], 4))
        .expect("recording a finding");
    // And one with no class at all: a schema violation is not about resolution,
    // and a synthetic class would put it in the blast radius of every rename
    // that shared a stem with it.
    request
        .record_finding(&violation("three.md"))
        .expect("recording a finding");

    // A change to any `**/glossary.md` reaches the class and the suffixes inside
    // it, whichever document each finding was written in.
    let affected = request
        .findings_in_class(&class_probe("glossary"))
        .expect("reading findings");
    let mut targets: Vec<&str> = affected
        .iter()
        .map(|finding| finding.target.as_deref().expect("a target"))
        .collect();
    targets.sort();
    assert_eq!(targets, vec!["glossary", "glossary", "norn/glossary"]);

    // And it reaches nothing outside the class.
    let other = request
        .findings_in_class(&class_probe("index"))
        .expect("reading findings");
    assert_eq!(other.len(), 1);
    assert_eq!(other[0].target.as_deref(), Some("index"));

    // The class-free finding is reachable by its path and by no class.
    let by_path = request
        .stored_findings(&path("three.md"))
        .expect("reading findings");
    assert_eq!(by_path.len(), 2);
    assert!(
        by_path
            .iter()
            .any(|finding| finding.class_key.is_none() && finding.candidates.is_empty())
    );
    assert_eq!(request.counters().get("findings_written"), 5);
}

/// The tombstone keeps a dead path's class computable, which is what a deletion's
/// findings maintenance needs after the document row is gone.
#[test]
fn a_deleted_paths_class_is_still_computable_from_its_tombstone() {
    let scratch = Scratch::new("class-after-delete");
    let mut store = scratch.open();
    let subject = path("docs/norn/glossary.md");

    let mut request = store.begin_request();
    request
        .upsert_document(&document(subject.as_str(), "hash-1", "a body\n"))
        .expect("writing a document");
    request
        .delete_document(&subject, Provenance::HealPrune)
        .expect("deleting a document");

    let tombstone = request
        .stored_tombstone(&subject)
        .expect("reading a tombstone")
        .expect("a tombstone");
    assert_eq!(tombstone.path.class_key(), "glossary/");
    assert_eq!(tombstone.path.suffix_key(), "glossary/norn/docs/");
}

/// **A schema edit re-derives exactly the tables it keys.** Findings carry the
/// vault-schema fingerprint they were derived under; parse-fact rows carry no key
/// and are not touched.
#[test]
fn a_vault_schema_change_discards_findings_and_nothing_else() {
    let scratch = Scratch::new("schema-key");
    let mut store = scratch.open();
    let subject = path("docs/index.md");

    let mut request = store.begin_request();
    let pinned = request
        .pin_vault_schema(b"fields:\n  status: string\n", "schema-1")
        .expect("pinning a vault schema");
    let pin = request
        .vault_schema_pin()
        .expect("reading the pin")
        .expect("a pinned schema");
    assert_eq!(pin.bytes, b"fields:\n  status: string\n");
    assert_eq!(pin.fingerprint, "schema-1");
    assert_eq!(pin.generation, pinned);

    let facts = document_with_every_fact(subject.as_str(), "hash-1");
    request.upsert_document(&facts).expect("writing a document");
    request
        .record_finding(&violation(subject.as_str()))
        .expect("recording a finding");
    request
        .store_vector(&vector(subject.as_str(), "hash-1"))
        .expect("storing a vector");
    assert_eq!(
        request
            .stored_findings(&subject)
            .expect("reading findings")
            .len(),
        1
    );

    // Nothing to discard while the schema stands: the pinned fingerprint is the
    // one the findings carry.
    assert_eq!(
        request
            .discard_schema_dependent()
            .expect("an invalidation")
            .findings_discarded,
        0
    );

    // The schema moves, and exactly the schema-keyed table goes.
    request
        .pin_vault_schema(b"fields:\n  status: enum\n", "schema-2")
        .expect("re-pinning a vault schema");
    let invalidation = request
        .discard_schema_dependent()
        .expect("an invalidation after a schema change");
    assert_eq!(invalidation.findings_discarded, 1);
    assert_eq!(
        request
            .stored_findings(&subject)
            .expect("reading findings")
            .len(),
        0
    );

    let after = request
        .stored_facts(&subject)
        .expect("reading a document")
        .expect("a document");
    assert_eq!(after.links, facts.links, "a schema edit reached the links");
    assert_eq!(after.headings, facts.headings);
    assert_eq!(after.tags, facts.tags);
    assert_eq!(
        after.document.frontmatter.as_deref(),
        Some(r#"{"draft":false,"title":"Norn"}"#),
        "a schema edit reached the frontmatter projection"
    );
    let pillars = request.pillars().expect("a pillar report");
    assert_eq!(pillars.documents, 1);
    assert_eq!(pillars.vectors, 1, "a schema edit reached the vectors");
    assert_eq!(pillars.findings, 0);
    assert_eq!(pillars.finding_candidates, 0);
}

/// A store with no pinned schema stamps the empty fingerprint, so a schema
/// arriving later invalidates the findings derived before it — which is a schema
/// change and should.
#[test]
fn findings_derived_before_a_schema_was_pinned_are_invalidated_by_it() {
    let scratch = Scratch::new("unpinned");
    let mut store = scratch.open();
    let subject = path("docs/index.md");

    let mut request = store.begin_request();
    assert_eq!(request.vault_schema_pin().expect("no pin"), None);
    request
        .record_finding(&violation(subject.as_str()))
        .expect("recording a finding");
    assert_eq!(
        request
            .stored_findings(&subject)
            .expect("reading findings")
            .pop()
            .expect("a finding")
            .vault_schema_fingerprint,
        ""
    );

    request
        .pin_vault_schema(b"fields: {}\n", "schema-1")
        .expect("pinning a vault schema");
    assert_eq!(
        request
            .discard_schema_dependent()
            .expect("an invalidation")
            .findings_discarded,
        1
    );
}

/// The migration ledger is carved and empty. Store schema version is pinned, so
/// nothing is migrating, and recording version 1 here would put the same fact in
/// two places.
#[test]
fn the_migration_ledger_is_empty_through_the_pre_release_build() {
    let scratch = Scratch::new("migrations");
    let mut store = scratch.open();
    assert_eq!(
        store
            .begin_request()
            .pillars()
            .expect("a pillar report")
            .migrations_applied,
        0
    );
}

//! The four pillars: full text, vectors, findings, migrations.
//!
//! Each is exercised against a real database, because a pillar's DDL is only
//! worth what a write and a read of it prove. The findings cases carry the most
//! weight: the bounded head at rest, the full enumeration as a range, the
//! ambiguity-class scoping that maintenance depends on, and the vault-schema
//! fingerprint that a schema edit invalidates by.

use crate::common::{
    Scratch, ambiguity, class, document, document_with_every_fact, path, vector, violation,
};
use norn_store::{
    CANDIDATE_HEAD, CandidateFact, ProbeReader, Provenance, StoreError, class_probe,
    induced_failure, suffix_probe,
};
use norn_testkit::explain::{PlanRow, QueryPlan};

/// The plan the store reported for one of its probe readers, in the shape the
/// harness asserts over. The pairing is the store's: a plan bar over SQL nobody
/// ran judges a string, and no crate outside the store may take a plan itself.
fn plan(emitted: norn_store::EmittedPlan) -> QueryPlan {
    QueryPlan::new(
        emitted.sql,
        emitted
            .steps
            .into_iter()
            .map(|step| PlanRow::new(step.id, step.parent, step.detail))
            .collect(),
    )
}

/// The full-text index is maintained by triggers, so it is consistent with the
/// column it indexes after an insert, an update and a delete alike — and it
/// *answers* about that column, which is the behaviour the consistency is about.
#[test]
fn the_full_text_index_stays_consistent_across_every_write() {
    let scratch = Scratch::new("full-text");
    let mut store = scratch.open();
    let subject = path("docs/norn/glossary.md");

    let mut request = store.begin_request();
    request
        .upsert_document(&document(subject.as_str(), "hash-1", "the first body\n"))
        .expect("writing a document");
    assert_eq!(
        request.full_text_matches("first").expect("matches"),
        vec![subject.clone()]
    );
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
    // The old terms are gone from the index and the new ones are in it, which is
    // what the delete-then-insert pair in the update trigger is for.
    assert!(
        request
            .full_text_matches("first")
            .expect("matches")
            .is_empty()
    );
    assert_eq!(
        request.full_text_matches("entirely").expect("matches"),
        vec![subject.clone()]
    );
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
    assert_eq!(
        request.full_text_matches("entirely").expect("matches"),
        vec![subject.clone()]
    );
    request.finish();
    store.verify_integrity().expect("after an unchanged body");

    let mut request = store.begin_request();
    request
        .delete_document(&subject, Provenance::PlanDelete)
        .expect("deleting a document");
    assert!(
        request
            .full_text_matches("entirely")
            .expect("matches")
            .is_empty(),
        "the index still answers about a document that is gone"
    );
    request.finish();
    store.verify_integrity().expect("after a delete");
}

/// **An external-content index can drift, and the verification is what says so.**
/// With its triggers gone the index keeps answering about text the column no
/// longer carries — the shape of damage nothing else notices. FTS5's rank-0
/// integrity check does not: it asks only whether the index is internally well
/// formed, which a desynchronized index is. Rank 1 compares it against
/// `documents.body`.
#[test]
fn a_full_text_index_that_drifted_from_its_column_is_damage() {
    let scratch = Scratch::new("full-text-desync");
    let mut store = scratch.open();
    let subject = path("docs/norn/glossary.md");

    store
        .begin_request()
        .upsert_document(&document(subject.as_str(), "hash-1", "the first body\n"))
        .expect("writing a document");
    store.verify_integrity().expect("a store just written to");

    // The triggers are the only thing that writes to the index, so dropping them
    // is what makes a later body edit invisible to it.
    induced_failure::damage_out_of_band(
        &mut store,
        "DROP TRIGGER documents_fts_update;
         DROP TRIGGER documents_fts_insert;
         DROP TRIGGER documents_fts_delete;
         UPDATE documents SET body = 'an entirely different body' WHERE path = 'docs/norn/glossary.md'",
    )
    .expect("desynchronizing the index");

    // The drift is observable: the index answers about text that is not there.
    assert_eq!(
        store
            .begin_request()
            .full_text_matches("first")
            .expect("matches"),
        vec![subject.clone()],
        "the index no longer holds the terms the desync left in it"
    );

    let error = store
        .verify_integrity()
        .expect_err("a desynchronized full-text index");
    let StoreError::Damaged { what } = &error else {
        panic!("the drift was reported as {error:?} rather than as damage");
    };
    assert!(what.contains("full-text index"), "{what}");
}

/// A row a delete never reached is the other half of the same drift, and the
/// same check catches it.
#[test]
fn a_full_text_index_holding_a_deleted_document_is_damage() {
    let scratch = Scratch::new("full-text-orphan");
    let mut store = scratch.open();

    store
        .begin_request()
        .upsert_document(&document("notes.md", "hash-1", "the only body\n"))
        .expect("writing a document");
    induced_failure::damage_out_of_band(
        &mut store,
        "DROP TRIGGER documents_fts_delete; DELETE FROM documents",
    )
    .expect("deleting behind the index's back");

    let error = store
        .verify_integrity()
        .expect_err("an orphaned index entry");
    assert!(matches!(error, StoreError::Damaged { .. }), "{error:?}");
}

/// **A closed vocabulary is checked rather than trusted.** A value outside one is
/// damage a reader reports, so a verification that did not look for it would say
/// healthy and the next read of the same row would say damaged.
#[test]
fn a_value_outside_a_closed_vocabulary_is_damage() {
    for (arrange, column) in [
        ("UPDATE links SET family = 'embed'", "links.family"),
        (
            "UPDATE document_tags SET source = 'inferred'",
            "document_tags.source",
        ),
        (
            "UPDATE tombstones SET provenance = 'rebuild-drop'",
            "tombstones.provenance",
        ),
    ] {
        let scratch = Scratch::new("vocabulary");
        let mut store = scratch.open();
        let subject = path("docs/norn/glossary.md");
        store
            .begin_request()
            .upsert_document(&document_with_every_fact(subject.as_str(), "hash-1"))
            .expect("writing a document");
        store
            .begin_request()
            .delete_document(&path("gone.md"), Provenance::HealPrune)
            .expect("recording a death");
        store.verify_integrity().expect("a store just written to");

        induced_failure::damage_out_of_band(&mut store, arrange)
            .expect("writing a value nothing writes");
        let error = store.verify_integrity().unwrap_err();
        let StoreError::Damaged { what } = &error else {
            panic!("`{column}` outside its vocabulary was reported as {error:?}");
        };
        assert!(what.contains(column), "{what}");
    }
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
    assert_eq!(request.counters().get("vectors_written"), Some(3));

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
    assert_eq!(stored.class_key, Some(class("glossary/")));
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

/// **A total below the head it heads is refused in the same guard.** The total is
/// what makes the head a head, so a finding claiming three candidates and
/// carrying five describes no vault — and a reader that trusted it would report a
/// class smaller than the rows it can see.
#[test]
fn a_candidate_total_below_the_head_it_heads_is_refused() {
    let scratch = Scratch::new("total");
    let mut store = scratch.open();
    let finding = ambiguity(
        "docs/index.md",
        "glossary",
        "glossary/",
        &["a/glossary.md", "b/glossary.md", "c/glossary.md"],
        2,
    );

    let error = store
        .begin_request()
        .record_finding(&finding)
        .expect_err("a total below its own head");
    let StoreError::Bound { what, limit, given } = error else {
        panic!("the total was not refused as a bound: {error:?}");
    };
    assert!(what.contains("total"), "{what}");
    assert_eq!((limit, given), (2, 3));

    // The head being exactly the total is the ordinary case and is not refused.
    let mut exact = finding;
    exact.candidates_total = 3;
    store
        .begin_request()
        .record_finding(&exact)
        .expect("a head that is the whole class");
}

/// **A class key is validated where it is minted, so no finding can be recorded
/// under one that no probe opens.** An unterminated key is a finding at rest that
/// class-scoped maintenance never reaches.
#[test]
fn a_finding_cannot_carry_a_class_key_no_probe_opens() {
    assert!(norn_store::ClassKey::new("glossary").is_err());

    // Recorded under the validated form, the finding is in the range the class
    // opens — which is what the type exists to guarantee.
    let scratch = Scratch::new("class-key");
    let mut store = scratch.open();
    let mut request = store.begin_request();
    request
        .record_finding(&ambiguity("one.md", "glossary", "glossary/", &[], 3))
        .expect("recording a finding");
    assert_eq!(
        request
            .findings_in_class(&class_probe("glossary"))
            .expect("reading findings")
            .len(),
        1
    );
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

    // A target that names its extension reaches the same documents, because the
    // probe opens both readings of the dot.
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

/// **A written dot is ambiguous, and the probe answers with both readings rather
/// than picking one.** Reducing always makes `v1.2` resolve to `v1` — a single,
/// confident, wrong candidate. Reducing never leaves a document whose leaf carries
/// two dots unreachable by anything shorter than the whole leaf.
#[test]
fn a_target_whose_leaf_carries_a_dot_reaches_both_readings_of_it() {
    let scratch = Scratch::new("two-reductions");
    let mut store = scratch.open();
    let mut request = store.begin_request();

    for (index, at) in [
        "notes/v1.md",
        "notes/v1.2.md",
        "archive/notes.tar.gz",
        ".env",
        ".env.local",
    ]
    .iter()
    .enumerate()
    {
        request
            .upsert_document(&document(at, &format!("hash-{index}"), "a body\n"))
            .expect("writing a document");
    }

    let paths = |target: &str| {
        let probe = suffix_probe(target).expect("a suffix target");
        let mut found = request
            .suffix_candidates(&probe)
            .expect("reading candidates")
            .iter()
            .map(|path| path.as_str().to_string())
            .collect::<Vec<String>>();
        found.sort();
        found
    };

    // Both readings, so the ambiguity is reported rather than resolved wrongly.
    assert_eq!(paths("v1.2"), vec!["notes/v1.2.md", "notes/v1.md"]);
    // And the document that stored its own dotted stem is reachable by a target
    // that names less than the whole leaf.
    assert_eq!(paths("notes.tar"), vec!["archive/notes.tar.gz"]);
    assert_eq!(paths("notes.tar.gz"), vec!["archive/notes.tar.gz"]);
    // A dotfile is a name: `.env` opens one range, and the class it opens is the
    // one the store puts both dotfiles in.
    assert_eq!(paths(".env"), vec![".env", ".env.local"]);
    assert_eq!(paths(".env.local"), vec![".env", ".env.local"]);
}

/// **The ladder's order is total.** Equal suffix keys are exactly what an
/// ambiguity class is made of, so ties decide the order for most of the rows the
/// reader returns — and a tie broken by row-insertion history reorders itself
/// every time one of the documents is re-derived.
#[test]
fn the_candidate_order_is_total_and_survives_a_re_derivation() {
    let scratch = Scratch::new("total-order");
    let mut store = scratch.open();
    let mut request = store.begin_request();

    // One class, three members at the same depth, written in an order that is
    // neither their path order nor its reverse. Their suffix keys differ only
    // past the shared class prefix, so the tie is what decides.
    for at in ["one/tie.md", "three/tie.md", "two/tie.md"] {
        request
            .upsert_document(&document(at, "hash-1", "a body\n"))
            .expect("writing a document");
    }
    let ladder = |request: &norn_store::Request<'_>| {
        request
            .suffix_candidates(&class_probe("tie"))
            .expect("reading candidates")
            .iter()
            .map(|path| path.as_str().to_string())
            .collect::<Vec<String>>()
    };

    let before = ladder(&request);
    let mut sorted = before.clone();
    sorted.sort();
    assert_eq!(before, sorted, "the ladder is not in a stated order");

    // Re-deriving the row that happened to be written first changes nothing,
    // because the order is the keys rather than the rows' history.
    request
        .upsert_document(&document("one/tie.md", "hash-2", "another body\n"))
        .expect("re-deriving");
    assert_eq!(
        before,
        ladder(&request),
        "a re-derivation reordered the ladder"
    );
}

/// **Both probe readers reach their rows through the index the probe is bounds
/// for.** The bar is asserted against the statement the reader actually emitted,
/// which is why the store hands the pair out rather than the caller re-spelling
/// the SQL.
#[test]
fn both_probe_readers_search_the_index_the_probe_opens() {
    let scratch = Scratch::new("plans");
    let mut store = scratch.open();
    let mut request = store.begin_request();
    for at in ["one/glossary.md", "two/glossary.md"] {
        request
            .upsert_document(&document(at, "hash-1", "a body\n"))
            .expect("writing a document");
        request
            .record_finding(&ambiguity(at, "glossary", "glossary/", &[], 2))
            .expect("recording a finding");
    }

    let candidates = plan(
        request
            .probe_reader_plan(ProbeReader::SuffixCandidates, &class_probe("glossary"))
            .expect("a query plan"),
    );
    candidates.assert_no_full_scan_of("documents");
    candidates.assert_searches("documents");
    candidates.assert_uses_index("documents_suffix_key");

    let findings = plan(
        request
            .probe_reader_plan(ProbeReader::FindingsInClass, &class_probe("glossary"))
            .expect("a query plan"),
    );
    findings.assert_no_full_scan_of("findings");
    findings.assert_searches("findings");
    findings.assert_uses_index("findings_class_key");
    // The findings direction sorts by the key it searched and the rowid behind
    // it, so the index answers the order too.
    findings.assert_no_temp_btree();

    // A two-reduction probe is two seeks rather than one wider read.
    let two = plan(
        request
            .probe_reader_plan(
                ProbeReader::SuffixCandidates,
                &suffix_probe("glossary.md").expect("a suffix target"),
            )
            .expect("a query plan"),
    );
    two.assert_no_full_scan_of("documents");
    assert_eq!(
        two.rows()
            .iter()
            .filter(|row| row.index() == Some("documents_suffix_key"))
            .count(),
        2,
        "a two-reduction probe did not open two ranges: {:?}",
        two.rows()
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
    assert_eq!(request.counters().get("findings_written"), Some(5));
}

/// The tombstone keeps a dead path's class reachable, which is what a deletion's
/// findings maintenance needs after the document row is gone — and it does so from
/// the path it is read by rather than from a stored copy of the derived forms.
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
    assert_eq!(tombstone.path.class_key(), class("glossary/"));
    assert_eq!(tombstone.path.suffix_key(), "glossary/norn/docs/");

    // And that class reaches the findings a deletion has to revisit.
    request
        .record_finding(&ambiguity("other.md", "glossary", "glossary/", &[], 3))
        .expect("recording a finding");
    assert_eq!(
        request
            .findings_in_class(&class_probe(tombstone.path.stem()))
            .expect("reading findings")
            .len(),
        1
    );
}

/// **Findings outlive their subject uniformly.** A finding is keyed by path and
/// class, so deleting the document it happens to be about takes nothing with it —
/// which is the only reading under which a finding about a path no document has
/// and a finding about one that was just deleted are the same kind of row.
#[test]
fn a_finding_outlives_the_document_it_is_about() {
    let scratch = Scratch::new("findings-outlive");
    let mut store = scratch.open();
    let subject = path("docs/index.md");

    let mut request = store.begin_request();
    request
        .upsert_document(&document(subject.as_str(), "hash-1", "a body\n"))
        .expect("writing a document");
    request
        .record_finding(&ambiguity(
            subject.as_str(),
            "glossary",
            "glossary/",
            &[],
            3,
        ))
        .expect("a finding about a stored document");
    request
        .record_finding(&ambiguity("never/derived.md", "index", "index/", &[], 2))
        .expect("a finding about a path nothing derived");
    assert_eq!(request.pillars().expect("a pillar report").findings, 2);

    request
        .delete_document(&subject, Provenance::PlanDelete)
        .expect("deleting a document");
    assert_eq!(
        request.pillars().expect("a pillar report").findings,
        2,
        "a delete cascaded into findings"
    );
    assert_eq!(
        request
            .stored_findings(&subject)
            .expect("reading findings")
            .len(),
        1,
        "the finding is no longer reachable by the path it is about"
    );
    assert_eq!(request.counters().get("findings_discarded"), Some(0));

    request.finish();
    store
        .verify_integrity()
        .expect("a store whose findings outlived a document");
}

/// **Class-scoped maintenance runs in both directions, and discard-then-record is
/// the idempotence story.** Re-deriving a class empties it and records what holds
/// now, so two derivations of one class cannot leave two copies — and every
/// finding that left is counted.
#[test]
fn re_deriving_a_class_is_a_discard_and_a_record() {
    let scratch = Scratch::new("class-maintenance");
    let mut store = scratch.open();
    let mut request = store.begin_request();

    for at in ["one.md", "two.md"] {
        request
            .record_finding(&ambiguity(at, "glossary", "glossary/", &[], 3))
            .expect("recording a finding");
    }
    request
        .record_finding(&ambiguity(
            "one.md",
            "norn/glossary",
            "glossary/norn/",
            &[],
            2,
        ))
        .expect("recording a finding");
    request
        .record_finding(&ambiguity("three.md", "index", "index/", &[], 4))
        .expect("recording a finding");

    // The class discard takes the whole class, longer suffixes inside it
    // included, and nothing outside it.
    let invalidation = request
        .discard_findings_in_class(&class_probe("glossary"))
        .expect("discarding a class");
    assert_eq!(invalidation.findings_discarded, 3);
    assert!(
        request
            .findings_in_class(&class_probe("glossary"))
            .expect("reading findings")
            .is_empty()
    );
    assert_eq!(
        request
            .findings_in_class(&class_probe("index"))
            .expect("reading findings")
            .len(),
        1
    );
    assert_eq!(
        request
            .pillars()
            .expect("a pillar report")
            .finding_candidates,
        0,
        "the discard left candidates behind"
    );

    // Re-deriving the class twice leaves one copy, and nothing had to remember a
    // dedupe rule to make that true.
    for _ in 0..2 {
        request
            .discard_findings_in_class(&class_probe("glossary"))
            .expect("discarding a class");
        request
            .record_finding(&ambiguity("one.md", "glossary", "glossary/", &[], 2))
            .expect("recording a finding");
    }
    assert_eq!(
        request
            .findings_in_class(&class_probe("glossary"))
            .expect("reading findings")
            .len(),
        1
    );

    // Discarding a class that holds nothing is a no-op rather than an error.
    assert_eq!(
        request
            .discard_findings_in_class(&class_probe("absent"))
            .expect("discarding an empty class")
            .findings_discarded,
        0
    );
}

/// **A schema edit re-derives exactly the tables it keys, in one act.** Findings
/// carry the vault-schema fingerprint they were derived under; parse-fact rows
/// carry no key and are not touched. The pin and the discard are one transaction,
/// so there is no generation in which the pinned key says a finding is dead and a
/// request can still read it.
#[test]
fn a_vault_schema_change_discards_findings_and_nothing_else() {
    let scratch = Scratch::new("schema-key");
    let mut store = scratch.open();
    let subject = path("docs/index.md");

    let mut request = store.begin_request();
    let pinned = request
        .pin_vault_schema(b"fields:\n  status: string\n", "schema-1")
        .expect("pinning a vault schema");
    assert!(pinned.repinned);
    assert_eq!(pinned.invalidated.findings_discarded, 0);
    let pin = request
        .vault_schema_pin()
        .expect("reading the pin")
        .expect("a pinned schema");
    assert_eq!(pin.bytes, b"fields:\n  status: string\n");
    assert_eq!(pin.fingerprint, "schema-1");
    assert_eq!(pin.generation, pinned.generation);

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

    // The schema moves, and exactly the schema-keyed table goes — reported by the
    // pin that moved it.
    let repinned = request
        .pin_vault_schema(b"fields:\n  status: enum\n", "schema-2")
        .expect("re-pinning a vault schema");
    assert!(repinned.repinned);
    assert!(repinned.generation > pinned.generation);
    assert_eq!(repinned.invalidated.findings_discarded, 1);
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

    let pinned = request
        .pin_vault_schema(b"fields: {}\n", "schema-1")
        .expect("pinning a vault schema");
    assert_eq!(pinned.invalidated.findings_discarded, 1);
    assert!(
        request
            .stored_findings(&subject)
            .expect("reading findings")
            .is_empty()
    );
}

/// **Pinning the schema that is already pinned is not a schema change.** It takes
/// no generation, so a caller that pins on every attach does not move the store's
/// write sequence for a schema that did not move — and it discards nothing.
#[test]
fn pinning_the_schema_that_is_already_pinned_is_a_no_op() {
    let scratch = Scratch::new("re-pin");
    let mut store = scratch.open();
    let subject = path("docs/index.md");

    let mut request = store.begin_request();
    let first = request
        .pin_vault_schema(b"fields: {}\n", "schema-1")
        .expect("pinning a vault schema");
    request
        .record_finding(&violation(subject.as_str()))
        .expect("recording a finding");
    let generation_after_the_finding = request
        .stored_findings(&subject)
        .expect("reading findings")
        .pop()
        .expect("a finding")
        .generation;

    let again = request
        .pin_vault_schema(b"fields: {}\n", "schema-1")
        .expect("pinning the same schema");
    assert!(!again.repinned);
    assert_eq!(again.generation, first.generation);
    assert_eq!(again.invalidated.findings_discarded, 0);
    assert_eq!(
        request
            .stored_findings(&subject)
            .expect("reading findings")
            .len(),
        1,
        "a no-op pin discarded the findings derived under the schema it re-pinned"
    );

    // No generation was consumed, so the next write takes the one after the
    // finding's.
    request
        .record_finding(&violation("other.md"))
        .expect("recording a finding");
    assert_eq!(
        request
            .stored_findings(&path("other.md"))
            .expect("reading findings")
            .pop()
            .expect("a finding")
            .generation,
        generation_after_the_finding + 1
    );

    // The bytes are part of what "already pinned" means: the same fingerprint over
    // different bytes is a schema change.
    let moved = request
        .pin_vault_schema(b"fields: {status: string}\n", "schema-1")
        .expect("pinning different bytes under the same fingerprint");
    assert!(moved.repinned);
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

/// **A verification that could not run is not a verdict that the database is
/// damaged.** The full-text check is a write, so it fails on a database nothing
/// can write to — and `Damaged` is what authorizes discarding the database, so
/// reporting one here would answer a broken environment by destroying sound
/// derived state.
#[test]
fn a_verification_that_cannot_write_is_refused_rather_than_called_damage() {
    let scratch = Scratch::new("read-only");
    let mut store = scratch.open();
    store
        .begin_request()
        .upsert_document(&document("notes.md", "hash-1", "a body\n"))
        .expect("writing a document");
    store.verify_integrity().expect("a store just written to");

    // The connection can read and cannot write, which is what a revoked
    // permission or a read-only mount looks like from inside a statement.
    induced_failure::damage_out_of_band(&mut store, "PRAGMA query_only = ON")
        .expect("making the connection read-only");

    let error = store
        .verify_integrity()
        .expect_err("a verification that cannot write");
    assert!(
        !matches!(error, StoreError::Damaged { .. }),
        "a database nothing can write to was reported as damaged: {error:?}"
    );
    let StoreError::Sql { operation, .. } = &error else {
        panic!("it was reported as {error:?} rather than as a refused operation");
    };
    assert!(operation.contains("full-text index"), "{operation}");

    // And the database it could not verify is still readable and still there.
    induced_failure::damage_out_of_band(&mut store, "PRAGMA query_only = OFF")
        .expect("restoring the connection");
    store
        .verify_integrity()
        .expect("the same store, once it can be written to");
}

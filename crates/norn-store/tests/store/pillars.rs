//! The three pillars: full text, findings, migrations — and the plan bars every
//! statement the store names is judged by, the field pillar's included. What
//! the field pillar's rows are is the `fields` suite's.
//!
//! Each is exercised against a real database, because a pillar's DDL is only
//! worth what a write and a read of it prove. The findings cases carry the most
//! weight: the bounded head at rest, the full enumeration as a range, the
//! ambiguity-class scoping that maintenance depends on, and the vault-schema
//! fingerprint that a schema edit invalidates by.

use crate::common::{
    Scratch, class, class_named, classes, document, document_with_every_fact, drained,
    full_text_matches, path, path_names_no_document_for_target, path_names_no_document_in_class,
    record_death, violation, write_document, write_documents, written_links, written_tags,
};
use norn_store::{
    CANDIDATE_HEAD, CandidateFact, DiscardScope, ExplainedStatement, PathKey, Provenance, Store,
    StoreError, WriteStatement, induced_failure, suffix_probe,
};
use std::num::NonZeroUsize;

use norn_testkit::equivalence::assert_operationally_valid;
use norn_testkit::explain::{Access, PlanRow, QueryPlan};
use norn_testkit::readings;
use norn_testkit::work::WorkBar;
use norn_wire::FindingKind;

/// The plan the store reported for one of its named statements, in the shape the
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
    let only_the_subject = vec![subject.as_str().to_string()];

    write_document(
        &mut store.begin_request(),
        &document(subject.as_str(), "hash-1", "the first body\n"),
    );
    assert_eq!(full_text_matches(&store, "first"), only_the_subject);
    store.verify_integrity().expect("after an insert");

    write_document(
        &mut store.begin_request(),
        &document(subject.as_str(), "hash-2", "an entirely new body\n"),
    );
    // The old terms are gone from the index and the new ones are in it, which is
    // what the delete-then-insert pair in the update trigger is for.
    assert!(full_text_matches(&store, "first").is_empty());
    assert_eq!(full_text_matches(&store, "entirely"), only_the_subject);
    store.verify_integrity().expect("after an update");

    // A re-derivation that did not change the body leaves the index agreeing
    // with the column. The update trigger's `WHEN old.body IS NOT new.body`
    // guard is what makes that free rather than a delete and an insert of the
    // same terms — a cost difference rather than a behavioural one, since no
    // read distinguishes the two. What is asserted here is the agreement.
    write_document(
        &mut store.begin_request(),
        &document(subject.as_str(), "hash-3", "an entirely new body\n"),
    );
    assert_eq!(full_text_matches(&store, "entirely"), only_the_subject);
    store.verify_integrity().expect("after an unchanged body");

    record_death(&mut store.begin_request(), &subject, Provenance::PlanDelete);
    assert!(
        full_text_matches(&store, "entirely").is_empty(),
        "the index still answers about a document that is gone"
    );
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

    write_document(
        &mut store.begin_request(),
        &document(subject.as_str(), "hash-1", "the first body\n"),
    );
    store.verify_integrity().expect("a store just written to");

    // The triggers are the only thing that writes to the index, so dropping them
    // is what makes a later body edit invisible to it.
    induced_failure::execute_out_of_band(
        &mut store,
        "DROP TRIGGER documents_fts_update;
         DROP TRIGGER documents_fts_insert;
         DROP TRIGGER documents_fts_delete;
         UPDATE documents SET body = 'an entirely different body' WHERE path = 'docs/norn/glossary.md'",
    )
    .expect("desynchronizing the index");

    // The drift is observable: the index answers about text that is not there.
    assert_eq!(
        full_text_matches(&store, "first"),
        vec![subject.as_str().to_string()],
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

    write_document(
        &mut store.begin_request(),
        &document("notes.md", "hash-1", "the only body\n"),
    );
    induced_failure::execute_out_of_band(
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
        (
            "UPDATE findings SET kind = 'document/unreadable'",
            "findings.kind",
        ),
        (
            "UPDATE findings SET severity = 'urgent'",
            "findings.severity",
        ),
    ] {
        let scratch = Scratch::new("vocabulary");
        let mut store = scratch.open();
        let subject = path("docs/norn/glossary.md");
        write_document(
            &mut store.begin_request(),
            &document_with_every_fact(subject.as_str(), "hash-1"),
        );
        record_death(
            &mut store.begin_request(),
            &path("gone.md"),
            Provenance::HealPrune,
        );
        store
            .begin_request()
            .record_finding(&path_names_no_document_in_class(
                subject.as_str(),
                "glossary",
                "glossary/",
                &[subject.as_str()],
                1,
            ))
            .expect("recording a finding");
        store.verify_integrity().expect("a store just written to");

        induced_failure::execute_out_of_band(&mut store, arrange)
            .expect("writing a value nothing writes");
        let error = store.verify_integrity().unwrap_err();
        let StoreError::Damaged { what } = &error else {
            panic!("`{column}` outside its vocabulary was reported as {error:?}");
        };
        assert!(what.contains(column), "{what}");
    }
}

/// **A document row's stored position below zero is damage.** A document's
/// body offset and byte length are read through the one position reader, so
/// a value this crate never writes is refused as damage rather than failing
/// the statement.
#[test]
fn a_document_position_below_zero_is_damage() {
    for (arrange, column) in [
        (
            "UPDATE documents SET body_offset = -1",
            "documents.body_offset",
        ),
        (
            "UPDATE documents SET byte_length = -1",
            "documents.byte_length",
        ),
    ] {
        let scratch = Scratch::new("document-position");
        let mut store = scratch.open();
        let subject = path("docs/norn/glossary.md");
        write_document(
            &mut store.begin_request(),
            &document(subject.as_str(), "hash-1", "the only body\n"),
        );
        induced_failure::execute_out_of_band(&mut store, arrange)
            .expect("writing a position nothing writes");
        let error = store
            .begin_request()
            .stored_document(&subject)
            .expect_err("a document row below zero");
        let StoreError::Damaged { what } = &error else {
            panic!("`{column}` below zero was reported as {error:?}");
        };
        assert!(what.contains(column), "{what}");
    }
}

/// **The pillar disjointness is checked rather than trusted.** Nothing
/// structural holds a tombstone away from a live path — the
/// `tombstones_clear_on_derive` trigger is one maintainer on one write path —
/// so a store where the two pillars meet is damage the verification has to see,
/// exactly as a value outside a closed vocabulary is. A store an older write
/// path produced is the way such a pair arrives without a defect: its DDL
/// carries no trigger, so its fingerprint differs and it rebuilds, and this
/// check is what stands behind that mechanism.
#[test]
fn a_tombstone_at_a_live_path_is_damage() {
    let scratch = Scratch::new("undead");
    let mut store = scratch.open();
    let subject = path("docs/norn/glossary.md");
    write_document(
        &mut store.begin_request(),
        &document(subject.as_str(), "hash-1", "a body\n"),
    );
    store.verify_integrity().expect("a store just written to");

    induced_failure::execute_out_of_band(
        &mut store,
        "INSERT INTO tombstones (path, last_content_hash, provenance, generation, recorded_at)
         VALUES ('docs/norn/glossary.md', 'hash-0', 'heal-prune', 1, 0)",
    )
    .expect("writing a pair nothing writes");
    let error = store.verify_integrity().unwrap_err();
    let StoreError::Damaged { what } = &error else {
        panic!("a tombstone at a live path was reported as {error:?}");
    };
    assert!(
        what.contains("tombstones stand at a path"),
        "the damage does not name the pillars that met: {what}"
    );
}

/// **A sub-fingerprint is recomputed at rest rather than trusted.** It is a
/// derived column, and every read that would notice one drifting from the column
/// it hashes is a read that has already trusted it: a change-feed consumer
/// triages on these values and fetches nothing where they match, so a body hash
/// that stopped describing its body is a document that quietly stops being
/// re-derived.
///
/// Both directions of the drift are arranged, because the increment writes the
/// pair together and only an out-of-band write can separate them: the hash moved
/// away from its column, and the column moved away from its hash.
#[test]
fn a_sub_fingerprint_that_does_not_describe_its_column_is_damage() {
    for (arrange, named) in [
        (
            "UPDATE documents SET body_hash = 'not the hash of anything'",
            "body hash",
        ),
        (
            "UPDATE documents SET body = 'an entirely different body'",
            "body hash",
        ),
        (
            "UPDATE documents SET frontmatter_projection_hash = 'not the hash of anything'",
            "frontmatter projection hash",
        ),
        (
            "UPDATE documents SET frontmatter = '{\"title\":\"another title\"}'",
            "frontmatter projection hash",
        ),
    ] {
        let scratch = Scratch::new("sub-fingerprint");
        let mut store = scratch.open();
        let subject = path("docs/norn/glossary.md");
        let mut facts = document(subject.as_str(), "hash-1", "a body\n");
        facts = facts.with_frontmatter(titled_frontmatter(), &norn_store::ContentModel::none());
        write_document(&mut store.begin_request(), &facts);
        store.verify_integrity().expect("a store just written to");

        // The triggers stay, so a rewritten body carries the full-text index
        // with it and the checks ahead of the recompute all pass. What is left
        // wrong is the pair the recompute is the only reader of.
        induced_failure::execute_out_of_band(&mut store, arrange)
            .expect("separating a hash from the column it describes");

        let error = store
            .verify_integrity()
            .expect_err("a sub-fingerprint that describes nothing");
        let StoreError::Damaged { what } = &error else {
            panic!("`{arrange}` was reported as {error:?} rather than as damage");
        };
        assert!(what.contains(named), "{what}");
        assert!(
            what.contains(subject.as_str()),
            "the damage does not name the row it was found at: {what}"
        );
    }
}

/// A finding keeps the head of its candidates and the number there really were.
/// The bound holds at rest exactly as it holds on the wire.
#[test]
fn a_finding_keeps_a_bounded_head_and_the_total() {
    let scratch = Scratch::new("findings");
    let mut store = scratch.open();
    let citing = path("docs/index.md");

    let mut request = store.begin_request();
    write_document(
        &mut request,
        &document(citing.as_str(), "hash-1", "a body\n"),
    );
    let finding = path_names_no_document_in_class(
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
    assert_eq!(stored.kind, finding.kind.as_str());
    assert_eq!(stored.severity, finding.severity.as_str());
    assert_eq!(stored.target.as_deref(), Some("glossary"));
    assert_eq!(stored.class_keys, classes(&["glossary/"]));
    assert_eq!(stored.span, finding.span);
    assert_eq!(stored.candidates, finding.candidates);
    assert_eq!(stored.candidates.len(), CANDIDATE_HEAD);
    assert_eq!(stored.candidates_total, 400);
    assert_eq!(stored.message, finding.message);
    assert!(stored.generation > 0);
}

/// **A finding keeps the ordinal of the link it is about, and a finding about
/// the document keeps none**, and the store orders both by a position with no
/// `NULL` in it. A row value that reaches `NULL` is `NULL`, so a bound past
/// the first finding about the document on `(path, ordinal, id)` passes no
/// finding at its path — not the second finding about the document, not the
/// one about link 1 — where the same bound on `(path, position, id)` passes
/// both.
#[test]
fn a_findings_position_is_its_ordinal_with_the_documents_first() {
    let scratch = Scratch::new("finding-ordinal");
    let mut store = scratch.open();
    let at = path("a.md");
    let about = |ordinal: Option<u64>| {
        let mut finding = violation(at.as_str());
        finding.ordinal = ordinal;
        finding
    };
    let ordinals = |store: &mut Store| -> Vec<Option<u64>> {
        store
            .begin_request()
            .stored_findings(&at)
            .expect("reading findings")
            .into_iter()
            .map(|finding| finding.ordinal)
            .collect()
    };
    // A finding about a link is the store's own to file, so it stands
    // through the fenced door.
    for ordinal in [None, None, Some(1)] {
        induced_failure::record_finding_out_of_band(&mut store, &about(ordinal))
            .expect("recording a finding");
    }
    assert_eq!(ordinals(&mut store), [None, None, Some(1)]);

    induced_failure::execute_out_of_band(
        &mut store,
        "DELETE FROM findings
         WHERE (path, ordinal, id) > ('a.md', NULL, (SELECT min(id) FROM findings))",
    )
    .expect("a bound on the ordinal");
    assert_eq!(
        ordinals(&mut store),
        [None, None, Some(1)],
        "a bound reaching NULL passed a finding"
    );
    induced_failure::execute_out_of_band(
        &mut store,
        "DELETE FROM findings
         WHERE (path, position, id) > ('a.md', -1, (SELECT min(id) FROM findings))",
    )
    .expect("a bound on the position");
    assert_eq!(
        ordinals(&mut store),
        [None],
        "a bound on the position passes every finding after it"
    );
}

/// **One link holds at most one finding.** The kinds a finding about a link is
/// filed under exclude one another, so a second finding about a link that
/// already holds one is refused where it is written, whatever its kind, and
/// the first stands. Another link of the same document takes its own finding,
/// and the findings about the document itself are as many as are filed.
#[test]
fn a_second_finding_about_one_link_is_refused() {
    let scratch = Scratch::new("finding-one-per-link");
    let mut store = scratch.open();
    let at = path("a.md");
    let about = |kind: FindingKind, ordinal: Option<u64>| {
        let mut finding = violation(at.as_str());
        finding.kind = kind;
        finding.ordinal = ordinal;
        finding
    };
    // A finding about a link is the store's own to file, so each stands
    // through the fenced door, which writes it as the store files one.
    induced_failure::record_finding_out_of_band(&mut store, &about(FindingKind::Broken, Some(1)))
        .expect("recording the first finding about link 1");
    for kind in [FindingKind::Ambiguous, FindingKind::Broken] {
        let refused =
            induced_failure::record_finding_out_of_band(&mut store, &about(kind, Some(1)))
                .expect_err("a second finding about link 1");
        let StoreError::Sql {
            operation, message, ..
        } = &refused
        else {
            panic!("the second finding was refused as {refused:?}");
        };
        assert_eq!(*operation, "writing a finding");
        assert!(message.contains("UNIQUE"), "{message}");
    }
    for finding in [
        about(FindingKind::Broken, Some(2)),
        about(FindingKind::BodyBytesNotUtf8, None),
        about(FindingKind::BodyBytesNotUtf8, None),
    ] {
        induced_failure::record_finding_out_of_band(&mut store, &finding)
            .expect("recording a finding");
    }
    let stored: Vec<(String, Option<u64>)> = store
        .begin_request()
        .stored_findings(&at)
        .expect("reading findings")
        .into_iter()
        .map(|finding| (finding.kind, finding.ordinal))
        .collect();
    assert_eq!(
        stored,
        [
            (FindingKind::Broken.as_str().to_string(), Some(1)),
            (FindingKind::Broken.as_str().to_string(), Some(2)),
            (FindingKind::BodyBytesNotUtf8.as_str().to_string(), None),
            (FindingKind::BodyBytesNotUtf8.as_str().to_string(), None),
        ]
    );
}

/// A head handed more than it holds is refused rather than truncated. A silently
/// truncated payload is indistinguishable from a complete one.
#[test]
fn a_candidate_head_beyond_the_bound_is_refused() {
    let scratch = Scratch::new("bound");
    let mut store = scratch.open();
    let mut finding =
        path_names_no_document_in_class("docs/index.md", "glossary", "glossary/", &[], 6);
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
    let finding = path_names_no_document_in_class(
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
        .record_finding(&path_names_no_document_in_class(
            "one.md",
            "glossary",
            "glossary/",
            &[],
            3,
        ))
        .expect("recording a finding");
    assert_eq!(
        request
            .findings_in_class(&request.class_probe("glossary").expect("a class stem"))
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
        write_document(
            &mut request,
            &document(at, &format!("hash-{index}"), "a body\n"),
        );
    }

    let paths = |target: &str| {
        request
            .suffix_candidates(&class_named(&request, target))
            .expect("reading candidates")
            .iter()
            .map(|path| path.as_str().to_string())
            .collect::<Vec<String>>()
    };

    // The whole class of the stem: every `**/glossary.md`, and nothing whose
    // leaf merely starts the same way.
    let mut whole_class = paths("glossary");
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
    let mut narrowed = paths("norn/glossary");
    narrowed.sort();
    assert_eq!(
        narrowed,
        vec!["archive/docs/norn/glossary.md", "docs/norn/glossary.md"]
    );

    // A target that names its extension reaches the same documents, because the
    // probe opens both readings of the dot.
    assert_eq!(paths("norn/glossary.md").len(), 2);

    // A suffix that identifies one document is not an ambiguity class at all.
    assert_eq!(paths("docs/norn/index"), vec!["docs/norn/index.md"]);
    assert_eq!(paths("nothing").len(), 0);
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
        write_document(
            &mut request,
            &document(at, &format!("hash-{index}"), "a body\n"),
        );
    }

    let paths = |target: &str| {
        let mut found = request
            .suffix_candidates(&class_named(&request, target))
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
        write_document(&mut request, &document(at, "hash-1", "a body\n"));
    }
    let ladder = |request: &norn_store::Request<'_>| {
        request
            .suffix_candidates(&class_named(request, "tie"))
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
    write_document(
        &mut request,
        &document("one/tie.md", "hash-2", "another body\n"),
    );
    assert_eq!(
        before,
        ladder(&request),
        "a re-derivation reordered the ladder"
    );
}

/// Which test bars each named statement.
///
/// The `match` is exhaustive over [`ExplainedStatement`], so a statement named
/// through that seam without a bar behind it does not compile: the author has to
/// say which test covers it, and the only answers are the tests below.
fn barred_by(statement: ExplainedStatement<'_>) -> &'static str {
    match statement {
        ExplainedStatement::SuffixCandidates(_) => {
            "a_class_read_seeks_the_suffix_key_its_root_probes"
        }
        ExplainedStatement::FindingsInClass(_)
        | ExplainedStatement::SubjectDiscard(..)
        | ExplainedStatement::FindingSubjectsWithoutRows(..) => {
            "every_findings_maintenance_statement_searches_the_index_its_parameters_are_bounds_for"
        }
        ExplainedStatement::TypedValueDiscard => {
            "a_pins_typed_value_clear_reads_only_the_rows_that_hold_one"
        }
        ExplainedStatement::StoredDocumentPage(..) => {
            "a_heal_page_seeks_the_index_that_holds_its_order"
        }
        ExplainedStatement::StoredFindingPage
        | ExplainedStatement::StoredTombstonePage
        | ExplainedStatement::StoredSuffixKeyPage
        | ExplainedStatement::IndexedTermPage => {
            "an_enumeration_page_reaches_its_first_row_without_reading_the_rows_ahead_of_it"
        }
        ExplainedStatement::DocumentFeedPage | ExplainedStatement::TombstoneFeedPage => {
            "a_feed_page_walks_its_covering_index_and_reads_no_row"
        }
        ExplainedStatement::StoredDocument(_)
        | ExplainedStatement::StoredFactsDocument(_)
        | ExplainedStatement::DocumentLinks
        | ExplainedStatement::DocumentLinkKeys
        | ExplainedStatement::DocumentHeadings
        | ExplainedStatement::DocumentBlocks
        | ExplainedStatement::DocumentTags
        | ExplainedStatement::DocumentFields
        | ExplainedStatement::StoredTombstone(_)
        | ExplainedStatement::StoredFindings(_)
        | ExplainedStatement::VaultSchemaPin
        | ExplainedStatement::WriteGeneration => {
            "a_keyed_point_read_seeks_the_index_its_key_is_a_bound_for"
        }
        ExplainedStatement::PathDiscard(_) => "the_path_discard_seeks_finding_paths_path_key",
        ExplainedStatement::FindingCandidates(_)
        | ExplainedStatement::FindingClasses(_)
        | ExplainedStatement::FindingPaths(_) => {
            "a_finding_detail_chunk_seeks_the_primary_key_its_ids_lead"
        }
        ExplainedStatement::LinkHealthLinks
        | ExplainedStatement::LinkHealthClassLinks
        | ExplainedStatement::LinkHealthPathLinks
        | ExplainedStatement::LinkHealthHeads
        | ExplainedStatement::LinkHealthTotals
        | ExplainedStatement::LinkHealthSuffixes
        | ExplainedStatement::LinkHealthAnchors
        | ExplainedStatement::LinkHealthWrittenLinks
        | ExplainedStatement::LinkHealthClassFindings
        | ExplainedStatement::LinkHealthFoundLinks
        | ExplainedStatement::LinkHealthDiscard
        | ExplainedStatement::LinkHealthOccupied => {
            "the_link_health_judgment_seeks_every_row_it_reads"
        }
        ExplainedStatement::Write(_) => "every_write_path_statement_reaches_its_rows_by_a_seek",
    }
}

/// The four statements a caller drains end to end to account for everything one
/// pillar holds, named once so the bar and the cursor-spelling bar judge the
/// same set.
const ENUMERATIONS: &[ExplainedStatement<'static>] = &[
    ExplainedStatement::StoredFindingPage,
    ExplainedStatement::StoredTombstonePage,
    ExplainedStatement::StoredSuffixKeyPage,
    ExplainedStatement::IndexedTermPage,
];

/// The two statements a lane-2 consumer drains change through, named once so
/// the plan bar, the cursor-spelling bar and the work bar judge the same pair.
const FEEDS: &[ExplainedStatement<'static>] = &[
    ExplainedStatement::DocumentFeedPage,
    ExplainedStatement::TombstoneFeedPage,
];

/// The three statements a findings read collects each finding's detail
/// through, once per chunk of ids, named once so the plan bar and the census
/// judge the same set. Each is spelled by how many ids its chunk holds.
const FINDING_DETAIL: &[fn(NonZeroUsize) -> ExplainedStatement<'static>] = &[
    ExplainedStatement::FindingCandidates,
    ExplainedStatement::FindingClasses,
    ExplainedStatement::FindingPaths,
];

/// The seek a findings delete fires into one of the three detail tables.
///
/// Each table references `findings(id)` with `ON DELETE CASCADE` and lead their
/// primary key with that reference, so a deleted finding's detail rows are one
/// equality seek of the key away. The plan reports the cascade as a search of
/// the detail table, and a cascade that fell to reading the table end to end
/// would make every discard cost every finding's detail the store holds.
///
/// The judgment is taken on one row: some step that searches `table` runs
/// through the primary key with the equality constraint on `finding`. A table
/// the discard also searches for another reason — the membership rows a class
/// discard selects its findings through — keeps that search, and it is judged
/// by the assertion that names it.
fn assert_cascade_seeks_the_primary_key(plan: &QueryPlan, table: &str) {
    plan.assert_no_full_scan_of(table);
    plan.assert_searches_through(table, Access::PrimaryKey);
    assert!(
        plan.searches_of(table).iter().any(|row| {
            row.access() == Some(Access::PrimaryKey) && row.constraint() == Some("(finding=?)")
        }),
        "no step searching `{table}` seeks its primary key by `finding`: {:?}\nemitted SQL: {}",
        plan.searches_of(table),
        plan.sql()
    );
}

/// **A class read seeks the suffix key its root probes, and no other.** A root
/// that tells spellings apart ranges over `documents_suffix_key`, and a root
/// that folds ASCII case over `documents_folded_suffix_key`; either way a
/// target opens one seek per reduction, bound to the key column, and the
/// ignore set tests the rows those seeks reached rather than widening what is
/// read. The bar is judged on the row that searches `documents`: its index and
/// its range constraint.
///
/// **The candidates direction sorts through one temporary B-tree, and that is
/// the stated baseline.** Its order is total — the probed key, then `path` —
/// and each key's index leads with the key alone, so the index answers the
/// range and not the tie-break.
///
/// Control: each key's index dropped, and the same bar fails under every
/// resolution that probes it.
#[test]
fn a_class_read_seeks_the_suffix_key_its_root_probes() {
    use norn_store::{AmbiguityIgnore, StoredPathOrder, TargetClass};

    let judge = |store: &mut norn_store::Store,
                 target: &str,
                 ignore: &AmbiguityIgnore,
                 index: &str,
                 column: &str| {
        let request = store.begin_request();
        let resolution: TargetClass = request
            .target_class(target, ignore)
            .expect("a suffix target");
        let read = plan(
            request
                .emitted_plan(ExplainedStatement::SuffixCandidates(&resolution))
                .expect("a query plan"),
        );
        read.assert_no_full_scan_of("documents");
        read.assert_searches_through("documents", Access::Index(index));
        read.assert_search_constraint("documents", &format!("({column}>? AND {column}<?)"));
        assert_eq!(
            read.searches_of("documents").len(),
            resolution.probe().range_count(),
            "a resolution does not open one seek per reduction: {:?}\nemitted SQL: {}",
            read.rows(),
            read.sql()
        );
        let sorters: Vec<&PlanRow> = read
            .rows()
            .iter()
            .filter(|row| row.detail.contains("TEMP B-TREE"))
            .collect();
        assert!(
            sorters.len() == 1 && sorters[0].detail.contains("ORDER BY"),
            "the candidates direction no longer sorts through exactly one temporary B-tree for \
             its own order, so the baseline this bar states has moved: {:?}",
            read.rows()
        );
    };

    let archive = AmbiguityIgnore::new([norn_wire::Pattern::parse("archive/**").expect("a glob")]);
    let ignores = [AmbiguityIgnore::none(), archive];
    let targets = ["glossary", "Glossary.md", "one/glossary"];
    for (order, index, column) in [
        (
            StoredPathOrder::Sensitive,
            "documents_suffix_key",
            "suffix_key",
        ),
        (
            StoredPathOrder::AsciiCaseInsensitive,
            "documents_folded_suffix_key",
            "folded_suffix_key",
        ),
    ] {
        let scratch = Scratch::new(&format!("class-read-plans-{}", order.as_str()));
        let mut store =
            norn_store::Store::open(scratch.database(), order, crate::common::DERIVATION)
                .expect("opening a store");
        write_documents(
            &mut store.begin_request(),
            &[
                document("one/glossary.md", "hash-1", "a body\n"),
                document("Two/Glossary.md", "hash-1", "a body\n"),
                document("archive/glossary.md", "hash-1", "a body\n"),
            ],
        );
        for target in targets {
            for ignore in &ignores {
                judge(&mut store, target, ignore, index, column);
            }
        }

        induced_failure::execute_out_of_band(&mut store, &format!("DROP INDEX {index}"))
            .unwrap_or_else(|problem| panic!("dropping {index}: {problem}"));
        for target in targets {
            for ignore in &ignores {
                let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    judge(&mut store, target, ignore, index, column)
                }));
                assert!(
                    failed.is_err(),
                    "the bar held with {index} dropped under {order:?} for `{target}`"
                );
            }
        }
    }
}

/// **Every statement findings maintenance runs reaches its rows through an
/// index.** The bar is asserted against the statement the store actually
/// emitted, which is why the store hands the pair out rather than the caller
/// re-spelling the SQL.
#[test]
fn every_findings_maintenance_statement_searches_the_index_its_parameters_are_bounds_for() {
    let scratch = Scratch::new("plans");
    let mut store = scratch.open();
    let mut request = store.begin_request();
    // The documents go in one changeset and the findings after it. A changeset
    // discards the findings in every class its paths are in, so writing the
    // second document between the two findings would take the first one.
    write_documents(
        &mut request,
        &[
            document("one/glossary.md", "hash-1", "a body\n"),
            document("two/glossary.md", "hash-1", "a body\n"),
        ],
    );
    for at in ["one/glossary.md", "two/glossary.md"] {
        request
            .record_finding(&path_names_no_document_in_class(
                at,
                "glossary",
                "glossary/",
                &[],
                2,
            ))
            .expect("recording a finding");
    }

    let findings = plan(
        request
            .emitted_plan(ExplainedStatement::FindingsInClass(
                &request.class_probe("glossary").expect("a class stem"),
            ))
            .expect("a query plan"),
    );
    findings.assert_no_full_scan_of("findings");
    findings.assert_searches("findings");
    // The class direction seeks the membership index and reaches each finding by
    // row id, which is the order the reader states — so nothing sorts.
    findings.assert_searches_through(
        "finding_classes",
        Access::Index("finding_classes_class_key"),
    );
    findings.assert_searches_through("findings", Access::RowId);
    findings.assert_no_temp_btree();

    // The subject-scoped discard an increment runs once per changed path seeks
    // `findings_path`, so a changeset of fifty thousand entries is that many
    // seeks rather than that many reads of the table.
    let subject_discard = plan(
        request
            .emitted_plan(ExplainedStatement::SubjectDiscard(
                &path("one/glossary.md"),
                DiscardScope::EveryKind,
            ))
            .expect("a query plan"),
    );
    subject_discard.assert_no_full_scan_of("findings");
    subject_discard.assert_searches("findings");
    subject_discard.assert_searches_through("findings", Access::Index("findings_path"));
    assert_cascade_seeks_the_primary_key(&subject_discard, "finding_candidates");
    assert_cascade_seeks_the_primary_key(&subject_discard, "finding_classes");
    assert_cascade_seeks_the_primary_key(&subject_discard, "finding_paths");

    // Naming kinds narrows what the discard takes and not how it reaches it: the
    // path is the seek in both forms and the kinds filter the rows it reached,
    // so a producer that re-derives part of a subject pays the same seek.
    let kind_discard = plan(
        request
            .emitted_plan(ExplainedStatement::SubjectDiscard(
                &path("one/glossary.md"),
                DiscardScope::Kinds(&[FindingKind::PathNamesNoDocument]),
            ))
            .expect("a query plan"),
    );
    kind_discard.assert_no_full_scan_of("findings");
    kind_discard.assert_searches("findings");
    kind_discard.assert_searches_through("findings", Access::Index("findings_path"));
    assert_cascade_seeks_the_primary_key(&kind_discard, "finding_candidates");
    assert_cascade_seeks_the_primary_key(&kind_discard, "finding_classes");
    assert_cascade_seeks_the_primary_key(&kind_discard, "finding_paths");

    // **The walked-scope prune's page is one ordered pass over `findings_path`,
    // and the bar it carries is the findings table's rather than the vault's.**
    // A page is a cursor into that index — the rows come off it in the order the
    // page states, so nothing sorts and the distinct subjects are adjacent — and
    // the anti-join that drops a subject a document row stands at is a seek of
    // `documents_path` per row rather than a read of the documents table. What
    // the page does not carry is a seek bound: a scope, a kind and a standing row
    // are all filters on rows the index order already reached, so a scope holding
    // no unaccounted subject costs one pass over the findings index. That is the
    // measured trade — the findings table holds a vault's defective documents
    // rather than its documents — and a narrower bound is a second index this
    // table has no reader asking for.
    let prefix = norn_store::DirectoryPrefix::new("one").expect("a directory");
    for scope in [
        norn_store::SubjectScope::Vault,
        norn_store::SubjectScope::Subtree(&path("one/glossary.md")),
        norn_store::SubjectScope::Under(&prefix),
    ] {
        for order in [
            norn_store::StoredPathOrder::Sensitive,
            norn_store::StoredPathOrder::AsciiCaseInsensitive,
        ] {
            let subjects = plan(
                request
                    .emitted_plan(ExplainedStatement::FindingSubjectsWithoutRows(
                        scope,
                        &[
                            FindingKind::PathNamesNoDocument,
                            FindingKind::BodyBytesNotUtf8,
                        ],
                        order,
                    ))
                    .expect("a query plan"),
            );
            subjects.assert_no_table_scan();
            subjects.assert_no_full_scan_of("documents");
            subjects.assert_searches("documents");
            subjects.assert_searches_through("findings", Access::Index("findings_path"));
            subjects.assert_searches_through("documents", Access::Index("documents_path"));
            subjects.assert_no_temp_btree();
            // The cursor is a bound on `findings_path` rather than a filter over
            // it, so a page seeks the index in every scope and both orders —
            // which is what makes the pass one per prune rather than one per
            // page of it. What the two orders differ in is how much of the index
            // that seek skips, not whether there is one: a scope the vault
            // spells as it stores it narrows the seek to the scope's own range,
            // while a vault that folds ASCII case bounds its scope under the
            // folding against an index that orders bytewise, so there the fold
            // is a filter over the rows the cursor's seek reaches.
            subjects.assert_searches("findings");
            subjects.assert_no_full_scan_of("findings");
        }
    }

    // Every statement the plan seam names carries a bar, and `barred_by` is
    // where that is stated: its `match` is exhaustive, so a variant added to
    // `ExplainedStatement` does not compile until its author names the test that
    // covers it. Every test is named here, so a variant cannot be routed to a
    // bar that does not exist.
    //
    // The statements the bars judge are the ones named inline above and in
    // the heal bar, and the lists the other bars iterate. Together they cover
    // every slot of `ExplainedStatement::all` exactly once, so a statement
    // dropped from a list its bar iterates leaves a slot empty here rather
    // than leaving the bar one statement narrower without a word.
    let probe = request.class_probe("glossary").expect("a class stem");
    let resolution = request
        .target_class("glossary", &norn_store::AmbiguityIgnore::none())
        .expect("a suffix target");
    let subject = path("one/glossary.md");
    let path_key = PathKey::new("one/glossary.md").expect("a path key");
    let width = NonZeroUsize::MIN;
    let judged: Vec<ExplainedStatement<'_>> = [
        ExplainedStatement::SuffixCandidates(&resolution),
        ExplainedStatement::FindingsInClass(&probe),
        ExplainedStatement::PathDiscard(&path_key),
        ExplainedStatement::SubjectDiscard(&subject, DiscardScope::EveryKind),
        ExplainedStatement::TypedValueDiscard,
        ExplainedStatement::FindingSubjectsWithoutRows(
            norn_store::SubjectScope::Vault,
            &[FindingKind::PathNamesNoDocument],
            norn_store::StoredPathOrder::Sensitive,
        ),
        ExplainedStatement::StoredDocumentPage(
            norn_store::SubjectScope::Vault,
            norn_store::StoredPathOrder::Sensitive,
        ),
    ]
    .into_iter()
    .chain(crate::health::LINK_HEALTH)
    .chain(ENUMERATIONS.iter().copied())
    .chain(FEEDS.iter().copied())
    .chain(ExplainedStatement::point_reads(&subject))
    .chain(FINDING_DETAIL.iter().map(|detail| detail(width)))
    // The write registry is one slot here, and its own census is the bar's.
    .chain([ExplainedStatement::Write(WriteStatement::all()[0])])
    .collect();
    let mut slots: Vec<usize> = judged.iter().map(|statement| statement.slot()).collect();
    slots.sort_unstable();
    assert_eq!(
        slots,
        (0..norn_store::STATEMENTS).collect::<Vec<usize>>(),
        "the statements the bars judge do not cover every slot of the seam exactly once, so \
         some statement is judged twice or by no bar at all: {judged:?}"
    );
    let bars: std::collections::BTreeSet<&str> = judged.into_iter().map(barred_by).collect();
    assert_eq!(
        bars,
        [
            "a_class_read_seeks_the_suffix_key_its_root_probes",
            "a_feed_page_walks_its_covering_index_and_reads_no_row",
            "a_finding_detail_chunk_seeks_the_primary_key_its_ids_lead",
            "a_heal_page_seeks_the_index_that_holds_its_order",
            "a_keyed_point_read_seeks_the_index_its_key_is_a_bound_for",
            "a_pins_typed_value_clear_reads_only_the_rows_that_hold_one",
            "an_enumeration_page_reaches_its_first_row_without_reading_the_rows_ahead_of_it",
            "every_findings_maintenance_statement_searches_the_index_its_parameters_are_bounds_for",
            "every_write_path_statement_reaches_its_rows_by_a_seek",
            "the_link_health_judgment_seeks_every_row_it_reads",
            "the_path_discard_seeks_finding_paths_path_key",
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<&str>>()
    );
}

/// **The path discard is one equality seek of `finding_paths_path_key`.** An
/// increment runs it once per path its changeset writes or kills, so a path it
/// does not name costs nothing, and a discard that read the membership table
/// end to end would make every changed path cost every path-keyed finding the
/// store holds. The bar is judged on the row that searches `finding_paths`: the
/// index it runs through and its equality constraint, which is what separates
/// the seek from a range over the same index. Each finding is then reached by
/// row id, and its detail rows go by the cascades' own primary-key seeks.
///
/// Control: with `finding_paths_path_key` dropped, the same statement no longer
/// seeks it, and the bar says so.
#[test]
fn the_path_discard_seeks_finding_paths_path_key() {
    let scratch = Scratch::new("path-discard-plan");
    let mut store = scratch.open();
    let key = PathKey::new("dir/t.md").expect("a path key");
    let judge = |store: &mut Store| {
        let discard = plan(
            store
                .begin_request()
                .emitted_plan(ExplainedStatement::PathDiscard(&key))
                .expect("a query plan for the path discard"),
        );
        discard.assert_no_full_scan_of("findings");
        discard.assert_no_full_scan_of("finding_paths");
        // The cascade into `finding_paths` searches it too, by `finding`, so
        // the seek is judged on the one step that runs through the index.
        assert!(
            discard.searches_of("finding_paths").iter().any(|row| {
                row.access() == Some(Access::Index("finding_paths_path_key"))
                    && row.constraint() == Some("(path_key=?)")
            }),
            "no step seeks `finding_paths_path_key` by equality on `path_key`: {:?}\n\
             emitted SQL: {}",
            discard.rows(),
            discard.sql()
        );
        discard.assert_searches_through("findings", Access::RowId);
        assert_cascade_seeks_the_primary_key(&discard, "finding_candidates");
        assert_cascade_seeks_the_primary_key(&discard, "finding_classes");
        assert_cascade_seeks_the_primary_key(&discard, "finding_paths");
    };
    judge(&mut store);

    induced_failure::execute_out_of_band(&mut store, "DROP INDEX finding_paths_path_key")
        .expect("dropping the path-key index");
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| judge(&mut store)));
    assert!(
        failed.is_err(),
        "the bar held with `finding_paths_path_key` dropped"
    );
}

/// **A pillar's enumeration is a seek, not a pass over what was already
/// drained.** Each of these four pages is drained end to end by a caller
/// accounting for a whole pillar, so a page that reaches its first row by
/// stepping over the rows ahead of it makes that drain cost the pillar once per
/// page of it.
///
/// The bar reads what each page reaches its first row through:
///
/// - The findings page seeks `findings.id`, which is the row id, so the order
///   the page states is the order the primary key already holds and nothing
///   sorts.
/// - The tombstone page seeks `tombstones_path`, which is unique, so `path`
///   orders the table totally and the page states that order.
/// - The suffix-key page seeks `documents_path`, unique for the same reason, and
///   reads the key column off the row that seek reached.
/// - The vocabulary page hands its bound to the full-text index's own module,
///   which is what a virtual table's index selection is. A module given no
///   constraint reports the pair `0:` and is a read of everything; this one
///   reports a chosen index, and the assertions below are what say so.
#[test]
fn an_enumeration_page_reaches_its_first_row_without_reading_the_rows_ahead_of_it() {
    let scratch = Scratch::new("enumeration-plans");
    let mut store = scratch.open();
    let mut request = store.begin_request();
    write_documents(
        &mut request,
        &[
            document("one/glossary.md", "hash-1", "alpha beta\n"),
            document("two/glossary.md", "hash-2", "beta gamma\n"),
        ],
    );
    request
        .record_finding(&path_names_no_document_in_class(
            "one/glossary.md",
            "glossary",
            "glossary/",
            &[],
            2,
        ))
        .expect("recording a finding");
    record_death(&mut request, &path("three/gone.md"), Provenance::HealPrune);

    for statement in ENUMERATIONS {
        let page = plan(
            request
                .emitted_plan(*statement)
                .expect("a query plan for an enumeration page"),
        );
        page.assert_no_full_scan();
        page.assert_no_temp_btree();
    }

    // The three pages over ordinary tables name the index the seek runs through,
    // which a plan over a virtual table cannot: a module reports which index it
    // chose by number and never by name.
    plan(
        request
            .emitted_plan(ExplainedStatement::StoredFindingPage)
            .expect("a query plan"),
    )
    .assert_searches("findings");
    let tombstones = plan(
        request
            .emitted_plan(ExplainedStatement::StoredTombstonePage)
            .expect("a query plan"),
    );
    tombstones.assert_searches("tombstones");
    tombstones.assert_searches_through("tombstones", Access::Index("tombstones_path"));
    let suffix_keys = plan(
        request
            .emitted_plan(ExplainedStatement::StoredSuffixKeyPage)
            .expect("a query plan"),
    );
    suffix_keys.assert_searches("documents");
    suffix_keys.assert_searches_through("documents", Access::Index("documents_path"));
}

/// **The point-read census is exactly the statements that say they are point
/// reads.** [`ExplainedStatement::point_reads`] is a list, and a list can be
/// short: an edit that drops an entry and shrinks the count beside it compiles,
/// and every bar that iterates the census then judges one statement fewer
/// without a word.
///
/// So the two declarations are walked against each other. [`ExplainedStatement::all`]
/// enumerates the seam, [`ExplainedStatement::slot`] is exhaustive over the enum
/// and says where each statement stands in that enumeration, and this reads the
/// point reads out of it and demands the census be that set — same count, same
/// members, same order.
#[test]
fn the_point_read_census_holds_every_statement_that_says_it_is_one() {
    let scratch = Scratch::new("point-read-census");
    let mut store = scratch.open();
    let request = store.begin_request();
    let subject = path("one/glossary.md");
    let probe = request.class_probe("glossary").expect("a class stem");
    let kinds = [FindingKind::PathNamesNoDocument];

    let resolution = request
        .target_class("glossary", &norn_store::AmbiguityIgnore::none())
        .expect("a suffix target");
    let path_key = PathKey::new("one/glossary.md").expect("a path key");
    let every = ExplainedStatement::all(
        &subject,
        &resolution,
        &probe,
        &path_key,
        &kinds,
        NonZeroUsize::MIN,
    );
    assert_eq!(every.len(), norn_store::STATEMENTS);
    for (position, statement) in every.iter().enumerate() {
        assert_eq!(
            statement.slot(),
            position,
            "the enumeration and the slot a statement claims disagree: {statement:?}"
        );
    }

    let declared: Vec<ExplainedStatement<'_>> = every
        .into_iter()
        .filter(|statement| statement.is_point_read())
        .collect();
    assert_eq!(
        declared,
        ExplainedStatement::point_reads(&subject).to_vec(),
        "a statement says it is a point read and the census does not hold it, or holds \
         one that does not — so the point-read bar judges a set nothing else agrees with"
    );
}

/// How the bar judges one keyed point read.
struct PointReadBar {
    /// The table the statement reaches its rows in.
    table: &'static str,
    /// What the seek runs through: a declared index, or for `meta` — which is
    /// `WITHOUT ROWID`, so its rows live in the key b-tree itself — the
    /// primary key.
    access: Access<'static>,
    /// The constraint an equality seek of this statement's key prints. It is
    /// what separates a point read from a range: `path>?` opens a cursor on
    /// the same index as `path=?` and reports the same index name, so a bar
    /// that read the name alone would green a reader that seeks to a key and
    /// then walks the rest of the table.
    constraint: &'static str,
    /// Whether the statement's stated order is one no index over its table
    /// holds, so the rows the seek reached are sorted after it. Only the
    /// findings read does: it orders by generation then row key, and
    /// `findings_path` orders by path. Every other entry states an order its
    /// own index already holds, and the temporary-B-tree bar is what says so.
    sorts: bool,
}

/// What the bar expects of one statement, or [`None`] where the statement is a
/// page or a probe another bar holds.
///
/// The `match` is exhaustive over [`ExplainedStatement`], so a variant added to
/// that seam has to say whether this bar judges it, and the census the bar
/// iterates is [`ExplainedStatement::point_reads`] rather than a second list
/// kept here.
fn point_read_bar(statement: ExplainedStatement<'_>) -> Option<PointReadBar> {
    let seek = |table, index, constraint| {
        Some(PointReadBar {
            table,
            access: Access::Index(index),
            constraint,
            sorts: false,
        })
    };
    match statement {
        // The document row and the row a facts snapshot opens on both seek the
        // unique path index, so one seek reaches the one row a path can have.
        ExplainedStatement::StoredDocument(_) | ExplainedStatement::StoredFactsDocument(_) => {
            seek("documents", "documents_path", "(path=?)")
        }
        // Each fact read seeks its own table's `(document, ordinal)` index, so
        // it costs one document's rows and comes off that index in the ordinal
        // order the reader states.
        ExplainedStatement::DocumentLinks => {
            seek("links", "links_document_ordinal", "(document=?)")
        }
        // The link keys are reached by the link rows the document's own index
        // hands back, each link's keys by the link, in link and key order.
        ExplainedStatement::DocumentLinkKeys => seek("link_keys", "link_keys_link", "(link=?)"),
        ExplainedStatement::DocumentHeadings => {
            seek("headings", "headings_document_ordinal", "(document=?)")
        }
        ExplainedStatement::DocumentBlocks => {
            seek("blocks", "blocks_document_ordinal", "(document=?)")
        }
        ExplainedStatement::DocumentTags => seek(
            "document_tags",
            "document_tags_document_ordinal",
            "(document=?)",
        ),
        // The field pillar is `WITHOUT ROWID` and its primary key leads with
        // the document, so its read is a seek of that key, in the key and
        // ordinal order the reader states.
        ExplainedStatement::DocumentFields => Some(PointReadBar {
            table: "document_fields",
            access: Access::PrimaryKey,
            constraint: "(document=?)",
            sorts: false,
        }),
        ExplainedStatement::StoredTombstone(_) => seek("tombstones", "tombstones_path", "(path=?)"),
        // The subject's own findings, and the one entry that sorts.
        ExplainedStatement::StoredFindings(_) => Some(PointReadBar {
            table: "findings",
            access: Access::Index("findings_path"),
            constraint: "(path=?)",
            sorts: true,
        }),
        // Both pinned-scalar reads seek `meta`'s primary key, which is the
        // table itself: `meta` is `WITHOUT ROWID`.
        ExplainedStatement::VaultSchemaPin | ExplainedStatement::WriteGeneration => {
            Some(PointReadBar {
                table: "meta",
                access: Access::PrimaryKey,
                constraint: "(key=?)",
                sorts: false,
            })
        }
        ExplainedStatement::SuffixCandidates(_)
        | ExplainedStatement::FindingsInClass(_)
        | ExplainedStatement::SubjectDiscard(..)
        | ExplainedStatement::TypedValueDiscard
        | ExplainedStatement::FindingSubjectsWithoutRows(..)
        | ExplainedStatement::StoredDocumentPage(..)
        | ExplainedStatement::StoredFindingPage
        | ExplainedStatement::StoredTombstonePage
        | ExplainedStatement::StoredSuffixKeyPage
        | ExplainedStatement::IndexedTermPage
        | ExplainedStatement::DocumentFeedPage
        | ExplainedStatement::TombstoneFeedPage
        | ExplainedStatement::FindingCandidates(_)
        | ExplainedStatement::FindingClasses(_)
        | ExplainedStatement::PathDiscard(_)
        | ExplainedStatement::FindingPaths(_)
        | ExplainedStatement::LinkHealthLinks
        | ExplainedStatement::LinkHealthClassLinks
        | ExplainedStatement::LinkHealthPathLinks
        | ExplainedStatement::LinkHealthHeads
        | ExplainedStatement::LinkHealthTotals
        | ExplainedStatement::LinkHealthSuffixes
        | ExplainedStatement::LinkHealthAnchors
        | ExplainedStatement::LinkHealthWrittenLinks
        | ExplainedStatement::LinkHealthClassFindings
        | ExplainedStatement::LinkHealthFoundLinks
        | ExplainedStatement::LinkHealthDiscard
        | ExplainedStatement::LinkHealthOccupied
        | ExplainedStatement::Write(_) => None,
    }
}

/// **A keyed point read reaches its rows by an equality seek on the key it was
/// given.** Each of these answers about one subject a caller already holds — a
/// path, the document row that path resolved to, or one pinned key — so a read
/// that found its rows by stepping over the rows ahead of them would cost the
/// whole table for every question about one place.
///
/// Four assertions hold every statement, and a fifth holds eleven of the twelve:
///
/// - It does not read its table end to end, and it searches that table.
/// - The step that searches the table runs through [`PointReadBar::access`]
///   and carries an **equality** constraint. These two are what make it a bar
///   rather than a description: a search alone is not a point read, because a
///   range over the same index reports the same `SEARCH … USING INDEX` row,
///   and a range over a `WITHOUT ROWID` primary key the same `SEARCH … USING
///   PRIMARY KEY` row.
/// - It builds no temporary B-tree. This is the fifth assertion, and it holds
///   every statement but the one whose [`PointReadBar::sorts`] says its stated
///   order is one no index holds, so a reader that grew a sorter over an order
///   an index already gave it reddens here.
///
/// The census is [`ExplainedStatement::point_reads`], which lives beside the
/// statements rather than here: a bar with its own copy of the list is a bar
/// that greens whatever the copy forgot.
///
/// **The store holds no rows.** A plan is read off the statement text, and
/// nothing here runs `ANALYZE`, so the planner's choice is the same over an
/// empty table as over a full one — rows would be scenery.
#[test]
fn a_keyed_point_read_seeks_the_index_its_key_is_a_bound_for() {
    let scratch = Scratch::new("point-read-plans");
    let mut store = scratch.open();
    let request = store.begin_request();
    let subject = path("one/glossary.md");

    let census = ExplainedStatement::point_reads(&subject);
    assert_eq!(census.len(), norn_store::POINT_READS);
    for statement in census {
        assert!(
            statement.is_point_read(),
            "the census hands the point-read bar a statement another bar holds: {statement:?}"
        );
        let expected = point_read_bar(statement)
            .unwrap_or_else(|| panic!("the point-read bar states nothing for {statement:?}"));
        let read = plan(
            request
                .emitted_plan(statement)
                .expect("a query plan for a point read"),
        );
        read.assert_no_full_scan_of(expected.table);
        read.assert_searches(expected.table);
        read.assert_search_constraint(expected.table, expected.constraint);
        read.assert_searches_through(expected.table, expected.access);
        if !expected.sorts {
            read.assert_no_temp_btree();
        }
    }
}

/// **A pin's clear of the typed field values reads the rows that hold one, and
/// never the table.** A pin that moves the schema fingerprint clears every
/// typed value in its own transaction, and a clear that read the pillar end to
/// end would make every such pin cost every field row the vault holds — the
/// rows with no typed value included, which under a schema declaring no typed
/// field is all of them.
///
/// The clear is stated over `typed IS NOT NULL`, which is the typed index's own
/// predicate, so the index holds exactly the rows it clears. The plan reads that
/// index end to end, which is the bar: a scan of it is a scan of the typed rows
/// alone. The control is run in the same case: with the index dropped, the same
/// statement reads the table, and the bar says so.
#[test]
fn a_pins_typed_value_clear_reads_only_the_rows_that_hold_one() {
    let scratch = Scratch::new("typed-clear-plan");
    let mut store = scratch.open();
    let judge = |store: &mut norn_store::Store| {
        plan(
            store
                .begin_request()
                .emitted_plan(ExplainedStatement::TypedValueDiscard)
                .expect("a query plan for the typed clear"),
        )
    };
    let reads_the_typed_index = |read: &QueryPlan| {
        read.table_scans().is_empty()
            && read.rows().iter().any(|row| {
                row.scans() == Some("document_fields")
                    && row.index() == Some("document_fields_typed")
            })
    };

    let read = judge(&mut store);
    assert!(
        reads_the_typed_index(&read),
        "the typed clear reads something other than the typed index: {:?}\nemitted SQL: {}",
        read.rows(),
        read.sql()
    );

    induced_failure::execute_out_of_band(&mut store, "DROP INDEX document_fields_typed")
        .expect("dropping the typed index");
    let unindexed = judge(&mut store);
    assert!(
        !reads_the_typed_index(&unindexed),
        "the bar holds a clear with no typed index to read: {:?}",
        unindexed.rows()
    );
}

/// **A finding's detail is read by primary key, a chunk of ids at a time.**
/// Every findings read — one path's, one class's, and the page that drains the
/// table — collects each finding's candidate head, class memberships and path
/// keys through these three statements, once per chunk of the findings it
/// found. A chunk that reached its rows by reading the detail tables end to end
/// would make every findings read cost every finding the store holds.
///
/// All three tables are `WITHOUT ROWID` and keyed by `(finding, …)`, so the
/// ids lead each primary key: one seek per id, and the rows come off each seek
/// in the order the statement states, so nothing sorts. `finding_classes`
/// carries a second index as well, on `class_key`, which is the direction a
/// class read seeks; a chunk holds no class key, so the bar names the primary
/// key and a plan that answered out of the class index fails it.
///
/// The bar ranges over every chunk width a read can emit, from one id to
/// [`norn_store::FINDING_ID_CHUNK`]: the statement text is spelled by the width,
/// and a one-id list is a different statement to the planner than a longer one.
/// So each plan is judged together with the statement it was taken of, which
/// binds exactly as many ids as the width it was asked for: its last
/// placeholder is `?n` and there is no `?n+1`. A plan of some narrower
/// statement would pass every plan assertion here and say nothing about the
/// width a read runs.
///
/// A width above the chunk bound is not a statement any reader runs, and the
/// seam refuses to explain one. An empty chunk has no spelling at all: the
/// width is a [`NonZeroUsize`].
#[test]
fn a_finding_detail_chunk_seeks_the_primary_key_its_ids_lead() {
    let scratch = Scratch::new("finding-detail-plans");
    let mut store = scratch.open();
    let request = store.begin_request();

    for detail in FINDING_DETAIL {
        let table = match detail(NonZeroUsize::MIN) {
            ExplainedStatement::FindingCandidates(_) => "finding_candidates",
            ExplainedStatement::FindingClasses(_) => "finding_classes",
            ExplainedStatement::FindingPaths(_) => "finding_paths",
            other => panic!("`FINDING_DETAIL` names {other:?}, which is not a detail read"),
        };
        for ids in 1..=norn_store::FINDING_ID_CHUNK {
            let width = NonZeroUsize::new(ids).expect("the range starts at one");
            let chunk = plan(
                request
                    .emitted_plan(detail(width))
                    .expect("a query plan for a finding-detail chunk"),
            );
            assert!(
                chunk.sql().contains(&format!("?{ids}"))
                    && !chunk.sql().contains(&format!("?{}", ids + 1)),
                "the plan judged as a chunk of {ids} ids was taken of a statement that binds \
                 some other number of them: {}",
                chunk.sql()
            );
            chunk.assert_no_full_scan();
            chunk.assert_searches_through(table, Access::PrimaryKey);
            chunk.assert_search_constraint(table, "(finding=?)");
            chunk.assert_no_temp_btree();
        }
        let above = NonZeroUsize::new(norn_store::FINDING_ID_CHUNK + 1).expect("above one");
        assert!(
            matches!(
                request.emitted_plan(detail(above)),
                Err(StoreError::Bound { given, .. }) if given == above.get()
            ),
            "a chunk of {above} ids is not one a reader emits, and the seam explained it"
        );
    }
}

/// **Every enumeration is complete and drains one page at a time.** A pillar
/// read through a page of one is what says the cursor advances by exactly one
/// row: a cursor that does not advance repeats a row forever, and one that
/// advances too far drops the row after it.
#[test]
fn an_enumeration_drained_a_page_at_a_time_reaches_every_row() {
    let scratch = Scratch::new("enumeration-drain");
    let mut store = scratch.open();
    let mut request = store.begin_request();
    write_documents(
        &mut request,
        &[
            document("one/glossary.md", "hash-1", "alpha beta\n"),
            document("two/glossary.md", "hash-2", "beta gamma\n"),
        ],
    );
    for at in ["one/glossary.md", "two/glossary.md", "nowhere/absent.md"] {
        request
            .record_finding(&path_names_no_document_in_class(
                at,
                "glossary",
                "glossary/",
                &[],
                3,
            ))
            .expect("recording a finding");
    }
    for at in ["three/gone.md", "four/gone.md"] {
        record_death(&mut request, &path(at), Provenance::HealPrune);
    }

    let mut findings = Vec::new();
    let mut cursor = None;
    while let Some((next, finding)) = request
        .stored_findings_after(cursor, 1)
        .expect("a page of findings")
        .into_iter()
        .next()
    {
        findings.push(finding.path.as_str().to_string());
        cursor = Some(next);
        assert!(findings.len() < 32, "the findings cursor did not advance");
    }
    assert_eq!(
        findings,
        vec!["one/glossary.md", "two/glossary.md", "nowhere/absent.md"],
        "the enumeration reaches a finding at a path no document row stands at, which is the \
         finding no keyed reader can be asked for"
    );

    let mut deaths: Vec<String> = Vec::new();
    let mut after: Option<norn_store::DocumentPath> = None;
    while let Some(tombstone) = request
        .stored_tombstones_after(after.as_ref(), 1)
        .expect("a page of tombstones")
        .into_iter()
        .next()
    {
        deaths.push(tombstone.path.as_str().to_string());
        after = Some(tombstone.path);
        assert!(deaths.len() < 32, "the tombstone cursor did not advance");
    }
    assert_eq!(deaths, vec!["four/gone.md", "three/gone.md"]);

    let mut terms: Vec<String> = Vec::new();
    let mut term_cursor: Option<String> = None;
    while let Some(term) = request
        .indexed_terms_after(term_cursor.as_deref(), 1)
        .expect("a page of indexed terms")
        .into_iter()
        .next()
    {
        term_cursor = Some(term.term.clone());
        terms.push(term.term);
        assert!(terms.len() < 32, "the indexed-term cursor did not advance");
    }
    let mut keyed: Vec<(String, String, String)> = Vec::new();
    let mut keyed_after: Option<norn_store::DocumentPath> = None;
    while let Some(stored) = request
        .suffix_keys_after(keyed_after.as_ref(), 1)
        .expect("a page of stored suffix keys")
        .into_iter()
        .next()
    {
        keyed.push((stored.path.as_str().to_string(), stored.raw, stored.folded));
        keyed_after = Some(stored.path);
        assert!(keyed.len() < 32, "the suffix-key cursor did not advance");
    }
    let key = |path: &str, key: &str| (path.to_string(), key.to_string(), key.to_string());
    assert_eq!(
        keyed,
        vec![
            key("one/glossary.md", "glossary/one/"),
            key("two/glossary.md", "glossary/two/"),
        ],
        "the enumeration reaches every row's stored key beside the path that has to produce it, \
         which is the pair no keyed read hands back"
    );

    assert_eq!(terms, vec!["alpha", "beta", "gamma"]);
    let beta = request
        .indexed_terms_after(Some("alpha"), 1)
        .expect("a page of indexed terms")
        .into_iter()
        .next()
        .expect("the index holds a term after `alpha`");
    assert_eq!(
        (beta.documents, beta.occurrences),
        (2, 2),
        "the vocabulary reports what the index holds about a term, and `beta` is in both bodies"
    );
}

/// A page bound outside what an enumeration accepts is refused rather than
/// clamped, which is the same answer a document page gives.
#[test]
fn an_enumeration_refuses_a_page_bound_it_does_not_hold() {
    let scratch = Scratch::new("enumeration-bound");
    let mut store = scratch.open();
    let request = store.begin_request();
    for limit in [0, norn_store::MAX_PAGE + 1] {
        assert!(request.stored_findings_after(None, limit).is_err());
        assert!(request.stored_tombstones_after(None, limit).is_err());
        assert!(request.suffix_keys_after(None, limit).is_err());
        assert!(request.indexed_terms_after(None, limit).is_err());
        assert!(request.changed_documents_after(None, limit).is_err());
        assert!(request.changed_tombstones_after(None, limit).is_err());
    }
}

/// **A change-feed page is a walk of an index and never a read of a row.** A
/// lane-2 consumer drains the whole feed to catch up and drains it again on
/// every wake, so a page that reached the table would make catching up cost the
/// pillar rather than the pillar's key columns.
///
/// The bar reads three things off each plan:
///
/// - The page **searches** rather than scans, which is the row-value floor
///   working as a seek. A covering index read end to end reports `SCAN … USING
///   COVERING INDEX` and would fail the scan bars here, so a page that lost its
///   seek fails whether or not it kept the index.
/// - It names the covering index the feed's columns live in, so a page that
///   answered out of some other index — and therefore read the row for the
///   fingerprints — fails.
/// - Nothing sorts. The index holds `generation` then `path`, which is the order
///   the page states, so a temporary B-tree here means the order and the index
///   have come apart.
#[test]
fn a_feed_page_walks_its_covering_index_and_reads_no_row() {
    let scratch = Scratch::new("feed-plans");
    let mut store = scratch.open();
    let mut request = store.begin_request();
    write_documents(
        &mut request,
        &[
            document("one/glossary.md", "hash-1", "alpha beta\n"),
            document("two/glossary.md", "hash-2", "beta gamma\n"),
        ],
    );
    record_death(&mut request, &path("three/gone.md"), Provenance::HealPrune);

    for statement in FEEDS {
        let page = plan(
            request
                .emitted_plan(*statement)
                .expect("a query plan for a change-feed page"),
        );
        page.assert_no_table_scan();
        page.assert_no_full_scan();
        page.assert_no_temp_btree();
        let (table, index) = match statement {
            ExplainedStatement::DocumentFeedPage => ("documents", "documents_change_feed"),
            ExplainedStatement::TombstoneFeedPage => ("tombstones", "tombstones_change_feed"),
            other => panic!("`FEEDS` names {other:?}, which is not a feed"),
        };
        page.assert_searches(table);
        page.assert_searches_through(table, Access::Index(index));
        // The index answers the page whole. SQLite says so itself, on the row
        // that searches the feed's table, and it is the difference between a
        // page that costs its own columns and one that costs a row lookup per
        // row it returns.
        assert!(
            page.searches_of(table).iter().any(|row| row
                .detail
                .contains(&format!("USING COVERING INDEX {index} "))),
            "the page reads the row for columns the index was declared to carry: {:?}",
            page.rows()
        );
    }
}

/// **A feed drained a page at a time reaches every row, in generation order,
/// once.** The claim the composite cursor exists for is here: one changeset
/// stamps many rows with one generation, so a cursor that carried a generation
/// alone would repeat the changeset it stopped inside or skip the rest of it.
///
/// The fixture is arranged for exactly that. Two changesets write four documents
/// between them and two record deaths, so each generation holds more than one
/// row of each feed — a drain a page at a time therefore stops inside a
/// changeset twice per feed, which is the position a bare generation cannot
/// spell.
#[test]
fn a_feed_drained_a_page_at_a_time_reaches_every_row_in_generation_order() {
    let scratch = Scratch::new("feed-drain");
    let mut store = scratch.open();
    let mut request = store.begin_request();
    write_documents(
        &mut request,
        &[
            document("second/b.md", "hash-b", "beta\n"),
            document("first/a.md", "hash-a", "alpha\n"),
        ],
    );
    write_documents(
        &mut request,
        &[
            document("fourth/d.md", "hash-d", "delta\n"),
            document("third/c.md", "hash-c", "gamma\n"),
        ],
    );
    // One changeset, so the two deaths share a generation and the death feed's
    // cursor has to break the tie exactly as the document feed's does.
    let deaths_at = request
        .apply_increment(
            norn_store::IncrementProvenance::Derived,
            ["gone/y.md", "gone/x.md"].map(|at| norn_store::Change::Death {
                path: path(at),
                provenance: Provenance::HealPrune,
            }),
            &[],
            &norn_store::ContentModel::none(),
        )
        .expect("recording two deaths in one changeset")
        .generation
        .expect("a changeset that recorded deaths took a generation");

    let mut documents: Vec<(i64, String)> = Vec::new();
    let mut cursor: Option<norn_store::FeedCursor> = None;
    while let Some((next, fed)) = request
        .changed_documents_after(cursor.as_ref(), 1)
        .expect("a page of the document feed")
        .pop()
    {
        documents.push((fed.generation, fed.path.as_str().to_string()));
        cursor = Some(next);
        assert!(
            documents.len() < 32,
            "the document feed cursor did not advance"
        );
    }
    let generations: Vec<i64> = documents
        .iter()
        .map(|(generation, _)| *generation)
        .collect();
    let paths: Vec<&str> = documents.iter().map(|(_, at)| at.as_str()).collect();
    assert_eq!(
        paths,
        vec!["first/a.md", "second/b.md", "fourth/d.md", "third/c.md"],
        "the feed is not ordered by generation with the path breaking the tie"
    );
    assert_eq!(
        generations[0], generations[1],
        "the two documents of one changeset are not at one generation, so the fixture no longer \
         puts the cursor inside a changeset"
    );
    assert!(
        generations[1] < generations[2],
        "the second changeset did not take a later generation"
    );

    // A page wide enough for the whole feed reads the same rows in the same
    // order, so what the page-of-one drain proves is the cursor rather than the
    // statement.
    let whole: Vec<String> = request
        .changed_documents_after(None, norn_store::MAX_PAGE)
        .expect("the whole document feed")
        .into_iter()
        .map(|(_, fed)| fed.path.as_str().to_string())
        .collect();
    assert_eq!(whole, paths);

    let mut deaths: Vec<String> = Vec::new();
    let mut cursor: Option<norn_store::FeedCursor> = None;
    while let Some((next, fed)) = request
        .changed_tombstones_after(cursor.as_ref(), 1)
        .expect("a page of the death feed")
        .pop()
    {
        assert_eq!(
            fed.last_content_hash, None,
            "a death at a path nothing derived carries no hash to compare against"
        );
        assert_eq!(
            fed.generation, deaths_at,
            "a death is not at the generation of the changeset that recorded it"
        );
        deaths.push(fed.path.as_str().to_string());
        cursor = Some(next);
        assert!(deaths.len() < 32, "the death feed cursor did not advance");
    }
    assert_eq!(
        deaths,
        vec!["gone/x.md", "gone/y.md"],
        "the two deaths of one changeset did not come off in the path order that breaks their tie"
    );
}

/// **A path killed and written again in one changeset stands in the document
/// feed alone.** The upsert clears the same-path tombstone — the death's row
/// included, when the death landed earlier in the same changeset — so the two
/// feeds are disjoint over stored paths and the `(generation, path)` position
/// holds one row, not two. The document-outranks-death tie-break stays specified for a
/// merge that ever presents both, and this is the case that keeps it vacuous.
#[test]
fn a_path_killed_and_rewritten_in_one_changeset_stands_only_in_the_document_feed() {
    let scratch = Scratch::new("feed-tie");
    let mut store = scratch.open();
    let mut request = store.begin_request();
    let at = path("revived.md");
    write_document(&mut request, &document(at.as_str(), "hash-1", "a body\n"));
    request
        .apply_increment(
            norn_store::IncrementProvenance::Derived,
            [
                norn_store::Change::Death {
                    path: at.clone(),
                    provenance: Provenance::HealPrune,
                },
                norn_store::Change::Upsert(document(at.as_str(), "hash-2", "another body\n")),
            ],
            &[],
            &norn_store::ContentModel::none(),
        )
        .expect("killing and rewriting one path in one changeset");

    let mut living = request
        .changed_documents_after(None, norn_store::MAX_PAGE)
        .expect("the document feed");
    let dead = request
        .changed_tombstones_after(None, norn_store::MAX_PAGE)
        .expect("the death feed");
    assert_eq!(living.len(), 1, "the document feed is not one row");
    assert!(
        dead.is_empty(),
        "the death feed still carries a death the same changeset's rewrite outlived: {dead:?}"
    );
    let (_, fed) = living.pop().expect("a row of the document feed");

    assert_eq!(fed.path, at);
    assert!(
        request
            .stored_tombstone(&at)
            .expect("reading a tombstone")
            .is_none(),
        "the rewrite did not clear the tombstone the same changeset's death recorded"
    );

    // The changeset's end state is the document it wrote.
    assert_eq!(
        fed.content_hash, "hash-2",
        "the document feed does not carry the hash the changeset wrote"
    );
    assert_eq!(
        request
            .stored_facts(&at)
            .expect("reading the rewritten path")
            .map(|facts| facts.document.content_hash),
        Some("hash-2".to_string()),
        "the changeset's end state is not the document it wrote"
    );
}

/// **A cursor comes apart into the two values a consumer records, and those two
/// values go back together into the position it was.** A consumer that keeps
/// progress across runs keeps it at rest beside the store epoch, so the drain it
/// resumes starts from a pair it read back rather than from a handle it held.
#[test]
fn a_recorded_feed_position_rebuilds_into_the_cursor_it_was_taken_from() {
    let scratch = Scratch::new("feed-cursor-round-trip");
    let mut store = scratch.open();
    let mut request = store.begin_request();
    write_documents(
        &mut request,
        &[
            document("first/a.md", "hash-a", "alpha\n"),
            document("second/b.md", "hash-b", "beta\n"),
        ],
    );
    write_document(&mut request, &document("third/c.md", "hash-c", "gamma\n"));

    let (taken, _) = request
        .changed_documents_after(None, 1)
        .expect("the first page of the document feed")
        .pop()
        .expect("a first row");
    let rebuilt = norn_store::FeedCursor::at(taken.generation(), taken.path().clone());
    assert_eq!(
        rebuilt, taken,
        "a cursor rebuilt from the values it hands out is not the position it was"
    );

    let from_the_rebuilt: Vec<String> = request
        .changed_documents_after(Some(&rebuilt), norn_store::MAX_PAGE)
        .expect("the rest of the feed, from the rebuilt position")
        .into_iter()
        .map(|(_, fed)| fed.path.as_str().to_string())
        .collect();
    assert_eq!(
        from_the_rebuilt,
        vec!["second/b.md", "third/c.md"],
        "a drain resumed from a rebuilt position does not read what the position it was resumes"
    );
}

/// **The feed projects the fingerprints a consumer triages on, and they describe
/// the parts they name.** A body hash equal for two documents with different
/// frontmatter is what lets a body-deriving consumer skip a fetch; a projection
/// hash that moved when only the frontmatter did is what makes a
/// frontmatter-deriving consumer take one.
#[test]
fn the_feed_projects_a_fingerprint_per_part_a_consumer_derives_from() {
    let scratch = Scratch::new("feed-fingerprints");
    let mut store = scratch.open();
    let mut request = store.begin_request();

    let plain = document("plain.md", "hash-plain", "one body\n");
    let mut titled = document("titled.md", "hash-titled", "one body\n");
    titled = titled.with_frontmatter(titled_frontmatter(), &norn_store::ContentModel::none());
    write_documents(&mut request, &[plain, titled]);

    let fed: std::collections::BTreeMap<String, norn_store::FeedDocument> = request
        .changed_documents_after(None, norn_store::MAX_PAGE)
        .expect("the document feed")
        .into_iter()
        .map(|(_, fed)| (fed.path.as_str().to_string(), fed))
        .collect();
    let plain = &fed["plain.md"];
    let titled = &fed["titled.md"];

    assert_ne!(
        plain.content_hash, titled.content_hash,
        "two different documents share a content hash"
    );
    assert_eq!(
        plain.body_hash, titled.body_hash,
        "two documents with one body do not share a body hash, so a body-deriving consumer \
         re-reads a document whose frontmatter alone moved"
    );
    assert_eq!(
        plain.frontmatter_projection_hash, None,
        "a document with no frontmatter projection records a hash of one"
    );
    assert!(
        titled.frontmatter_projection_hash.is_some(),
        "a document with a frontmatter projection records no hash of it"
    );

    // Re-deriving the body alone moves the body hash and leaves the projection
    // hash where it was, which is the discrimination the two columns exist for.
    let mut rewritten = document("titled.md", "hash-titled-2", "another body\n");
    rewritten = rewritten.with_frontmatter(titled_frontmatter(), &norn_store::ContentModel::none());
    write_document(&mut request, &rewritten);
    let after = request
        .changed_documents_after(None, norn_store::MAX_PAGE)
        .expect("the document feed")
        .into_iter()
        .map(|(_, fed)| fed)
        .find(|fed| fed.path.as_str() == "titled.md")
        .expect("the re-derived document is in the feed");
    assert_ne!(
        after.body_hash, titled.body_hash,
        "the body hash did not move"
    );
    assert_eq!(
        after.frontmatter_projection_hash, titled.frontmatter_projection_hash,
        "the frontmatter projection hash moved for a change the frontmatter did not make"
    );
}

/// The frontmatter the fingerprint bar writes twice, unchanged between the two
/// derivations it is written in.
fn titled_frontmatter() -> Option<norn_store::FrontmatterValue> {
    Some(norn_store::FrontmatterValue::Map(vec![(
        "title".to_string(),
        norn_store::FrontmatterValue::String("a title".to_string()),
    )]))
}

/// **A heal's page is a seek, in every scope and both orders.** The merge that
/// heals a vault pages the rows through one of these three statements and walks
/// the files beside them, so a page that reads more of the table than it returns
/// makes the whole heal cost the vault once per page. The bar is the plan's:
/// each page reaches its first row through the index that already holds the
/// order the page states, and nothing sorts — a page that sorts has read
/// everything the scope holds before it returns its first row.
#[test]
fn a_heal_page_seeks_the_index_that_holds_its_order() {
    let scratch = Scratch::new("heal-page-plans");
    let mut store = scratch.open();
    let mut request = store.begin_request();
    write_documents(
        &mut request,
        &[
            document("one/glossary.md", "hash-1", "a body\n"),
            document("one/nested/deep.md", "hash-2", "a body\n"),
            document("two/glossary.md", "hash-3", "a body\n"),
        ],
    );

    let root = path("one/glossary.md");
    let prefix = norn_store::DirectoryPrefix::new("one").expect("a directory");
    for scope in [
        norn_store::SubjectScope::Vault,
        norn_store::SubjectScope::Subtree(&root),
        norn_store::SubjectScope::Under(&prefix),
    ] {
        for order in [
            norn_store::StoredPathOrder::Sensitive,
            norn_store::StoredPathOrder::AsciiCaseInsensitive,
        ] {
            let page = plan(
                request
                    .emitted_plan(ExplainedStatement::StoredDocumentPage(scope, order))
                    .expect("a query plan"),
            );
            page.assert_no_table_scan();
            page.assert_no_full_scan_of("documents");
            page.assert_searches("documents");
            // The order the page states is the order an index holds, and which
            // index that is, is the vault's proven case behaviour: a bytewise
            // vault pages through the unique path index, and a vault that folds
            // ASCII case pages through the index declared under the same fold.
            page.assert_searches_through(
                "documents",
                Access::Index(match order {
                    norn_store::StoredPathOrder::Sensitive => "documents_path",
                    norn_store::StoredPathOrder::AsciiCaseInsensitive => "documents_path_nocase",
                }),
            );
            // The access bar names the index the page searches, and this names
            // the range it opens there: a lower bound in every scope, and a
            // bounded scope's upper edge beside it. A search of the same index
            // that lost its lower bound would still answer the access bar, and
            // every page would read the index from its start.
            page.assert_search_constraint(
                "documents",
                match scope {
                    norn_store::SubjectScope::Vault => "(path>?)",
                    norn_store::SubjectScope::Subtree(_) | norn_store::SubjectScope::Under(_) => {
                        "(path>? AND path<?)"
                    }
                },
            );
            page.assert_no_temp_btree();
        }
    }
}

/// **A paged statement names its cursor as the floor it seeks from.**
///
/// This bar reads the statement rather than its plan, because the plan cannot
/// answer the question. A bounded scope's plan reports `SEARCH … (path>? AND
/// path<?)` whether the seek took the cursor or took the scope's own floor and
/// left the cursor a filter — the two are byte-identical as plan text, and the
/// difference between them is the whole defect: one is a page, the other is a
/// pass over everything already paged. So the shape is pinned where it is
/// visible, in the emitted SQL: the cursor is `COALESCE`'s first argument, which
/// is what makes it the bound.
///
/// **What this is and is not.** It pins a spelling, not a cost. A rewrite that
/// seeks from the cursor by some other spelling would fail it, and a reviewer
/// would have to move the bar rather than route around it. That is why it is
/// half of a pair: this bar catches the spelling the defect had, and
/// [`a_paged_reader_costs_a_line_in_the_rows_it_drained`] catches a spelling
/// nobody anticipated by measuring what the engine spent instead of reading
/// what the crate wrote.
///
/// **A composite cursor is the same rule over a pair.** The change feeds seek
/// from `(generation, path)`, because one changeset stamps many rows with one
/// generation and a generation alone is therefore not a position. Their floor is
/// a row value and each half of it coalesces, so what this bar reads is
/// unchanged: the cursor's generation is the first `COALESCE`'s first argument
/// and its path is the second's, and a spelling that led with the pillar's own
/// floor would demote both halves to filters exactly as a scalar page's would.
#[test]
fn a_paged_statement_binds_its_cursor_as_the_floor_it_seeks_from() {
    let scratch = Scratch::new("paged-statement-text");
    let mut store = scratch.open();
    let mut request = store.begin_request();
    write_documents(
        &mut request,
        &[document("one/glossary.md", "hash-1", "a body\n")],
    );

    let root = path("one/glossary.md");
    let prefix = norn_store::DirectoryPrefix::new("one").expect("a directory");
    let mut judged = 0;
    for scope in [
        norn_store::SubjectScope::Vault,
        norn_store::SubjectScope::Subtree(&root),
        norn_store::SubjectScope::Under(&prefix),
    ] {
        for order in [
            norn_store::StoredPathOrder::Sensitive,
            norn_store::StoredPathOrder::AsciiCaseInsensitive,
        ] {
            for statement in [
                ExplainedStatement::StoredDocumentPage(scope, order),
                ExplainedStatement::FindingSubjectsWithoutRows(
                    scope,
                    &[
                        FindingKind::PathNamesNoDocument,
                        FindingKind::BodyBytesNotUtf8,
                    ],
                    order,
                ),
            ] {
                let sql = request.emitted_plan(statement).expect("a query plan").sql;
                assert!(
                    sql.contains("WHERE path > COALESCE(?1,")
                        || sql.contains("WHERE path >= COALESCE(?1,"),
                    "the WHERE does not open on the coalesced cursor, so the seek \
                     starts where the scope does and steps over every row already paged: {sql}"
                );
                assert!(
                    !sql.contains("WHERE (?1 IS NULL"),
                    "the page opens on the cursor as a filter, which is the shape a seek \
                     cannot use: {sql}"
                );
                judged += 1;
            }
        }
    }
    // The four enumerations bind the same shape over their own column: the
    // cursor is `COALESCE`'s first argument, and the floor it falls back to is
    // below every key the column holds.
    for statement in ENUMERATIONS {
        let sql = request.emitted_plan(*statement).expect("a query plan").sql;
        assert!(
            sql.contains("WHERE id > COALESCE(?1,")
                || sql.contains("WHERE path > COALESCE(?1,")
                || sql.contains("WHERE term > COALESCE(?1,"),
            "the WHERE does not open on the coalesced cursor, so the seek starts where the \
             pillar does and steps over every row already drained: {sql}"
        );
        assert!(
            !sql.contains("WHERE (?1 IS NULL"),
            "the page opens on the cursor as a filter, which is the shape a seek cannot \
             use: {sql}"
        );
        judged += 1;
    }
    // The two feed drains bind the same shape over a pair rather than a column.
    // A feed's position is `(generation, path)` — one changeset stamps many rows
    // with one generation — so the floor is a row value, and what the bar reads
    // is unchanged by that: the cursor is still `COALESCE`'s first argument, and
    // the second half of the pair is the second `COALESCE`'s. A spelling that
    // put the pillar's own floor first would demote both halves to filters
    // exactly as a scalar page's would.
    for statement in FEEDS {
        let sql = request.emitted_plan(*statement).expect("a query plan").sql;
        assert!(
            sql.contains("WHERE (generation, path) > (COALESCE(?1,"),
            "the WHERE does not open on the coalesced cursor, so the seek starts where the \
             pillar does and steps over every row already drained: {sql}"
        );
        assert!(
            sql.contains(", COALESCE(?2,"),
            "the cursor's path half is not the row value's second floor, so a page that stops \
             inside a changeset cannot resume inside it: {sql}"
        );
        assert!(
            !sql.contains("WHERE (?1 IS NULL"),
            "the page opens on the cursor as a filter, which is the shape a seek cannot \
             use: {sql}"
        );
        judged += 1;
    }
    assert_eq!(
        judged, 18,
        "a scope, an order, a pillar or a feed went unjudged"
    );
}

/// How many rows the work bar drains.
///
/// **The row count is what the bar's separating power is made of**, not a
/// convenience. A quadratic drain of `c` steps per row squared is refused when
/// `c · rows² > floor + per_row · rows`, so the smallest coefficient the bar
/// sees is `(floor + per_row · rows) / rows²` — 0.81 at 200 rows and 0.40 at
/// 400. Doubling the count halves the coefficient, and therefore halves the
/// fraction of a re-read a drain can hide.
///
/// That is what 400 buys, measured on the narrowest reader here. A revisited row
/// costs about 6 engine steps in the suffix-key pillar, so a drain that re-reads
/// its whole prefix every fifth page — a cursor that seeks four pages out of
/// five and restarts on the fifth — pays about `0.65 · rows²`: **28,401 steps at
/// 200 rows against a ceiling of 32,500, and 104,801 at 400 against 64,500**.
/// The shape the bar exists to exclude is admitted at 200 and refused at 400,
/// and a row count chosen against the full-prefix control alone would never have
/// shown it.
///
/// 400 is where the gain stops being worth its cost. Six subjects drained a row
/// at a time, each with a full-prefix control beside it, take the suite about a
/// second at 400 rows against a third of a second at 200 — the controls are
/// quadratic, so the price of the next doubling is four times that again.
const DRAINED_ROWS: usize = 400;

/// **The work bar**, measured against the SQLite build this workspace's
/// lockfile pins.
///
/// A page of one row through a seeking cursor steps the engine a small constant
/// number of times: open the cursor at the coalesced floor, step to the first
/// row, read its columns, stop. Drained a row at a time over [`DRAINED_ROWS`]
/// rows, the measured cost per row is 20 for stored suffix keys, 23 for
/// tombstones, 26 for the heal page in path order, 28 for indexed terms, 37 for
/// the heal page in folded order, 43 for the death change feed, 49 for the
/// document change feed, and 62 for the findings page — the widest, because a
/// findings page issues three further chunked statements per page to collect
/// each finding's candidates, its classes and its path keys. The two feeds sit
/// high in that range because a row-value floor opens its cursor by comparing a
/// pair, and the document feed reads five columns off the index where the death
/// feed reads three.
///
/// `per_row` is between two and three times that widest reading, and `floor` absorbs the
/// empty page every advancing drain ends on. **The absorber is deliberately
/// wide** because a step count is engine-version sensitive: the same statement
/// over the same rows steps a different number of times under a different SQLite
/// build. What this bar separates is a line from a parabola, and the full-prefix
/// controls beside it come in at about 1400 to 9300 steps per row against a
/// ceiling of 162 — nine to fifty-seven times it — so a coefficient loose enough to survive
/// an engine bump still fails the shape the bar exists to exclude. What gives a
/// passing reading its authority is the control and the row count it is taken
/// at, not the tightness of this number.
const READER_WORK: WorkBar = WorkBar {
    floor: 500,
    per_row: 160,
};

/// What a drain that never ends has met: a cursor that stopped moving, so the
/// reader hands back the row it just handed back. Every drain below is bounded
/// by the rows its fixture holds, because non-advancement is the hazard a
/// keyset cursor exists to close and a bar reports it best as an assertion
/// naming the reader rather than as a run that never returns.
const STUCK: &str = "the cursor did not advance past the rows the fixture holds";

/// **A paged reader costs a line in the rows it drained, and a re-reading one
/// does not.**
///
/// Every reader below is drained twice over the same rows. Once with its cursor
/// advancing, which is how a heal merges and how the comparator projects: each
/// page seeks to the row after the last one it saw. Once with the cursor left at
/// the start and the page bound widened by one each time, which is what a cursor
/// demoted to a filter costs — the i-th row is reached by reading the i-1 rows
/// ahead of it, so the drain pays about `rows²/2` where the first pays `rows`.
///
/// **The pair is the point.** The first assertion alone would pass under a bar
/// wide enough to admit anything; the second is what says the bar can fail. Both
/// go through the public paged readers, so what is measured is the statement the
/// crate really runs rather than one the test spelled.
#[test]
fn a_paged_reader_costs_a_line_in_the_rows_it_drained() {
    let scratch = Scratch::new("reader-work");
    let mut store = scratch.open();
    let mut request = store.begin_request();
    seed_a_drainable_store(&mut request, DRAINED_ROWS);

    // Taking a plan runs a statement, and what it steps is not work a reader
    // spent: an explain is a report about a statement rather than a run of it.
    // A bar taken beside a plan assertion would otherwise read the explain's own
    // cost as the reader's.
    let before_the_explain = request.read_steps();
    for statement in ENUMERATIONS.iter().chain(FEEDS) {
        request.emitted_plan(*statement).expect("a query plan");
    }
    assert_eq!(
        request.read_steps(),
        before_the_explain,
        "taking a query plan moved the reader work count"
    );

    let rows = DRAINED_ROWS as u64;
    for order in [
        norn_store::StoredPathOrder::Sensitive,
        norn_store::StoredPathOrder::AsciiCaseInsensitive,
    ] {
        judge_a_drain(
            &request,
            &format!("the heal page, {order:?}"),
            rows,
            |request| {
                let mut cursor: Option<norn_store::DocumentPath> = None;
                let mut reached = 0;
                while let Some(row) = request
                    .stored_documents_after_ordered(cursor.as_ref(), 1, order)
                    .expect("a page of stored documents")
                    .pop()
                {
                    cursor = Some(row.path);
                    reached += 1;
                    assert!(reached <= DRAINED_ROWS, "the heal page: {STUCK}");
                }
                reached
            },
            |request| {
                for width in 1..=DRAINED_ROWS {
                    let page = request
                        .stored_documents_after_ordered(None, width, order)
                        .expect("a page of stored documents");
                    assert_eq!(page.len(), width, "the control did not reach its i-th row");
                }
                DRAINED_ROWS
            },
        );
    }

    judge_a_drain(
        &request,
        "the findings enumeration",
        rows,
        |request| {
            let mut cursor = None;
            let mut reached = 0;
            while let Some((next, _)) = request
                .stored_findings_after(cursor, 1)
                .expect("a page of findings")
                .pop()
            {
                cursor = Some(next);
                reached += 1;
                assert!(reached <= DRAINED_ROWS, "the findings enumeration: {STUCK}");
            }
            reached
        },
        |request| {
            for width in 1..=DRAINED_ROWS {
                let page = request
                    .stored_findings_after(None, width)
                    .expect("a page of findings");
                assert_eq!(page.len(), width, "the control did not reach its i-th row");
            }
            DRAINED_ROWS
        },
    );

    judge_a_drain(
        &request,
        "the tombstone enumeration",
        rows,
        |request| {
            let mut cursor: Option<norn_store::DocumentPath> = None;
            let mut reached = 0;
            while let Some(death) = request
                .stored_tombstones_after(cursor.as_ref(), 1)
                .expect("a page of tombstones")
                .pop()
            {
                cursor = Some(death.path);
                reached += 1;
                assert!(
                    reached <= DRAINED_ROWS,
                    "the tombstone enumeration: {STUCK}"
                );
            }
            reached
        },
        |request| {
            for width in 1..=DRAINED_ROWS {
                let page = request
                    .stored_tombstones_after(None, width)
                    .expect("a page of tombstones");
                assert_eq!(page.len(), width, "the control did not reach its i-th row");
            }
            DRAINED_ROWS
        },
    );

    judge_a_drain(
        &request,
        "the suffix-key enumeration",
        rows,
        |request| {
            let mut cursor: Option<norn_store::DocumentPath> = None;
            let mut reached = 0;
            while let Some(stored) = request
                .suffix_keys_after(cursor.as_ref(), 1)
                .expect("a page of stored suffix keys")
                .pop()
            {
                cursor = Some(stored.path);
                reached += 1;
                assert!(
                    reached <= DRAINED_ROWS,
                    "the suffix-key enumeration: {STUCK}"
                );
            }
            reached
        },
        |request| {
            for width in 1..=DRAINED_ROWS {
                let page = request
                    .suffix_keys_after(None, width)
                    .expect("a page of stored suffix keys");
                assert_eq!(page.len(), width, "the control did not reach its i-th row");
            }
            DRAINED_ROWS
        },
    );

    judge_a_drain(
        &request,
        "the indexed-term enumeration",
        rows,
        |request| {
            let mut cursor: Option<String> = None;
            let mut reached = 0;
            while let Some(term) = request
                .indexed_terms_after(cursor.as_deref(), 1)
                .expect("a page of indexed terms")
                .pop()
            {
                cursor = Some(term.term);
                reached += 1;
                assert!(
                    reached <= DRAINED_ROWS,
                    "the indexed-term enumeration: {STUCK}"
                );
            }
            reached
        },
        |request| {
            for width in 1..=DRAINED_ROWS {
                let page = request
                    .indexed_terms_after(None, width)
                    .expect("a page of indexed terms");
                assert_eq!(page.len(), width, "the control did not reach its i-th row");
            }
            DRAINED_ROWS
        },
    );

    judge_a_drain(
        &request,
        "the document change feed",
        rows,
        |request| {
            let mut cursor: Option<norn_store::FeedCursor> = None;
            let mut reached = 0;
            while let Some((next, _)) = request
                .changed_documents_after(cursor.as_ref(), 1)
                .expect("a page of the document feed")
                .pop()
            {
                cursor = Some(next);
                reached += 1;
                assert!(reached <= DRAINED_ROWS, "the document change feed: {STUCK}");
            }
            reached
        },
        |request| {
            for width in 1..=DRAINED_ROWS {
                let page = request
                    .changed_documents_after(None, width)
                    .expect("a page of the document feed");
                assert_eq!(page.len(), width, "the control did not reach its i-th row");
            }
            DRAINED_ROWS
        },
    );

    judge_a_drain(
        &request,
        "the death change feed",
        rows,
        |request| {
            let mut cursor: Option<norn_store::FeedCursor> = None;
            let mut reached = 0;
            while let Some((next, _)) = request
                .changed_tombstones_after(cursor.as_ref(), 1)
                .expect("a page of the death feed")
                .pop()
            {
                cursor = Some(next);
                reached += 1;
                assert!(reached <= DRAINED_ROWS, "the death change feed: {STUCK}");
            }
            reached
        },
        |request| {
            for width in 1..=DRAINED_ROWS {
                let page = request
                    .changed_tombstones_after(None, width)
                    .expect("a page of the death feed");
                assert_eq!(page.len(), width, "the control did not reach its i-th row");
            }
            DRAINED_ROWS
        },
    );

    // The bar is over a store an operation could really have produced. A fixture
    // that populated the three pillars into a state the operational-validity leg
    // forbids would be measuring a shape no reader ever drains, and the pillar
    // this one could get wrong is the findings: a place-scoped finding standing
    // beside a live document row. The request's borrow of the store ends at its
    // last use above, which is what lets the leg take the store back.
    assert_operationally_valid(&mut store, "the store the work bar drains");
}

/// Drain one reader both ways and judge the pair, recording what each cost.
///
/// Each drain is bracketed by [`norn_store::Request::read_steps`], so what is
/// judged is the work those statements spent and not the request's whole
/// history. Both drains are required to reach `rows` rows, because a bar over a
/// drain that stopped early is a bar over nothing.
fn judge_a_drain(
    request: &norn_store::Request<'_>,
    subject: &str,
    rows: u64,
    seeking: impl FnOnce(&norn_store::Request<'_>) -> usize,
    re_reading: impl FnOnce(&norn_store::Request<'_>) -> usize,
) {
    let opened = request.read_steps();
    let reached = seeking(request);
    let seeking_steps = request.read_steps() - opened;
    assert_eq!(
        reached as u64, rows,
        "{subject}: the advancing drain did not reach every row"
    );

    let opened = request.read_steps();
    let reached = re_reading(request);
    let re_reading_steps = request.read_steps() - opened;
    assert_eq!(
        reached as u64, rows,
        "{subject}: the control did not reach every row"
    );

    readings::record(
        &format!("work bar: {subject}"),
        &[
            (
                "steps per row, advancing",
                readings::multiple(readings::per_mille(seeking_steps, rows)),
            ),
            (
                "steps per row, re-reading",
                readings::multiple(readings::per_mille(re_reading_steps, rows)),
            ),
            // Per row like the two readings above it: a ceiling in total steps
            // printed beside two per-row costs is a column a reader compares
            // down and gets a wrong answer from.
            (
                "steps per row, ceiling",
                readings::multiple(readings::per_mille(READER_WORK.ceiling(rows), rows)),
            ),
        ],
    );

    READER_WORK.assert_within(subject, rows, seeking_steps);
    READER_WORK.assert_exceeded(
        &format!("{subject}, drained by a cursor demoted to a filter"),
        rows,
        re_reading_steps,
    );
}

/// Enough rows in every pillar the work bar drains for the line and the parabola
/// to be told apart.
///
/// The documents go in first and whole, because a changeset discards the
/// findings in every class its paths are in. Each body carries a term of its
/// own, so the full-text vocabulary has a row per document; each document is
/// its own ambiguity class, so no finding takes another's place; and the deaths
/// are recorded at paths no document stands at, so the tombstone pillar is
/// populated without pruning what was just written.
///
/// **The findings stand at places with no document row.** Their kind is
/// place-scoped — a path addressing no document — and a place-scoped finding
/// beside a live row is the state the operational-validity leg forbids: it
/// reports that nothing was derivable somewhere something was. A fixture that
/// arranged it would be a fixture the invariant refuses, which is why these sit
/// at `unresolved/` and the documents at `drained/`.
fn seed_a_drainable_store(request: &mut norn_store::Request<'_>, rows: usize) {
    let documents: Vec<_> = (0..rows)
        .map(|n| {
            document(
                &format!("drained/{n:04}.md",),
                &format!("hash-{n}"),
                &format!("term{n:04}\n"),
            )
        })
        .collect();
    write_documents(request, &documents);
    for n in 0..rows {
        request
            .record_finding(&path_names_no_document_in_class(
                &format!("unresolved/{n:04}.md"),
                &format!("target{n:04}"),
                &format!("target{n:04}/"),
                &[],
                3,
            ))
            .expect("recording a finding");
    }
    for n in 0..rows {
        record_death(
            request,
            &path(&format!("gone/{n:04}.md")),
            Provenance::HealPrune,
        );
    }
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
        write_document(&mut request, &document(at, "hash-1", "a body\n"));
    }
    // Two findings about the same class, written in different documents, plus
    // one about a longer suffix in the same class and one about another class.
    request
        .record_finding(&path_names_no_document_in_class(
            "one.md",
            "glossary",
            "glossary/",
            &[],
            3,
        ))
        .expect("recording a finding");
    request
        .record_finding(&path_names_no_document_in_class(
            "two.md",
            "glossary",
            "glossary/",
            &[],
            3,
        ))
        .expect("recording a finding");
    request
        .record_finding(&path_names_no_document_in_class(
            "two.md",
            "norn/glossary",
            "glossary/norn/",
            &[],
            2,
        ))
        .expect("recording a finding");
    request
        .record_finding(&path_names_no_document_in_class(
            "three.md",
            "index",
            "index/",
            &[],
            4,
        ))
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
        .findings_in_class(&request.class_probe("glossary").expect("a class stem"))
        .expect("reading findings");
    let mut targets: Vec<&str> = affected
        .iter()
        .map(|finding| finding.target.as_deref().expect("a target"))
        .collect();
    targets.sort();
    assert_eq!(targets, vec!["glossary", "glossary", "norn/glossary"]);

    // And it reaches nothing outside the class.
    let other = request
        .findings_in_class(&request.class_probe("index").expect("a class stem"))
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
            .any(|finding| finding.class_keys.is_empty() && finding.candidates.is_empty())
    );
    assert_eq!(request.counters().get("findings_written"), Some(5));
}

/// **A finding belongs to every class its target's reductions read, and any one of
/// them reaches it.** The two reductions are disjoint rather than nested — `.` is
/// `0x2e` and `/` is `0x2f`, so `notes.tar/` sorts before `notes/` — so a finding
/// filed under one class alone would be invisible to maintenance that named the
/// other: a finding nothing ever revisits, which is the hazard class scoping
/// exists to close.
#[test]
fn a_finding_is_reachable_and_discardable_through_every_class_it_is_in() {
    // The byte order the hazard is made of, stated where it is relied on.
    assert!("notes.tar/" < "notes/", "`.` does not sort before `/`");
    assert!(
        !"notes.tar/".starts_with("notes/"),
        "the reductions nest after all"
    );

    for (target, at) in [
        // The flagship pair: the written target reduces one way, the document it
        // resolves to is in the class the *other* reduction names.
        ("v1.2", "notes/v1.2.md"),
        ("notes.tar", "archive/notes.tar.gz"),
        ("notes.tar.gz", "archive/notes.tar.gz"),
        // And a target with one reduction, which is one class and unchanged.
        ("glossary", "docs/glossary.md"),
    ] {
        let scratch = Scratch::new("two-reduction");
        let mut store = scratch.open();
        let mut request = store.begin_request();
        let subject = path(at);
        let probe = suffix_probe(target).expect("a suffix target");
        write_document(
            &mut request,
            &document(subject.as_str(), "hash-1", "a body\n"),
        );
        assert!(
            request
                .suffix_candidates(&class_named(&request, target))
                .expect("reading candidates")
                .contains(&subject),
            "`{target}` does not resolve to `{at}`"
        );

        // The finding is recorded from the probe, as a resolution reading mints
        // it, and a schema violation with no class sits beside it.
        request
            .record_finding(&path_names_no_document_for_target(
                "docs/index.md",
                target,
                &[at],
                1,
            ))
            .expect("recording a finding");
        request
            .record_finding(&violation("docs/index.md"))
            .expect("recording a finding");

        // The class the document is in is one of the finding's, which is what
        // makes a change to that document reach it.
        assert!(
            probe.class_keys().contains(&request.class_key_of(&subject)),
            "a finding about `{target}` is not in the class `{at}` is in"
        );
        let stored = request
            .findings_in_class(&request.class_probe(subject.stem()).expect("a class stem"))
            .expect("reading findings");
        assert_eq!(
            stored.len(),
            1,
            "a change to `{at}` does not reach the finding about `{target}`"
        );
        assert_eq!(stored[0].class_keys, probe.class_keys());

        // So is every other class the target read, whichever reduction produced
        // it, and each of them reads the finding once.
        for key in probe.class_keys() {
            let stem = key
                .as_str()
                .strip_suffix('/')
                .expect("a class key is separator-terminated");
            assert_eq!(
                request
                    .findings_in_class(&request.class_probe(stem).expect("a class stem"))
                    .expect("reading findings")
                    .len(),
                1,
                "the class `{}` does not reach the finding about `{target}`",
                key.as_str()
            );
        }

        // And a changeset writing the document reaches it through the
        // document's class, taking the whole finding — every membership row it
        // had included, so no other class still holds it and nothing is left
        // referencing a finding that is gone.
        let invalidation = write_document(
            &mut request,
            &document(subject.as_str(), "hash-2", "a body\n"),
        )
        .invalidated;
        assert_eq!(
            invalidation.findings_discarded, 1,
            "a changeset writing `{at}` discarded nothing"
        );
        for key in probe.class_keys() {
            let stem = key.as_str().strip_suffix('/').expect("a class key");
            assert!(
                request
                    .findings_in_class(&request.class_probe(stem).expect("a class stem"))
                    .expect("reading findings")
                    .is_empty(),
                "the finding survives in `{}` after its class was discarded",
                key.as_str()
            );
        }

        // The schema violation is in no class, so no class discard touches it.
        let surviving = request
            .stored_findings(&path("docs/index.md"))
            .expect("reading findings");
        assert_eq!(surviving.len(), 1, "the class-free finding went with it");
        assert!(surviving[0].class_keys.is_empty());

        request.finish();
        store
            .verify_integrity()
            .expect("a store whose findings left through a changeset's class discard");
    }
}

/// A finding in two of one probe's own classes is still one finding on both sides
/// of maintenance: read once, and discarded once by a changeset changing both
/// classes. The unit is the finding, so a range that matched two membership rows
/// has not found two findings.
#[test]
fn a_finding_in_two_of_a_probes_classes_is_read_and_counted_once() {
    let scratch = Scratch::new("both-classes");
    let mut store = scratch.open();
    let mut request = store.begin_request();
    let probe = suffix_probe("v1.2").expect("a suffix target");
    assert_eq!(probe.range_count(), 2);

    request
        .record_finding(&path_names_no_document_for_target(
            "docs/index.md",
            "v1.2",
            &[],
            2,
        ))
        .expect("recording a finding");
    assert_eq!(
        request
            .findings_in_class(&probe)
            .expect("reading findings")
            .len(),
        1,
        "a finding in both of the probe's classes came back twice"
    );
    let outcome = write_documents(
        &mut request,
        &[
            document("v1.2.md", "hash-1", "a body\n"),
            document("v1.md", "hash-1", "a body\n"),
        ],
    );
    assert_eq!(outcome.affected_classes, probe.class_keys());
    assert_eq!(outcome.invalidated.findings_discarded, 1);
    assert_eq!(request.counters().get("findings_discarded"), Some(1));
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
    write_document(
        &mut request,
        &document(subject.as_str(), "hash-1", "a body\n"),
    );
    record_death(&mut request, &subject, Provenance::HealPrune);

    let tombstone = request
        .stored_tombstone(&subject)
        .expect("reading a tombstone")
        .expect("a tombstone");
    assert_eq!(request.class_key_of(&tombstone.path), class("glossary/"));
    assert_eq!(tombstone.path.suffix_key(), "glossary/norn/docs/");

    // And that class reaches the findings a deletion has to revisit.
    request
        .record_finding(&path_names_no_document_in_class(
            "other.md",
            "glossary",
            "glossary/",
            &[],
            3,
        ))
        .expect("recording a finding");
    assert_eq!(
        request
            .findings_in_class(
                &request
                    .class_probe(tombstone.path.stem())
                    .expect("a class stem")
            )
            .expect("reading findings")
            .len(),
        1
    );
}

/// **A finding is not coupled to a document row; maintenance is what takes it.**
/// Nothing in the schema references `documents` from `findings`, so a finding is
/// recordable about a path nothing has ever derived, no row-existence ordering
/// applies to writing one, and no delete cascades into the table. What removes a
/// finding is a statement the store runs and counts.
///
/// The two findings here are in classes the death names none of, so the class
/// axis reaches neither and the axes are told apart. The finding about
/// `docs/index.md` goes on the **subject** axis, because it describes a document
/// that is now gone. The finding about `never/derived.md` stays: it was recorded
/// against a path with no row at all, and it is untouched by a change to a
/// different path.
#[test]
fn a_finding_is_taken_by_maintenance_and_never_by_a_cascade() {
    let scratch = Scratch::new("findings-maintenance");
    let mut store = scratch.open();
    let subject = path("docs/index.md");
    let never_derived = path("never/derived.md");

    let mut request = store.begin_request();
    write_document(
        &mut request,
        &document(subject.as_str(), "hash-1", "a body\n"),
    );
    request
        .record_finding(&path_names_no_document_in_class(
            subject.as_str(),
            "glossary",
            "glossary/",
            &[],
            3,
        ))
        .expect("a finding about a stored document");
    request
        .record_finding(&path_names_no_document_in_class(
            never_derived.as_str(),
            "notes",
            "notes/",
            &[],
            2,
        ))
        .expect("a finding about a path nothing derived");
    assert_eq!(request.pillars().expect("a pillar report").findings, 2);

    let death = record_death(&mut request, &subject, Provenance::PlanDelete);
    assert_eq!(
        death.affected_classes,
        classes(&["index/"]),
        "the death names a class one of the findings is in, so the axes are not told apart"
    );
    assert_eq!(
        death.invalidated.findings_discarded, 1,
        "the death took a finding that is not about the path that died"
    );
    assert_eq!(request.counters().get("findings_discarded"), Some(1));
    assert!(
        request
            .stored_findings(&subject)
            .expect("reading findings")
            .is_empty(),
        "a finding about the dead path survived the death"
    );
    assert_eq!(
        request
            .stored_findings(&never_derived)
            .expect("reading findings")
            .len(),
        1,
        "a finding about a path nothing derived was reached by another path's death"
    );
    assert_eq!(request.pillars().expect("a pillar report").findings, 1);

    request.finish();
    store
        .verify_integrity()
        .expect("a store whose findings left through maintenance");
}

/// The two kinds [`re_deriving_a_subject_is_a_discard_and_a_record`] records
/// about its subject, which is what re-deriving that subject discards.
const BOTH_KINDS: DiscardScope<'static> = DiscardScope::Kinds(&[
    FindingKind::BodyBytesNotUtf8,
    FindingKind::PathNamesNoDocument,
]);

/// The subject axis reached by a path rather than by a change: the door a
/// producer whose subject no changeset can name needs in order to re-derive at
/// all.
#[test]
fn re_deriving_a_subject_is_a_discard_and_a_record() {
    let scratch = Scratch::new("subject-maintenance");
    let mut store = scratch.open();
    let mut request = store.begin_request();

    request
        .record_finding(&violation("one.md"))
        .expect("recording a finding");
    request
        .record_finding(&path_names_no_document_in_class(
            "one.md",
            "glossary",
            "glossary/",
            &[],
            3,
        ))
        .expect("recording a finding");
    request
        .record_finding(&violation("two.md"))
        .expect("recording a finding");

    // Everything of the kinds re-derived about the subject goes, whichever
    // axis put it there, and nothing about any other subject does.
    let invalidation = request
        .discard_findings_about(&path("one.md"), BOTH_KINDS)
        .expect("discarding a subject");
    assert_eq!(invalidation.findings_discarded, 2);
    assert!(
        request
            .stored_findings(&path("one.md"))
            .expect("reading findings")
            .is_empty()
    );
    assert_eq!(
        request
            .stored_findings(&path("two.md"))
            .expect("reading findings")
            .len(),
        1
    );

    // Recording what holds now leaves one copy, which is the whole of the
    // idempotence story on this axis.
    request
        .record_finding(&violation("one.md"))
        .expect("recording a finding");
    assert_eq!(
        request
            .stored_findings(&path("one.md"))
            .expect("reading findings")
            .len(),
        1
    );

    // A subject with nothing recorded about it discards nothing and refuses
    // nothing: a producer re-deriving a place it has never spoken about is the
    // ordinary first pass.
    assert_eq!(
        request
            .discard_findings_about(&path("never/spoken.md"), BOTH_KINDS)
            .expect("discarding an untouched subject")
            .findings_discarded,
        0
    );
    assert_eq!(request.counters().get("findings_discarded"), Some(2));
}

/// **A producer replaces the kinds it re-derives and leaves the rest.** Two
/// producers can speak about one subject — one reads a path, the other reads the
/// bytes at it — and only one of them is re-deriving when it discards. Taking
/// the whole subject there would delete a finding nobody is about to record
/// again, so the scope a caller names is what its discard reaches.
#[test]
fn re_deriving_some_of_a_subjects_kinds_leaves_the_rest_standing() {
    let scratch = Scratch::new("subject-kind-maintenance");
    let mut store = scratch.open();
    let mut request = store.begin_request();

    // One subject, two kinds: `violation` is read from bytes and `ambiguity`
    // from a path.
    request
        .record_finding(&violation("one.md"))
        .expect("recording a finding");
    request
        .record_finding(&path_names_no_document_in_class(
            "one.md",
            "glossary",
            "glossary/",
            &[],
            3,
        ))
        .expect("recording a finding");

    let invalidation = request
        .discard_findings_about(
            &path("one.md"),
            DiscardScope::Kinds(&[FindingKind::PathNamesNoDocument]),
        )
        .expect("discarding one kind about a subject");
    assert_eq!(invalidation.findings_discarded, 1);
    assert_eq!(
        request
            .stored_findings(&path("one.md"))
            .expect("reading findings")
            .iter()
            .map(|finding| finding.kind.as_str())
            .collect::<Vec<_>>(),
        ["document/body-bytes-not-utf8"],
        "the discard took a kind its caller does not re-derive"
    );

    // Recording the kind that was discarded leaves one copy of each, which is
    // discard-then-record holding per kind rather than per subject.
    request
        .record_finding(&path_names_no_document_in_class(
            "one.md",
            "glossary",
            "glossary/",
            &[],
            3,
        ))
        .expect("recording a finding");
    assert_eq!(
        request
            .stored_findings(&path("one.md"))
            .expect("reading findings")
            .len(),
        2
    );

    // A scope that names no kind re-derives nothing and so takes nothing: an
    // empty list is not a wider discard.
    assert_eq!(
        request
            .discard_findings_about(&path("one.md"), DiscardScope::Kinds(&[]))
            .expect("discarding no kind about a subject")
            .findings_discarded,
        0
    );
    assert_eq!(request.counters().get("findings_discarded"), Some(1));
}

/// **A walk's scope reaches the subjects it has to account for, and reaches no
/// others.** The page is the read half of the walked-scope prune: the subjects a
/// scope holds, of the kinds its caller derives, that no document row stands at.
/// A row standing at a subject is the walk's own account of that place, so such a
/// subject never comes back here.
#[test]
fn the_walked_scope_page_names_the_subjects_no_document_row_stands_at() {
    let scratch = Scratch::new("walked-scope-page");
    let mut store = scratch.open();
    let mut request = store.begin_request();
    let kinds = &[
        FindingKind::BodyBytesNotUtf8,
        FindingKind::PathNamesNoDocument,
    ];
    let page = |request: &norn_store::Request<'_>,
                scope: norn_store::SubjectScope<'_>,
                kinds: &[FindingKind],
                after: Option<&norn_store::DocumentPath>| {
        request
            .finding_subjects_without_rows_after(
                scope,
                kinds,
                after,
                16,
                norn_store::StoredPathOrder::Sensitive,
            )
            .expect("reading a page of finding subjects")
            .iter()
            .map(|subject| subject.as_str().to_string())
            .collect::<Vec<String>>()
    };

    // The rows go in first: a changeset discards the findings in every class its
    // paths are in, so a row written after a finding about its class takes it.
    write_documents(
        &mut request,
        &[
            document("standing.md", "hash-1", "a body\n"),
            document("one/standing.md", "hash-1", "a body\n"),
        ],
    );
    for at in [
        "standing.md",
        "one/standing.md",
        "gone.md",
        "one",
        "one.md",
        "one/gone.md",
        "one/deep/gone.md",
    ] {
        request
            .record_finding(&violation(at))
            .expect("recording a finding");
    }
    request
        .record_finding(&path_names_no_document_in_class(
            "one/gone.md",
            "glossary",
            "glossary/",
            &[],
            2,
        ))
        .expect("recording a finding");

    // The vault scope holds every subject no row stands at, once however many
    // findings stand there, in the order a cursor pages forward through.
    assert_eq!(
        page(&request, norn_store::SubjectScope::Vault, kinds, None),
        [
            "gone.md",
            "one",
            "one.md",
            "one/deep/gone.md",
            "one/gone.md"
        ],
        "the page did not name the subjects a walk of the vault has to account for"
    );

    // The kinds are the caller's: a subject holding only another producer's
    // finding is outside the page, exactly as it is outside that caller's
    // discard.
    assert_eq!(
        page(
            &request,
            norn_store::SubjectScope::Vault,
            &[FindingKind::PathNamesNoDocument],
            None
        ),
        ["one/gone.md"]
    );
    assert!(
        page(&request, norn_store::SubjectScope::Vault, &[], None).is_empty(),
        "a caller deriving no kind read a subject it could not take"
    );

    // A subtree holds its root's own subject and its segment-aligned
    // descendants, and a textual neighbour of the root is neither.
    assert_eq!(
        page(
            &request,
            norn_store::SubjectScope::Subtree(&path("one")),
            kinds,
            None
        ),
        ["one", "one/deep/gone.md", "one/gone.md"]
    );
    // A directory that names no document holds only what is under it.
    assert_eq!(
        page(
            &request,
            norn_store::SubjectScope::Under(
                &norn_store::DirectoryPrefix::new("one").expect("a directory")
            ),
            kinds,
            None
        ),
        ["one/deep/gone.md", "one/gone.md"]
    );

    // The cursor is exclusive, so a caller pages forward across its own
    // discards rather than re-reading what it already took.
    assert_eq!(
        page(
            &request,
            norn_store::SubjectScope::Subtree(&path("one")),
            kinds,
            Some(&path("one/deep/gone.md"))
        ),
        ["one/gone.md"]
    );

    // A page outside the bound is refused rather than answered short.
    assert!(matches!(
        request.finding_subjects_without_rows_after(
            norn_store::SubjectScope::Vault,
            kinds,
            None,
            0,
            norn_store::StoredPathOrder::Sensitive,
        ),
        Err(StoreError::Bound { .. })
    ));
}

/// **A vault that folds ASCII case bounds a prune's scope under the same fold,
/// and pages it bytewise all the same.** Scope membership is the walk's question
/// — the two sides of one walk have to address one set of places, so a root
/// spelled one way holds a subject spelled the other — while the cursor and the
/// order are the page's own, and a page read against a caller's set needs only a
/// total order. A page of one is what states that the cursor is exclusive under
/// that split rather than only the first page of it.
#[test]
fn a_folded_prune_scope_holds_the_subjects_its_fold_reaches() {
    let scratch = Scratch::new("walked-scope-page-folded");
    let mut store = scratch.open();
    let mut request = store.begin_request();
    let kinds = &[FindingKind::BodyBytesNotUtf8];
    for at in ["One", "One/Gone.md", "one/deep/gone.md", "two/gone.md"] {
        request
            .record_finding(&violation(at))
            .expect("recording a finding");
    }
    let drained_subjects = |scope: norn_store::SubjectScope<'_>| {
        drained(|after| {
            request
                .finding_subjects_without_rows_after(
                    scope,
                    kinds,
                    after,
                    1,
                    norn_store::StoredPathOrder::AsciiCaseInsensitive,
                )
                .expect("a page of one")
        })
    };

    // The root's own subject and its descendants, whichever case each is spelled
    // in — and never the subject under a root the fold does not reach.
    assert_eq!(
        drained_subjects(norn_store::SubjectScope::Subtree(&path("one"))),
        ["One", "One/Gone.md", "one/deep/gone.md"]
    );
    // A directory names no subject of its own, so the same span read that way
    // holds the descendants alone.
    assert_eq!(
        drained_subjects(norn_store::SubjectScope::Under(
            &norn_store::DirectoryPrefix::new("ONE").expect("a directory")
        )),
        ["One/Gone.md", "one/deep/gone.md"]
    );
}

/// **Class-scoped maintenance runs in both directions, and discard-then-record is
/// the idempotence story.** A changeset writing `glossary.md` changes the class
/// `glossary/` and empties it, and recording what holds now after it means two
/// derivations of one class cannot leave two copies — and every finding that
/// left is counted.
#[test]
fn re_deriving_a_class_is_a_discard_and_a_record() {
    let scratch = Scratch::new("class-maintenance");
    let mut store = scratch.open();
    let mut request = store.begin_request();

    for at in ["one.md", "two.md"] {
        request
            .record_finding(&path_names_no_document_in_class(
                at,
                "glossary",
                "glossary/",
                &[],
                3,
            ))
            .expect("recording a finding");
    }
    request
        .record_finding(&path_names_no_document_in_class(
            "one.md",
            "norn/glossary",
            "glossary/norn/",
            &[],
            2,
        ))
        .expect("recording a finding");
    request
        .record_finding(&path_names_no_document_in_class(
            "three.md",
            "index",
            "index/",
            &[],
            4,
        ))
        .expect("recording a finding");

    // The class discard takes the whole class, longer suffixes inside it
    // included, and nothing outside it.
    let glossary = document("glossary.md", "hash-1", "a body\n");
    let invalidation = write_document(&mut request, &glossary).invalidated;
    assert_eq!(invalidation.findings_discarded, 3);
    assert!(
        request
            .findings_in_class(&request.class_probe("glossary").expect("a class stem"))
            .expect("reading findings")
            .is_empty()
    );
    assert_eq!(
        request
            .findings_in_class(&request.class_probe("index").expect("a class stem"))
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
        write_document(&mut request, &glossary);
        request
            .record_finding(&path_names_no_document_in_class(
                "one.md",
                "glossary",
                "glossary/",
                &[],
                2,
            ))
            .expect("recording a finding");
    }
    assert_eq!(
        request
            .findings_in_class(&request.class_probe("glossary").expect("a class stem"))
            .expect("reading findings")
            .len(),
        1
    );

    // Changing a class that holds nothing discards nothing rather than
    // refusing.
    assert_eq!(
        write_document(&mut request, &document("absent.md", "hash-1", "a body\n"))
            .invalidated
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

    // The document's first link names no document, so the write files its
    // broken-link finding beside the violation recorded after it.
    let facts = document_with_every_fact(subject.as_str(), "hash-1");
    write_document(&mut request, &facts);
    request
        .record_finding(&violation(subject.as_str()))
        .expect("recording a finding");
    assert_eq!(
        request
            .stored_findings(&subject)
            .expect("reading findings")
            .len(),
        2
    );

    // The schema moves, and exactly the schema-keyed table goes — reported by the
    // pin that moved it.
    let repinned = request
        .pin_vault_schema(b"fields:\n  status: enum\n", "schema-2")
        .expect("re-pinning a vault schema");
    assert!(repinned.repinned);
    assert!(repinned.generation > pinned.generation);
    assert_eq!(repinned.invalidated.findings_discarded, 2);
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
    assert_eq!(
        written_links(&after),
        facts.links,
        "a schema edit reached the links"
    );
    assert_eq!(after.headings, facts.headings);
    assert_eq!(written_tags(&after), facts.tags);
    assert_eq!(
        after.document.frontmatter.as_deref(),
        Some(r#"{"draft":false,"title":"Norn"}"#),
        "a schema edit reached the frontmatter projection"
    );
    let pillars = request.pillars().expect("a pillar report");
    assert_eq!(pillars.documents, 1);
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
    write_document(
        &mut store.begin_request(),
        &document("notes.md", "hash-1", "a body\n"),
    );
    store.verify_integrity().expect("a store just written to");

    // The connection can read and cannot write, which is what a revoked
    // permission or a read-only mount looks like from inside a statement.
    induced_failure::execute_out_of_band(&mut store, "PRAGMA query_only = ON")
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
    induced_failure::execute_out_of_band(&mut store, "PRAGMA query_only = OFF")
        .expect("restoring the connection");
    store
        .verify_integrity()
        .expect("the same store, once it can be written to");
}

/// **The feed reports how far the store's writes had got, not how far its rows
/// reach.** A write that takes a generation without presenting a feed row — a
/// schema pin — still moves the reading, so a consumer that records it at the
/// end of a drain is told it caught up to that write too. A write that takes no
/// generation — a pin of the schema already pinned — leaves it where it was.
#[test]
fn the_feed_reads_the_last_write_generation_whichever_write_took_it() {
    let scratch = Scratch::new("feed-write-generation");
    let mut store = scratch.open();
    assert_eq!(
        store.feed_read().write_generation().expect("a reading"),
        0,
        "a created store has committed no write"
    );

    write_document(
        &mut store.begin_request(),
        &document("docs/a.md", "hash-a", "alpha\n"),
    );
    let (last_row, _) = store
        .begin_request()
        .changed_documents_after(None, norn_store::MAX_PAGE)
        .expect("the document feed")
        .pop()
        .expect("the row just written");
    assert_eq!(
        store.feed_read().write_generation().expect("a reading"),
        last_row.generation(),
        "the reading is not the generation the last write stamped"
    );

    let pin = store
        .begin_request()
        .pin_vault_schema(b"schema", "fingerprint")
        .expect("a schema pin");
    assert!(pin.repinned);
    assert_eq!(
        store.feed_read().write_generation().expect("a reading"),
        pin.generation,
        "a write no feed row presents did not move the reading"
    );
    assert!(pin.generation > last_row.generation());

    let again = store
        .begin_request()
        .pin_vault_schema(b"schema", "fingerprint")
        .expect("the same pin again");
    assert!(!again.repinned);
    assert_eq!(
        store.feed_read().write_generation().expect("a reading"),
        pin.generation,
        "a write that took no generation moved the reading"
    );
}

/// What the write-path bar demands of one registered statement: each seek the
/// plan must report, as the table, what it runs through and the equality or
/// range constraint it prints. Every statement is also judged to scan no table.
struct WriteBar {
    seeks: &'static [(&'static str, Access<'static>, &'static str)],
}

/// The bar for one write-path statement. Exhaustive over [`WriteStatement`], so
/// a statement added to the registry has to say here what its plan must show.
///
/// An `INSERT` reaches its row by no search: its uniqueness checks and the
/// triggers it fires are not in a plan, so its bar is the universal one, that
/// the plan reports no scan. The foreign-key actions SQLite does report beside a
/// statement are not asserted either way: what a cascade costs is not what these
/// bars judge, and the bar would otherwise be pinned to how the driver chooses to
/// describe one.
fn write_bar(statement: WriteStatement) -> WriteBar {
    use WriteStatement::*;
    let seeks: &'static [(&'static str, Access<'static>, &'static str)] = match statement {
        DiscardLinks => &[(
            "links",
            Access::Index("links_document_ordinal"),
            "(document=?)",
        )],
        DiscardHeadings => &[(
            "headings",
            Access::Index("headings_document_ordinal"),
            "(document=?)",
        )],
        DiscardBlocks => &[(
            "blocks",
            Access::Index("blocks_document_ordinal"),
            "(document=?)",
        )],
        DiscardTags => &[(
            "document_tags",
            Access::Index("document_tags_document_ordinal"),
            "(document=?)",
        )],
        DiscardFields => &[("document_fields", Access::PrimaryKey, "(document=?)")],
        DeleteDocument | RowAt => &[("documents", Access::Index("documents_path"), "(path=?)")],
        // Two open ranges over the fingerprint rather than an inequality.
        DiscardStaleFindings => &[
            (
                "findings",
                Access::Index("findings_fingerprint_kind_nocase"),
                "(vault_schema_fingerprint<?)",
            ),
            (
                "findings",
                Access::Index("findings_fingerprint_kind_nocase"),
                "(vault_schema_fingerprint>?)",
            ),
        ],
        NextGeneration => &[("meta", Access::PrimaryKey, "(key=?)")],
        UpsertDocument
        | InsertLink
        | InsertLinkKey
        | InsertHeading
        | InsertBlock
        | InsertTag
        | InsertField
        | RecordTombstone
        | InsertFinding
        | InsertFindingCandidate
        | InsertFindingClass
        | InsertFindingPath
        | PutMeta => &[],
    };
    WriteBar { seeks }
}

/// Whether a plan meets a write bar: no table scan, and every seek it names.
fn meets_write_bar(read: &QueryPlan, bar: &WriteBar) -> bool {
    read.table_scans().is_empty()
        && read.unbounded_steps().is_empty()
        && bar.seeks.iter().all(|(table, access, constraint)| {
            read.searches_of(table)
                .iter()
                .any(|row| row.access() == Some(*access) && row.constraint() == Some(*constraint))
        })
}

fn write_plan(store: &mut Store, statement: WriteStatement) -> QueryPlan {
    plan(
        store
            .begin_request()
            .emitted_plan(ExplainedStatement::Write(statement))
            .unwrap_or_else(|problem| panic!("a query plan for {statement:?}: {problem}")),
    )
}

/// **Every statement the write path runs reaches its rows by a seek, and none
/// scans a table.** An increment runs these once per document, per fact row or
/// per finding, so a statement that read a table end to end would make a
/// changeset cost the store's size on top of its own. The census is
/// [`WriteStatement::all`], the registry the increment prepares from, so a
/// statement is judged because it is prepared and not because a list remembers
/// it; a plan taken of any statement that binds a different number of values
/// than its text names is an error here.
///
/// Controls: the index each keyed statement seeks is dropped, and the same bar
/// fails — for the two statements that seek a primary key, whose index cannot
/// be dropped, a plan that scans the table is handed to the same judgment.
#[test]
fn every_write_path_statement_reaches_its_rows_by_a_seek() {
    let scratch = Scratch::new("write-plans");
    let mut store = scratch.open();

    for statement in WriteStatement::all() {
        let read = write_plan(&mut store, statement);
        assert!(
            meets_write_bar(&read, &write_bar(statement)),
            "{statement:?} does not reach its rows by a seek: {:?}\nemitted SQL: {}",
            read.rows(),
            read.sql()
        );
    }

    // A primary key cannot be dropped, so the same judgment is handed the plan
    // a scan of the table would report.
    for (statement, table) in [
        (WriteStatement::DiscardFields, "document_fields"),
        (WriteStatement::NextGeneration, "meta"),
    ] {
        let scanning = QueryPlan::new(
            "scanning",
            vec![PlanRow::new(2, 0, format!("SCAN {table}"))],
        );
        assert!(
            !meets_write_bar(&scanning, &write_bar(statement)),
            "the bar for {statement:?} holds a plan that scans {table}"
        );
    }

    for (statements, indexes) in [
        (
            &[WriteStatement::DiscardLinks][..],
            &["links_document_ordinal"][..],
        ),
        (
            &[WriteStatement::DiscardHeadings],
            &["headings_document_ordinal"],
        ),
        (
            &[WriteStatement::DiscardBlocks],
            &["blocks_document_ordinal"],
        ),
        (
            &[WriteStatement::DiscardTags],
            &["document_tags_document_ordinal"],
        ),
        (
            &[WriteStatement::DeleteDocument, WriteStatement::RowAt],
            &["documents_path"],
        ),
        // Two indexes lead with the fingerprint, so both go.
        (
            &[WriteStatement::DiscardStaleFindings],
            &[
                "findings_fingerprint_kind_nocase",
                "findings_fingerprint_kind_severity_nocase",
            ],
        ),
    ] {
        let scratch = Scratch::new("write-plans-control");
        let mut store = scratch.open();
        for index in indexes {
            induced_failure::execute_out_of_band(&mut store, &format!("DROP INDEX {index}"))
                .unwrap_or_else(|problem| panic!("dropping {index}: {problem}"));
        }
        for statement in statements {
            let read = write_plan(&mut store, *statement);
            assert!(
                !meets_write_bar(&read, &write_bar(*statement)),
                "the bar for {statement:?} held with {indexes:?} dropped: {:?}",
                read.rows()
            );
        }
    }
}

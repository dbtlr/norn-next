//! The write-through increment: what a changeset is, and what survives one that
//! never finished.
//!
//! The cases here are the ones the entry point exists for — atomicity across
//! several documents, one generation for the whole act, the order entries apply
//! in, the findings a changed path invalidates on either axis, and the counter
//! reading a `Composed` changeset is held to. Atomicity is judged from both
//! ends: a changeset refused partway through, and a process killed partway
//! through. Every other suite writes through the same entry point; these are the
//! ones that judge it.

use std::path::Path;
use std::process::Command;

use crate::common::{
    Scratch, ambiguity, classes, document, document_with_every_fact, path, snapshot, violation,
    write_document, write_documents,
};
use norn_store::{Change, IncrementProvenance, OpenOutcome, Provenance, Store, StoreError};

/// One upsert entry, from a document and a hash.
fn upsert(at: &str, hash: &str, body: &str) -> Change {
    Change::Upsert(document(at, hash, body))
}

/// One death entry.
fn death(at: &str, provenance: Provenance) -> Change {
    Change::Death {
        path: path(at),
        provenance,
    }
}

#[test]
fn stored_document_pages_are_bounded_ordered_and_exclusive() {
    let scratch = Scratch::new("document-pages");
    let mut store = scratch.open();
    let mut request = store.begin_request();
    write_documents(
        &mut request,
        &[
            document("c.md", "hash-c", "c\n"),
            document("a.md", "hash-a", "a\n"),
            document("b.md", "hash-b", "b\n"),
        ],
    );

    let first = request
        .stored_documents_after(None, 2)
        .expect("the first page");
    assert_eq!(
        first
            .iter()
            .map(|row| row.path.as_str())
            .collect::<Vec<_>>(),
        ["a.md", "b.md"]
    );
    let second = request
        .stored_documents_after(Some(&first[1].path), 2)
        .expect("the second page");
    assert_eq!(
        second
            .iter()
            .map(|row| row.path.as_str())
            .collect::<Vec<_>>(),
        ["c.md"]
    );
    assert!(matches!(
        request.stored_documents_after(None, 0),
        Err(StoreError::Bound { .. })
    ));
}

/// **One generation stamps every row a changeset wrote.** Generations order
/// writes in the store, and a changeset is one write however many documents it
/// names — so a reader comparing generations sees it arrive at an instant rather
/// than as a run of numbers it would have to recognize as one act.
#[test]
fn one_changeset_stamps_one_generation_across_every_row_it_wrote() {
    let scratch = Scratch::new("one-generation");
    let mut store = scratch.open();
    let mut request = store.begin_request();

    // A path to kill, derived by a changeset of its own so its own generation is
    // in the past when the changeset under test runs.
    write_document(&mut request, &document("gone.md", "hash-1", "a body\n"));

    let outcome = request
        .apply_increment(
            IncrementProvenance::Derived,
            [
                upsert("one.md", "hash-1", "one\n"),
                upsert("two.md", "hash-1", "two\n"),
                upsert("three.md", "hash-1", "three\n"),
                death("gone.md", Provenance::WatcherRemoval),
            ],
        )
        .expect("applying a changeset");
    let stamped = outcome
        .generation
        .expect("a changeset that wrote takes one");

    for at in ["one.md", "two.md", "three.md"] {
        assert_eq!(
            request
                .stored_document(&path(at))
                .expect("reading a document")
                .expect("a document")
                .generation,
            stamped,
            "`{at}` is stamped with a generation of its own"
        );
    }
    assert_eq!(
        request
            .stored_tombstone(&path("gone.md"))
            .expect("reading a tombstone")
            .expect("a tombstone")
            .generation,
        stamped,
        "the death in the changeset took a generation of its own"
    );

    // The next changeset takes the next one, so changesets still order.
    let next = request
        .apply_increment(
            IncrementProvenance::Derived,
            [upsert("four.md", "hash-1", "four\n")],
        )
        .expect("applying a changeset");
    assert_eq!(next.generation, Some(stamped + 1));
}

/// **An empty changeset is a valid no-op, and it takes no generation.** It writes
/// nothing, so moving the store's write sequence for it would make the sequence
/// report an act that never happened — which is why a re-pin of the schema that
/// is already pinned takes none either.
#[test]
fn an_empty_changeset_writes_nothing_and_takes_no_generation() {
    let scratch = Scratch::new("empty-changeset");
    let mut store = scratch.open();
    let mut request = store.begin_request();

    let before = request
        .apply_increment(
            IncrementProvenance::Derived,
            [upsert("one.md", "hash-1", "one\n")],
        )
        .expect("applying a changeset");

    let empty = request
        .apply_increment(IncrementProvenance::Derived, [])
        .expect("applying an empty changeset");
    assert_eq!(empty.generation, None);
    assert_eq!(empty.documents_upserted, 0);
    assert_eq!(empty.documents_deleted, 0);
    assert_eq!(empty.tombstones_recorded, 0);
    assert!(empty.affected_classes.is_empty());
    assert_eq!(empty.invalidated.findings_discarded, 0);

    // The generation the next changeset takes is the one it would have taken
    // with no empty changeset in between.
    let after = request
        .apply_increment(
            IncrementProvenance::Derived,
            [upsert("two.md", "hash-1", "two\n")],
        )
        .expect("applying a changeset");
    assert_eq!(
        after.generation,
        before.generation.map(|generation| generation + 1),
        "the empty changeset consumed a generation"
    );
    assert_eq!(request.pillars().expect("a pillar report").documents, 2);
}

/// **Entries apply in the order they arrive, and the last one for a path
/// decides.** Nothing is coalesced ahead of time: coalescing would have to
/// decide which of two facts about one path is the true one, and the caller
/// already said by ordering them.
#[test]
fn the_last_entry_for_a_path_is_the_one_that_stands() {
    let scratch = Scratch::new("in-order");
    let mut store = scratch.open();
    let mut request = store.begin_request();

    let outcome = request
        .apply_increment(
            IncrementProvenance::Derived,
            [
                // Written twice: the second writing is what is at rest.
                upsert("twice.md", "hash-1", "first\n"),
                upsert("twice.md", "hash-2", "second\n"),
                // Written and then killed: no document, and a tombstone carrying
                // the hash the row it removed was derived at.
                upsert("killed.md", "hash-1", "a body\n"),
                death("killed.md", Provenance::WatcherRemoval),
                // Killed and then written: the document stands, beside the
                // tombstone the death left.
                death("revived.md", Provenance::HealPrune),
                upsert("revived.md", "hash-1", "a body\n"),
            ],
        )
        .expect("applying a changeset");

    assert_eq!(outcome.documents_upserted, 4, "one per upsert entry");
    assert_eq!(outcome.documents_deleted, 1, "only one entry found a row");
    assert_eq!(outcome.tombstones_recorded, 2);

    let stored = request
        .stored_facts(&path("twice.md"))
        .expect("reading a document")
        .expect("a document");
    assert_eq!(stored.document.content_hash, "hash-2");
    assert_eq!(stored.body, "second\n");

    assert_eq!(
        request
            .stored_document(&path("killed.md"))
            .expect("reading a document"),
        None
    );
    let killed = request
        .stored_tombstone(&path("killed.md"))
        .expect("reading a tombstone")
        .expect("a tombstone");
    assert_eq!(
        killed.last_content_hash.as_deref(),
        Some("hash-1"),
        "the death did not see the upsert that ran before it"
    );

    assert!(
        request
            .stored_document(&path("revived.md"))
            .expect("reading a document")
            .is_some(),
        "the upsert after the death did not stand"
    );
    let revived = request
        .stored_tombstone(&path("revived.md"))
        .expect("reading a tombstone")
        .expect("a tombstone");
    assert_eq!(
        revived.last_content_hash, None,
        "the death had a row to hash after all"
    );

    assert_eq!(request.pillars().expect("a pillar report").documents, 2);
    assert_eq!(request.pillars().expect("a pillar report").tombstones, 2);

    request.finish();
    store.verify_integrity().expect("a store after a changeset");
}

/// **The ordering rule is per entry, not per pair.** A third entry decides over a
/// second exactly as a second decides over a first, and a path that only ever
/// dies keeps one tombstone however many deaths name it.
///
/// The changeset also leaves a live document beside its own tombstone at the
/// **same generation**, which is why row presence is what decides liveness:
/// comparing the two generations answers nothing.
#[test]
fn a_path_named_three_times_ends_where_its_last_entry_left_it() {
    let scratch = Scratch::new("three-entries");
    let mut store = scratch.open();
    let mut request = store.begin_request();

    let outcome = request
        .apply_increment(
            IncrementProvenance::Derived,
            [
                upsert("revived.md", "hash-1", "first\n"),
                death("revived.md", Provenance::WatcherRemoval),
                upsert("revived.md", "hash-2", "second\n"),
                // A path nothing ever derived, killed twice.
                death("twice/dead.md", Provenance::HealPrune),
                death("twice/dead.md", Provenance::PlanDelete),
            ],
        )
        .expect("applying a changeset");

    assert_eq!(outcome.documents_upserted, 2);
    assert_eq!(
        outcome.documents_deleted, 1,
        "a death that found no row removed one"
    );
    assert_eq!(outcome.tombstones_recorded, 3);

    let revived = request
        .stored_facts(&path("revived.md"))
        .expect("reading a document")
        .expect("the upsert after the death did not stand");
    assert_eq!(revived.document.content_hash, "hash-2");
    assert_eq!(revived.body, "second\n");

    let tombstone = request
        .stored_tombstone(&path("revived.md"))
        .expect("reading a tombstone")
        .expect("a tombstone");
    assert_eq!(
        tombstone.last_content_hash.as_deref(),
        Some("hash-1"),
        "the death did not read the row the entry before it wrote"
    );
    assert_eq!(
        revived.document.generation, tombstone.generation,
        "a document and the tombstone it outlived stand at one generation, so nothing about \
         which is current can be read off them"
    );

    let twice_dead = request
        .stored_tombstone(&path("twice/dead.md"))
        .expect("reading a tombstone")
        .expect("a tombstone");
    assert_eq!(
        twice_dead.last_content_hash, None,
        "a path nothing ever derived had a hash to record"
    );
    assert_eq!(
        twice_dead.provenance,
        Provenance::PlanDelete,
        "the second death is the most recent answer"
    );

    assert_eq!(request.pillars().expect("a pillar report").documents, 1);
    assert_eq!(request.pillars().expect("a pillar report").tombstones, 2);

    request.finish();
    store.verify_integrity().expect("a store after a changeset");
}

/// **A re-death inside one changeset keeps the hash already recorded.** The hash
/// is the comparison basis a tombstone exists to carry, and a death learned from
/// a path that is already absent has nothing to hash — so the retention rule has
/// to hold between two entries of one changeset exactly as it holds between two
/// changesets.
#[test]
fn a_re_death_within_one_changeset_keeps_the_hash_already_recorded() {
    let scratch = Scratch::new("changeset-retention");
    let mut store = scratch.open();
    let mut request = store.begin_request();

    let outcome = request
        .apply_increment(
            IncrementProvenance::Derived,
            [
                upsert("glossary.md", "hash-1", "a body\n"),
                death("glossary.md", Provenance::WatcherRemoval),
                // The heal found it absent after the watcher had already
                // reported it gone: nothing left to hash.
                death("glossary.md", Provenance::HealPrune),
            ],
        )
        .expect("applying a changeset");
    assert_eq!(outcome.documents_deleted, 1);
    assert_eq!(outcome.tombstones_recorded, 2);

    let tombstone = request
        .stored_tombstone(&path("glossary.md"))
        .expect("reading a tombstone")
        .expect("a tombstone");
    assert_eq!(
        tombstone.last_content_hash.as_deref(),
        Some("hash-1"),
        "the re-death overwrote the comparison basis the tombstone carried"
    );
    // Everything the second death did know is the most recent answer.
    assert_eq!(tombstone.provenance, Provenance::HealPrune);
    assert_eq!(Some(tombstone.generation), outcome.generation);
    assert_eq!(request.pillars().expect("a pillar report").tombstones, 1);
}

/// **The resolution axis is scoped by affected ambiguity class, and the scope is
/// exact.** A finding in a class the changeset's paths are in dies; a finding in
/// a class none of them is in survives; a finding in no class at all is outside
/// this axis, and the subject axis is what reaches it. The outcome names the
/// classes, because re-recording what holds now is the caller's half of
/// discard-then-record.
///
/// Every finding here is recorded about `docs/index.md`, and no changeset names
/// that path until the last one — so what the discards reach is the class axis
/// alone, until the case says otherwise.
#[test]
fn a_changeset_discards_the_findings_in_the_classes_its_paths_are_in() {
    let scratch = Scratch::new("class-scope");
    let mut store = scratch.open();
    let mut request = store.begin_request();

    write_documents(
        &mut request,
        &[
            document("docs/norn/glossary.md", "hash-1", "a body\n"),
            document("docs/index.md", "hash-1", "a body\n"),
        ],
    );
    // In the class a changed `**/glossary.md` affects — including one about a
    // longer suffix inside that class, which the same range reaches.
    request
        .record_finding(&ambiguity("docs/index.md", "glossary", "glossary/", &[], 3))
        .expect("recording a finding");
    request
        .record_finding(&ambiguity(
            "docs/index.md",
            "norn/glossary",
            "glossary/norn/",
            &[],
            2,
        ))
        .expect("recording a finding");
    // In another class, and in no class at all.
    request
        .record_finding(&ambiguity("docs/index.md", "notes", "notes/", &[], 4))
        .expect("recording a finding");
    request
        .record_finding(&violation("docs/index.md"))
        .expect("recording a finding");
    assert_eq!(request.pillars().expect("a pillar report").findings, 4);

    let outcome = request
        .apply_increment(
            IncrementProvenance::Derived,
            [upsert("docs/norn/glossary.md", "hash-2", "another body\n")],
        )
        .expect("applying a changeset");

    assert_eq!(
        outcome.affected_classes,
        classes(&["glossary/"]),
        "the changeset named a class its paths are not in"
    );
    assert_eq!(outcome.invalidated.findings_discarded, 2);
    assert_eq!(request.counters().get("findings_discarded"), Some(2));

    let surviving = request
        .stored_findings(&path("docs/index.md"))
        .expect("reading findings");
    assert_eq!(surviving.len(), 2, "the discard reached outside the class");
    let mut targets: Vec<Option<&str>> = surviving
        .iter()
        .map(|finding| finding.target.as_deref())
        .collect();
    targets.sort();
    assert_eq!(targets, vec![None, Some("notes")]);

    // A death scopes the same way, from the class the dead path was in.
    let died = request
        .apply_increment(
            IncrementProvenance::Derived,
            [death("archive/notes.md", Provenance::HealPrune)],
        )
        .expect("applying a changeset");
    assert_eq!(died.affected_classes, classes(&["notes/"]));
    assert_eq!(died.invalidated.findings_discarded, 1);
    assert_eq!(request.counters().get("findings_discarded"), Some(3));

    // What is left is the finding no class reaches: two changesets have run and
    // neither named the path it is about.
    let left = request
        .stored_findings(&path("docs/index.md"))
        .expect("reading findings");
    assert_eq!(left.len(), 1);
    assert!(left[0].class_keys.is_empty());

    // It is reachable on the other axis, and only there. Re-deriving the
    // document it is about discards it, from a changeset naming a class it is
    // not in.
    let subject = request
        .apply_increment(
            IncrementProvenance::Derived,
            [upsert("docs/index.md", "hash-2", "another body\n")],
        )
        .expect("applying a changeset");
    assert_eq!(subject.affected_classes, classes(&["index/"]));
    assert_eq!(
        subject.invalidated.findings_discarded, 1,
        "a re-derivation kept the findings read off the facts it replaced"
    );
    assert!(
        request
            .stored_findings(&path("docs/index.md"))
            .expect("reading findings")
            .is_empty()
    );

    request.finish();
    store
        .verify_integrity()
        .expect("a store whose findings left through a changeset");
}

/// **A changeset discards the findings recorded about every path it names,
/// whatever class they are in.** A re-derivation's findings were read off facts
/// the changeset has just replaced and a death's describe a document that is
/// gone, so the subject axis is what makes discard-then-record total: the caller
/// re-mints from the facts it handed over, entry by entry.
///
/// The findings here are in classes no path of the changeset is in, so the class
/// axis reaches none of them and what the changeset takes is exactly the two
/// subjects it names.
#[test]
fn a_changeset_discards_the_findings_recorded_about_every_path_it_names() {
    let scratch = Scratch::new("subject-scope");
    let mut store = scratch.open();
    let mut request = store.begin_request();

    write_documents(
        &mut request,
        &[
            document("re/derived.md", "hash-1", "a body\n"),
            document("about/to/die.md", "hash-1", "a body\n"),
            document("untouched.md", "hash-1", "a body\n"),
        ],
    );
    // One finding per subject, all in the same class — a class none of the
    // changeset's own paths is in, so only the subject axis can reach them. The
    // one about `about/to/die.md` carries candidate rows, so its subject discard
    // also pins the finding_candidates FK cascade.
    for at in ["re/derived.md", "about/to/die.md", "untouched.md"] {
        let finding = if at == "about/to/die.md" {
            ambiguity(
                at,
                "glossary",
                "glossary/",
                &["a/glossary.md", "b/glossary.md"],
                2,
            )
        } else {
            ambiguity(at, "glossary", "glossary/", &[], 3)
        };
        request
            .record_finding(&finding)
            .expect("recording a finding");
    }
    // And one that is in no class at all, about a path the changeset names.
    request
        .record_finding(&violation("re/derived.md"))
        .expect("recording a finding");
    assert_eq!(request.pillars().expect("a pillar report").findings, 4);

    let outcome = request
        .apply_increment(
            IncrementProvenance::Derived,
            [
                upsert("re/derived.md", "hash-2", "another body\n"),
                death("about/to/die.md", Provenance::PlanDelete),
            ],
        )
        .expect("applying a changeset");

    assert_eq!(
        outcome.affected_classes,
        classes(&["derived/", "die/"]),
        "the changeset names the class the findings are in, so the axes are not told apart"
    );
    assert_eq!(
        outcome.invalidated.findings_discarded, 3,
        "the changeset did not take exactly the findings about the paths it named"
    );
    assert_eq!(request.counters().get("findings_discarded"), Some(3));

    for at in ["re/derived.md", "about/to/die.md"] {
        assert!(
            request
                .stored_findings(&path(at))
                .expect("reading findings")
                .is_empty(),
            "a finding about `{at}`, which the changeset named, survived it"
        );
    }
    let surviving = request
        .stored_findings(&path("untouched.md"))
        .expect("reading findings");
    assert_eq!(
        surviving.len(),
        1,
        "the discard reached a path the changeset never named"
    );

    request.finish();
    store
        .verify_integrity()
        .expect("a store whose findings left through a changeset");
}

/// **A finding both axes reach leaves once.** The subject discard runs first and
/// the class range then finds it gone, so a changeset that re-derives the
/// document a finding is written in *and* names the class that finding is about
/// reports one departure rather than two.
#[test]
fn a_finding_both_axes_reach_is_counted_once() {
    let scratch = Scratch::new("both-axes");
    let mut store = scratch.open();
    let mut request = store.begin_request();

    // The finding is written in `glossary.md` and about the class `glossary/`,
    // which is the class `glossary.md` itself is in — so re-deriving that one
    // document reaches it on both axes.
    write_document(
        &mut request,
        &document("docs/glossary.md", "hash-1", "a body\n"),
    );
    request
        .record_finding(&ambiguity(
            "docs/glossary.md",
            "glossary",
            "glossary/",
            &[],
            3,
        ))
        .expect("recording a finding");

    let outcome = request
        .apply_increment(
            IncrementProvenance::Derived,
            [upsert("docs/glossary.md", "hash-2", "another body\n")],
        )
        .expect("applying a changeset");
    assert_eq!(outcome.affected_classes, classes(&["glossary/"]));
    assert_eq!(
        outcome.invalidated.findings_discarded, 1,
        "one finding on two axes was counted twice"
    );
    assert_eq!(request.counters().get("findings_discarded"), Some(1));
    assert_eq!(request.pillars().expect("a pillar report").findings, 0);
}

/// **A changeset names one class per distinct stem among its paths, and the
/// discard is one act per class.** A finding in two of the affected classes is
/// one finding on the way out, and the count is findings rather than membership
/// rows.
#[test]
fn a_changeset_counts_a_finding_in_two_of_its_classes_once() {
    let scratch = Scratch::new("two-classes");
    let mut store = scratch.open();
    let mut request = store.begin_request();

    // `notes.tar` reduces two ways, and the two prefixes are disjoint under
    // `BINARY`, so the finding is in both `notes.tar/` and `notes/`.
    request
        .record_finding(&crate::common::ambiguity_for_target(
            "docs/index.md",
            "notes.tar",
            &[],
            2,
        ))
        .expect("recording a finding");

    let outcome = request
        .apply_increment(
            IncrementProvenance::Derived,
            [
                upsert("archive/notes.tar.gz", "hash-1", "a body\n"),
                upsert("notes.md", "hash-1", "a body\n"),
            ],
        )
        .expect("applying a changeset");

    assert_eq!(
        outcome.affected_classes,
        classes(&["notes.tar/", "notes/"]),
        "the two paths did not name the two classes the finding is in"
    );
    assert_eq!(
        outcome.invalidated.findings_discarded, 1,
        "one finding in two affected classes was counted twice"
    );
    assert_eq!(request.counters().get("findings_discarded"), Some(1));
    assert_eq!(request.pillars().expect("a pillar report").findings, 0);
    assert_eq!(
        request
            .pillars()
            .expect("a pillar report")
            .finding_candidates,
        0
    );
}

/// Two paths with the same stem are one class, so the set the changeset carries
/// is its distinct stems rather than its entries.
#[test]
fn two_paths_in_one_class_name_that_class_once() {
    let scratch = Scratch::new("one-class");
    let mut store = scratch.open();
    let mut request = store.begin_request();

    let outcome = request
        .apply_increment(
            IncrementProvenance::Derived,
            [
                upsert("one/glossary.md", "hash-1", "a body\n"),
                upsert("two/glossary.md", "hash-1", "a body\n"),
                death("three/glossary.md", Provenance::HealPrune),
            ],
        )
        .expect("applying a changeset");
    assert_eq!(outcome.affected_classes, classes(&["glossary/"]));
}

/// **A `Composed` changeset records no store-side recomputation of the state it
/// was handed.** The mark changes no statement the increment runs, so the bar is
/// the counters: the same changeset reads the same counters under either mark,
/// and the counters that moved are the rows it wrote and discarded.
///
/// The one counter here that names a computation is the canonical-JSON
/// projection, and it is storage encoding rather than derivation — the identical
/// code path under either mark, learning nothing the caller did not hand over.
/// See `norn_store::DerivationCounters`.
#[test]
fn a_composed_changeset_recomputes_nothing_it_was_handed() {
    let reading = |provenance| {
        let scratch = Scratch::new("composed");
        let mut store = scratch.open();
        let mut request = store.begin_request();
        request
            .apply_increment(
                provenance,
                [
                    Change::Upsert(document_with_every_fact("docs/norn/glossary.md", "hash-1")),
                    Change::Upsert(document_with_every_fact("docs/index.md", "hash-1")),
                    death("gone.md", Provenance::PlanDelete),
                ],
            )
            .expect("applying a changeset");
        let outcome = request
            .apply_increment(
                provenance,
                [Change::Upsert(document_with_every_fact(
                    "docs/norn/glossary.md",
                    "hash-2",
                ))],
            )
            .expect("re-deriving a document");
        assert_eq!(outcome.documents_upserted, 1);
        snapshot(&request.finish())
    };

    let derived = reading(IncrementProvenance::Derived);
    let composed = reading(IncrementProvenance::Composed);
    derived.assert_equal_counts(&composed, "the same changeset under either mark");

    // The whole of what a Composed changeset moved: rows written, rows
    // discarded, deaths recorded, and the projections that encoded supplied
    // frontmatter for storage. Nothing was minted and nothing was re-derived.
    assert_eq!(
        composed.nonzero(),
        vec![
            ("block_rows_written", 6),
            ("documents_upserted", 3),
            ("fact_rows_discarded", 8),
            ("frontmatter_projections", 3),
            ("heading_rows_written", 6),
            ("link_rows_written", 6),
            ("tag_rows_written", 6),
            ("tombstones_recorded", 1),
        ]
    );
    for minted in ["findings_written", "vectors_written", "vault_schema_pins"] {
        assert_eq!(
            composed.get(minted),
            0,
            "a changeset minted `{minted}`, which is not a changeset's to mint"
        );
    }
}

/// **A changeset is streamed, and what lives across its entries is bounded.** The
/// entry point takes an `IntoIterator`, so a caller hands over something that
/// yields entries rather than a collection of them — this case hands over a
/// generator that materializes no collection at all and applies a thousand
/// documents out of it.
///
/// What the store keeps across entries is stated at
/// `norn_store::Request::apply_increment`: the prepared statements, which are a
/// fixed cost; the running tally, which is scalars; and the set of affected
/// classes, which holds one key per distinct stem among the changed paths. The
/// entry itself is dropped before the next one is pulled, so the store holds
/// one.
///
/// What the counter pair either side of the thousand asserts is **attribution
/// size-independence**: writing one document of a given shape costs the same
/// counters whatever else the store now holds. Counters count rows, so they say
/// nothing about what a stream held in memory or how long a lock was kept — that
/// is measured, not counted.
#[test]
fn a_changeset_is_applied_from_a_generator_that_materializes_nothing() {
    const BULK: usize = 1_000;
    let scratch = Scratch::new("streamed");
    let mut store = scratch.open();

    let mut first = store.begin_request();
    write_document(
        &mut first,
        &document_with_every_fact("small/one.md", "hash-1"),
    );
    let small = snapshot(&first.finish());

    let mut request = store.begin_request();
    let mut produced = 0_usize;
    let outcome = request
        .apply_increment(
            IncrementProvenance::Derived,
            std::iter::from_fn(|| {
                if produced == BULK {
                    return None;
                }
                let index = produced;
                produced += 1;
                Some(upsert(
                    &format!("bulk/note-{index}.md"),
                    &format!("hash-{index}"),
                    "a body\n",
                ))
            }),
        )
        .expect("applying a changeset out of a generator");

    assert_eq!(outcome.documents_upserted, BULK as u64);
    assert_eq!(
        outcome.affected_classes.len(),
        BULK,
        "the classes are the changed paths' distinct stems"
    );
    assert!(
        outcome.generation.is_some(),
        "a thousand documents landed under one generation"
    );
    request.finish();

    let mut second = store.begin_request();
    write_document(
        &mut second,
        &document_with_every_fact("large/one.md", "hash-1"),
    );
    let large = snapshot(&second.finish());
    small.assert_equal_counts(&large, "one document of the same shape, either side");

    store.verify_integrity().expect("a store after a thousand");
}

/// **A changeset that refuses leaves nothing behind and consumes no
/// generation.** The entries ahead of the refusal already ran their statements,
/// so what makes them absent afterwards is the rollback rather than their never
/// having happened — and the same rollback returns the generation the changeset
/// took before its first entry.
///
/// This is the other end of the tear: a process killed mid-changeset and a
/// changeset refused mid-changeset leave the same store, and only one of them is
/// reachable without killing anything.
#[test]
fn a_refused_changeset_rolls_back_the_entries_that_ran_before_it() {
    let scratch = Scratch::new("refused-changeset");
    let mut store = scratch.open();
    let mut request = store.begin_request();

    let before = write_documents(
        &mut request,
        &[
            document("notes/one.md", "hash-1", "the first body\n"),
            document("notes/two.md", "hash-1", "the second body\n"),
        ],
    );
    // Written in the first document and about the third one's class, so both
    // axes of the refused changeset's findings maintenance would reach it.
    request
        .record_finding(&ambiguity("notes/one.md", "three", "three/", &[], 2))
        .expect("recording a finding");
    // A committed changeset immediately before, so the generation the refused
    // one would have taken is known exactly.
    let marker = write_document(&mut request, &document("marker.md", "hash-1", "a body\n"));
    let counted = snapshot(request.counters());

    // The third entry does not add up: its body offset and body do not account
    // for the byte length it claims.
    let mut refused = document("notes/three.md", "hash-1", "a body\n");
    refused.byte_length = 4_096;

    let error = request
        .apply_increment(
            IncrementProvenance::Derived,
            [
                upsert("notes/one.md", "hash-2", "a replaced body\n"),
                death("notes/two.md", Provenance::PlanDelete),
                Change::Upsert(refused),
                upsert("notes/four.md", "hash-1", "never reached\n"),
            ],
        )
        .expect_err("a changeset carrying an entry that does not add up");

    let StoreError::Entry {
        index,
        path: named,
        problem,
    } = &error
    else {
        panic!("the refusal does not say which entry it came from: {error:?}");
    };
    assert_eq!(*index, 2, "the refusal named the wrong entry");
    assert_eq!(named, "notes/three.md");
    assert!(matches!(**problem, StoreError::Bound { .. }), "{problem:?}");

    // The entries that ran before the refusal are rolled back: the re-derivation
    // did not stand and the death did not stand.
    let one = request
        .stored_facts(&path("notes/one.md"))
        .expect("reading a document")
        .expect("a document the refused changeset removed");
    assert_eq!(one.document.content_hash, "hash-1");
    assert_eq!(one.body, "the first body\n");
    assert_eq!(
        Some(one.document.generation),
        before.generation,
        "a re-derivation inside a refused changeset stood"
    );
    assert!(
        request
            .stored_document(&path("notes/two.md"))
            .expect("reading a document")
            .is_some(),
        "a death inside a refused changeset stood"
    );
    assert_eq!(
        request
            .stored_tombstone(&path("notes/two.md"))
            .expect("reading a tombstone"),
        None,
        "a refused changeset left a tombstone at rest"
    );
    // Neither the entry that was refused nor the one after it is anywhere.
    for at in ["notes/three.md", "notes/four.md"] {
        assert_eq!(
            request
                .stored_document(&path(at))
                .expect("reading a document"),
            None,
            "`{at}` is at rest after the changeset that named it refused"
        );
    }
    assert_eq!(request.pillars().expect("a pillar report").documents, 3);
    assert!(
        request
            .full_text_matches("replaced")
            .expect("reading matches")
            .is_empty(),
        "the full-text index holds terms only the refused changeset wrote"
    );
    // The discards the entries ahead of the refusal ran are rolled back too.
    assert_eq!(
        request
            .stored_findings(&path("notes/one.md"))
            .expect("reading findings")
            .len(),
        1,
        "a refused changeset's findings discard stood"
    );

    // Counters record what happened, and none of this did.
    snapshot(request.counters())
        .assert_equal_counts(&counted, "a request either side of a refused changeset");

    // No generation was consumed: the next changeset takes the one the refused
    // changeset would have.
    let next = request
        .apply_increment(
            IncrementProvenance::Derived,
            [upsert("notes/five.md", "hash-1", "a body\n")],
        )
        .expect("applying a changeset");
    assert_eq!(
        next.generation,
        marker.generation.map(|generation| generation + 1),
        "the refused changeset consumed a generation"
    );

    request.finish();
    store
        .verify_integrity()
        .expect("a store a changeset was refused in");
}

/// The environment variable that puts this suite's own binary in the child role,
/// carrying the database whose changeset the child is to be killed inside.
const TORN_CHANGESET_DATABASE: &str = "NORN_STORE_TORN_CHANGESET_DATABASE";

/// The case the child is asked to run, which is this one. A name that has moved
/// runs nothing in the child, and the parent's assertion on how the child ended
/// is what says so.
const TORN_CHANGESET_CASE: &str =
    "increments::a_torn_changeset_leaves_the_previous_generation_whole";

/// `SIGABRT`, which is how [`std::process::abort`] ends a process. Spelled as
/// the number rather than taken from a dependency, because the harness's own
/// `libc` is not this crate's.
#[cfg(unix)]
const SIGABRT: i32 = 6;

/// **A process killed mid-changeset leaves the previous generation whole.** The
/// child opens the store the parent wrote and applies a changeset it does not
/// survive: the abort lands between two entries, with the transaction open and
/// nothing committed, which is what a `SIGKILL` looks like to the file. No
/// unwinding, no rollback, no destructor — the tidy end is not the one rung 2
/// has to survive.
///
/// The parent then reopens and judges what is there: the pre-changeset documents
/// read back exactly, at the generation they were written at; nothing the torn
/// changeset was writing is at rest at any generation; the findings it would
/// have discarded by class are still there; and the database is a database this
/// build reuses rather than one it has to rebuild.
///
/// **This case is its own child.** The environment variable is what tells the
/// two roles apart, so the child is a run of this same binary filtered to this
/// same name — which keeps the arrangement and the assertions in one place
/// instead of behind an ignored case that says nothing when it is run alone.
#[test]
fn a_torn_changeset_leaves_the_previous_generation_whole() {
    if let Some(database) = std::env::var_os(TORN_CHANGESET_DATABASE) {
        tear_a_changeset(Path::new(&database));
    }

    let scratch = Scratch::new("torn-changeset");
    let database = scratch.database();

    let mut store = Store::open(&database).expect("creating a store");
    let mut request = store.begin_request();
    let before = write_documents(
        &mut request,
        &[
            document("notes/one.md", "hash-1", "the first body\n"),
            document("notes/two.md", "hash-1", "the second body\n"),
        ],
    );
    // A finding in a class the torn changeset affects, so the discard it folds
    // in is observable by its absence.
    request
        .record_finding(&ambiguity("notes/two.md", "one", "one/", &[], 2))
        .expect("recording a finding");
    request.finish();
    store.verify_integrity().expect("a store just written to");
    drop(store);

    let child = Command::new(std::env::current_exe().expect("this suite's own executable"))
        .args(["--exact", TORN_CHANGESET_CASE])
        .env(TORN_CHANGESET_DATABASE, &database)
        .output()
        .expect("running this suite in the child role");
    assert!(
        !child.status.success(),
        "the child was to be killed inside a changeset and it finished: {}",
        String::from_utf8_lossy(&child.stderr)
    );
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            child.status.signal(),
            Some(SIGABRT),
            "the child ended some way other than the abort the arrangement arms, so it did not \
             reach the changeset: {}",
            String::from_utf8_lossy(&child.stderr)
        );
    }

    // The write-ahead log the child left is the evidence that the tear happened
    // to the file. Read before reopening, because an open recovers the log.
    let spilled = write_ahead_log_size(&database);
    assert!(
        spilled >= SPILLED_WAL_FLOOR,
        "the torn changeset left a {spilled}-byte write-ahead log, so it never reached the file \
         and what survived the tear was one process's memory"
    );

    let mut reopened = Store::open(&database).expect("reopening a store");
    assert_eq!(
        *reopened.open_outcome(),
        OpenOutcome::Reused,
        "a torn changeset left a database this build cannot read"
    );

    let mut request = reopened.begin_request();
    for (at, hash, body) in [
        ("notes/one.md", "hash-1", "the first body\n"),
        ("notes/two.md", "hash-1", "the second body\n"),
    ] {
        let stored = request
            .stored_facts(&path(at))
            .expect("reading a document")
            .expect("a document the previous generation wrote");
        assert_eq!(
            stored.document.content_hash, hash,
            "`{at}` carries what the torn changeset was writing"
        );
        assert_eq!(stored.body, body);
        assert_eq!(
            Some(stored.document.generation),
            before.generation,
            "`{at}` moved to a generation the torn changeset took"
        );
    }

    // Nothing the torn changeset was writing is at rest, at any generation —
    // neither the entry it applied before the tear nor the one it never reached.
    for at in ["notes/three.md", "notes/four.md"] {
        assert_eq!(
            request
                .stored_document(&path(at))
                .expect("reading a document"),
            None,
            "`{at}`, which only the torn changeset wrote, is at rest"
        );
    }
    assert_eq!(request.pillars().expect("a pillar report").documents, 2);
    assert!(
        request
            .full_text_matches("torn")
            .expect("reading matches")
            .is_empty(),
        "the full-text index holds terms only the torn changeset wrote"
    );
    // The discard the torn changeset would have folded in did not happen either.
    assert_eq!(
        request
            .stored_findings(&path("notes/two.md"))
            .expect("reading findings")
            .len(),
        1,
        "the torn changeset's class discard survived it"
    );

    request.finish();
    reopened
        .verify_integrity()
        .expect("a store a changeset was torn in");
}

/// How many bytes stand in the write-ahead log beside a database, or zero where
/// there is none.
#[allow(clippy::disallowed_methods)] // Harness scaffolding: sizing the sidecar a torn changeset left.
fn write_ahead_log_size(database: &Path) -> u64 {
    let mut wal = database.to_path_buf().into_os_string();
    wal.push("-wal");
    std::fs::metadata(&wal).map_or(0, |wal| wal.len())
}

/// The child half of the case above: apply a changeset this process does not
/// survive, and never return.
fn tear_a_changeset(database: &Path) -> ! {
    let mut store = Store::open(database).expect("opening the store the parent wrote");
    // A page cache far smaller than the changeset's own pages. SQLite then
    // writes the uncommitted pages into the write-ahead log rather than holding
    // them, which is what makes the file — and not one process's memory — the
    // thing the invariant is asserted about.
    norn_store::induced_failure::execute_out_of_band(&mut store, "PRAGMA cache_size = 16")
        .expect("shrinking the page cache");

    // Two entries either side of the tear: a re-derivation and a new document
    // are applied and never committed, and a third entry is never reached at
    // all. Both are absent afterwards, and neither for the same reason.
    norn_store::induced_failure::abort_after_changeset_entries(2);
    let _ = store.begin_request().apply_increment(
        IncrementProvenance::Derived,
        [
            upsert("notes/one.md", "hash-2", &torn_body()),
            upsert("notes/three.md", "hash-1", &torn_body()),
            upsert("notes/four.md", "hash-1", &torn_body()),
        ],
    );
    panic!("the changeset committed, so the arrangement that arms the abort did not fire");
}

/// A body large enough that writing it dirties far more pages than the child's
/// page cache holds, so the transaction has to spill before it is torn.
fn torn_body() -> String {
    "a torn body\n".repeat(TORN_BODY_LINES)
}

/// How many lines each torn body carries. Twenty thousand of them is a little
/// over 200 KiB per document, against a 16-page cache.
const TORN_BODY_LINES: usize = 20_000;

/// The smallest write-ahead log the torn changeset can plausibly have left.
///
/// One document's body is over 200 KiB and two of them were written before the
/// tear, so a log this size cannot have been produced by anything but the
/// changeset's own uncommitted pages. The bar is a floor rather than a
/// measurement: what it fails on is a log that is absent or trivial, which is
/// what "the increment stopped writing through an open transaction" looks like
/// from outside.
const SPILLED_WAL_FLOOR: u64 = 128 * 1024;

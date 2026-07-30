//! The parse-fact tables: what a document write puts at rest, and what a delete
//! takes with it.
//!
//! Every table's shape is exercised by writing rows and reading them back, so a
//! column that does not hold what it claims fails here rather than at the first
//! query that needed it. The fact types go in and come back, which is what makes
//! the comparison a statement about the store rather than about the assertion.

mod common;

use common::{Scratch, document, document_with_every_fact, path, span};
use norn_store::{
    BlockFact, DocumentFacts, FrontmatterValue, HeadingFact, LinkFact, LinkFamily, Provenance,
    TagFact, TagSource, ddl,
};

/// One of every fact shape, written and read back unchanged — including the
/// optional fields that are absent, which is where a column that quietly
/// substitutes a default shows up.
#[test]
fn every_fact_shape_survives_the_round_trip() {
    let scratch = Scratch::new("round-trip");
    let mut store = scratch.open();
    let facts = document_with_every_fact("docs/norn/glossary.md", "hash-1");

    let mut request = store.begin_request();
    request.upsert_document(&facts).expect("writing a document");
    let stored = request
        .stored_facts(&facts.path)
        .expect("reading a document")
        .expect("a document that was just written");

    assert_eq!(stored.document.path, facts.path);
    assert_eq!(stored.document.content_hash, facts.content_hash);
    assert_eq!(stored.document.byte_length, facts.byte_length);
    assert_eq!(stored.document.body_offset, facts.body_offset);
    assert_eq!(stored.document.diagnostic_count, facts.diagnostic_count);
    assert_eq!(stored.body, facts.body);
    assert_eq!(stored.links, facts.links);
    assert_eq!(stored.headings, facts.headings);
    assert_eq!(stored.blocks, facts.blocks);
    assert_eq!(stored.tags, facts.tags);

    // The projection is the one thing that does not come back as it went in: it
    // is JSON, and it is canonical.
    assert_eq!(
        stored.document.frontmatter.as_deref(),
        Some(r#"{"draft":false,"title":"Norn"}"#)
    );

    store.verify_integrity().expect("a store just written to");
}

/// The derived path forms come back as the same type that produced them, so a
/// reader gets the suffix key and the stem without recomputing either.
#[test]
fn a_stored_document_carries_the_derived_path_forms() {
    let scratch = Scratch::new("derived-path");
    let mut store = scratch.open();
    let facts = document("docs/norn/glossary.md", "hash-1", "a body\n");

    let mut request = store.begin_request();
    request.upsert_document(&facts).expect("writing a document");
    let stored = request
        .stored_document(&facts.path)
        .expect("reading a document")
        .expect("a document that was just written");
    assert_eq!(stored.path.suffix_key(), "glossary/norn/docs/");
    assert_eq!(stored.path.stem(), "glossary");
    assert_eq!(stored.path.depth(), 3);
}

/// A document with no frontmatter block projects `NULL`, and one with an empty
/// block projects `{}`. They are different documents and the column says so.
#[test]
fn an_absent_frontmatter_block_and_an_empty_one_are_different_values() {
    let scratch = Scratch::new("frontmatter");
    let mut store = scratch.open();
    let absent = document("absent.md", "hash-1", "a body\n");
    let mut empty = document("empty.md", "hash-2", "a body\n");
    empty.frontmatter = Some(FrontmatterValue::Map(Vec::new()));

    let mut request = store.begin_request();
    request
        .upsert_document(&absent)
        .expect("writing a document");
    request.upsert_document(&empty).expect("writing a document");

    assert_eq!(
        request
            .stored_document(&absent.path)
            .expect("reading a document")
            .expect("a document")
            .frontmatter,
        None
    );
    assert_eq!(
        request
            .stored_document(&empty.path)
            .expect("reading a document")
            .expect("a document")
            .frontmatter
            .as_deref(),
        Some("{}")
    );

    // Only the projection that exists was made.
    assert_eq!(request.counters().get("frontmatter_projections"), 1);
}

/// A re-derivation replaces a document's fact rows wholesale and keeps the
/// document's own row. Nothing accumulates, and nothing is merged.
#[test]
fn a_re_derivation_replaces_fact_rows_wholesale() {
    let scratch = Scratch::new("replace");
    let mut store = scratch.open();
    let first = document_with_every_fact("docs/norn/glossary.md", "hash-1");

    let mut request = store.begin_request();
    request.upsert_document(&first).expect("writing a document");
    let before = request
        .stored_document(&first.path)
        .expect("reading a document")
        .expect("a document");

    let mut second = document("docs/norn/glossary.md", "hash-2", "a shorter body\n");
    second.headings = vec![HeadingFact {
        level: 2,
        text: "Only one".to_string(),
        slug: "only-one".to_string(),
        span: span(1, 1, 0),
        body_offset: 10,
        inside_container: false,
    }];
    request.upsert_document(&second).expect("re-deriving");

    let after = request
        .stored_facts(&second.path)
        .expect("reading a document")
        .expect("a document");
    assert_eq!(after.links, Vec::new(), "the old links are still there");
    assert_eq!(after.headings, second.headings);
    assert_eq!(after.blocks, Vec::new());
    assert_eq!(after.tags, Vec::new());
    assert_eq!(after.document.content_hash, "hash-2");
    assert_eq!(after.body, second.body);
    assert_eq!(
        after.document.frontmatter, None,
        "the old projection outlived the block it projected"
    );

    // Eight fact rows went in and were discarded; the generation moved and the
    // document is still one document.
    assert_eq!(request.counters().get("fact_rows_discarded"), 8);
    assert!(after.document.generation > before.generation);
    assert_eq!(request.pillars().expect("a pillar report").documents, 1);

    request.finish();
    store.verify_integrity().expect("a re-derived store");
}

/// Ordinals are dense and ascending because the store assigns them from the
/// emission order it was handed. `UNIQUE(document, ordinal)` is what keeps that
/// true of the rows at rest, and it is declared for every fact table.
#[test]
fn fact_rows_keep_the_emission_order_they_were_handed() {
    let scratch = Scratch::new("ordinals");
    let mut store = scratch.open();

    let mut facts = document("notes.md", "hash-1", "a body\n");
    facts.links = (0..8)
        .map(|index| LinkFact {
            family: if index % 2 == 0 {
                LinkFamily::Wikilink
            } else {
                LinkFamily::Markdown
            },
            embed: false,
            protocol: None,
            // Every link addresses the same target from the same byte, so the
            // only thing that can order the rows is the order they arrived in.
            target: format!("target-{index}"),
            title: None,
            anchor: None,
            block_ref: None,
            span: span(1, 1, 0),
        })
        .collect();

    let mut request = store.begin_request();
    request.upsert_document(&facts).expect("writing a document");
    let stored = request
        .stored_facts(&facts.path)
        .expect("reading a document")
        .expect("a document");
    assert_eq!(stored.links, facts.links);

    // Writing the same document again produces the same rows rather than a
    // second copy of them.
    request.upsert_document(&facts).expect("re-deriving");
    let again = request
        .stored_facts(&facts.path)
        .expect("reading a document")
        .expect("a document");
    assert_eq!(again.links, facts.links);
}

/// The constraint behind that: every fact table declares its ordinal unique
/// within a document, so a second row claiming one position cannot be at rest
/// whatever writes it.
#[test]
fn every_fact_table_declares_its_ordinal_unique_within_a_document() {
    let declared: Vec<&str> = ddl::statements()
        .filter(|statement| statement.contains("UNIQUE INDEX"))
        .collect();
    for table in ["links", "headings", "blocks", "document_tags"] {
        let needle = format!("ON {table}(document, ordinal)");
        assert!(
            declared.iter().any(|statement| statement.contains(&needle)),
            "`{table}` does not declare `{needle}`: {declared:?}"
        );
    }
}

/// A delete is hard: the document row goes, the cascade takes everything derived
/// from it, and a tombstone records the death in the same transaction.
#[test]
fn a_delete_takes_the_derived_rows_and_leaves_a_tombstone() {
    let scratch = Scratch::new("delete");
    let mut store = scratch.open();
    let facts = document_with_every_fact("docs/norn/glossary.md", "hash-1");

    let mut request = store.begin_request();
    request.upsert_document(&facts).expect("writing a document");
    let deletion = request
        .delete_document(&facts.path, Provenance::HealPrune)
        .expect("deleting a document");
    assert!(deletion.removed);

    assert_eq!(
        request
            .stored_facts(&facts.path)
            .expect("reading a document"),
        None
    );
    let tombstone = request
        .stored_tombstone(&facts.path)
        .expect("reading a tombstone")
        .expect("a tombstone");
    assert_eq!(tombstone.path, facts.path);
    assert_eq!(tombstone.last_content_hash.as_deref(), Some("hash-1"));
    assert_eq!(tombstone.provenance, Provenance::HealPrune);
    assert_eq!(tombstone.generation, deletion.generation);

    let pillars = request.pillars().expect("a pillar report");
    assert_eq!(pillars.documents, 0);
    assert_eq!(pillars.tombstones, 1);

    request.finish();
    // The cascade left nothing referencing a document that is not there.
    store.verify_integrity().expect("a store after a delete");
}

/// A death learned for a path nothing had derived is still recorded. The
/// ordering a tombstone carries is worth most exactly then: a late event has
/// something to compare against instead of guessing.
#[test]
fn a_death_learned_for_an_underived_path_is_still_recorded() {
    let scratch = Scratch::new("unknown-delete");
    let mut store = scratch.open();
    let missing = path("never/derived.md");

    let mut request = store.begin_request();
    let deletion = request
        .delete_document(&missing, Provenance::WatcherRemoval)
        .expect("recording a death");
    assert!(!deletion.removed);

    let tombstone = request
        .stored_tombstone(&missing)
        .expect("reading a tombstone")
        .expect("a tombstone");
    assert_eq!(tombstone.last_content_hash, None);
    assert_eq!(tombstone.provenance, Provenance::WatcherRemoval);
    assert_eq!(request.counters().get("documents_deleted"), 0);
    assert_eq!(request.counters().get("tombstones_recorded"), 1);
}

/// One row per path, holding the most recent death — and a tombstone is a record
/// of a death, never a claim about the present, so a recreated path has both.
#[test]
fn a_tombstone_holds_the_most_recent_death_and_says_nothing_about_the_present() {
    let scratch = Scratch::new("re-death");
    let mut store = scratch.open();
    let subject = path("glossary.md");

    let mut request = store.begin_request();
    request
        .upsert_document(&document(subject.as_str(), "hash-1", "one\n"))
        .expect("writing a document");
    let first = request
        .delete_document(&subject, Provenance::WatcherRemoval)
        .expect("deleting a document");

    request
        .upsert_document(&document(subject.as_str(), "hash-2", "two\n"))
        .expect("recreating the document");
    let second = request
        .delete_document(&subject, Provenance::PlanDelete)
        .expect("deleting it again");

    let tombstone = request
        .stored_tombstone(&subject)
        .expect("reading a tombstone")
        .expect("a tombstone");
    assert_eq!(tombstone.last_content_hash.as_deref(), Some("hash-2"));
    assert_eq!(tombstone.provenance, Provenance::PlanDelete);
    assert_eq!(tombstone.generation, second.generation);
    assert!(second.generation > first.generation);
    assert_eq!(request.pillars().expect("a pillar report").tombstones, 1);

    // Recreated, the path has a document row and a tombstone at once. The
    // document row is the one that says what is there now.
    request
        .upsert_document(&document(subject.as_str(), "hash-3", "three\n"))
        .expect("recreating the document");
    assert!(
        request
            .stored_document(&subject)
            .expect("reading a document")
            .is_some()
    );
    assert!(
        request
            .stored_tombstone(&subject)
            .expect("reading a tombstone")
            .is_some()
    );
}

/// Generations order every write in the store, across documents and across
/// kinds of write. A clock does not.
#[test]
fn every_write_takes_the_next_generation() {
    let scratch = Scratch::new("generations");
    let mut store = scratch.open();
    let mut request = store.begin_request();

    let mut generations = Vec::new();
    for (index, name) in ["one.md", "two.md", "three.md"].iter().enumerate() {
        request
            .upsert_document(&document(name, &format!("hash-{index}"), "a body\n"))
            .expect("writing a document");
        generations.push(
            request
                .stored_document(&path(name))
                .expect("reading a document")
                .expect("a document")
                .generation,
        );
    }
    assert!(
        generations.windows(2).all(|pair| pair[0] < pair[1]),
        "{generations:?} is not ascending"
    );
}

/// A path the filesystem seam would never produce is refused before it reaches a
/// column, so the index cannot hold a key that addresses the wrong documents.
#[test]
fn a_document_facts_value_cannot_be_built_from_an_unnormalized_path() {
    assert!(norn_store::DocumentPath::new("../escape.md").is_err());
    let ok = DocumentFacts::new(path("docs/note.md"), "hash-1", "a body\n");
    assert_eq!(ok.byte_length, ok.body.len() as u64);
}

/// Tags are recorded as written, from both homes, because deciding that `#Work`
/// and `#work` are one tag is a matching question and matching is not storage.
#[test]
fn a_tag_is_stored_as_written_and_says_which_home_it_came_from() {
    let scratch = Scratch::new("tags");
    let mut store = scratch.open();
    let mut facts = document("notes.md", "hash-1", "a body\n");
    facts.tags = vec![
        TagFact {
            name: "Work".to_string(),
            source: TagSource::Body,
            span: Some(span(1, 1, 0)),
        },
        TagFact {
            name: "work".to_string(),
            source: TagSource::Frontmatter,
            span: None,
        },
        TagFact {
            name: "area/sub-project".to_string(),
            source: TagSource::Body,
            span: Some(span(2, 3, 20)),
        },
    ];

    let mut request = store.begin_request();
    request.upsert_document(&facts).expect("writing a document");
    let stored = request
        .stored_facts(&facts.path)
        .expect("reading a document")
        .expect("a document");
    assert_eq!(stored.tags, facts.tags);
}

/// A block id with no locatable position is stored with none, rather than with a
/// position that was invented for it.
#[test]
fn a_block_id_with_no_span_is_stored_without_one() {
    let scratch = Scratch::new("blocks");
    let mut store = scratch.open();
    let mut facts = document("notes.md", "hash-1", "a body\n");
    facts.blocks = vec![
        BlockFact {
            block_id: "with".to_string(),
            span: Some(span(4, 1, 30)),
        },
        BlockFact {
            block_id: "without".to_string(),
            span: None,
        },
    ];

    let mut request = store.begin_request();
    request.upsert_document(&facts).expect("writing a document");
    let stored = request
        .stored_facts(&facts.path)
        .expect("reading a document")
        .expect("a document");
    assert_eq!(stored.blocks, facts.blocks);
}

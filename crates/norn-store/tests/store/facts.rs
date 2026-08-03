//! The parse-fact tables: what a document write puts at rest, and what a delete
//! takes with it.
//!
//! Every table's shape is exercised by writing rows and reading them back, so a
//! column that does not hold what it claims fails here rather than at the first
//! query that needed it. The fact types go in and come back, which is what makes
//! the comparison a statement about the store rather than about the assertion.

use crate::common::{
    Scratch, document, document_with_every_fact, path, record_death, span, write_document,
};
use norn_store::{
    BlockFact, Change, DocumentFacts, FrontmatterValue, HeadingFact, IncrementProvenance, LinkFact,
    LinkFamily, Provenance, StoreError, TagFact, TagSource, ddl,
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
    write_document(&mut request, &facts);
    let stored = request
        .stored_facts(&facts.path)
        .expect("reading a document")
        .expect("a document that was just written");

    assert_eq!(stored.document.path, facts.path);
    assert_eq!(stored.document.content_hash, facts.content_hash);
    assert_eq!(stored.document.byte_length, facts.byte_length);
    assert_eq!(stored.document.body_offset, facts.body_offset);
    assert_eq!(
        stored.document.frontmatter_diagnostic_count,
        facts.frontmatter_diagnostic_count
    );
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
    write_document(&mut request, &facts);
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
    write_document(&mut request, &absent);
    write_document(&mut request, &empty);

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
    assert_eq!(request.counters().get("frontmatter_projections"), Some(1));
}

/// **A refused frontmatter projection stays a refusal all the way out of the
/// scope that took it.** The changeset owns the entry it was handed, so a value
/// nested past the bound is refused and freed inside the refusal — and freeing
/// it is a walk with an explicit stack, because a recursive drop would abort the
/// process at the far end of the call that refused.
#[test]
fn a_frontmatter_value_past_the_bound_is_refused_and_its_facts_freed() {
    let scratch = Scratch::new("deep-frontmatter");
    let mut store = scratch.open();
    let mut facts = document("docs/deep.md", "hash-1", "a body\n");
    let mut value = FrontmatterValue::Null;
    for _ in 0..100_000 {
        value = FrontmatterValue::Sequence(vec![value]);
    }
    facts.frontmatter = Some(value);
    let subject = facts.path.clone();

    let error = store
        .begin_request()
        .apply_increment(IncrementProvenance::Derived, [Change::Upsert(facts)])
        .expect_err("a document whose frontmatter nests past the bound");
    let StoreError::Entry {
        index,
        path,
        problem,
    } = &error
    else {
        panic!("the refusal does not say which entry it came from: {error:?}");
    };
    assert_eq!(*index, 0);
    assert_eq!(path, subject.as_str());
    let StoreError::Bound { what, .. } = &**problem else {
        panic!("the nesting depth was not refused as a bound: {problem:?}");
    };
    assert!(what.contains("nesting"), "{what}");
    // Nothing was written, and the changeset freed what it was refusing rather
    // than leaking it past the error.
    assert_eq!(
        store
            .begin_request()
            .stored_document(&subject)
            .expect("reading a document"),
        None
    );
}

/// A re-derivation replaces a document's fact rows wholesale and keeps the
/// document's own row. Nothing accumulates, and nothing is merged.
#[test]
fn a_re_derivation_replaces_fact_rows_wholesale() {
    let scratch = Scratch::new("replace");
    let mut store = scratch.open();
    let first = document_with_every_fact("docs/norn/glossary.md", "hash-1");

    let mut request = store.begin_request();
    write_document(&mut request, &first);
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
    write_document(&mut request, &second);

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
    assert_eq!(request.counters().get("fact_rows_discarded"), Some(8));
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
    write_document(&mut request, &facts);
    let stored = request
        .stored_facts(&facts.path)
        .expect("reading a document")
        .expect("a document");
    assert_eq!(stored.links, facts.links);

    // Writing the same document again produces the same rows rather than a
    // second copy of them.
    write_document(&mut request, &facts);
    let again = request
        .stored_facts(&facts.path)
        .expect("reading a document")
        .expect("a document");
    assert_eq!(again.links, facts.links);
}

/// The guard behind that: every fact table declares its ordinal unique within a
/// document, so a second row claiming one position cannot be at rest whatever
/// the crate's own write loop does.
#[test]
fn every_fact_table_declares_its_ordinal_unique_within_a_document() {
    let declared: Vec<String> = ddl::statements()
        .into_iter()
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

/// The bound the API states and the bound the schema enforces are one number,
/// because the statement builds its `CHECK` out of the constant.
#[test]
fn the_candidate_rank_check_is_built_from_the_bound_the_api_states() {
    let declared = ddl::statements()
        .into_iter()
        .find(|statement| statement.contains("CREATE TABLE finding_candidates"))
        .expect("the candidate table");
    assert!(
        declared.contains(&format!(
            "CHECK (rank BETWEEN 0 AND {})",
            norn_store::CANDIDATE_HEAD - 1
        )),
        "{declared}"
    );
}

/// **A finding's ambiguity classes are rows, not a column.** A target whose leaf
/// carries a dot reduces two ways, and the two prefixes are disjoint under
/// `BINARY` — `notes.tar/` sorts before `notes/` — so a single-valued column could
/// hold only one of them and the finding would be invisible to the class the other
/// opens.
#[test]
fn a_findings_class_membership_is_a_set_of_rows() {
    let declared = ddl::statements();
    let findings = declared
        .iter()
        .find(|statement| statement.contains("CREATE TABLE findings "))
        .expect("the findings table");
    assert!(
        !findings.contains("class_key"),
        "`findings` carries a single-valued class column: {findings}"
    );

    let membership = declared
        .iter()
        .find(|statement| statement.contains("CREATE TABLE finding_classes "))
        .expect("the class membership table");
    // One row per pair, and the pair is the row rather than a payload beside one.
    assert!(
        membership.contains("PRIMARY KEY (finding, class_key)")
            && membership.contains("WITHOUT ROWID"),
        "{membership}"
    );
    // A finding's memberships are parts of it, so they go when it goes.
    assert!(
        membership.contains("REFERENCES findings(id) ON DELETE CASCADE"),
        "{membership}"
    );
    assert!(
        declared
            .iter()
            .any(|statement| statement.contains("INDEX finding_classes_class_key ")),
        "the class direction of findings maintenance has no index to seek through"
    );
}

/// A nullable span triple is all three columns or none. The reader turns a
/// partial triple into no span at all, so a row carrying one would be a position
/// silently thrown away — the `CHECK` is what means the writer cannot make one.
#[test]
fn a_nullable_span_triple_is_declared_whole() {
    let declared = ddl::statements();
    for table in ["blocks", "document_tags", "findings"] {
        let statement = declared
            .iter()
            .find(|statement| statement.contains(&format!("CREATE TABLE {table} ")))
            .unwrap_or_else(|| panic!("`{table}` has no create statement"));
        assert!(
            statement.contains("(span_line IS NULL) = (span_column IS NULL)")
                && statement.contains("(span_line IS NULL) = (span_offset IS NULL)"),
            "`{table}` does not declare its span triple whole: {statement}"
        );
    }
}

/// Whether `declared` names `column` as one of its own tokens.
///
/// Punctuation and whitespace are both boundaries, so the check does not
/// depend on how a `CREATE TABLE` statement happens to be indented, and it
/// does not fire on a column whose name merely contains `column` as a
/// substring — `system` does not carry a `stem` column.
fn declares_column(declared: &str, column: &str) -> bool {
    declared
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .any(|token| token == column)
}

/// The columns nothing reads are not columns. The stem and the segment count are
/// functions of the path, derived where the path is read, so a second home for
/// either is a spelling that can disagree with it.
#[test]
fn a_derived_path_form_has_one_home() {
    let declared = ddl::statements();
    let documents = declared
        .iter()
        .find(|statement| statement.contains("CREATE TABLE documents "))
        .expect("the documents table");
    // The positive control: a column the table does carry, so the check above
    // is known to be able to find one rather than vacuously finding none.
    assert!(
        declares_column(documents, "path"),
        "the column detector cannot find `path`, which `documents` does carry: {documents}"
    );
    for absent in ["stem", "depth"] {
        assert!(
            !declares_column(documents, absent),
            "`documents` carries a `{absent}` column: {documents}"
        );
    }
    let tombstones = declared
        .iter()
        .find(|statement| statement.contains("CREATE TABLE tombstones "))
        .expect("the tombstones table");
    for absent in ["stem", "suffix_key"] {
        assert!(
            !declares_column(tombstones, absent),
            "`tombstones` carries a `{absent}` column: {tombstones}"
        );
    }
    // And no index stands over a column no statement reads.
    for absent in [
        "documents_stem",
        "tombstones_stem",
        "tombstones_generation",
        "links_target",
        "headings_document_slug",
        "headings_document_text",
        "blocks_document_block_id",
        "document_tags_name",
        "findings_generation",
        "findings_document",
        "document_vectors_model",
    ] {
        assert!(
            !declared
                .iter()
                .any(|statement| statement.contains(&format!("INDEX {absent} "))),
            "`{absent}` is declared, and no statement in this build reads it"
        );
    }
    // The three that stay, because a statement in this build reads each: the
    // resolution ladder's range, the class direction of findings maintenance, and
    // the schema-key discard's two ranges.
    for present in [
        "documents_suffix_key",
        "finding_classes_class_key",
        "findings_vault_schema_fingerprint",
    ] {
        assert!(
            declared
                .iter()
                .any(|statement| statement.contains(&format!("INDEX {present} "))),
            "`{present}` is not declared, and a statement in this build reads it"
        );
    }
}

/// `document_vectors` is a rowid table. Its primary key is a uniqueness
/// constraint; making it the storage order too puts the embedding blob inside the
/// index B-tree, so every key comparison walks overflow chains and `count(*)` —
/// which is what the pillar report asks for — reads every leaf.
#[test]
fn the_vector_table_keeps_its_rows_behind_a_rowid() {
    let declared = ddl::statements()
        .into_iter()
        .find(|statement| statement.contains("CREATE TABLE document_vectors"))
        .expect("the vector table");
    assert!(
        declared.contains("PRIMARY KEY (document, model_id, model_version)"),
        "{declared}"
    );
    assert!(
        !declared.contains("WITHOUT ROWID"),
        "the embedding blob is stored inside the primary key's B-tree: {declared}"
    );
}

/// A delete is hard: the document row goes, the cascade takes everything derived
/// from it, and a tombstone records the death in the same transaction.
#[test]
fn a_delete_takes_the_derived_rows_and_leaves_a_tombstone() {
    let scratch = Scratch::new("delete");
    let mut store = scratch.open();
    let facts = document_with_every_fact("docs/norn/glossary.md", "hash-1");

    let mut request = store.begin_request();
    write_document(&mut request, &facts);
    let deletion = record_death(&mut request, &facts.path, Provenance::HealPrune);
    assert_eq!(deletion.documents_deleted, 1);
    assert_eq!(deletion.tombstones_recorded, 1);

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
    assert_eq!(Some(tombstone.generation), deletion.generation);

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
    let deletion = record_death(&mut request, &missing, Provenance::WatcherRemoval);
    assert_eq!(deletion.documents_deleted, 0);
    assert_eq!(deletion.tombstones_recorded, 1);

    let tombstone = request
        .stored_tombstone(&missing)
        .expect("reading a tombstone")
        .expect("a tombstone");
    assert_eq!(tombstone.last_content_hash, None);
    assert_eq!(tombstone.provenance, Provenance::WatcherRemoval);
    assert_eq!(request.counters().get("documents_deleted"), Some(0));
    assert_eq!(request.counters().get("tombstones_recorded"), Some(1));
}

/// A quarantine is a death of the derived row and not of the file, so it is its
/// own provenance: reading it back as a prune or a removal would say the path
/// left the vault.
#[test]
fn a_quarantine_death_is_recorded_under_its_own_provenance() {
    let scratch = Scratch::new("quarantine-provenance");
    let mut store = scratch.open();
    let subject = path("undecodable.md");

    let mut request = store.begin_request();
    write_document(&mut request, &document(subject.as_str(), "hash-1", "one\n"));
    record_death(&mut request, &subject, Provenance::Quarantine);

    let tombstone = request
        .stored_tombstone(&subject)
        .expect("reading a tombstone")
        .expect("a tombstone");
    assert_eq!(tombstone.provenance, Provenance::Quarantine);
    assert_eq!(tombstone.last_content_hash.as_deref(), Some("hash-1"));

    request.finish();
    // The closed vocabulary is checked rather than trusted, so a provenance the
    // store writes has to be one the integrity check admits.
    store
        .verify_integrity()
        .expect("a store after a quarantine");
}

/// One row per path, holding the most recent death — and a tombstone is a record
/// of a death, never a claim about the present, so a recreated path has both.
#[test]
fn a_tombstone_holds_the_most_recent_death_and_says_nothing_about_the_present() {
    let scratch = Scratch::new("re-death");
    let mut store = scratch.open();
    let subject = path("glossary.md");

    let mut request = store.begin_request();
    write_document(&mut request, &document(subject.as_str(), "hash-1", "one\n"));
    let first = record_death(&mut request, &subject, Provenance::WatcherRemoval);

    write_document(&mut request, &document(subject.as_str(), "hash-2", "two\n"));
    let second = record_death(&mut request, &subject, Provenance::PlanDelete);

    let tombstone = request
        .stored_tombstone(&subject)
        .expect("reading a tombstone")
        .expect("a tombstone");
    assert_eq!(tombstone.last_content_hash.as_deref(), Some("hash-2"));
    assert_eq!(tombstone.provenance, Provenance::PlanDelete);
    assert_eq!(Some(tombstone.generation), second.generation);
    assert!(second.generation > first.generation);
    assert_eq!(request.pillars().expect("a pillar report").tombstones, 1);

    // Recreated, the path has a document row and a tombstone at once. The
    // document row is the one that says what is there now.
    write_document(
        &mut request,
        &document(subject.as_str(), "hash-3", "three\n"),
    );
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

/// **A re-death keeps the hash already recorded.** The hash is the comparison
/// basis a tombstone exists to carry, and a death learned from a path that is
/// already absent has nothing to hash — so overwriting with nothing would destroy
/// the one fact a late event needs and leave a tombstone that can only guess.
#[test]
fn a_re_death_with_no_hash_of_its_own_keeps_the_one_recorded() {
    let scratch = Scratch::new("re-death-hash");
    let mut store = scratch.open();
    let subject = path("glossary.md");

    let mut request = store.begin_request();
    write_document(&mut request, &document(subject.as_str(), "hash-1", "one\n"));
    record_death(&mut request, &subject, Provenance::WatcherRemoval);

    // A second death for the same path, learned when nothing is there to hash:
    // the heal found it absent after the watcher had already reported it gone.
    let again = record_death(&mut request, &subject, Provenance::HealPrune);
    assert_eq!(
        again.documents_deleted, 0,
        "there was a document row to remove"
    );

    let tombstone = request
        .stored_tombstone(&subject)
        .expect("reading a tombstone")
        .expect("a tombstone");
    assert_eq!(
        tombstone.last_content_hash.as_deref(),
        Some("hash-1"),
        "the re-death overwrote the comparison basis the tombstone carried"
    );
    // Everything the second death did know is the most recent answer.
    assert_eq!(tombstone.provenance, Provenance::HealPrune);
    assert_eq!(Some(tombstone.generation), again.generation);

    // And a re-death that does have a hash still replaces it.
    write_document(&mut request, &document(subject.as_str(), "hash-2", "two\n"));
    record_death(&mut request, &subject, Provenance::PlanDelete);
    assert_eq!(
        request
            .stored_tombstone(&subject)
            .expect("reading a tombstone")
            .expect("a tombstone")
            .last_content_hash
            .as_deref(),
        Some("hash-2")
    );
}

/// `byte_length` is the whole document's size, which for a document with a
/// frontmatter block is not its body's. The store takes it rather than deriving
/// one, so a caller that named a frame named the size that goes with it.
#[test]
fn byte_length_is_the_document_and_not_the_body() {
    let scratch = Scratch::new("byte-length");
    let mut store = scratch.open();
    let facts = document_with_every_fact("docs/norn/glossary.md", "hash-1");
    assert!(
        facts.byte_length > facts.body_offset,
        "the fixture stores a document shorter than the frame its body starts at"
    );
    assert_eq!(
        facts.byte_length,
        facts.body_offset + facts.body.len() as u64
    );

    let mut request = store.begin_request();
    write_document(&mut request, &facts);
    let stored = request
        .stored_document(&facts.path)
        .expect("reading a document")
        .expect("a document");
    assert_eq!(stored.byte_length, facts.byte_length);
    assert_eq!(stored.body_offset, facts.body_offset);
}

/// **A document whose frame and body do not account for its size is refused.**
/// The body runs to the end of the document, so the three numbers are checkable
/// against one another — and a body offset past the end of the document would
/// turn every span stored beside it into an offset outside the file. This is the
/// one write path for a document row, so it is where the check lives.
#[test]
fn a_document_whose_numbers_do_not_add_up_is_refused() {
    let scratch = Scratch::new("document-size");
    let mut store = scratch.open();
    let subject = path("docs/norn/glossary.md");

    for (body_offset, byte_length) in [
        // A frame past the end of the document it claims to be inside.
        (1_000, 3),
        // A document larger than the frame and body that account for it.
        (0, 4_096),
        // And smaller.
        (4, 5),
    ] {
        let mut facts = document(subject.as_str(), "hash-1", "a body\n");
        facts.body_offset = body_offset;
        facts.byte_length = byte_length;
        let error = store
            .begin_request()
            .apply_increment(IncrementProvenance::Derived, [Change::Upsert(facts)])
            .expect_err("a document whose numbers do not add up");
        let StoreError::Entry { problem, .. } = &error else {
            panic!("the refusal does not say which entry it came from: {error:?}");
        };
        let StoreError::Bound { what, .. } = &**problem else {
            panic!("body_offset {body_offset} and byte_length {byte_length}: {problem:?}");
        };
        assert!(what.contains("byte length"), "{what}");
    }

    assert_eq!(
        store
            .begin_request()
            .stored_document(&subject)
            .expect("reading a document"),
        None,
        "a refused document reached the table"
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
        write_document(
            &mut request,
            &document(name, &format!("hash-{index}"), "a body\n"),
        );
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
    let ok = DocumentFacts::new(path("docs/note.md"), "hash-1", "a body\n", 7);
    assert_eq!(ok.byte_length, 7);
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
    write_document(&mut request, &facts);
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
    write_document(&mut request, &facts);
    let stored = request
        .stored_facts(&facts.path)
        .expect("reading a document")
        .expect("a document");
    assert_eq!(stored.blocks, facts.blocks);
}

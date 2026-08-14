//! **The mutation pins.** Every field the comparator projects, changed on its
//! own, makes two stores unequal.
//!
//! A comparator is only worth what it would catch, and the failure that costs
//! most is the silent one: a field the projection stopped reading still compares
//! equal, so every store passes and the suites built on it certify nothing. So
//! each case here changes exactly one projected field in one of two otherwise
//! identical stores and asserts that the comparison reports **that** field.
//! Naming the field is the point — a case that only asserted inequality would
//! pass while a different field diverged.
//!
//! The mirror image is here too. The three values the projection deliberately
//! drops — row identifiers, write generations, timestamps — are changed in one
//! store and the two stay equal, because a comparator that reported those would
//! fail every honest pair. The tombstone pillar is pinned the same way round: a
//! death one store recorded leaves the two equal, and each store answers for
//! its own deaths through the operational leg instead.
//!
//! Most mutations go through the store's own writer, which is what a caller can
//! really do. Four fields have no writer that reaches them alone — the full-text
//! index, which is only ever written through `documents.body`; the recorded size
//! and body offset, which the store refuses to accept unless they add up with
//! the body; and the timestamp, which the store reads off the clock itself.
//! Those four reach the database through `induced_failure`, the store's own
//! fenced seam, and each case says why where it does.

use norn_store::{Provenance, Store, induced_failure};
use norn_testkit::equivalence::{
    Divergence, PAGE, Population, StoreProjection, assert_operationally_valid, tombstones,
};

use super::common::{
    Scratch, document, document_with_every_fact, path, record_death, unread_block, violation,
    write_document, write_documents,
};

/// The documents both stores in a pin start from.
///
/// One carries every fact shape the store holds — links, headings, blocks, tags,
/// a frontmatter projection and a diagnostic count — so a pin on any of them has
/// something to change. The second is what keeps a pin about one document from
/// being a pin about the only document.
fn populate(store: &mut Store) {
    let mut request = store.begin_request();
    write_documents(
        &mut request,
        &[
            document_with_every_fact("one/glossary.md", "hash-1"),
            document("two/notes.md", "hash-2", "a second body\n"),
        ],
    );
    request
        .pin_vault_schema(b"version: 1\n", "schema-fingerprint")
        .expect("pinning the vault schema");
}

/// Two stores holding the same facts, and the label the assertions carry.
struct Pair {
    left: Store,
    right: Store,
    // Held for the length of the pair: dropping a scratch removes the directory
    // its store's file sits in.
    _scratch: (Scratch, Scratch),
}

impl Pair {
    fn new(label: &str) -> Self {
        let left_scratch = Scratch::new(&format!("{label}-left"));
        let right_scratch = Scratch::new(&format!("{label}-right"));
        let mut left = left_scratch.open();
        let mut right = right_scratch.open();
        populate(&mut left);
        populate(&mut right);
        let mut pair = Pair {
            left,
            right,
            _scratch: (left_scratch, right_scratch),
        };
        pair.assert_equivalent();
        pair
    }

    fn projections(&mut self) -> (StoreProjection, StoreProjection) {
        (
            StoreProjection::read(&mut self.left).expect("projecting the first store"),
            StoreProjection::read(&mut self.right).expect("projecting the second store"),
        )
    }

    fn assert_equivalent(&mut self) {
        let (left, right) = self.projections();
        left.assert_equivalent(&right, "two stores written the same way");
    }

    /// Change one field in the second store and report the field the comparison
    /// names.
    fn diverged(&mut self, mutate: impl FnOnce(&mut Store)) -> Divergence {
        mutate(&mut self.right);
        let (left, right) = self.projections();
        let comparison = left.compare(&right);
        assert!(
            !comparison.left.is_empty(),
            "a pin over an empty projection would pass however the comparator was neutered"
        );
        comparison.divergence.expect(
            "a projected field changed in one store and the comparison found the two equal, \
             which is what a comparator that stopped projecting that field reports",
        )
    }
}

/// The field a divergence names, asserted against what the case changed.
fn assert_names(divergence: &Divergence, field: &str) {
    assert!(
        divergence.field.starts_with(field),
        "the comparison named `{}` and the case changed `{field}`: {divergence}",
        divergence.field
    );
}

#[test]
fn a_document_only_one_store_holds_is_a_divergence() {
    let mut pair = Pair::new("pin-path");
    let divergence = pair.diverged(|store| {
        let mut request = store.begin_request();
        write_document(&mut request, &document("three/new.md", "hash-3", "third\n"));
    });
    assert_names(&divergence, "document[three/new.md]");
}

#[test]
fn a_changed_content_hash_is_a_divergence() {
    let mut pair = Pair::new("pin-content-hash");
    let divergence = pair.diverged(|store| {
        let mut facts = document_with_every_fact("one/glossary.md", "hash-changed");
        facts.byte_length = facts.body_offset + facts.body.len() as u64;
        let mut request = store.begin_request();
        write_document(&mut request, &facts);
    });
    assert_names(&divergence, "document[one/glossary.md].content_hash");
}

/// **The size pin, and why it is out of band.** A document is its frontmatter
/// block and then its body, so the size, the offset and the body's length are
/// one arithmetic fact and the store refuses a set of them that does not add up.
/// No writer can therefore move the size alone: every write that changes it
/// changes the offset or the body with it, and the divergence would be one of
/// those. Writing the column on its own is what leaves every other field
/// standing.
#[test]
fn a_changed_byte_length_is_a_divergence() {
    let mut pair = Pair::new("pin-byte-length");
    let divergence = pair.diverged(|store| {
        induced_failure::execute_out_of_band(
            store,
            "UPDATE documents SET byte_length = byte_length + 8",
        )
        .expect("moving the recorded document size alone");
    });
    assert_names(&divergence, "document[one/glossary.md].byte_length");
}

/// The body offset is the same arithmetic fact from the other end, and out of
/// band for the same reason.
#[test]
fn a_changed_body_offset_is_a_divergence() {
    let mut pair = Pair::new("pin-body-offset");
    let divergence = pair.diverged(|store| {
        induced_failure::execute_out_of_band(
            store,
            "UPDATE documents SET body_offset = body_offset + 8",
        )
        .expect("moving the recorded body offset alone");
    });
    assert_names(&divergence, "document[one/glossary.md].body_offset");
}

#[test]
fn a_changed_frontmatter_projection_is_a_divergence() {
    let mut pair = Pair::new("pin-frontmatter");
    let divergence = pair.diverged(|store| {
        let mut facts = document_with_every_fact("one/glossary.md", "hash-1");
        facts.frontmatter = Some(norn_store::FrontmatterValue::Map(vec![(
            "title".to_string(),
            norn_store::FrontmatterValue::String("Another".to_string()),
        )]));
        let mut request = store.begin_request();
        write_document(&mut request, &facts);
    });
    assert_names(&divergence, "document[one/glossary.md].frontmatter");
}

#[test]
fn a_changed_frontmatter_diagnostic_count_is_a_divergence() {
    let mut pair = Pair::new("pin-frontmatter-count");
    let divergence = pair.diverged(|store| {
        let mut facts = document_with_every_fact("one/glossary.md", "hash-1");
        facts.frontmatter_diagnostic_count += 1;
        let mut request = store.begin_request();
        write_document(&mut request, &facts);
    });
    assert_names(
        &divergence,
        "document[one/glossary.md].frontmatter_diagnostic_count",
    );
}

#[test]
fn a_changed_body_is_a_divergence() {
    let mut pair = Pair::new("pin-body");
    let divergence = pair.diverged(|store| {
        let mut facts = document_with_every_fact("one/glossary.md", "hash-1");
        // The same number of bytes, so the size and the offset stand and the
        // body is the only field that moved.
        facts.body = facts.body.replace("body", "BODY");
        let mut request = store.begin_request();
        write_document(&mut request, &facts);
    });
    assert_names(&divergence, "document[one/glossary.md].body");
}

#[test]
fn a_changed_link_is_a_divergence() {
    let mut pair = Pair::new("pin-links");
    let divergence = pair.diverged(|store| {
        let mut facts = document_with_every_fact("one/glossary.md", "hash-1");
        facts.links[0].target = "another-target".to_string();
        let mut request = store.begin_request();
        write_document(&mut request, &facts);
    });
    assert_names(&divergence, "document[one/glossary.md].link");
}

#[test]
fn a_changed_heading_is_a_divergence() {
    let mut pair = Pair::new("pin-headings");
    let divergence = pair.diverged(|store| {
        let mut facts = document_with_every_fact("one/glossary.md", "hash-1");
        facts.headings[1].slug = "another-slug".to_string();
        let mut request = store.begin_request();
        write_document(&mut request, &facts);
    });
    assert_names(&divergence, "document[one/glossary.md].heading");
}

#[test]
fn a_changed_block_id_is_a_divergence() {
    let mut pair = Pair::new("pin-blocks");
    let divergence = pair.diverged(|store| {
        let mut facts = document_with_every_fact("one/glossary.md", "hash-1");
        facts.blocks.pop();
        let mut request = store.begin_request();
        write_document(&mut request, &facts);
    });
    assert_names(&divergence, "document[one/glossary.md].block");
}

#[test]
fn a_changed_tag_is_a_divergence() {
    let mut pair = Pair::new("pin-tags");
    let divergence = pair.diverged(|store| {
        let mut facts = document_with_every_fact("one/glossary.md", "hash-1");
        facts.tags[0].name = "area/Other".to_string();
        let mut request = store.begin_request();
        write_document(&mut request, &facts);
    });
    assert_names(&divergence, "document[one/glossary.md].tag");
}

/// **The full-text pin, and why it is out of band.** No writer reaches the index
/// without going through `documents.body`, so a mutation a caller could make
/// changes the column too and the divergence would be the body's. Writing terms
/// into the index alone is what leaves every other projected field standing —
/// and it is exactly the state an index whose triggers were dropped drifts into.
#[test]
fn a_changed_full_text_index_is_a_divergence() {
    let mut pair = Pair::new("pin-full-text");
    let divergence = pair.diverged(|store| {
        induced_failure::execute_out_of_band(
            store,
            "INSERT INTO documents_fts(rowid, body) VALUES (1, 'interloper')",
        )
        .expect("writing a term into the index alone");
    });
    assert_names(&divergence, "indexed term[interloper]");
}

/// **The schema's bytes, pinned apart from the fingerprint over them.** A store
/// that re-pinned a changed schema under the fingerprint it already recorded
/// holds bytes no other store holds, and the fingerprint says nothing about it.
/// A projection that summarised the bytes — by their length, or by the recorded
/// fingerprint standing in for them — would compare the two equal.
#[test]
fn changed_vault_schema_bytes_are_a_divergence() {
    let mut pair = Pair::new("pin-vault-schema-bytes");
    let divergence = pair.diverged(|store| {
        store
            .begin_request()
            .pin_vault_schema(b"version: 2\n", "schema-fingerprint")
            .expect("pinning other bytes under the fingerprint already recorded");
    });
    assert_names(&divergence, "vault schema.bytes");
}

/// **The fingerprint, pinned apart from the bytes it is over.** It is the key
/// every finding is keyed by, so two stores that hold one schema under two
/// fingerprints disagree about which schema their findings were derived under.
#[test]
fn a_changed_vault_schema_fingerprint_is_a_divergence() {
    let mut pair = Pair::new("pin-vault-schema-fingerprint");
    let divergence = pair.diverged(|store| {
        store
            .begin_request()
            .pin_vault_schema(b"version: 1\n", "another-fingerprint")
            .expect("pinning the same bytes under another fingerprint");
    });
    assert_names(&divergence, "vault schema.fingerprint");
}

#[test]
fn a_finding_only_one_store_holds_is_a_divergence() {
    let mut pair = Pair::new("pin-findings");
    let divergence = pair.diverged(|store| {
        store
            .begin_request()
            .record_finding(&unread_block("one/glossary.md"))
            .expect("recording a finding");
    });
    assert_names(&divergence, "finding[one/glossary.md][0]");
}

/// **The pin the keyed readers cannot carry.** A finding about a path no
/// document row stands at is reachable by no read that starts from a document,
/// so a projection assembled from the rows it holds would miss it entirely and
/// the two stores would compare equal.
#[test]
fn a_finding_at_a_path_without_a_document_row_is_a_divergence() {
    let mut pair = Pair::new("pin-findings-without-rows");
    let divergence = pair.diverged(|store| {
        store
            .begin_request()
            .record_finding(&violation("nowhere/absent.md"))
            .expect("recording a finding about a path nothing derived");
    });
    assert_names(&divergence, "finding[nowhere/absent.md][0]");
}

/// **The exclusions, pinned the other way round.** A row identifier, a write
/// generation and a timestamp differ between two honest derivations of one
/// vault, so a comparator that read any of them would fail every pair it was
/// ever pointed at.
///
/// Two of the three are arranged the way a vault really produces them. The
/// second store writes the same two documents in the opposite order, so the
/// store assigns them the opposite row identifiers; and it re-derives one of
/// them afterwards, which takes a further write generation without changing a
/// single fact. The third has no such arrangement — the store reads the clock
/// itself — so the timestamps are moved through the fenced seam.
#[test]
fn a_row_identifier_a_generation_and_a_timestamp_leave_two_stores_equal() {
    let left_scratch = Scratch::new("pin-exclusions-left");
    let right_scratch = Scratch::new("pin-exclusions-right");
    let mut left = left_scratch.open();
    let mut right = right_scratch.open();

    let one = document_with_every_fact("one/glossary.md", "hash-1");
    let two = document("two/notes.md", "hash-2", "a second body\n");
    write_documents(&mut left.begin_request(), &[one.clone(), two.clone()]);
    write_documents(&mut right.begin_request(), &[two, one.clone()]);
    for store in [&mut left, &mut right] {
        store
            .begin_request()
            .pin_vault_schema(b"version: 1\n", "schema-fingerprint")
            .expect("pinning the vault schema");
    }
    // A re-derivation of facts that did not change still takes a generation, so
    // from here the two stores describe one vault at two different generations.
    write_document(&mut right.begin_request(), &one);
    induced_failure::execute_out_of_band(
        &mut right,
        "UPDATE documents SET derived_at = derived_at + 86400",
    )
    .expect("moving the timestamps a person reads");

    let left = StoreProjection::read(&mut left).expect("projecting the first store");
    let right = StoreProjection::read(&mut right).expect("projecting the second store");
    left.assert_equivalent(
        &right,
        "one vault derived twice, at two sets of identifiers, generations and instants",
    );
}

/// **The suffix key is checked against the path that produced it.** The column
/// is written once, by the path type, and read back only as the range the
/// resolution ladder seeks: a key that drifted from its path answers probes it
/// does not belong to, and every read that would notice goes through the same
/// drifted range. Nothing but a recompute at rest catches it, so the leg
/// recomputes — and this is the drift, moved through the fenced seam because no
/// writer reaches the column without the path beside it.
#[test]
#[should_panic(expected = "holds a suffix key its own path does not produce")]
fn a_suffix_key_that_drifted_from_its_path_fails_the_operational_leg() {
    let scratch = Scratch::new("operational-leg-suffix-key");
    let mut store = scratch.open();
    populate(&mut store);
    induced_failure::execute_out_of_band(&mut store, "UPDATE documents SET suffix_key = 'drift/'")
        .expect("moving the stored suffix key alone");
    assert_operationally_valid(&mut store, "a store whose suffix key drifted from its path");
}

/// The path of the `ordinal`-th document of a numbered vault, counting from one.
///
/// The zero padding is what makes the numbering the byte order: `doc-0002.md`
/// sorts ahead of `doc-0010.md`, so a case can name the row a page ends on.
fn numbered(ordinal: usize) -> String {
    format!("doc-{ordinal:04}.md")
}

/// A vault of `count` documents at those paths.
fn many_documents(store: &mut Store, count: usize) {
    let facts: Vec<_> = (1..=count)
        .map(|ordinal| document(&numbered(ordinal), "hash-1", "a body\n"))
        .collect();
    write_documents(&mut store.begin_request(), &facts);
}

/// Move one row's stored suffix key off the path that produced it.
///
/// `documents.path` is unique, so the predicate names one row. A predicate that
/// named none would leave the store sound and the case around it would fail for
/// finding no panic, which is what keeps these cases from passing vacuously.
fn drift_one_key(store: &mut Store, at: &str) {
    induced_failure::execute_out_of_band(
        store,
        &format!("UPDATE documents SET suffix_key = 'drift/' WHERE path = '{at}'"),
    )
    .expect("moving one stored suffix key");
}

/// **The recompute reaches the row a page ends on.** The keys are drained a
/// bounded page at a time, and a drain that advanced its cursor over the row it
/// paged last without checking it would pass a store whose only drifted key sits
/// exactly there. The vault holds more rows than two pages carry and one row
/// drifts, through the fenced seam for the reason the pin above states.
#[test]
#[should_panic(expected = "holds a suffix key its own path does not produce")]
fn a_suffix_key_that_drifted_where_a_page_ends_fails_the_operational_leg() {
    let scratch = Scratch::new("operational-leg-suffix-key-page-end");
    let mut store = scratch.open();
    many_documents(&mut store, PAGE * 2 + 1);
    drift_one_key(&mut store, &numbered(PAGE));
    assert_operationally_valid(&mut store, "a store whose key drifted where a page ends");
}

/// **The recompute reaches the row the drain ends on.** A drain that stopped at
/// the last full page, or that read a short final page and dropped it, would
/// pass a store whose only drifted key is on that page.
#[test]
#[should_panic(expected = "holds a suffix key its own path does not produce")]
fn a_suffix_key_that_drifted_on_the_last_page_fails_the_operational_leg() {
    let scratch = Scratch::new("operational-leg-suffix-key-last-page");
    let mut store = scratch.open();
    let rows = PAGE * 2 + 1;
    many_documents(&mut store, rows);
    drift_one_key(&mut store, &numbered(rows));
    assert_operationally_valid(&mut store, "a store whose key drifted on the last page");
}

/// **The tombstone exclusion, pinned the way a vault produces it.** One store
/// derived a document and then watched it leave; the other never saw it. The
/// tree they describe now is the same tree, so they are equivalent — a
/// comparator that projected deaths would fail the healed-against-rebuilt pair
/// it exists to judge — and each still answers for its own deaths through the
/// operational leg, which is the only guarantee the pillar carries.
#[test]
fn a_death_one_store_recorded_leaves_the_two_equal() {
    let mut pair = Pair::new("pin-tombstone-exclusion");
    {
        let mut request = pair.right.begin_request();
        write_document(
            &mut request,
            &document("three/gone.md", "hash-3", "a departed body\n"),
        );
        record_death(&mut request, &path("three/gone.md"), Provenance::HealPrune);
    }
    // The pillar really holds a death on one side and none on the other, so the
    // equality below is the exclusion holding rather than nothing having
    // happened.
    assert_eq!(
        tombstones(&mut pair.left)
            .expect("draining the first store's tombstones")
            .len(),
        0
    );
    assert_eq!(
        tombstones(&mut pair.right)
            .expect("draining the second store's tombstones")
            .len(),
        1
    );
    pair.assert_equivalent();
    assert_operationally_valid(&mut pair.left, "the store that never saw the document");
    assert_operationally_valid(&mut pair.right, "the store that watched it leave");
}

/// **The operational leg, and what it is separate from.** A store answers for
/// its own soundness whatever another store says, and the leg reads the pillars
/// no equivalence claim covers: the recorded store schema against the schema
/// actually held, the deaths against the vocabulary and the generations that
/// order them, and the migration ledger the pre-release build keeps empty.
#[test]
fn a_store_written_through_its_own_api_is_operationally_valid() {
    let scratch = Scratch::new("operational-leg");
    let mut store = scratch.open();
    populate(&mut store);
    record_death(
        &mut store.begin_request(),
        &path("two/notes.md"),
        Provenance::HealPrune,
    );
    assert_operationally_valid(&mut store, "a store written through its own API");

    let projection = StoreProjection::read(&mut store).expect("projecting the store");
    projection.assert_holds(
        "a store written through its own API",
        &[(
            "one/glossary.md",
            "the body of the document\n\nwith two paragraphs\n",
        )],
    );
    projection.assert_population_at_least(
        "a store written through its own API",
        Population {
            documents: 1,
            facts: 8,
            findings: 0,
            indexed_terms: 1,
            vault_schema_pinned: true,
        },
    );
}

//! Broken documents keep their names and disclose no facts from unread bytes.

use std::sync::Arc;

use norn_store::{
    Change, ContentModel, DerivedFinding, IncrementProvenance, Provenance, Request, induced_failure,
};
use norn_wire::FindingKind;

use crate::common::{
    Scratch, document, document_with_every_fact, full_text_matches, path, unread_block,
    write_document,
};
use crate::health::derived;

fn unreadable(request: &mut Request<'_>, at: &str) -> norn_store::IncrementOutcome {
    let mut finding = unread_block(at);
    finding.kind = FindingKind::UnreadableDocument;
    finding.message = format!("`{at}` could not be read");
    finding.detail = Some("Permission denied (os error 13)".to_string());
    request
        .apply_increment(
            IncrementProvenance::Derived,
            [Change::Unreadable { path: path(at) }],
            &[DerivedFinding {
                facts: finding,
                replaces: None,
                bytes_hash: None,
            }],
            &ContentModel::none(),
        )
        .expect("recording the broken document")
}

fn assert_names_only(request: &mut Request<'_>, at: &str) {
    let facts = request
        .stored_facts(&path(at))
        .expect("reading facts")
        .expect("the name stands");
    assert_eq!(facts.document.content_hash, None);
    assert_eq!(facts.document.byte_length, None);
    assert_eq!(facts.document.body_offset, None);
    assert_eq!(facts.document.frontmatter, None);
    assert_eq!(facts.document.frontmatter_diagnostic_count, 0);
    assert_eq!(facts.body, None);
    assert!(
        facts.links.is_empty()
            && facts.link_keys.is_empty()
            && facts.headings.is_empty()
            && facts.blocks.is_empty()
            && facts.tags.is_empty()
            && facts.fields.is_empty()
    );
    let findings = request.stored_findings(&path(at)).expect("findings");
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].kind, "document/unreadable");
    assert_eq!(findings[0].severity, "error");
    assert_eq!(
        findings[0].detail.as_deref(),
        Some("Permission denied (os error 13)")
    );
    assert!(
        request
            .stored_tombstone(&path(at))
            .expect("tombstone lookup")
            .is_none()
    );
}

#[test]
fn an_unreadable_change_clears_a_row_to_names_only() {
    let scratch = Scratch::new("broken-cleared");
    let mut store = scratch.open();
    let before = write_document(
        &mut store.begin_request(),
        &document_with_every_fact("a.md", "old"),
    )
    .generation
    .unwrap();
    store
        .begin_request()
        .record_finding(&unread_block("a.md"))
        .expect("old finding");
    induced_failure::execute_out_of_band(
        &mut store,
        "CREATE TEMP TABLE original_identity AS SELECT id FROM documents WHERE path = 'a.md'",
    )
    .expect("remembering identity");
    let outcome = unreadable(&mut store.begin_request(), "a.md");
    assert!(outcome.generation.unwrap() > before);
    assert_eq!(outcome.documents_upserted, 1);
    assert_eq!(outcome.documents_deleted, 0);
    assert_eq!(outcome.tombstones_recorded, 0);
    assert_names_only(&mut store.begin_request(), "a.md");
    induced_failure::execute_out_of_band(&mut store, "CREATE TEMP TABLE identity_check (same INTEGER CHECK(same = 1)); INSERT INTO identity_check SELECT id = (SELECT id FROM original_identity) FROM documents WHERE path = 'a.md'").expect("the row keeps its identity");
}

#[test]
fn an_unreadable_change_with_no_row_writes_names_only() {
    let scratch = Scratch::new("broken-birth");
    let mut store = scratch.open();
    write_document(&mut store.begin_request(), &derived("ref.md", "[[a]]\n"));
    assert_eq!(
        store
            .begin_request()
            .stored_findings(&path("ref.md"))
            .unwrap()
            .len(),
        1
    );
    unreadable(&mut store.begin_request(), "a.md");
    assert_names_only(&mut store.begin_request(), "a.md");
    assert!(
        store
            .begin_request()
            .stored_findings(&path("ref.md"))
            .unwrap()
            .is_empty()
    );
    let request = store.begin_request();
    let class = request
        .target_class("a", &norn_store::AmbiguityIgnore::none())
        .unwrap();
    assert_eq!(request.suffix_candidates(&class).unwrap(), [path("a.md")]);
}

#[test]
fn a_hashless_row_has_no_live_holder() {
    let scratch = Scratch::new("broken-live-holder");
    let mut store = scratch.open();
    write_document(
        &mut store.begin_request(),
        &document("a.md", "old", "text\n"),
    );
    unreadable(&mut store.begin_request(), "a.md");
    let reader = Arc::new(store.open_reader().reader.unwrap());
    let snapshot = reader.try_take().unwrap().establish().unwrap();
    assert!(snapshot.live_holders("old").unwrap().is_empty());
}

#[test]
fn deleting_a_broken_document_records_no_hash() {
    let scratch = Scratch::new("broken-death");
    let mut store = scratch.open();
    unreadable(&mut store.begin_request(), "a.md");
    let outcome = store
        .begin_request()
        .apply_increment(
            IncrementProvenance::Derived,
            [Change::Death {
                path: path("a.md"),
                provenance: Provenance::WatcherRemoval,
            }],
            &[],
            &ContentModel::none(),
        )
        .unwrap();
    assert_eq!(outcome.documents_deleted, 1);
    let tombstone = store
        .begin_request()
        .stored_tombstone(&path("a.md"))
        .unwrap()
        .unwrap();
    assert_eq!(tombstone.last_content_hash, None);
}

#[test]
fn an_anchor_on_a_broken_document_is_not_judged() {
    let scratch = Scratch::new("broken-anchor");
    let mut store = scratch.open();
    write_document(
        &mut store.begin_request(),
        &derived("a.md", "# Heading\n\nblock ^b\n"),
    );
    write_document(
        &mut store.begin_request(),
        &derived(
            "ref.md",
            "[[a#missing]] [[a#^missing]] [[a#Heading]] [[a#^b]]\n",
        ),
    );
    assert_eq!(
        store
            .begin_request()
            .stored_findings(&path("ref.md"))
            .unwrap()
            .len(),
        2
    );
    unreadable(&mut store.begin_request(), "a.md");
    assert!(
        store
            .begin_request()
            .stored_findings(&path("ref.md"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn an_anchor_on_an_empty_readable_document_is_missing() {
    let scratch = Scratch::new("empty-anchor");
    let mut store = scratch.open();
    write_document(&mut store.begin_request(), &derived("a.md", ""));
    write_document(
        &mut store.begin_request(),
        &derived("ref.md", "[[a#Heading]] [[a#^b]]\n"),
    );
    let findings = store
        .begin_request()
        .stored_findings(&path("ref.md"))
        .unwrap();
    assert_eq!(findings.len(), 2);
    assert!(
        findings
            .iter()
            .all(|finding| finding.kind == "link/missing-anchor")
    );
}

#[test]
fn a_broken_documents_outgoing_links_and_their_findings_end() {
    let scratch = Scratch::new("broken-outgoing");
    let mut store = scratch.open();
    write_document(&mut store.begin_request(), &derived("a.md", "[[absent]]\n"));
    assert_eq!(
        store
            .begin_request()
            .stored_findings(&path("a.md"))
            .unwrap()
            .len(),
        1
    );
    unreadable(&mut store.begin_request(), "a.md");
    assert_names_only(&mut store.begin_request(), "a.md");
}

#[test]
fn the_feed_carries_no_fingerprint_for_a_names_only_row() {
    let scratch = Scratch::new("broken-feed");
    let mut store = scratch.open();
    unreadable(&mut store.begin_request(), "a.md");
    let feed = store.feed_read().changed_documents_after(None, 10).unwrap();
    assert_eq!(feed.len(), 1);
    assert_eq!(feed[0].1.content_hash, None);
    assert_eq!(feed[0].1.body_hash, None);
    assert_eq!(feed[0].1.frontmatter_projection_hash, None);
}

#[test]
fn the_integrity_audit_passes_a_names_only_row() {
    let scratch = Scratch::new("broken-integrity");
    let mut store = scratch.open();
    unreadable(&mut store.begin_request(), "a.md");
    store.verify_integrity().expect("a names-only row is sound");
}

#[test]
fn a_body_cleared_to_null_leaves_fts_consistent() {
    let scratch = Scratch::new("broken-fts");
    let mut store = scratch.open();
    write_document(
        &mut store.begin_request(),
        &document("a.md", "old", "searchable words\n"),
    );
    assert_eq!(full_text_matches(&store, "searchable"), ["a.md"]);
    unreadable(&mut store.begin_request(), "a.md");
    assert!(full_text_matches(&store, "searchable").is_empty());
    induced_failure::execute_out_of_band(
        &mut store,
        "INSERT INTO documents_fts(documents_fts, rank) VALUES ('integrity-check', 1)",
    )
    .expect("FTS agrees with the NULL body");
}

#[test]
fn a_names_only_row_that_reads_is_searchable() {
    let scratch = Scratch::new("broken-fts-recovery");
    let mut store = scratch.open();
    unreadable(&mut store.begin_request(), "a.md");
    write_document(
        &mut store.begin_request(),
        &document("a.md", "new", "searchable words\n"),
    );
    assert_eq!(full_text_matches(&store, "searchable"), ["a.md"]);
    store.verify_integrity().expect("recovery agrees with FTS");
}

#[test]
fn the_ddl_refuses_a_partial_body() {
    let scratch = Scratch::new("broken-body-checks");
    let mut store = scratch.open();
    write_document(
        &mut store.begin_request(),
        &document("a.md", "old", "body\n"),
    );
    for column in ["body", "body_hash", "byte_length", "body_offset"] {
        let result = induced_failure::execute_out_of_band(
            &mut store,
            &format!("UPDATE documents SET {column} = NULL WHERE path = 'a.md'"),
        );
        assert!(
            result.is_err(),
            "a partial body through {column} was admitted"
        );
    }
    assert!(induced_failure::execute_out_of_band(&mut store, "UPDATE documents SET body = NULL, body_hash = NULL, byte_length = NULL, body_offset = NULL WHERE path = 'a.md'").is_err(), "a content hash cannot stand without its body");
}

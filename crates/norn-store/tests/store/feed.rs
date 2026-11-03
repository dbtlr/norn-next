//! The feed source: the lane-2 read surface over a connection of its own,
//! minted from a live store without borrowing it.

use crate::common::{Scratch, document, path, write_document};

/// **A feed source reads the feed through its own connection, and its page
/// is one snapshot.** Minting it borrows nothing, so the writer goes on
/// writing; a page sees the store as it stood at the page's first read, and
/// the next page sees what was committed since.
#[test]
fn a_feed_source_reads_through_its_own_connection_one_snapshot_per_page() {
    let scratch = Scratch::new("feed-source");
    let mut store = scratch.open();
    write_document(&mut store.begin_request(), &document("a.md", "ha", "alpha"));
    let mut source = store.feed_source();
    assert_eq!(source.epoch(), store.epoch());
    write_document(&mut store.begin_request(), &document("b.md", "hb", "beta"));

    {
        let mut page = source.page().expect("a feed page");
        let generation = page.write_generation().expect("the generation");
        assert_eq!(page.changed_documents_after(None, 10).unwrap().len(), 2);
        // Committed after the page's snapshot was taken.
        write_document(&mut store.begin_request(), &document("c.md", "hc", "gamma"));
        assert_eq!(
            page.changed_documents_after(None, 10).unwrap().len(),
            2,
            "a page read past its own snapshot"
        );
        assert_eq!(page.write_generation().unwrap(), generation);
        let facts = page
            .stored_facts(&path("b.md"))
            .unwrap()
            .expect("b.md stands");
        assert_eq!(facts.body.as_deref(), Some("beta"));
        assert!(page.stored_document(&path("c.md")).unwrap().is_none());
    }

    let mut page = source.page().expect("the next page");
    assert_eq!(page.changed_documents_after(None, 10).unwrap().len(), 3);
    assert_eq!(
        page.write_generation().unwrap(),
        store.begin_request().write_generation().unwrap()
    );
}

/// **A feed source is bound to the store lifetime it was minted from.** A
/// store discarded and rebuilt at the same path is another lifetime, and an
/// establishment against it is refused rather than read: as a refusal of the
/// environment, never as damage.
#[test]
fn a_feed_source_refuses_a_store_of_another_lifetime() {
    let scratch = Scratch::new("feed-source-lifetime");
    let store = scratch.open();
    let mut source = store.feed_source();
    let order = store.path_order();
    let rebuilt = store.discard_and_reopen(order).expect("the rebuild");
    assert_ne!(rebuilt.epoch(), source.epoch());

    let Err(refused) = source.page() else {
        panic!("a page over another lifetime was read");
    };
    assert_eq!(refused.damage(), None, "{refused}");
    drop(rebuilt);
}

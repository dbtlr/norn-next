//! Every ordinary changeset is visible after its own commit in either build.
use norn_store::{
    Change, ContentModel, DerivationVersion, DocumentFacts, DocumentPath, IncrementProvenance,
    Store, StoredPathOrder,
};

#[test]
fn ordinary_changesets_commit_individually_to_independent_readers() {
    let scratch = norn_testkit::scratch::Scratch::new("ordinary-store-commits");
    let path = scratch.join("store.sqlite3");
    let mut store =
        Store::open(&path, StoredPathOrder::Sensitive, DerivationVersion::new(1)).unwrap();
    for id in 1..=2 {
        let document = DocumentFacts::new(
            DocumentPath::new(&format!("note-{id}.md")).unwrap(),
            "hash",
            "body",
            4,
        );
        store
            .begin_request()
            .apply_increment(
                IncrementProvenance::Derived,
                [Change::Upsert(document)],
                &[],
                &ContentModel::none(),
            )
            .unwrap();
        let reader = norn_db::open_read_only(&path).adopted.unwrap();
        let count: i64 = reader
            .connection()
            .query_row("SELECT count(*) FROM documents", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            count, id,
            "ordinary changeset has not committed independently"
        );
    }
}

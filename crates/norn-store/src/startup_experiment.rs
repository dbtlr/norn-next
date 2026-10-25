//! Harness-only import alternatives. Production loading shares its catalogue and builders.
use crate::StoreError;
pub use crate::increment::WriteMode;
pub use crate::loading::{Catalogue, CatalogueCandidates, CataloguePrefix, IntakeBuilder};
pub use norn_db::RetirementCheck;
use std::path::Path;

/// A first-import cache budget, capped at the experiment's 8 MiB starting point.
#[derive(Clone, Copy, Debug)]
pub struct WriterCacheSize(pub(crate) u32);

impl WriterCacheSize {
    pub fn new(kib: u32) -> Result<Self, StoreError> {
        if kib == 0 || kib > 8192 {
            return Err(StoreError::Bound {
                what: "initial-import writer cache KiB (minimum 1)",
                limit: 8192,
                given: kib as usize,
            });
        }
        Ok(Self(kib))
    }
}

/// Saved state must be restored on the same live writer that opened the window.
/// The owner must restore before readiness or close/replace that writer on failure.
#[must_use = "restore on the same writer before readiness, or close/replace the writer"]
pub struct WriterCacheWindow {
    pub(crate) before: i64,
    pub(crate) during: Option<i64>,
    pub(crate) retained_cache_budget_kib: Option<u64>,
}

/// Extra live reads are diagnostic-only. Shrinking the default SQLite cache can
/// evict pages, but the allocator may retain RSS. Restoration gives no process-RSS
/// guarantee; attach high-water and retained warm RSS still require measurement.
#[derive(Clone, Copy, Debug)]
pub struct WriterCacheReceipt {
    pub before: i64,
    pub during: Option<i64>,
    pub after: Option<i64>,
    /// Conservatively charge the larger configured budget until writer closure.
    /// This is a page-cache budget, not a measurement or bound of process RSS.
    pub retained_cache_budget_kib: Option<u64>,
    pub reclamation_verified: bool,
}

/// Fresh helper state has neither a database nor any journal sidecar.
/// Inspect under maintainership before the ordinary open can rebuild old state.
pub fn fresh_namespace(path: &Path) -> Result<bool, StoreError> {
    norn_db::database_is_absent(path).map_err(StoreError::from)
}

#[cfg(test)]
mod intake_tests {
    use crate::{
        Change, ContentModel, DerivationVersion, DocumentFacts, DocumentPath, Store,
        StoredPathOrder,
    };

    #[test]
    fn metadata_refuses_a_lasting_store_without_changing_it() {
        let scratch = norn_testkit::scratch::Scratch::new("intake-lasting-refusal");
        let path = scratch.join("lasting.sqlite3");
        let mut store =
            Store::open(&path, StoredPathOrder::Sensitive, DerivationVersion::new(1)).unwrap();
        let epoch = store.epoch().to_string();
        let facts = DocumentFacts::new(DocumentPath::new("first.md").unwrap(), "hash", "body", 4);
        assert!(
            store
                .begin_request()
                .apply_intake_increment([Change::Upsert(facts)], &[], &ContentModel::none())
                .is_err()
        );
        store.close().unwrap();
        let mut reopened =
            Store::open(&path, StoredPathOrder::Sensitive, DerivationVersion::new(1)).unwrap();
        assert_eq!(reopened.epoch(), epoch);
        assert!(
            reopened
                .begin_request()
                .stored_facts(&DocumentPath::new("first.md").unwrap())
                .unwrap()
                .is_none()
        );
        reopened.verify_integrity().unwrap();
    }

    #[test]
    fn metadata_omits_payload_after_validating_real_source_lengths() {
        let scratch = norn_testkit::scratch::Scratch::new("intake-storage");
        let mut builder = crate::loading::IntakeBuilder::create(
            &scratch.join("metadata.sqlite3"),
            StoredPathOrder::Sensitive,
            DerivationVersion::new(1),
        )
        .unwrap();
        builder
            .write(|store| {
                let path = DocumentPath::new("document.md").unwrap();
                let model = ContentModel::none();
                let facts =
                    DocumentFacts::new(path.clone(), "real-source-hash", "uniquebodytoken", 99);
                assert!(
                    store
                        .begin_request()
                        .apply_intake_increment([Change::Upsert(facts.clone())], &[], &model)
                        .is_err(),
                    "an inconsistent real length must still fail"
                );
                let mut facts = facts;
                facts.byte_length = 15;
                store
                    .begin_request()
                    .apply_intake_increment([Change::Upsert(facts)], &[], &model)
                    .unwrap();
                let stored = store.begin_request().stored_facts(&path).unwrap().unwrap();
                assert_eq!(stored.document.content_hash, "real-source-hash");
                assert_eq!(stored.document.byte_length, 15);
                assert!(stored.body.is_empty());
                let matches: u64 = store
            .database
            .connection()
            .query_row(
                "SELECT count(*) FROM documents_fts WHERE documents_fts MATCH 'uniquebodytoken'",
                [],
                |row| row.get(0),
            )
            .unwrap();
                assert_eq!(matches, 0);
                Ok::<_, crate::StoreError>(())
            })
            .unwrap();
    }
}

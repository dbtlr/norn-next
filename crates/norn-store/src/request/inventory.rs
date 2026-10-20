//! Disk-backed ordering for opaque observations of one rolling walk.

use super::*;

/// One path-order key and the opaque name observation stored under it.
pub type InventoryEntry = (Vec<u8>, Vec<u8>);

/// Opaque ordered observations, held in SQLite's disk-backed temporary
/// database rather than the lane-1 schema. They do not change its generation
/// or fingerprint and disappear when the writer connection closes.
impl Request<'_> {
    /// A full recovery supersedes an incomplete background inventory.
    pub fn reset_inventory(&mut self) -> Result<(), StoreError> {
        self.store.connection().execute_batch("DROP TABLE IF EXISTS temp.rolling_inventory; DROP TABLE IF EXISTS temp.rolling_account;")
            .map_err(|error| error::sql("discarding a superseded walk inventory", error))?;
        Ok(())
    }

    pub fn prepare_inventory(&mut self) -> Result<(), StoreError> {
        let connection = self.store.connection();
        connection.execute_batch("PRAGMA temp_store = FILE;
            PRAGMA temp.cache_size = -256;
            CREATE TEMP TABLE IF NOT EXISTS rolling_inventory (
                key BLOB PRIMARY KEY NOT NULL, value BLOB NOT NULL
            ) WITHOUT ROWID;
            CREATE TEMP TABLE IF NOT EXISTS rolling_account (path TEXT PRIMARY KEY NOT NULL, flags INTEGER NOT NULL) WITHOUT ROWID;")
            .map_err(|error| error::sql("preparing the rolling inventory", error))?;
        Ok(())
    }

    pub fn inventory_record(&mut self, key: &[u8], value: &[u8]) -> Result<(), StoreError> {
        self.store
            .connection()
            .execute(
                "INSERT OR REPLACE INTO temp.rolling_inventory (key, value) VALUES (?1, ?2)",
                params![key, value],
            )
            .map_err(|error| error::sql("ordering a walk observation", error))?;
        Ok(())
    }

    pub fn inventory_after(
        &self,
        after: Option<&[u8]>,
        limit: usize,
    ) -> Result<Vec<InventoryEntry>, StoreError> {
        if limit == 0 || limit > MAX_PAGE {
            return Err(StoreError::Bound {
                what: "an inventory page",
                limit: MAX_PAGE,
                given: limit,
            });
        }
        self.read_all(
            "SELECT key, value FROM temp.rolling_inventory
            WHERE key > COALESCE(?1, x'') ORDER BY key LIMIT ?2",
            params![after, limit as i64],
            |row| Ok(Ok((row.get(0)?, row.get(1)?))),
            "reading a walk inventory page",
        )
    }

    /// Accumulate a pass-local account of a subject or a withheld root.
    pub fn inventory_account(&mut self, path: &str, flags: u32) -> Result<(), StoreError> {
        self.store
            .connection()
            .execute(
                "INSERT INTO temp.rolling_account(path, flags) VALUES (?1, ?2)
            ON CONFLICT(path) DO UPDATE SET flags = flags | excluded.flags",
                params![path, flags],
            )
            .map_err(|error| error::sql("recording a rolling walk account", error))?;
        Ok(())
    }

    pub fn inventory_flags(&self, path: &str) -> Result<u32, StoreError> {
        self.store
            .connection()
            .query_row(
                "SELECT flags FROM temp.rolling_account WHERE path = ?1",
                params![path],
                |row| row.get(0),
            )
            .optional()
            .map(|flags| flags.unwrap_or(0))
            .map_err(|error| error::sql("reading a rolling walk account", error))
    }

    pub fn inventory_clear_accounts(&mut self, limit: usize) -> Result<usize, StoreError> {
        if limit == 0 || limit > MAX_PAGE {
            return Err(StoreError::Bound {
                what: "a rolling account cleanup",
                limit: MAX_PAGE,
                given: limit,
            });
        }
        self.store
            .connection()
            .execute(
                "DELETE FROM temp.rolling_account WHERE path IN
            (SELECT path FROM temp.rolling_account ORDER BY path LIMIT ?1)",
                params![limit as i64],
            )
            .map_err(|error| error::sql("retiring rolling walk accounts", error))
    }

    /// Drive cleanup by every subject, including subjects still holding rows,
    /// so a sparse match cannot turn one background page into a vault scan.
    pub fn finding_subjects_after(
        &self,
        after: Option<&DocumentPath>,
        limit: usize,
    ) -> Result<Vec<DocumentPath>, StoreError> {
        if limit == 0 || limit > MAX_PAGE {
            return Err(StoreError::Bound {
                what: "a finding subject page",
                limit: MAX_PAGE,
                given: limit,
            });
        }
        self.read_all(
            "SELECT DISTINCT path FROM findings WHERE path > COALESCE(?1, '')
            ORDER BY path LIMIT ?2",
            params![after.map(DocumentPath::as_str), limit as i64],
            |row| {
                let path: String = row.get(0)?;
                Ok(DocumentPath::new(&path))
            },
            "reading all finding subjects",
        )
    }

    /// The caller bounds this deletion by the records it just consumed.
    pub fn inventory_discard_through(&mut self, through: &[u8]) -> Result<(), StoreError> {
        self.store
            .connection()
            .execute(
                "DELETE FROM temp.rolling_inventory WHERE key <= ?1",
                params![through],
            )
            .map_err(|error| error::sql("retiring walk observations", error))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::{DerivationVersion, Store, StoredPathOrder};

    #[test]
    fn window_pages_seek_both_bounds_instead_of_filtering_unrelated_rows() {
        use crate::{
            Change, ContentModel, DocumentFacts, DocumentPath, IncrementProvenance, SubjectScope,
        };
        let scratch = norn_testkit::scratch::Scratch::new("window-cost");
        let mut store = Store::open_throwaway(
            scratch.join("store.sqlite3"),
            StoredPathOrder::Sensitive,
            DerivationVersion::new(1),
        )
        .unwrap();
        let facts = (0..1000).map(|index| {
            Change::Upsert(DocumentFacts::new(
                DocumentPath::new(&format!("z{index:05}.md")).unwrap(),
                "hash",
                "body",
                4,
            ))
        });
        store
            .begin_request()
            .apply_increment(
                IncrementProvenance::Derived,
                facts,
                &[],
                &ContentModel::none(),
            )
            .unwrap();
        for order in [
            StoredPathOrder::Sensitive,
            StoredPathOrder::AsciiCaseInsensitive,
        ] {
            for (after, through, cursor) in [
                (b"".as_slice(), b"a".as_slice(), None),
                (
                    b"a".as_slice(),
                    b"z00998.md".as_slice(),
                    Some(DocumentPath::new("z00995.md").unwrap()),
                ),
            ] {
                let request = store.begin_request();
                request
                    .stored_documents_in_range_after_ordered(
                        SubjectScope::Window {
                            after,
                            through: Some(through),
                        },
                        cursor.as_ref(),
                        3,
                        order,
                    )
                    .unwrap();
                assert!(
                    request.read_steps() < 500,
                    "{order:?}: {} steps",
                    request.read_steps()
                );
            }
        }
    }

    #[test]
    fn path_windows_seek_past_the_lower_bound_and_stop_at_the_upper_bound() {
        use crate::{
            Change, ContentModel, DocumentFacts, DocumentPath, IncrementProvenance, SubjectScope,
        };
        let scratch = norn_testkit::scratch::Scratch::new("window-pages");
        let mut store = Store::open_throwaway(
            scratch.join("store.sqlite3"),
            StoredPathOrder::Sensitive,
            DerivationVersion::new(1),
        )
        .unwrap();
        let facts = ["a.md", "b.md", "c.md", "d.md"].map(|path| {
            Change::Upsert(DocumentFacts::new(
                DocumentPath::new(path).unwrap(),
                "hash",
                "body",
                4,
            ))
        });
        store
            .begin_request()
            .apply_increment(
                IncrementProvenance::Derived,
                facts,
                &[],
                &ContentModel::none(),
            )
            .unwrap();
        for order in [
            StoredPathOrder::Sensitive,
            StoredPathOrder::AsciiCaseInsensitive,
        ] {
            let page = store
                .begin_request()
                .stored_documents_in_range_after_ordered(
                    SubjectScope::Window {
                        after: b"a.md",
                        through: Some(b"c.md"),
                    },
                    None,
                    1,
                    order,
                )
                .unwrap();
            assert_eq!(page[0].path.as_str(), "b.md");
            let tail = store
                .begin_request()
                .stored_documents_in_range_after_ordered(
                    SubjectScope::Window {
                        after: b"a.md",
                        through: Some(b"c.md"),
                    },
                    Some(&page[0].path),
                    5,
                    order,
                )
                .unwrap();
            assert_eq!(
                tail.iter().map(|row| row.path.as_str()).collect::<Vec<_>>(),
                ["c.md"]
            );
        }
    }

    #[test]
    fn inventory_orders_opaque_records_and_drains_only_the_requested_page() {
        let scratch = norn_testkit::scratch::Scratch::new("ordered-inventory");
        let mut store = Store::open_throwaway(
            scratch.join("store.sqlite3"),
            StoredPathOrder::Sensitive,
            DerivationVersion::new(1),
        )
        .unwrap();
        store.begin_request().prepare_inventory().unwrap();
        for key in [b"z", b"a", b"m"] {
            store.begin_request().inventory_record(key, key).unwrap();
        }
        let first = store.begin_request().inventory_after(None, 2).unwrap();
        assert_eq!(
            first,
            vec![
                (b"a".to_vec(), b"a".to_vec()),
                (b"m".to_vec(), b"m".to_vec())
            ]
        );
        store
            .begin_request()
            .inventory_discard_through(b"m")
            .unwrap();
        assert_eq!(
            store.begin_request().inventory_after(None, 2).unwrap(),
            vec![(b"z".to_vec(), b"z".to_vec())]
        );
        store
            .begin_request()
            .inventory_discard_through(b"z")
            .unwrap();
        assert!(
            store
                .begin_request()
                .inventory_after(None, 1)
                .unwrap()
                .is_empty()
        );
    }
}

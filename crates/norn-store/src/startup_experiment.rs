//! Disposable metadata and queue storage for explicitly invoked loading experiments.
//! Catalogue membership states that a name was observed, not that its bytes form a document.

use std::path::Path;

use norn_db::TemporaryDatabase;
use norn_db::rusqlite::{params, params_from_iter};

use crate::error;
use crate::find::{compose_class_head, compose_class_total};
use crate::resolve::TargetClass;
use crate::{AmbiguityIgnore, DocumentPath, StoreError};

pub use crate::store::IntakeBuilder;
pub use norn_db::RetirementCheck;

/// A bounded head and exact count of provisional file and document names, using production SQL.
pub struct CatalogueCandidates {
    pub paths: Vec<String>,
    pub total: u64,
}

/// The experimental name catalogue is discarded before ordinary warm operation.
pub struct Catalogue {
    database: TemporaryDatabase,
    complete: bool,
    find_order: bool,
}

/// Bounded candidate names, not eligible rows. EOF is namespace exhaustion only.
pub struct CataloguePrefix {
    pub paths: Vec<String>,
    pub eof: bool,
}

impl Catalogue {
    pub fn retirement_check(&self) -> RetirementCheck {
        self.database
            .shared_file()
            .expect("named catalogue")
            .retirement_check()
    }

    pub fn open(directory: &Path) -> Result<Self, StoreError> {
        let database = TemporaryDatabase::create_in(directory, 256, None)?;
        // The resolver sees the same key columns and SQL as the final store,
        // without paying for bodies, fact indexes, or content eligibility here.
        // The secondary indexes wait for `finish_inventory`, so insertion keeps
        // only the rowid table and the UNIQUE path constraint.
        for table in ["documents", "vault_files"] {
            database.connection().execute_batch(&format!(
                "CREATE TABLE {table} (id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE,
                 suffix_key TEXT NOT NULL, path_key TEXT NOT NULL, admitting_segments INTEGER NOT NULL);"
            )).map_err(|error| error::sql("creating experimental catalogue", error))?;
        }
        database
            .connection()
            .execute_batch("BEGIN")
            .map_err(|error| error::sql("starting experimental catalogue", error))?;
        Ok(Self {
            database,
            complete: false,
            find_order: false,
        })
    }

    pub fn record_file(
        &mut self,
        path: &DocumentPath,
        ignore: &AmbiguityIgnore,
    ) -> Result<(), StoreError> {
        self.record_name("vault_files", path, ignore)
    }

    pub fn record_document(
        &mut self,
        path: &DocumentPath,
        ignore: &AmbiguityIgnore,
    ) -> Result<(), StoreError> {
        self.record_name("documents", path, ignore)
    }

    fn record_name(
        &mut self,
        table: &str,
        path: &DocumentPath,
        ignore: &AmbiguityIgnore,
    ) -> Result<(), StoreError> {
        assert!(!self.complete, "a completed catalogue is immutable");
        self.database.connection().prepare_cached(
            &format!("INSERT INTO {table}(path, suffix_key, path_key, admitting_segments) VALUES (?1, ?2, ?3, ?4)")
        ).and_then(|mut statement| statement.execute(params![path.as_str(), path.suffix_key(),
            path.path_key(), ignore.admitting_segments(path.as_str()) as i64]))
            .map_err(|error| error::sql("recording experimental catalogue", error))?;
        Ok(())
    }

    /// Separate from inventory completion: the frozen lab calls this after Get ACK.
    pub fn prepare_find_order(&mut self) -> Result<(), StoreError> {
        if !self.complete {
            return Err(prefix_error("catalogue inventory is incomplete"));
        }
        self.database
            .connection()
            .execute_batch(&format!(
                "CREATE INDEX IF NOT EXISTS documents_find_order ON documents({})",
                crate::read::answer_ordering("path", "")
            ))
            .map_err(|error| error::sql("indexing catalogue Find order", error))?;
        self.find_order = true;
        Ok(())
    }

    /// Read at most 128 names plus one EOF witness, without collapsing folded ties.
    pub fn document_prefix(&self, cap: usize) -> Result<CataloguePrefix, StoreError> {
        if !self.find_order || cap == 0 || cap > 128 {
            return Err(prefix_error(
                "catalogue prefix requires Find index and cap 1..=128",
            ));
        }
        let sql = format!(
            "SELECT path FROM documents ORDER BY {} LIMIT ?1",
            crate::read::answer_ordering("path", "")
        );
        let mut paths = self
            .database
            .connection()
            .prepare_cached(&sql)
            .and_then(|mut statement| {
                statement
                    .query_map([cap + 1], |row| row.get(0))?
                    .collect::<Result<Vec<String>, _>>()
            })
            .map_err(|error| error::sql("reading catalogue Find prefix", error))?;
        let eof = paths.len() <= cap;
        paths.truncate(cap);
        Ok(CataloguePrefix { paths, eof })
    }

    /// Publish names only after enumeration completes, so absence is not guessed.
    /// The secondary indexes are built here, in the construction transaction, so
    /// SQLite sorts each one instead of maintaining it row by row during insertion.
    pub fn finish_inventory(&mut self) -> Result<(), StoreError> {
        let mut indexes = String::new();
        for table in ["documents", "vault_files"] {
            indexes.push_str(&format!(
                "CREATE INDEX {table}_suffix_key ON {table}(suffix_key, path);
                 CREATE INDEX {table}_admitted ON {table}(admitting_segments, suffix_key, path);
                 CREATE INDEX {table}_path_key ON {table}(path_key);"
            ));
        }
        indexes.push_str("COMMIT");
        self.database
            .connection()
            .execute_batch(&indexes)
            .map_err(|error| error::sql("completing experimental catalogue", error))?;
        self.complete = true;
        Ok(())
    }

    /// The result is provisional content eligibility until candidates are read.
    pub fn candidates(
        &self,
        target: &str,
        ignore: &AmbiguityIgnore,
    ) -> Result<Option<CatalogueCandidates>, StoreError> {
        if !self.complete {
            return Ok(None);
        }
        let class = TargetClass::compile(target, ignore)?;
        let (sql, parameters) = compose_class_head(&class, crate::CANDIDATE_HEAD);
        let paths = self
            .database
            .connection()
            .prepare_cached(&sql)
            .and_then(|mut statement| {
                statement
                    .query_map(params_from_iter(parameters), |row| row.get::<_, String>(1))?
                    .collect::<Result<Vec<_>, _>>()
            })
            .map_err(|error| error::sql("reading experimental catalogue candidates", error))?;
        let (sql, parameters) = compose_class_total(&class);
        let total = self
            .database
            .connection()
            .query_row(&sql, params_from_iter(parameters), |row| row.get(0))
            .map_err(|error| error::sql("counting experimental catalogue candidates", error))?;
        Ok(Some(CatalogueCandidates { paths, total }))
    }
}

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

impl Catalogue {
    /// Conservative eligibility superset, including exact-name fallback and the
    /// neighboring names needed for minimal disambiguating suffixes. This is
    /// not resolution: only the ordinary reader over derived bytes resolves.
    /// None means incomplete inventory or a class exceeding the explicit bound.
    pub fn demand_names(
        &self,
        target: &str,
        bound: usize,
    ) -> Result<Option<Vec<String>>, StoreError> {
        if !self.complete {
            return Ok(None);
        }
        let class = match TargetClass::compile(target, &AmbiguityIgnore::none()) {
            Ok(class) => class,
            Err(StoreError::Path { .. }) => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut names = std::collections::BTreeSet::new();
        for (lower, _) in class.probe().ranges() {
            if !self.add_leaf_names(lower, bound, &mut names)? {
                return Ok(None);
            }
        }
        Ok(Some(names.into_iter().collect()))
    }

    /// Outgoing dependency names use the same addressing and path expansion as
    /// the writer. The superset deliberately precedes content eligibility and
    /// exact-path precedence; invalid exact documents cannot hide suffixes.
    pub fn demand_link_names(
        &self,
        link: &crate::LinkFact,
        holder: &DocumentPath,
        bound: usize,
    ) -> Result<Option<Vec<String>>, StoreError> {
        use crate::link::Addressing;
        match Addressing::of(link, holder) {
            Addressing::Elsewhere => Ok(Some(Vec::new())),
            Addressing::Suffix(target) => self.demand_names(&target, bound),
            Addressing::Paths(paths) => {
                let mut names = std::collections::BTreeSet::new();
                for path in paths {
                    let path = DocumentPath::new(&path)?;
                    if !self.add_leaf_names(path.suffix_key(), bound, &mut names)? {
                        return Ok(None);
                    }
                }
                Ok(Some(names.into_iter().collect()))
            }
        }
    }

    fn add_leaf_names(
        &self,
        key: &str,
        bound: usize,
        names: &mut std::collections::BTreeSet<String>,
    ) -> Result<bool, StoreError> {
        let leaf = format!("{}/", key.split('/').next().expect("a suffix has a leaf"));
        let upper = crate::path::prefix_successor(&leaf);
        // Indexed eligibility harvesting, not a second resolver. Include ignored
        // names too: deeper spellings may admit them when naming candidates.
        let mut statement = self.database.connection().prepare_cached(
            "SELECT path FROM documents WHERE suffix_key >= ?1 AND suffix_key < ?2
             UNION SELECT path FROM vault_files WHERE suffix_key >= ?1 AND suffix_key < ?2 LIMIT ?3"
        ).map_err(|e| error::sql("preparing demand names", e))?;
        let rows = statement
            .query_map(params![leaf, upper, (bound + 1) as i64], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|e| error::sql("reading demand names", e))?;
        for row in rows {
            names.insert(row.map_err(|e| error::sql("reading demand name", e))?);
            if names.len() > bound {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

/// Write depth for isolated experiments; metadata rows never belong to a durable store.
#[derive(Clone, Copy, PartialEq)]
pub enum WriteMode {
    Ordinary,
    DeferredHealth,
    Metadata,
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
        let mut store = Store::open_throwaway(
            scratch.join("metadata.sqlite3"),
            StoredPathOrder::Sensitive,
            DerivationVersion::new(1),
        )
        .unwrap();
        let path = DocumentPath::new("document.md").unwrap();
        let model = ContentModel::none();
        let facts = DocumentFacts::new(path.clone(), "real-source-hash", "uniquebodytoken", 99);
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
        store.close().unwrap();
    }
}

fn prefix_error(message: &str) -> StoreError {
    StoreError::Sql {
        operation: "reading experimental catalogue prefix",
        condition: None,
        message: message.into(),
    }
}

/// Fresh helper state has neither a database nor any journal sidecar.
/// Inspect under maintainership before the ordinary open can rebuild old state.
pub fn fresh_namespace(path: &Path) -> Result<bool, StoreError> {
    norn_db::database_is_absent(path).map_err(StoreError::from)
}

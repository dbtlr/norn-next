//! Disposable metadata and queue storage for explicitly invoked loading experiments.
//! Catalogue membership states that a name was observed, not that its bytes form a document.

use std::path::Path;

use norn_db::TemporaryDatabase;
use norn_db::rusqlite::{params, params_from_iter};

use crate::error;
use crate::find::{compose_class_head, compose_class_total};
use crate::resolve::TargetClass;
use crate::{AmbiguityIgnore, DocumentPath, StoreError};

/// A bounded head and exact count of provisional file and document names, using production SQL.
pub struct CatalogueCandidates {
    pub paths: Vec<String>,
    pub total: u64,
}

/// The experimental name catalogue is discarded before ordinary warm operation.
pub struct Catalogue {
    database: TemporaryDatabase,
    complete: bool,
}

impl Catalogue {
    pub fn open(directory: &Path) -> Result<Self, StoreError> {
        let database = TemporaryDatabase::create_in(directory, 256, None)?;
        // The resolver sees the same key columns and SQL as the final store,
        // without paying for bodies, fact indexes, or content eligibility here.
        for table in ["documents", "vault_files"] {
            database.connection().execute_batch(&format!(
                "CREATE TABLE {table} (id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE,
                 suffix_key TEXT NOT NULL, path_key TEXT NOT NULL, admitting_segments INTEGER NOT NULL);
                 CREATE INDEX {table}_suffix_key ON {table}(suffix_key, path);
                 CREATE INDEX {table}_admitted ON {table}(admitting_segments, suffix_key, path);
                 CREATE INDEX {table}_path_key ON {table}(path_key);"
            )).map_err(|error| error::sql("creating experimental catalogue", error))?;
        }
        database
            .connection()
            .execute_batch("BEGIN")
            .map_err(|error| error::sql("starting experimental catalogue", error))?;
        Ok(Self {
            database,
            complete: false,
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

    /// Publish names only after enumeration completes, so absence is not guessed.
    pub fn finish_inventory(&mut self) -> Result<(), StoreError> {
        self.database
            .connection()
            .execute_batch("COMMIT")
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

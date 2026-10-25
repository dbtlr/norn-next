//! Complete first-index names and sealed disposable projections.
//! Catalogue membership states that a name was observed, not that its bytes form a document.

use std::path::Path;

use norn_db::TemporaryDatabase;
use norn_db::rusqlite::{params, params_from_iter};
pub use norn_db::{TemporaryCleanupReceipt, TemporaryCleanupStatus};

use crate::error;
use crate::find::{compose_class_head, compose_class_total};
use crate::resolve::TargetClass;
use crate::{AmbiguityIgnore, DocumentPath, StoreError};

pub use crate::store::{GetBuilder, IntakeBuilder};

/// A bounded head and exact count of provisional file and document names, using production SQL.
pub struct CatalogueCandidates {
    pub paths: Vec<String>,
    pub total: u64,
}

/// The first-index name catalogue is discarded before ordinary warm operation.
pub struct Catalogue {
    database: TemporaryDatabase,
    complete: bool,
}

impl Catalogue {
    /// Retain eventual cleanup evidence without retaining this catalogue file.
    pub fn cleanup_receipt(&self) -> norn_db::TemporaryCleanupReceipt {
        self.database
            .shared_file()
            .expect("named catalogue")
            .cleanup_receipt()
    }

    pub fn open(directory: &Path) -> Result<Self, StoreError> {
        Self::open_observed(directory, |_| {})
    }

    /// Record this source's cleanup receipt before any fallible schema work.
    pub fn open_observed(
        directory: &Path,
        observe: impl FnOnce(&norn_db::TemporaryCleanupReceipt),
    ) -> Result<Self, StoreError> {
        let database = TemporaryDatabase::create_in_observed(directory, 256, None, observe)?;
        // The resolver sees the same key columns and SQL as the final store,
        // without paying for bodies, fact indexes, or content eligibility here.
        // The secondary indexes wait for `finish_inventory`, so insertion keeps
        // only the rowid table and the UNIQUE path constraint.
        for table in ["documents", "vault_files"] {
            database.connection().execute_batch(&format!(
                "CREATE TABLE {table} (id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE,
                 suffix_key TEXT NOT NULL, path_key TEXT NOT NULL, admitting_segments INTEGER NOT NULL);"
            )).map_err(|error| error::sql("creating loading catalogue", error))?;
        }
        database
            .connection()
            .execute_batch("BEGIN")
            .map_err(|error| error::sql("starting loading catalogue", error))?;
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
            .map_err(|error| error::sql("recording loading catalogue", error))?;
        Ok(())
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
            .map_err(|error| error::sql("completing loading catalogue", error))?;
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
            .map_err(|error| error::sql("reading loading catalogue candidates", error))?;
        let (sql, parameters) = compose_class_total(&class);
        let total = self
            .database
            .connection()
            .prepare_cached(&sql)
            .and_then(|mut statement| {
                statement.query_row(params_from_iter(parameters), |row| row.get(0))
            })
            .map_err(|error| error::sql("counting loading catalogue candidates", error))?;
        Ok(Some(CatalogueCandidates { paths, total }))
    }
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
        if !self.complete {
            return Ok(None);
        }
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
        let limit = bound
            .checked_add(1)
            .and_then(|limit| i64::try_from(limit).ok())
            .ok_or_else(|| prefix_error("name closure bound cannot be represented"))?;
        // Read each covering range separately. UNION DISTINCT materializes the
        // entire class before LIMIT; each branch here stops at a bounded witness.
        for table in ["documents", "vault_files"] {
            let sql = format!(
                "SELECT path FROM {table} WHERE suffix_key >= ?1 AND suffix_key < ?2 LIMIT ?3"
            );
            let mut statement = self
                .database
                .connection()
                .prepare_cached(&sql)
                .map_err(|e| error::sql("preparing demand names", e))?;
            let rows = statement
                .query_map(params![leaf, upper, limit], |row| row.get::<_, String>(0))
                .map_err(|e| error::sql("reading demand names", e))?;
            for row in rows {
                names.insert(row.map_err(|e| error::sql("reading demand name", e))?);
                if names.len() > bound {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }
}

fn prefix_error(message: &str) -> StoreError {
    StoreError::Sql {
        operation: "reading loading catalogue prefix",
        condition: None,
        message: message.into(),
    }
}

fn metadata_predicate(predicate: &norn_wire::Predicate) -> bool {
    use norn_wire::Predicate;
    matches!(
        predicate,
        Predicate::Eq { .. }
            | Predicate::NotEq { .. }
            | Predicate::In { .. }
            | Predicate::Has { .. }
            | Predicate::Missing { .. }
            | Predicate::Before { .. }
            | Predicate::After { .. }
            | Predicate::Path { .. }
    )
}

pub fn supports_find(params: &norn_wire::FindParams) -> bool {
    !(!params.predicates.iter().all(metadata_predicate)
        || !params.columns.iter().all(|column| {
            matches!(
                column,
                norn_wire::Column::Path { .. }
                    | norn_wire::Column::Field { .. }
                    | norn_wire::Column::Fields { .. }
            )
        })
        || params.sort.as_ref().is_some_and(|sort| {
            !matches!(
                sort.key,
                norn_wire::SortKey::Path { .. } | norn_wire::SortKey::Field { .. }
            )
        }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use norn_db::rusqlite::{
        StatementStatus,
        trace::{TraceEvent, TraceEventCodes},
    };
    std::thread_local! {
        static READS: std::cell::RefCell<Vec<(String, String)>> = const { std::cell::RefCell::new(Vec::new()) };
    }
    fn capture(event: TraceEvent<'_>) {
        if let TraceEvent::Stmt(statement, _) = event {
            READS.with_borrow_mut(|reads| {
                reads.push((
                    statement.sql().to_string(),
                    statement.expanded_sql().unwrap(),
                ))
            });
        }
    }
    fn oversized_work(size: usize) -> u64 {
        let scratch = norn_testkit::scratch::Scratch::new("catalogue-bounded-closure");
        let mut catalogue = Catalogue::open(&scratch.join("loading")).unwrap();
        for n in 0..size {
            let path = DocumentPath::new(&format!("d{n:05}/shared.md")).unwrap();
            catalogue
                .record_document(&path, &AmbiguityIgnore::none())
                .unwrap();
            catalogue
                .record_file(&path, &AmbiguityIgnore::none())
                .unwrap();
        }
        catalogue.finish_inventory().unwrap();
        READS.with_borrow_mut(Vec::clear);
        catalogue
            .database
            .connection()
            .trace_v2(TraceEventCodes::SQLITE_TRACE_STMT, Some(capture));
        assert!(catalogue.demand_names("shared", 8).unwrap().is_none());
        catalogue
            .database
            .connection()
            .trace_v2(TraceEventCodes::empty(), None);
        let reads = READS.with_borrow_mut(std::mem::take);
        assert!(!reads.is_empty());
        let mut work = 0;
        for (sql, expanded) in reads {
            let statement = catalogue
                .database
                .connection()
                .prepare_cached(&sql)
                .unwrap();
            assert_eq!(
                statement.get_status(StatementStatus::FullscanStep),
                0,
                "{sql}"
            );
            work += statement.get_status(StatementStatus::VmStep) as u64;
            let plan = catalogue.database.emitted_plan(&expanded, []).unwrap();
            assert!(
                plan.steps.iter().any(|step| step.detail.contains("SEARCH")),
                "{plan:?}"
            );
        }
        work
    }
    #[test]
    fn oversized_closure_work_stops_at_its_bound_independent_of_class_size() {
        let small = oversized_work(100);
        let large = oversized_work(10_000);
        assert!(
            large <= small + 64,
            "closure work grew with an already oversized class: {small} -> {large}"
        );
    }
    fn candidates_work(size: usize) -> u64 {
        let scratch = norn_testkit::scratch::Scratch::new("catalogue-candidate-plan");
        let mut catalogue = Catalogue::open(&scratch.join("loading")).unwrap();
        let ignore = AmbiguityIgnore::none();
        for n in 0..size {
            catalogue
                .record_document(
                    &DocumentPath::new(&format!("filler/{n:05}.md")).unwrap(),
                    &ignore,
                )
                .unwrap();
        }
        for path in ["a/photo.png.md", "b/photo.png.md"] {
            catalogue
                .record_document(&DocumentPath::new(path).unwrap(), &ignore)
                .unwrap();
        }
        catalogue
            .record_file(&DocumentPath::new("assets/photo.png").unwrap(), &ignore)
            .unwrap();
        catalogue.finish_inventory().unwrap();
        READS.with_borrow_mut(Vec::clear);
        catalogue
            .database
            .connection()
            .trace_v2(TraceEventCodes::SQLITE_TRACE_STMT, Some(capture));
        let head = catalogue.candidates("photo.png", &ignore).unwrap().unwrap();
        assert_eq!(head.total, 3);
        assert_eq!(head.paths.len(), 3);
        catalogue
            .database
            .connection()
            .trace_v2(TraceEventCodes::empty(), None);
        let reads = READS.with_borrow_mut(std::mem::take);
        assert_eq!(reads.len(), 2, "bounded head and exact total");
        let mut work = 0;
        for (sql, expanded) in reads {
            let statement = catalogue
                .database
                .connection()
                .prepare_cached(&sql)
                .unwrap();
            assert_eq!(
                statement.get_status(StatementStatus::FullscanStep),
                0,
                "{sql}"
            );
            work += statement.get_status(StatementStatus::VmStep) as u64;
            let plan = catalogue.database.emitted_plan(&expanded, []).unwrap();
            assert!(
                plan.steps.iter().any(|step| step.detail.contains("SEARCH")),
                "{plan:?}"
            );
            assert!(
                plan.reads
                    .iter()
                    .all(|read| ["documents", "vault_files", "json_each"]
                        .contains(&read.table.as_str())),
                "{plan:?}"
            );
        }
        assert!(work > 0, "measure actual executed statements");
        work
    }

    #[test]
    fn candidate_heads_and_exact_totals_use_indexes_independent_of_unrelated_names() {
        let small = candidates_work(100);
        let large = candidates_work(10_000);
        assert!(
            large <= small + 64,
            "candidate work grew with unrelated names: {small} -> {large}"
        );
    }
}

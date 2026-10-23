//! Unpublished metadata construction for the frozen-copy startup experiment.
use super::*;

/// A disposable metadata writer that seals its deferred indexes before reads.
/// A failed write or seal abandons the whole file; journal-OFF has no rollback promise.
pub struct IntakeBuilder {
    store: Option<Store>,
    file: Option<Arc<norn_db::TemporaryFile>>,
    indexes: Vec<String>,
}

impl IntakeBuilder {
    /// Exclusively create disk-backed scratch storage, refusing every existing name.
    pub fn create(
        path: &Path,
        order: StoredPathOrder,
        derivation: DerivationVersion,
    ) -> Result<Self, StoreError> {
        let temporary = norn_db::TemporaryDatabase::create(path, 256, None)?;
        let connection = temporary.connection();
        connection
            .pragma_update(None, "foreign_keys", true)
            .map_err(|error| error::sql("enabling intake constraints", error))?;
        connection.set_prepared_statement_cache_capacity(32);
        let mut schema = store_schema();
        // Findings indexes also serve ingestion's per-subject cleanup, which
        // must stay bounded before sealing. UNIQUE indexes and constraints stay immediate.
        let (indexes, immediate): (Vec<_>, Vec<_>) =
            schema.statements.into_iter().partition(|statement| {
                statement.starts_with("CREATE INDEX ")
                    && !statement.starts_with("CREATE INDEX finding")
            });
        schema.statements = immediate;
        norn_db::initialize_disposable(
            connection,
            &schema,
            &StoreClient {
                mode: StoreMode::Throwaway,
                order,
                derivation,
            },
        )?;
        // Omitted payloads must also refuse ordinary adoption after sealing.
        meta::put_meta(connection, "startup_experiment_incomplete", 1_i64)?;
        let (database, file) = temporary.into_database()?;
        crate::resolve::register_functions(database.connection())?;
        Ok(Self {
            store: Some(Store {
                intake_unsealed: true,
                count_results: Arc::new(crate::count::CountResults::for_store(
                    crate::CountRetentionPolicy::default(),
                    path,
                )),
                database,
                mode: StoreMode::Throwaway,
                order,
                derivation,
                outcome: OpenOutcome::Created,
                // TemporaryFile owns cleanup, including on failed construction.
                torn_down: true,
            }),
            file: Some(file),
            indexes,
        })
    }

    /// Run one bounded loading step through the ordinary store writer.
    /// Returned errors close and delete scratch. Unwinding prevents resumption or
    /// sealing; the builder's drop then releases its remaining file owner.
    pub fn write<T, E>(&mut self, write: impl FnOnce(&mut Store) -> Result<T, E>) -> Result<T, E> {
        // Taking ownership also abandons the writer if the callback unwinds.
        let mut store = self
            .store
            .take()
            .expect("an abandoned intake cannot resume");
        match write(&mut store) {
            Ok(value) => {
                self.store = Some(store);
                Ok(value)
            }
            Err(error) => {
                drop(store);
                self.file.take();
                Err(error)
            }
        }
    }

    /// Build every deferred index before minting a reader, then close the writer.
    /// FILE temp storage permits sorter spill; page cache size does not bound sorter RSS.
    pub fn finish(mut self, budget: &Arc<crate::ReadBudget>) -> Result<SnapshotReader, StoreError> {
        let mut store = self.store.take().ok_or_else(|| StoreError::Sql {
            operation: "sealing experimental metadata",
            condition: None,
            message: "the intake was abandoned after a failed write".into(),
        })?;
        let connection = store.database.connection();
        let transaction = connection
            .unchecked_transaction()
            .map_err(|error| error::sql("starting metadata index sealing", error))?;
        for index in &self.indexes {
            transaction
                .execute_batch(index)
                .map_err(|error| error::sql("sealing metadata indexes", error))?;
        }
        meta::put_meta(&transaction, meta::DDL_FINGERPRINT, ddl::fingerprint())?;
        meta::put_meta(
            &transaction,
            meta::SCHEMA_DIGEST,
            norn_db::schema_digest(&transaction)
                .map_err(|error| error::sql("digesting sealed metadata", error))?,
        )?;
        transaction
            .commit()
            .map_err(|error| error::sql("committing metadata indexes", error))?;
        store.intake_unsealed = false;
        let mut reader = store.reader_pool(budget);
        reader.intake_file = self.file.take();
        // No writer connection or statement cache survives into canonical heal.
        drop(store);
        Ok(reader)
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Harness-only assertions inspect scratch-file lifetime.
mod tests {
    use super::*;
    use crate::{Change, ContentModel, DocumentFacts, DocumentPath};

    #[test]
    fn unreadable_metadata_does_not_turn_findings_cleanup_into_a_corpus_scan() {
        use crate::{DerivedFinding, FindingFacts};
        use norn_wire::{FindingKind, Severity};
        let scratch = norn_testkit::scratch::Scratch::new("intake-findings-cost");
        let mut builder = IntakeBuilder::create(
            &scratch.join("intake.sqlite3"),
            StoredPathOrder::Sensitive,
            DerivationVersion::new(1),
        )
        .unwrap();
        for batch in 0..2 {
            builder
                .write(|store| {
                    let paths: Vec<_> = (0..128)
                        .map(|i| {
                            DocumentPath::new(&format!("note-{}.md", batch * 128 + i)).unwrap()
                        })
                        .collect();
                    let changes = paths.iter().map(|path| {
                        Change::Upsert(DocumentFacts::new(path.clone(), "hash", "body", 4))
                    });
                    let findings: Vec<_> = paths
                        .iter()
                        .map(|path| DerivedFinding {
                            facts: FindingFacts {
                                kind: FindingKind::FrontmatterUnreadable,
                                severity: Severity::Error,
                                path: path.clone(),
                                class_keys: Default::default(),
                                path_keys: std::collections::BTreeSet::from([crate::PathKey::new(
                                    path.as_str(),
                                )
                                .unwrap()]),
                                target: None,
                                span: None,
                                ordinal: None,
                                candidates: Vec::new(),
                                candidates_total: 0,
                                message: "Unreadable frontmatter".into(),
                                detail: Some("Malformed block".into()),
                                rules: Default::default(),
                                value: None,
                            },
                            replaces: None,
                            bytes_hash: None,
                        })
                        .collect();
                    store.begin_request().apply_intake_increment(
                        changes,
                        &findings,
                        &ContentModel::none(),
                    )?;
                    for sql in [
                        crate::request::SUBJECT_DISCARD_SQL,
                        crate::request::PATH_DISCARD_SQL,
                    ] {
                        let mut statement = store.database.connection().prepare(sql).unwrap();
                        statement.execute(["absent.md"]).unwrap();
                        assert_eq!(
                            statement.get_status(rusqlite::StatementStatus::FullscanStep),
                            0,
                            "cleanup must visit only the selected subject, before sealing"
                        );
                    }
                    Ok::<_, StoreError>(())
                })
                .unwrap();
        }
    }

    #[test]
    fn an_unsealed_intake_declines_reads_without_opening_a_connection() {
        let scratch = norn_testkit::scratch::Scratch::new("intake-unsealed-reader");
        let mut builder = IntakeBuilder::create(
            &scratch.join("intake.sqlite3"),
            StoredPathOrder::Sensitive,
            DerivationVersion::new(1),
        )
        .unwrap();
        let budget = crate::ReadBudget::new(crate::ReadBudgetPolicy::default());
        builder
            .write(|store| {
                assert!(store.open_reader().reader.is_err());
                let reader = Arc::new(store.reader_pool(&budget));
                assert!(
                    reader
                        .acquire(
                            crate::ReadNeed::Source,
                            Instant::now() + std::time::Duration::from_secs(5)
                        )?
                        .is_none()
                );
                assert_eq!(budget.census().connections_opened, 0);
                Ok::<_, StoreError>(())
            })
            .unwrap();
    }

    #[test]
    fn construction_refuses_existing_files_and_cleans_a_failed_schema() {
        let scratch = norn_testkit::scratch::Scratch::new("intake-create-failure");
        let path = scratch.join("lasting.sqlite3");
        let store =
            Store::open(&path, StoredPathOrder::Sensitive, DerivationVersion::new(1)).unwrap();
        let epoch = store.epoch().to_string();
        assert!(
            IntakeBuilder::create(&path, StoredPathOrder::Sensitive, DerivationVersion::new(1))
                .is_err()
        );
        drop(store);
        let store =
            Store::open(&path, StoredPathOrder::Sensitive, DerivationVersion::new(1)).unwrap();
        assert_eq!(store.epoch(), epoch);
        store.verify_integrity().unwrap();
        let refused = scratch.join("refused.sqlite3");
        crate::induced_failure::refuse_the_next_store_creation_at(&refused);
        assert!(
            IntakeBuilder::create(
                &refused,
                StoredPathOrder::Sensitive,
                DerivationVersion::new(1)
            )
            .is_err()
        );
        assert!(!refused.exists());
    }

    #[test]
    fn a_failed_changeset_abandons_even_previously_committed_metadata() {
        let scratch = norn_testkit::scratch::Scratch::new("intake-write-failure");
        let path = scratch.join("intake.sqlite3");
        let mut builder =
            IntakeBuilder::create(&path, StoredPathOrder::Sensitive, DerivationVersion::new(1))
                .unwrap();
        builder
            .write(|store| {
                store.begin_request().apply_intake_increment(
                    [Change::Upsert(DocumentFacts::new(
                        DocumentPath::new("first.md").unwrap(),
                        "hash",
                        "body",
                        4,
                    ))],
                    &[],
                    &ContentModel::none(),
                )
            })
            .unwrap();
        assert!(
            builder
                .write(|store| store.begin_request().apply_intake_increment(
                    [Change::Upsert(DocumentFacts::new(
                        DocumentPath::new("second.md").unwrap(),
                        "hash",
                        "body",
                        99
                    ))],
                    &[],
                    &ContentModel::none()
                ))
                .is_err()
        );
        assert!(!path.exists());
        let budget = crate::ReadBudget::new(crate::ReadBudgetPolicy::default());
        assert!(builder.finish(&budget).is_err());
        assert_eq!(budget.census().live_connections, 0);
    }

    #[test]
    fn a_failed_index_seal_deletes_the_entire_unpublished_store() {
        let scratch = norn_testkit::scratch::Scratch::new("intake-seal-failure");
        let path = scratch.join("intake.sqlite3");
        let mut builder =
            IntakeBuilder::create(&path, StoredPathOrder::Sensitive, DerivationVersion::new(1))
                .unwrap();
        let duplicate = builder.indexes[1].clone();
        builder
            .write(|store| store.database.connection().execute_batch(&duplicate))
            .unwrap();
        let budget = crate::ReadBudget::new(crate::ReadBudgetPolicy::default());
        assert!(builder.finish(&budget).is_err());
        assert!(!path.exists());
        assert_eq!(budget.census().live_connections, 0);
    }

    #[test]
    fn disk_full_during_sealing_deletes_scratch_without_publishing_a_reader() {
        let scratch = norn_testkit::scratch::Scratch::new("intake-seal-full");
        let path = scratch.join("intake.sqlite3");
        let mut builder =
            IntakeBuilder::create(&path, StoredPathOrder::Sensitive, DerivationVersion::new(1))
                .unwrap();
        builder
            .write(|store| {
                let connection = store.database.connection();
                let pages: u64 = connection.query_row("PRAGMA page_count", [], |row| row.get(0))?;
                connection.pragma_update(None, "max_page_count", pages)
            })
            .unwrap();
        let budget = crate::ReadBudget::new(crate::ReadBudgetPolicy::default());
        let error = builder
            .finish(&budget)
            .expect_err("index pages exceed the disk quota");
        assert!(error.to_string().contains("full"), "{error}");
        assert!(!path.exists());
        assert_eq!(budget.census().connections_opened, 0);
    }

    #[test]
    fn unique_check_and_foreign_key_constraints_are_live_before_sealing() {
        let scratch = norn_testkit::scratch::Scratch::new("intake-constraints");
        for (name, sql, expected) in [
            (
                "unique",
                "UPDATE documents SET path='first.md' WHERE path='second.md'",
                "UNIQUE",
            ),
            ("check", "UPDATE documents SET path=''", "CHECK"),
            (
                "foreign",
                "INSERT INTO document_fields(document,key,ordinal,path,container) VALUES(999,'key',0,'absent.md','scalar')",
                "FOREIGN KEY",
            ),
        ] {
            let path = scratch.join(format!("{name}.sqlite3"));
            let mut builder =
                IntakeBuilder::create(&path, StoredPathOrder::Sensitive, DerivationVersion::new(1))
                    .unwrap();
            builder
                .write(|store| {
                    store.begin_request().apply_intake_increment(
                        ["first.md", "second.md"].map(|name| {
                            Change::Upsert(DocumentFacts::new(
                                DocumentPath::new(name).unwrap(),
                                "hash",
                                "body",
                                4,
                            ))
                        }),
                        &[],
                        &ContentModel::none(),
                    )
                })
                .unwrap();
            let error = builder
                .write(|store| store.database.connection().execute_batch(sql))
                .unwrap_err();
            assert!(error.to_string().contains(expected), "{error}");
            assert!(!path.exists());
        }
    }

    #[test]
    fn dropping_an_idle_intake_closes_its_connection_before_unlinking() {
        let scratch = norn_testkit::scratch::Scratch::new("intake-idle-cleanup");
        let path = scratch.join("intake.sqlite3");
        let budget = crate::ReadBudget::new(crate::ReadBudgetPolicy::default());
        let reader = Arc::new(
            IntakeBuilder::create(&path, StoredPathOrder::Sensitive, DerivationVersion::new(1))
                .unwrap()
                .finish(&budget)
                .unwrap(),
        );
        let snapshot = reader
            .acquire(
                crate::ReadNeed::Source,
                Instant::now() + std::time::Duration::from_secs(5),
            )
            .unwrap()
            .unwrap()
            .establish()
            .unwrap();
        drop(snapshot);
        assert_eq!(budget.census().idle_connections, 1);
        drop(reader);
        assert_eq!(budget.census().live_connections, 0);
        assert!(!path.exists());
    }
    #[test]
    fn disposable_intake_seals_canonical_indexes_and_releases_every_connection() {
        let scratch = norn_testkit::scratch::Scratch::new("intake-seal");
        let path = scratch.join("metadata.sqlite3");
        let mut builder =
            IntakeBuilder::create(&path, StoredPathOrder::Sensitive, DerivationVersion::new(1))
                .unwrap();
        builder
            .write(|store| {
                let connection = store.database.connection();
                assert_eq!(
                    connection
                        .query_row("PRAGMA cache_size", [], |r| r.get::<_, i64>(0))
                        .unwrap(),
                    -256
                );
                assert_eq!(
                    connection
                        .query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))
                        .unwrap(),
                    "off"
                );
                assert_eq!(
                    connection
                        .query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0))
                        .unwrap(),
                    1
                );
                assert_eq!(
                    connection
                        .query_row(
                            "SELECT count(*) FROM sqlite_schema WHERE name='document_fields_raw'",
                            [],
                            |r| r.get::<_, u64>(0)
                        )
                        .unwrap(),
                    0
                );
                assert!(store.open_reader().reader.is_err());
                let facts =
                    DocumentFacts::new(DocumentPath::new("first.md").unwrap(), "hash", "body", 4);
                store.begin_request().apply_intake_increment(
                    [Change::Upsert(facts)],
                    &[],
                    &ContentModel::none(),
                )?;
                Ok::<_, crate::StoreError>(())
            })
            .unwrap();
        let budget = crate::ReadBudget::new(crate::ReadBudgetPolicy::default());
        let reader = std::sync::Arc::new(builder.finish(&budget).unwrap());
        assert_eq!(budget.census().live_connections, 0);
        assert!(path.exists());
        let snapshot = reader
            .acquire(
                crate::ReadNeed::Source,
                std::time::Instant::now() + std::time::Duration::from_secs(5),
            )
            .unwrap()
            .unwrap()
            .establish()
            .unwrap();
        let ordinary = Store::open(
            scratch.join("ordinary.sqlite3"),
            StoredPathOrder::Sensitive,
            DerivationVersion::new(1),
        )
        .unwrap();
        let connection = snapshot.database.as_ref().unwrap().connection();
        assert_eq!(
            norn_db::schema_digest(connection).unwrap(),
            norn_db::schema_digest(ordinary.database.connection()).unwrap()
        );
        for sql in [
            "UPDATE documents SET body='wrong'",
            "PRAGMA query_only=OFF",
            "ATTACH ':memory:' AS escape",
        ] {
            assert!(
                connection.execute_batch(sql).is_err(),
                "sealed reader admitted {sql}"
            );
        }
        reader.retire();
        drop(reader);
        assert!(path.exists(), "the active snapshot owns its scratch file");
        drop(snapshot);
        assert_eq!(budget.census().live_connections, 0);
        assert!(!path.exists());
    }
}

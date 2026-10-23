//! Frozen-copy metadata capability. The raw snapshot is deliberately private:
//! omitted bodies and rich facts must never escape as complete document data.
use super::*;
use std::sync::Mutex;
use std::time::Instant;

/// Complete frozen-copy metadata, retained only until ordinary construction ends.
/// Retirement releases idle resources even when an observer keeps this handle.
pub struct IntakeReader {
    reader: Mutex<Option<Arc<SnapshotReader>>>,
    model: Arc<ContentModel>,
}

impl IntakeReader {
    /// Acquire a bounded source/scratch grant for Find or retained field counts.
    /// An already established snapshot survives retirement; a new one declines.
    pub fn snapshot(
        &self,
        deadline: Instant,
    ) -> Result<Option<IntakeSnapshot>, norn_store::StoreError> {
        let Some(reader) = self.reader.lock().expect("intake reader poisoned").clone() else {
            return Ok(None);
        };
        reader
            .acquire(norn_store::ReadNeed::SourceAndScratch, deadline)?
            .map(|turn| {
                turn.establish().map(|snapshot| IntakeSnapshot {
                    snapshot,
                    model: self.model.clone(),
                })
            })
            .transpose()
    }
    fn retire(&self) {
        if let Some(reader) = self.reader.lock().expect("intake reader poisoned").take() {
            reader.retire_intake();
        }
    }
}

/// Only guarded field/path reads are exposed; Get and rich reads are unavailable.
pub struct IntakeSnapshot {
    snapshot: norn_store::Snapshot,
    model: Arc<ContentModel>,
}

/// A capability decline is separate from the ordinary builder's refusal.
#[derive(Debug)]
pub enum IntakeRefusal {
    Unsupported,
    Page(norn_store::PageRefusal),
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

impl IntakeSnapshot {
    pub fn find(&self, params: &norn_wire::FindParams) -> Result<norn_store::Found, IntakeRefusal> {
        if !params.predicates.iter().all(metadata_predicate)
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
            })
        {
            return Err(IntakeRefusal::Unsupported);
        }
        self.snapshot
            .find(params, &self.model)
            .map_err(IntakeRefusal::Page)
    }

    pub fn count(
        &self,
        params: &norn_wire::CountParams,
    ) -> Result<norn_store::Counted, IntakeRefusal> {
        if !params.predicates.iter().all(metadata_predicate)
            || !params
                .by
                .iter()
                .all(|key| matches!(key, norn_wire::GroupKey::Field { .. }))
        {
            return Err(IntakeRefusal::Unsupported);
        }
        self.snapshot
            .count(params, &self.model)
            .map_err(IntakeRefusal::Page)
    }

    pub fn reading(&self) -> &norn_store::StoreReading {
        self.snapshot.reading()
    }
}

pub(super) struct Projection {
    reader: Arc<IntakeReader>,
}

impl Projection {
    pub(super) fn close(self) -> Result<(), JobFailure> {
        self.reader.retire();
        Ok(())
    }
}
impl Drop for Projection {
    fn drop(&mut self) {
        self.reader.retire();
    }
}

pub(super) fn prepare(
    attachment: &mut ProductionAttachment,
    exclusions: &[PathBuf],
    policy: ProductionPolicy,
    experiment: &Experiment,
    model: &Arc<ContentModel>,
) -> Result<Projection, JobFailure> {
    use std::hash::BuildHasher;
    let timing = norn_store::profiling::span("startup.canonical_intake");
    let root = &attachment.covered_root;
    let source = norn_fs::Vault::open(root, exclusions).map_err(effect)?;
    let mut inventory = InventoryWalk::from_vault(&source).map_err(effect)?;
    let nonce = std::collections::hash_map::RandomState::new().hash_one(std::process::id());
    let mut builder = norn_store::startup_experiment::IntakeBuilder::create(
        &attachment
            .shadows
            .directory()
            .join("canonical-intake")
            .join(format!("intake-{nonce:016x}.sqlite3")),
        attachment.store.path_order(),
        crate::DERIVATION_VERSION,
    )
    .map_err(store_effect)?;
    if let Some(pin) = attachment
        .store
        .begin_request()
        .vault_schema_pin()
        .map_err(store_effect)?
    {
        builder
            .write(|store| {
                store
                    .begin_request()
                    .pin_vault_schema(&pin.bytes, &pin.fingerprint)
            })
            .map_err(store_effect)?;
    }
    loop {
        let page = inventory.advance(policy.store_page_size).map_err(effect)?;
        // Quarantine bookkeeping is scoped to one page as well as the write
        // queue. Its rich diagnostics are outside this reader's capabilities.
        builder.write(|store| {
            let mut account = Account::default();
            let mut pending =
                Pending::new(store, policy.changeset_size, root, exclusions, &mut account)?;
            for record in page.records {
                load_fact(
                    &mut pending,
                    inventory.replay(&record.value).map_err(effect)?,
                    Depth::Metadata,
                    false,
                )?;
                if pending.is_full() {
                    pending.flush_for_mode(norn_store::startup_experiment::WriteMode::Metadata)?;
                }
            }
            pending.flush_for_mode(norn_store::startup_experiment::WriteMode::Metadata)?;
            drop(pending);
            if !account.withheld.roots.is_empty() || account.withheld.rendered {
                return Err(JobFailure::Environmental(
                    "metadata inventory could not read the complete frozen copy".into(),
                ));
            }
            Ok(())
        })?;
        if page.complete {
            break;
        }
    }
    let reader = Arc::new(IntakeReader {
        reader: Mutex::new(Some(Arc::new(
            builder.finish(&attachment.readers).map_err(store_effect)?,
        ))),
        model: model.clone(),
    });
    let projection = Projection {
        reader: reader.clone(),
    };
    drop(timing);
    {
        let _probe = norn_store::profiling::span("startup.probes");
        experiment
            .observer
            .intake(reader)
            .map_err(JobFailure::Environmental)?;
    }
    Ok(projection)
}

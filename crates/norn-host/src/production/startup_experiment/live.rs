//! Private observer-backed whole-Get eligibility, never public read admission.
use super::*;
use std::hash::BuildHasher;
use std::sync::Mutex;
use std::time::Instant;

/// Only the prepared target's default whole Get can run on this projection.
/// Every execution rechecks coverage; no snapshot acquisition API is exposed.
pub struct LiveGetReader {
    reader: Mutex<Option<Arc<SnapshotReader>>>,
    model: Arc<ContentModel>,
    observation: norn_fs::watch::Observation,
    target: String,
    retired: AtomicBool,
    failures: Mutex<Vec<JobFailure>>,
}

#[derive(Debug)]
pub enum LiveGetRefusal {
    Unavailable,
    Unsupported,
    Store(norn_store::StoreError),
    Page(norn_store::PageRefusal),
}

impl LiveGetReader {
    fn eligible(&self) -> bool {
        !self.retired.load(Ordering::SeqCst) && self.observation.is_current()
    }

    /// Uses the attachment's existing read budget and the caller's acquisition
    /// deadline. No watcher or entry lock spans acquisition or query execution.
    pub fn get(
        &self,
        params: &norn_wire::GetParams,
        deadline: Instant,
    ) -> Result<norn_store::Gotten, LiveGetRefusal> {
        if params.target.address() != self.target
            || params.target.anchor().is_some()
            || !params.columns.is_empty()
            || params.collection.is_some()
            || params.limit.is_some()
            || params.after.is_some()
        {
            return Err(LiveGetRefusal::Unsupported);
        }
        if !self.eligible() || Instant::now() >= deadline {
            return Err(LiveGetRefusal::Unavailable);
        }
        let reader = self
            .reader
            .lock()
            .expect("live Get reader poisoned")
            .clone()
            .ok_or(LiveGetRefusal::Unavailable)?;
        let run = || {
            let turn = reader
                .acquire(norn_store::ReadNeed::Source, deadline)
                .map_err(LiveGetRefusal::Store)?
                .ok_or(LiveGetRefusal::Unavailable)?;
            let snapshot = turn.establish().map_err(LiveGetRefusal::Store)?;
            if !self.eligible() {
                return Err(LiveGetRefusal::Unavailable);
            }
            // Preserve actual builder/storage errors even if coverage also moved.
            let answer = get(&snapshot, params, &self.model).map_err(LiveGetRefusal::Page)?;
            if !self.eligible() {
                return Err(LiveGetRefusal::Unavailable);
            }
            Ok(answer)
        };
        let result = run();
        if let Err(
            LiveGetRefusal::Store(error)
            | LiveGetRefusal::Page(norn_store::PageRefusal::Store(error)),
        ) = &result
        {
            self.failures
                .lock()
                .expect("live Get failures poisoned")
                .push(store_effect(error.clone()));
        }
        result
    }

    fn retire(&self) {
        self.retired.store(true, Ordering::SeqCst);
        if let Some(reader) = self.reader.lock().expect("live Get reader poisoned").take() {
            reader.retire();
        }
    }
}

struct Retire(Arc<LiveGetReader>);
impl Drop for Retire {
    fn drop(&mut self) {
        self.0.retire();
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run(
    attachment: &mut ProductionAttachment,
    exclusions: &[PathBuf],
    policy: ProductionPolicy,
    experiment: &Experiment,
    model: &Arc<ContentModel>,
    source: &norn_fs::Vault,
    catalogue: &Catalogue,
    observation: Option<norn_fs::watch::Observation>,
) -> Result<(), JobFailure> {
    let decline = || {
        experiment
            .observer
            .declined("live whole-Get eligibility unavailable; wait for ordinary readiness")
            .map_err(JobFailure::Environmental)
    };
    let Some(observation) = observation.filter(|guard| {
        attachment
            .subscription
            .as_ref()
            .is_some_and(|watch| watch.accepts_observation(guard))
    }) else {
        return decline();
    };
    let nonce = std::collections::hash_map::RandomState::new().hash_one(std::process::id());
    let mut projection = norn_store::Store::open_throwaway(
        attachment
            .shadows
            .directory()
            .join("canonical-demand")
            .join(format!("live-demand-{nonce:016x}.sqlite3")),
        attachment.store.path_order(),
        crate::DERIVATION_VERSION,
    )
    .map_err(store_effect)?;
    let mut failures = Vec::new();
    let result = (|| {
        if let Some(pin) = attachment
            .store
            .begin_request()
            .vault_schema_pin()
            .map_err(store_effect)?
        {
            projection
                .begin_request()
                .pin_vault_schema(&pin.bytes, &pin.fingerprint)
                .map_err(store_effect)?;
        }
        let prepared = canonical::prepare(
            &mut projection,
            catalogue,
            source,
            &attachment.covered_root,
            exclusions,
            policy,
            experiment,
        )?;
        if prepared != canonical::Preparation::Ready || !observation.is_current() {
            return decline();
        }
        let reader = Arc::new(projection.reader_pool(&attachment.readers));
        let _retire_raw = RetireReader(reader.clone());
        if !canonical::requested_class_exists(&reader, model, &experiment.get_target)?
            || !observation.is_current()
        {
            return decline();
        }
        let capability = Arc::new(LiveGetReader {
            reader: Mutex::new(Some(reader)),
            model: model.clone(),
            observation,
            target: experiment.get_target.clone(),
            retired: AtomicBool::new(false),
            failures: Mutex::new(Vec::new()),
        });
        let _retire = Retire(capability.clone());
        let observed = experiment
            .observer
            .live_get(capability.clone())
            .map_err(JobFailure::Environmental);
        capability.retire();
        failures.extend(
            capability
                .failures
                .lock()
                .expect("live Get failures poisoned")
                .drain(..),
        );
        observed
    })();
    failures.extend(result.err());
    failures.extend(projection.close().map_err(store_effect).err());
    // Keep damage typed and every operation/cleanup cause, even if an observer
    // swallowed a query error. An eligibility decline never erases a real error.
    if failures.len() == 1 {
        return Err(failures.pop().unwrap());
    }
    if !failures.is_empty() {
        let details = format!("live whole-Get operation/cleanup failures: {failures:?}");
        return Err(
            if failures
                .iter()
                .any(|error| matches!(error, JobFailure::StoreDamaged(_)))
            {
                JobFailure::StoreDamaged(details)
            } else {
                JobFailure::Environmental(details)
            },
        );
    }
    Ok(())
}

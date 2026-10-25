//! Private observer-backed Get and complete metadata Find, never public read admission.
//!
//! Both phases carry the observation captured before the names catalogue. A
//! known source contradiction withdraws that shared authority; a Get nonclaim
//! does not. Each actual call checks eligibility around the ordinary builder.
//! Complete metadata is sealed before Find is offered, and both projections
//! retire before ordinary canonical construction. Count remains ordinary-only.
//!
//! Find cursors retain their real disposable epoch and key. Ordinary Host::find
//! judges them after Ready, including epoch movement and incompatible orders.
//! Accepted watcher facts are the freshness boundary, not filesystem atomicity.
//! These serial lab calls do not certify concurrent retirement or late errors.
use super::*;
use std::hash::BuildHasher;
use std::sync::Mutex;
use std::time::Instant;

/// Only the prepared target's default whole Get can run on this projection.
/// Every execution rechecks coverage; no snapshot acquisition API is exposed.
pub struct LiveGetReader {
    guarded: LiveReader,
    model: Arc<ContentModel>,
    target: String,
}

/// Complete metadata only. A retained handle rechecks coverage at execution;
/// it cannot mint an unrestricted Snapshot or a frozen IntakeSnapshot.
pub struct LiveFindReader {
    guarded: LiveReader,
    model: Arc<ContentModel>,
}

#[derive(Debug)]
pub enum LiveRefusal {
    Unavailable,
    Unsupported,
    Store(norn_store::StoreError),
    Page(norn_store::PageRefusal),
}

pub type LiveGetRefusal = LiveRefusal;
pub type LiveFindRefusal = LiveRefusal;

// Get and Find share acquisition, eligibility, retirement and typed failures.
// The observation is cloned from the one boundary preceding the catalogue.
struct LiveReader {
    reader: Mutex<Option<Arc<SnapshotReader>>>,
    observation: norn_fs::watch::Observation,
    retired: AtomicBool,
    failures: Mutex<Vec<JobFailure>>,
}

impl LiveReader {
    fn new(reader: Arc<SnapshotReader>, observation: norn_fs::watch::Observation) -> Self {
        Self {
            reader: Mutex::new(Some(reader)),
            observation,
            retired: AtomicBool::new(false),
            failures: Mutex::new(Vec::new()),
        }
    }

    fn eligible(&self) -> bool {
        !self.retired.load(Ordering::SeqCst) && self.observation.is_current()
    }

    /// The existing shared budget and original deadline cover acquisition.
    /// Query/storage failures keep their types even if eligibility also moved.
    fn run<T>(
        &self,
        deadline: Instant,
        query: impl FnOnce(norn_store::Snapshot) -> Result<T, LiveRefusal>,
    ) -> Result<T, LiveRefusal> {
        if !self.eligible() || Instant::now() >= deadline {
            return Err(LiveRefusal::Unavailable);
        }
        let reader = self
            .reader
            .lock()
            .expect("live reader poisoned")
            .clone()
            .ok_or(LiveRefusal::Unavailable)?;
        let run = || {
            let turn = reader
                .acquire(norn_store::ReadNeed::Source, deadline)
                .map_err(LiveRefusal::Store)?
                .ok_or(LiveRefusal::Unavailable)?;
            let snapshot = turn.establish().map_err(LiveRefusal::Store)?;
            if !self.eligible() {
                return Err(LiveRefusal::Unavailable);
            }
            let answer = query(snapshot)?;
            if !self.eligible() {
                return Err(LiveRefusal::Unavailable);
            }
            Ok(answer)
        };
        let result = run();
        if let Err(
            LiveRefusal::Store(error) | LiveRefusal::Page(norn_store::PageRefusal::Store(error)),
        ) = &result
        {
            self.failures
                .lock()
                .expect("live failures poisoned")
                .push(store_effect(error.clone()));
        }
        result
    }

    fn retire(&self) {
        self.retired.store(true, Ordering::SeqCst);
        if let Some(reader) = self.reader.lock().expect("live reader poisoned").take() {
            reader.retire();
        }
    }

    fn take_failures(&self) -> Vec<JobFailure> {
        self.failures
            .lock()
            .expect("live failures poisoned")
            .drain(..)
            .collect()
    }
}

impl LiveGetReader {
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
            return Err(LiveRefusal::Unsupported);
        }
        self.guarded.run(deadline, |snapshot| {
            get(&snapshot, params, &self.model).map_err(LiveRefusal::Page)
        })
    }
}

impl LiveFindReader {
    /// Run the existing intake Find restrictions and builder under live eligibility.
    /// The result carries its actual source identity; no cursor is rewritten.
    pub fn find(
        &self,
        params: &norn_wire::FindParams,
        deadline: Instant,
    ) -> Result<norn_store::Found, LiveFindRefusal> {
        self.guarded.run(deadline, |snapshot| {
            let intake = IntakeSnapshot {
                snapshot,
                model: self.model.clone(),
            };
            intake.find(params).map_err(|refusal| match refusal {
                IntakeRefusal::Unsupported => LiveRefusal::Unsupported,
                IntakeRefusal::Page(error) => LiveRefusal::Page(error),
            })
        })
    }
}

struct Retire<'a>(&'a LiveReader);
impl Drop for Retire<'_> {
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
        if prepared == canonical::Preparation::SourceChanged {
            observation.invalidate();
        }
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
            guarded: LiveReader::new(reader, observation),
            model: model.clone(),
            target: experiment.get_target.clone(),
        });
        let _retire = Retire(&capability.guarded);
        let observed = experiment
            .observer
            .live_get(capability.clone())
            .map_err(JobFailure::Environmental);
        capability.guarded.retire();
        failures.extend(capability.guarded.take_failures());
        observed
    })();
    failures.extend(result.err());
    failures.extend(projection.close().map_err(store_effect).err());
    finish(failures)
}

// Keep every operation/cleanup cause, including errors swallowed by observers.
fn finish(mut failures: Vec<JobFailure>) -> Result<(), JobFailure> {
    if failures.len() == 1 {
        return Err(failures.pop().unwrap());
    }
    if !failures.is_empty() {
        let details = format!("live operation/cleanup failures: {failures:?}");
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

/// Metadata uses the complete intake construction, never the capped first page.
/// This scope retires before canonical heal, including on observer failure.
pub(super) fn metadata(
    attachment: &mut ProductionAttachment,
    exclusions: &[PathBuf],
    policy: ProductionPolicy,
    experiment: &Experiment,
    model: &Arc<ContentModel>,
    observation: Option<norn_fs::watch::Observation>,
) -> Result<(), JobFailure> {
    let Some(observation) = observation.filter(|guard| guard.is_current()) else {
        return experiment
            .observer
            .declined("live metadata eligibility unavailable; wait for ordinary readiness")
            .map_err(JobFailure::Environmental);
    };
    let mut retirement = None;
    let mut failures = Vec::new();
    let result = (|| {
        let Some(reader) = super::intake::build(
            attachment,
            exclusions,
            policy,
            Some((experiment.observer.as_ref(), &observation)),
            Some(&mut retirement),
        )?
        else {
            return experiment
                .observer
                .declined("live metadata changed during construction; wait for ordinary readiness")
                .map_err(JobFailure::Environmental);
        };
        let reader = Arc::new(reader);
        let capability = Arc::new(LiveFindReader {
            guarded: LiveReader::new(reader, observation),
            model: model.clone(),
        });
        let _retire = Retire(&capability.guarded);
        let result = if capability.guarded.eligible() {
            experiment.observer.live_find(capability.clone())
        } else {
            experiment
                .observer
                .declined("live metadata changed during construction; wait for ordinary readiness")
        }
        .map_err(JobFailure::Environmental);
        capability.guarded.retire();
        failures.extend(capability.guarded.take_failures());
        result
    })();
    failures.extend(result.err());
    // The live phase has released its owners. A held read is reported as an
    // outstanding owner, never preempted or waited out to claim physical cleanup.
    if let Some(retirement) = retirement {
        failures.extend(
            retirement
                .verify()
                .map_err(|error| store_effect(error.into()))
                .err(),
        );
    }
    finish(failures)
}

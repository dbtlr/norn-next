//! One sealed, capped metadata prefix under an explicit frozen-copy assumption.
//! No raw reader or count capability escapes; SQL remains ordinary Snapshot::find.
use super::*;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

const CAP: usize = 128;

/// Harness authority over frozen inputs, never inferred from an empty watcher slot.
/// Mutation probes must invalidate this before attempting an early answer.
pub struct FrozenInput(AtomicBool);
impl FrozenInput {
    pub fn assume_immutable() -> Arc<Self> {
        Arc::new(Self(AtomicBool::new(true)))
    }
    pub fn invalidate(&self) {
        self.0.store(false, Ordering::SeqCst);
    }
    pub(super) fn intact(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Clone, Debug, Default)]
pub struct PrefixCertificate {
    pub consumed: usize,
    pub catalogue_replayed: u64,
    pub catalogue_document_candidates: u64,
    pub frontier: Option<String>,
    pub eof: bool,
    pub bytes_read: u64,
    pub invalid: Option<String>,
    pub writer_work: BTreeMap<&'static str, u64>,
}

/// An attempted Find retains its real SQL cost even when no answer is published.
pub struct PrefixAttempt {
    pub certificate: PrefixCertificate,
    pub answer: Option<norn_store::Found>,
    pub decline: Option<String>,
    pub reading: Option<norn_store::StoreReading>,
    pub counters: Option<BTreeMap<&'static str, u64>>,
    pub query_executed: bool,
    pub read_budget: Option<[norn_store::ReadBudgetCensus; 3]>,
}

/// Borrowed only during the private callback; no snapshot or general reader accessor.
pub struct PrefixReader {
    reader: Option<Arc<IntakeReader>>,
    model: Arc<ContentModel>,
    frozen: Arc<FrozenInput>,
    certificate: PrefixCertificate,
    budget: Arc<norn_store::ReadBudget>,
}
impl Drop for PrefixReader {
    fn drop(&mut self) {
        if let Some(reader) = self.reader.take() {
            reader.retire();
        }
    }
}
impl PrefixReader {
    pub fn find(&self, params: &norn_wire::FindParams) -> PrefixAttempt {
        let mut attempt = PrefixAttempt {
            certificate: self.certificate.clone(),
            answer: None,
            decline: None,
            reading: None,
            counters: None,
            query_executed: false,
            read_budget: None,
        };
        let declined = if !self.frozen.intact() {
            Some("frozen-input certificate broken")
        } else if self.certificate.invalid.is_some() {
            self.certificate.invalid.as_deref()
        } else if params.limit != Some(25)
            || params.after.is_some()
            || params.sort.is_some()
            || !params.columns.is_empty()
            || params.predicates != [norn_wire::Predicate::equal_to("perf_bucket", "active")]
        {
            Some("unsupported capped-prefix operation")
        } else if self
            .model
            .typed_order("perf_bucket")
            .is_some_and(|order| order.is_dated())
        {
            Some("dated key has uncertified global diagnostics")
        } else {
            None
        };
        if let Some(reason) = declined {
            if !self.frozen.intact() {
                attempt.certificate.invalid = Some("frozen-input certificate broken".into());
            }
            attempt.decline = Some(reason.into());
            return attempt;
        }
        let Some(reader) = &self.reader else {
            attempt.decline = Some("prefix unavailable".into());
            return attempt;
        };
        let before = self.budget.census();
        let mut during = None;
        let mut run = || -> Result<_, norn_store::StoreError> {
            let snapshot = reader
                .snapshot(Instant::now() + Duration::from_secs(5))?
                .ok_or_else(|| norn_store::StoreError::Sql {
                    operation: "acquiring capped prefix",
                    condition: None,
                    message: "prefix reader unavailable".into(),
                })?;
            during = Some(self.budget.census());
            attempt.reading = Some(snapshot.reading().clone());
            attempt.query_executed = true;
            let answer = snapshot.snapshot.find(params, &self.model);
            attempt.counters = Some(snapshot.snapshot.counters().readings().collect());
            // The actual established snapshot ends here, before observer ACK.
            Ok(answer)
        };
        match run() {
            Ok(Ok(answer))
                if self.frozen.intact()
                    && answer.unsatisfied.is_empty()
                    && answer.advisories.is_empty()
                    && (answer.next.is_some() || self.certificate.eof) =>
            {
                attempt.answer = Some(answer);
            }
            Ok(Ok(answer)) => {
                attempt.decline = Some(
                    if !self.frozen.intact() {
                        "frozen-input certificate broken"
                    } else if !answer.unsatisfied.is_empty() || !answer.advisories.is_empty() {
                        "uncertified global diagnostics or unknown key"
                    } else {
                        "no limit+1 witness before catalogue EOF"
                    }
                    .into(),
                )
            }
            Ok(Err(error)) => attempt.decline = Some(format!("ordinary Find refusal: {error:?}")),
            Err(error) => attempt.decline = Some(format!("prefix acquisition failed: {error}")),
        }
        if let Some(during) = during {
            attempt.read_budget = Some([before, during, self.budget.census()]);
        }
        if !self.frozen.intact() {
            attempt.certificate.invalid = Some("frozen-input certificate broken".into());
        }
        attempt
    }
}

pub(super) struct PrefixEnd {
    pub check: Option<norn_store::startup_experiment::RetirementCheck>,
    pub failure: Option<JobFailure>,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run(
    attachment: &mut ProductionAttachment,
    exclusions: &[PathBuf],
    policy: ProductionPolicy,
    experiment: &Experiment,
    model: &Arc<ContentModel>,
    source: &norn_fs::Vault,
    catalogue: &mut Catalogue,
    frozen: &Arc<FrozenInput>,
    invalid: Option<String>,
    catalogue_replayed: u64,
    catalogue_document_candidates: u64,
) -> Result<PrefixEnd, JobFailure> {
    let _timing = norn_store::profiling::span("startup.capped_prefix");
    let before = crate::evidence::startup_writer_work();
    let mut capability = PrefixReader {
        reader: None,
        model: model.clone(),
        frozen: frozen.clone(),
        budget: attachment.readers.clone(),
        certificate: PrefixCertificate {
            invalid,
            catalogue_replayed,
            catalogue_document_candidates,
            ..Default::default()
        },
    };
    if !frozen.intact() {
        capability.certificate.invalid = Some("frozen-input certificate broken".into());
    }
    let mut failure = None;
    if capability.certificate.invalid.is_none() {
        let prepared = prepare(
            attachment,
            exclusions,
            policy,
            source,
            catalogue,
            &mut capability,
            experiment.observer.as_ref(),
        );
        if let Err(error) = prepared {
            capability.certificate.invalid =
                Some(format!("prefix preparation declined: {error:?}"));
            failure = Some(error);
        }
    }
    capability.certificate.writer_work = crate::evidence::startup_writer_work()
        .into_iter()
        .map(|(key, value)| (key, value - before[key]))
        .collect();
    experiment
        .observer
        .prefix(&capability)
        .map_err(JobFailure::Environmental)?;
    // Retirement is outside all budget bookkeeping locks. Active snapshots keep
    // their ordinary owner; there is no forced close or OS-release guarantee.
    let check = capability
        .reader
        .as_ref()
        .and_then(|reader| reader.retirement_check());
    drop(capability);
    Ok(PrefixEnd { check, failure })
}

fn prepare(
    attachment: &mut ProductionAttachment,
    exclusions: &[PathBuf],
    policy: ProductionPolicy,
    source: &norn_fs::Vault,
    catalogue: &mut Catalogue,
    capability: &mut PrefixReader,
    observer: &dyn Observer,
) -> Result<(), JobFailure> {
    catalogue.prepare_find_order().map_err(store_effect)?;
    let names = catalogue.document_prefix(CAP).map_err(store_effect)?;
    let root = &attachment.covered_root;
    let nonce = std::collections::hash_map::RandomState::new().hash_one(std::process::id());
    use std::hash::BuildHasher;
    let mut builder = norn_store::startup_experiment::IntakeBuilder::create(
        &attachment
            .shadows
            .directory()
            .join("canonical-intake")
            .join(format!("intake-prefix-{nonce:016x}.sqlite3")),
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
    builder.write(|store| {
        let mut account = Account::default();
        let mut pending =
            Pending::new(store, policy.changeset_size, root, exclusions, &mut account)?;
        for name in &names.paths {
            let fact = source.fact(Path::new(name)).map_err(effect)?;
            let norn_fs::WalkFact::File(file) = &fact else {
                capability.certificate.invalid =
                    Some("changed/vanished candidate; frozen-input certificate broken".into());
                break;
            };
            if file.path().as_path() != Path::new(name) {
                capability.certificate.invalid =
                    Some("changed candidate spelling; frozen-input certificate broken".into());
                break;
            }
            let path = document_path(Path::new(name))
                .map_err(|_| JobFailure::Environmental("invalid catalogue candidate".into()))?;
            if pending
                .store
                .begin_request()
                .stored_document(&path)
                .map_err(store_effect)?
                .is_some()
            {
                capability.certificate.invalid =
                    Some("duplicate/colliding prefix path; frozen-input certificate broken".into());
                break;
            }
            let hash = load_fact(&mut pending, fact, Depth::Metadata, false)?;
            if hash.is_none() {
                capability.certificate.invalid =
                    Some("unresolved prefix read; frozen-input certificate broken".into());
                break;
            }
            // The private observer can place a deterministic edit in this
            // load/rehash window; ordinary loading has no such callback.
            observer
                .prefix_loaded(name)
                .map_err(JobFailure::Environmental)?;
            let reobserved = match source.fact(Path::new(name)).map_err(effect)? {
                norn_fs::WalkFact::File(file) if file.path().as_path() == Path::new(name) => {
                    file.read_optional().map_err(effect)?
                }
                _ => None,
            };
            if let Some(read) = &reobserved {
                pending.account.bytes_read += read.bytes().len() as u64;
            }
            if reobserved
                .as_ref()
                .map(|read| read.content_hash().to_string())
                != hash
            {
                capability.certificate.invalid =
                    Some("changed/vanished bytes; frozen-input certificate broken".into());
                break;
            }
            capability.certificate.consumed += 1;
            capability.certificate.frontier = Some(name.clone());
            if pending.is_full() {
                pending.flush_for_mode(norn_store::startup_experiment::WriteMode::Metadata)?;
            }
        }
        pending.flush_for_mode(norn_store::startup_experiment::WriteMode::Metadata)?;
        drop(pending);
        capability.certificate.bytes_read = account.bytes_read;
        if !account.withheld.roots.is_empty() || account.withheld.rendered {
            capability.certificate.invalid =
                Some("unresolved prefix observation; frozen-input certificate broken".into());
        }
        Ok(())
    })?;
    capability.certificate.eof = names.eof
        && capability.certificate.consumed == names.paths.len()
        && capability.certificate.invalid.is_none();
    if capability.certificate.invalid.is_none() {
        capability.reader = Some(Arc::new(IntakeReader::from_sealed(
            builder.finish(&attachment.readers).map_err(store_effect)?,
            capability.model.clone(),
        )));
    }
    Ok(())
}

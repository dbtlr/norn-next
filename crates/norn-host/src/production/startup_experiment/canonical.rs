//! Disposable demand work preceding an unchanged ordinary heal. This is a
//! frozen-copy simulation or explicitly private live lab, never host admission
//! or a trust stance.
use super::*;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::BuildHasher;
use std::os::unix::ffi::OsStrExt;

// A large neighborhood waits for ordinary readiness; a truncated head never
// certifies absence. Residency follows this bound and one parsed document.
const CLOSURE_BOUND: usize = 256;

pub(super) fn run(
    attachment: &mut ProductionAttachment,
    exclusions: &[PathBuf],
    policy: ProductionPolicy,
    progress: &Healing<'_, ProductionAttachment>,
    experiment: &Experiment,
) -> Result<(), JobFailure> {
    let root = attachment.covered_root.clone();
    let model = prepare_prefix(
        attachment,
        exclusions,
        policy,
        experiment,
        &mut PrefixEvidence::default(),
    )?;
    let intake = if experiment.intake && !experiment.live {
        Some(super::intake::prepare(
            attachment, exclusions, policy, experiment, &model,
        )?)
    } else {
        None
    };
    if experiment.frozen_prefix.is_some() {
        experiment
            .observer
            .prefix_retired()
            .map_err(JobFailure::Environmental)?;
    }
    let before = crate::evidence::startup_writer_work();
    {
        let _timing = norn_store::profiling::span("startup.canonical_heal");
        // No priority writes, alternate packing, health deferral, or pragma
        // changes ever reach this store. This is the ordinary attach's call.
        attachment.skipped_links = super::super::heal_documents(
            &mut attachment.store,
            &root,
            exclusions,
            policy,
            progress,
        )?;
    }
    let work = crate::evidence::startup_writer_work()
        .into_iter()
        .map(|(key, value)| {
            value
                .checked_sub(before[key])
                .map(|delta| (key, delta))
                .ok_or_else(|| {
                    JobFailure::Environmental(format!("ordinary writer counter decreased: {key}"))
                })
        })
        .collect::<Result<_, _>>()?;
    experiment
        .observer
        .canonical_work(&work)
        .map_err(JobFailure::Environmental)?;
    // Close new acquisitions before the host can publish its ordinary reader.
    // Existing snapshots retain their normal connection/descriptor lifetime.
    if let Some(intake) = intake {
        intake.close()?;
    }
    let reader = Arc::new(attachment.store.reader_pool(&attachment.readers));
    let _retire = RetireReader(reader.clone());
    notify(experiment, Stage::CanonicalHealth, &reader, &model)
}

/// Evidence outlives failed preparation as well as the successful bounded phase.
#[derive(Default)]
pub(super) struct PrefixEvidence {
    pub demand_served: bool,
    pub demand_reader: std::sync::Weak<SnapshotReader>,
    pub checks: Vec<norn_store::startup_experiment::RetirementCheck>,
    pub failures: Vec<JobFailure>,
}

/// Both continuations consume this scope before healing or stopping.
pub(super) fn prepare_prefix(
    attachment: &mut ProductionAttachment,
    exclusions: &[PathBuf],
    policy: ProductionPolicy,
    experiment: &Experiment,
    evidence: &mut PrefixEvidence,
) -> Result<Arc<ContentModel>, JobFailure> {
    // Capture before inventory construction: accepted facts during enumeration
    // must invalidate just as facts during dependency loading do.
    let observation = if experiment.live {
        attachment
            .subscription
            .as_ref()
            .and_then(|watch| watch.unchanged_observation())
    } else {
        None
    };
    if let Some(observation) = &observation {
        experiment
            .observer
            .live_observation(observation)
            .map_err(JobFailure::Environmental)?;
    }
    let root = attachment.covered_root.clone();
    let source = norn_fs::Vault::open(&root, exclusions).map_err(effect)?;
    let declared = Declaration::read(&mut attachment.store)?;
    let model = Arc::clone(declared.model.shared_content_model());
    {
        let mut catalogue_decline = None;
        let mut catalogue_replayed = 0_u64;
        let mut catalogue_document_candidates = 0_u64;
        let timing = norn_store::profiling::span("startup.catalogue");
        let directory = attachment.shadows.directory().join("canonical-demand");
        let mut catalogue = Catalogue::open(&directory).map_err(store_effect)?;
        evidence.checks.push(catalogue.retirement_check());
        let mut inventory = InventoryWalk::from_vault(&source).map_err(effect)?;
        loop {
            let page = inventory.advance(policy.store_page_size).map_err(effect)?;
            for record in page.records {
                catalogue_replayed += 1;
                experiment
                    .observer
                    .catalogue_replay(
                        Path::new(std::ffi::OsStr::from_bytes(&record.value)),
                        &mut catalogue,
                        &model,
                    )
                    .map_err(JobFailure::Environmental)?;
                let fact = inventory.replay(&record.value).map_err(effect)?;
                if (experiment.frozen_prefix.is_some() || experiment.live)
                    && let norn_fs::WalkFact::Skipped(skip) = &fact
                    && !skip.reason().stands()
                {
                    if let Some(frozen) = &experiment.frozen_prefix {
                        frozen.invalidate();
                    }
                    catalogue_decline = Some(if experiment.live {
                        "unresolved catalogue observation; live whole-Get eligibility unavailable"
                    } else {
                        "unresolved catalogue observation; frozen-input certificate broken"
                    }.to_string());
                }
                if let norn_fs::WalkFact::File(file) = fact
                    && let Ok(path) = document_path(file.path().as_path())
                {
                    let recorded = if is_markdown(file.path().as_path()) {
                        catalogue_document_candidates += 1;
                        catalogue.record_document(&path, model.ambiguity_ignore())
                    } else if path.is_vault_file() {
                        catalogue.record_file(&path, model.ambiguity_ignore())
                    } else {
                        Ok(())
                    };
                    // A failed catalogue write is a storage failure in every mode.
                    // It cannot leave a projection eligible for an early claim.
                    recorded.map_err(store_effect)?;
                }
            }
            if page.complete {
                break;
            }
        }
        catalogue.finish_inventory().map_err(store_effect)?;
        drop(timing);
        {
            let _probe = norn_store::profiling::span("startup.probes");
            experiment
                .observer
                .catalogue(&catalogue, &model)
                .map_err(JobFailure::Environmental)?;
        }
        if experiment.live {
            super::live::run(
                attachment,
                exclusions,
                policy,
                experiment,
                &model,
                &source,
                &catalogue,
                observation.clone().filter(|_| catalogue_decline.is_none()),
            )?;
            if experiment.intake {
                super::live::metadata(
                    attachment,
                    exclusions,
                    policy,
                    experiment,
                    &model,
                    observation.filter(|_| catalogue_decline.is_none()),
                )?;
            }
            return Ok(model);
        }
        let timing = norn_store::profiling::span("startup.canonical_demand");
        let nonce = std::collections::hash_map::RandomState::new().hash_one(std::process::id());
        let mut projection = norn_store::Store::open_throwaway(
            directory.join(format!("demand-{nonce:016x}.sqlite3")),
            attachment.store.path_order(),
            crate::DERIVATION_VERSION,
        )
        .map_err(store_effect)?;
        evidence.checks.push(projection.retirement_check());
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
        let frozen_valid = || {
            experiment
                .frozen_prefix
                .as_ref()
                .is_none_or(|frozen| frozen.intact() && catalogue_decline.is_none())
        };
        let prepared = if frozen_valid() {
            prepare(
                &mut projection,
                &catalogue,
                &source,
                &root,
                exclusions,
                policy,
                experiment,
            )?
        } else {
            Preparation::Declined
        };
        if prepared == Preparation::SourceChanged
            && let Some(frozen) = &experiment.frozen_prefix
        {
            // Get and Find share this authority: an observed contradiction
            // closes every later early claim, not just the current Get.
            frozen.invalidate();
        }
        drop(timing);
        if prepared == Preparation::Ready
            && frozen_valid()
            && attachment.subscription.as_ref().is_some_and(|watch| {
                matches!(watch.state(), norn_fs::SubscriptionState::Live)
                    && !watch.holds_a_settled_batch()
            })
        {
            let reader = Arc::new(projection.reader_pool(&attachment.readers));
            evidence.demand_reader = Arc::downgrade(&reader);
            let _retire = RetireReader(reader.clone());
            if requested_class_exists(&reader, &model, &experiment.get_target)? && frozen_valid() {
                notify(experiment, Stage::CanonicalDemand, &reader, &model)?;
                evidence.demand_served = true;
            } else {
                experiment
                    .observer
                    .declined("this prototype leaves empty eligible classes to ordinary readiness")
                    .map_err(JobFailure::Environmental)?;
            }
        } else {
            experiment.observer.declined("closure exceeds the bound, request is unsupported, or source movement was observed; wait for ordinary readiness").map_err(JobFailure::Environmental)?;
        }
        projection.close().map_err(store_effect)?;
        if let Some(frozen) = &experiment.frozen_prefix {
            // Get's observer has returned (including its ACK) before this index exists.
            super::prefix::run(
                attachment,
                exclusions,
                policy,
                experiment,
                &model,
                &source,
                &mut catalogue,
                frozen,
                catalogue_decline,
                catalogue_replayed,
                catalogue_document_candidates,
                evidence,
            )?;
        }
    }
    Ok(model)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Preparation {
    Ready,
    Declined,
    SourceChanged,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn prepare(
    store: &mut norn_store::Store,
    catalogue: &Catalogue,
    source: &norn_fs::Vault,
    root: &Path,
    exclusions: &[PathBuf],
    policy: ProductionPolicy,
    experiment: &Experiment,
) -> Result<Preparation, JobFailure> {
    let target = &experiment.get_target;
    // This capability covers whole-record default gets only. Other request
    // shapes must wait; the ordinary external API is unchanged.
    if target.contains('#') {
        return Ok(Preparation::Declined);
    }
    let Some(demanded) = catalogue
        .demand_names(target, CLOSURE_BOUND)
        .map_err(store_effect)?
    else {
        return Ok(Preparation::Declined);
    };
    // This prototype serves existing records; missing gets wait for ordinary readiness.
    if demanded.is_empty() {
        return Ok(Preparation::Declined);
    }
    let demanded: BTreeSet<_> = demanded.into_iter().collect();
    let mut remaining = demanded.clone();
    let mut observed = BTreeMap::new();
    let mut account = Account::default();
    while let Some(name) = remaining.pop_first() {
        if observed.contains_key(&name) {
            continue;
        }
        if observed.len() >= CLOSURE_BOUND {
            return Ok(Preparation::Declined);
        }
        let fact = source.fact(Path::new(&name)).map_err(effect)?;
        if !matches!(fact, norn_fs::WalkFact::File(_)) {
            return Ok(Preparation::SourceChanged);
        }
        let hash = {
            let mut pending =
                Pending::new(store, policy.changeset_size, root, exclusions, &mut account)?;
            let hash = load_fact(&mut pending, fact, Depth::Rich, true)?;
            pending.flush()?;
            hash
        };
        if is_markdown(Path::new(&name)) && hash.is_none() {
            return Ok(Preparation::SourceChanged);
        }
        experiment
            .observer
            .demand_loaded(&name)
            .map_err(JobFailure::Environmental)?;
        observed.insert(name.clone(), hash);
        let path = document_path(Path::new(&name))
            .map_err(|_| JobFailure::Environmental("catalogue path became invalid".into()))?;
        // Complete the naming context for ambiguity heads and finding candidates.
        // This does not follow neighbors' outgoing links.
        for spelling in path.suffix_spellings() {
            let Some(names) = catalogue
                .demand_names(&spelling, CLOSURE_BOUND)
                .map_err(store_effect)?
            else {
                return Ok(Preparation::Declined);
            };
            remaining.extend(
                names
                    .into_iter()
                    .filter(|name| !observed.contains_key(name)),
            );
            if remaining.len() + observed.len() > CLOSURE_BOUND {
                return Ok(Preparation::Declined);
            }
        }
        if demanded.contains(&name)
            && let Some(facts) = store
                .begin_request()
                .stored_facts(&path)
                .map_err(store_effect)?
        {
            for link in &facts.links {
                let Some(names) = catalogue
                    .demand_link_names(&link.fact, &path, CLOSURE_BOUND)
                    .map_err(store_effect)?
                else {
                    return Ok(Preparation::Declined);
                };
                remaining.extend(
                    names
                        .into_iter()
                        .filter(|name| !observed.contains_key(name)),
                );
                if remaining.len() + observed.len() > CLOSURE_BOUND {
                    return Ok(Preparation::Declined);
                }
            }
        }
    }
    // Re-observe real bytes after closure construction, including invalid UTF-8
    // candidates. A changed or removed member closes this simulation checkpoint.
    for (name, hash) in observed {
        let norn_fs::WalkFact::File(file) = source.fact(Path::new(&name)).map_err(effect)? else {
            return Ok(Preparation::SourceChanged);
        };
        if let Some(hash) = hash {
            let Some(read) = file.read_optional().map_err(effect)? else {
                return Ok(Preparation::SourceChanged);
            };
            if read.content_hash().to_string() != hash {
                return Ok(Preparation::SourceChanged);
            }
        }
    }
    Ok(Preparation::Ready)
}

// Catalogue neighborhoods over-approximate eligibility: every name may be
// quarantined, ignored, or outside the requested suffix. The ordinary builder
// decides this too. This prototype conservatively leaves empty classes to readiness.
pub(super) fn requested_class_exists(
    reader: &Arc<SnapshotReader>,
    model: &ContentModel,
    target: &str,
) -> Result<bool, JobFailure> {
    requested_class_exists_observed(reader, model, target, None)
}

pub(super) fn requested_class_exists_observed(
    reader: &Arc<SnapshotReader>,
    model: &ContentModel,
    target: &str,
    observer: Option<&dyn Observer>,
) -> Result<bool, JobFailure> {
    use norn_wire::{Column, GetParams, ResolutionTarget, VaultAddress, VaultName};
    let _timing = norn_store::profiling::span("startup.canonical_demand");
    let Ok(target) = ResolutionTarget::new(target) else {
        return Ok(false);
    };
    let turn = reader
        .acquire(
            norn_store::ReadNeed::Source,
            std::time::Instant::now() + std::time::Duration::from_secs(5),
        )
        .map_err(store_effect)?
        .ok_or_else(|| JobFailure::Environmental("demand reader unavailable".into()))?;
    let snapshot = turn.establish().map_err(store_effect)?;
    // Store builders use the attached database, not the wire vault address.
    let params = GetParams::new(
        VaultAddress::name(VaultName::new("experiment").unwrap()),
        target,
    )
    .with_columns([Column::path()]);
    let result = get(&snapshot, &params, model);
    let observed = if let Some(observer) = observer {
        observer
            .live_eligibility_get(&LiveReadEvidence {
                builder_invoked: true,
                reading: Some(snapshot.reading().clone()),
                counters: Some(snapshot.counters()),
            })
            .map_err(JobFailure::Environmental)
    } else {
        Ok(())
    };
    let available = match result {
        Ok(_) | Err(norn_store::PageRefusal::AmbiguousTarget(_)) => Ok(true),
        Err(norn_store::PageRefusal::UnknownTarget { .. }) => Ok(false),
        Err(norn_store::PageRefusal::Store(error)) => Err(store_effect(error)),
        Err(refusal) => Err(JobFailure::Environmental(format!(
            "demand eligibility: {refusal:?}"
        ))),
    };
    match (available, observed) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(observed)) => {
            super::live::finish(vec![error, observed])?;
            unreachable!("nonempty failures always refuse")
        }
    }
}

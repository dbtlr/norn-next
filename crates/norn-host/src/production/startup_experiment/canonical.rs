//! Disposable demand work preceding an unchanged ordinary heal. This is a
//! frozen-copy, paused simulation, not live host admission or a trust stance.
use super::*;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::BuildHasher;

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
    let source = norn_fs::Vault::open(&root, exclusions).map_err(effect)?;
    let declared = Declaration::read(&mut attachment.store)?;
    let model = Arc::clone(declared.model.shared_content_model());
    {
        let timing = norn_store::profiling::span("startup.catalogue");
        let directory = attachment.shadows.directory().join("canonical-demand");
        let mut catalogue = Catalogue::open(&directory).map_err(store_effect)?;
        let mut inventory = InventoryWalk::from_vault(&source).map_err(effect)?;
        loop {
            let page = inventory.advance(policy.store_page_size).map_err(effect)?;
            for record in page.records {
                if let norn_fs::WalkFact::File(file) =
                    inventory.replay(&record.value).map_err(effect)?
                    && let Ok(path) = document_path(file.path().as_path())
                {
                    if is_markdown(file.path().as_path()) {
                        catalogue
                            .record_document(&path, model.ambiguity_ignore())
                            .map_err(store_effect)?;
                    } else if path.is_vault_file() {
                        catalogue
                            .record_file(&path, model.ambiguity_ignore())
                            .map_err(store_effect)?;
                    }
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
        let timing = norn_store::profiling::span("startup.canonical_demand");
        let nonce = std::collections::hash_map::RandomState::new().hash_one(std::process::id());
        let mut projection = norn_store::Store::open_throwaway(
            directory.join(format!("demand-{nonce:016x}.sqlite3")),
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
            projection
                .begin_request()
                .pin_vault_schema(&pin.bytes, &pin.fingerprint)
                .map_err(store_effect)?;
        }
        let prepared = prepare(
            &mut projection,
            &catalogue,
            &source,
            &root,
            exclusions,
            policy,
            &experiment.get_target,
        )?;
        drop(timing);
        if prepared
            && attachment.subscription.as_ref().is_some_and(|watch| {
                matches!(watch.state(), norn_fs::SubscriptionState::Live)
                    && !watch.holds_a_settled_batch()
            })
        {
            let reader = Arc::new(projection.reader_pool(&attachment.readers));
            let _retire = RetireReader(reader.clone());
            if requested_class_exists(&reader, &model, &experiment.get_target)? {
                notify(experiment, Stage::CanonicalDemand, &reader, &model)?;
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
    }
    let intake = if experiment.intake {
        Some(super::intake::prepare(
            attachment, exclusions, policy, experiment, &model,
        )?)
    } else {
        None
    };
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

#[allow(clippy::too_many_arguments)]
fn prepare(
    store: &mut norn_store::Store,
    catalogue: &Catalogue,
    source: &norn_fs::Vault,
    root: &Path,
    exclusions: &[PathBuf],
    policy: ProductionPolicy,
    target: &str,
) -> Result<bool, JobFailure> {
    // This capability covers whole-record default gets only. Other request
    // shapes must wait; the ordinary external API is unchanged.
    if target.contains('#') {
        return Ok(false);
    }
    let Some(demanded) = catalogue
        .demand_names(target, CLOSURE_BOUND)
        .map_err(store_effect)?
    else {
        return Ok(false);
    };
    // This prototype serves existing records; missing gets wait for ordinary readiness.
    if demanded.is_empty() {
        return Ok(false);
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
            return Ok(false);
        }
        let fact = source.fact(Path::new(&name)).map_err(effect)?;
        if !matches!(fact, norn_fs::WalkFact::File(_)) {
            return Ok(false);
        }
        let hash = {
            let mut pending =
                Pending::new(store, policy.changeset_size, root, exclusions, &mut account)?;
            let hash = load_fact(&mut pending, fact, Depth::Rich, true)?;
            pending.flush()?;
            hash
        };
        if is_markdown(Path::new(&name)) && hash.is_none() {
            return Ok(false);
        }
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
                return Ok(false);
            };
            remaining.extend(
                names
                    .into_iter()
                    .filter(|name| !observed.contains_key(name)),
            );
            if remaining.len() + observed.len() > CLOSURE_BOUND {
                return Ok(false);
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
                    return Ok(false);
                };
                remaining.extend(
                    names
                        .into_iter()
                        .filter(|name| !observed.contains_key(name)),
                );
                if remaining.len() + observed.len() > CLOSURE_BOUND {
                    return Ok(false);
                }
            }
        }
    }
    // Re-observe real bytes after closure construction, including invalid UTF-8
    // candidates. A changed or removed member closes this simulation checkpoint.
    for (name, hash) in observed {
        let norn_fs::WalkFact::File(file) = source.fact(Path::new(&name)).map_err(effect)? else {
            return Ok(false);
        };
        if let Some(hash) = hash {
            let Some(read) = file.read_optional().map_err(effect)? else {
                return Ok(false);
            };
            if read.content_hash().to_string() != hash {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

// Catalogue neighborhoods over-approximate eligibility: every name may be
// quarantined, ignored, or outside the requested suffix. The ordinary builder
// decides this too. This prototype conservatively leaves empty classes to readiness.
fn requested_class_exists(
    reader: &Arc<SnapshotReader>,
    model: &ContentModel,
    target: &str,
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
    match get(&snapshot, &params, model) {
        Ok(_) | Err(norn_store::PageRefusal::AmbiguousTarget(_)) => Ok(true),
        Err(norn_store::PageRefusal::UnknownTarget { .. }) => Ok(false),
        Err(refusal) => Err(JobFailure::Environmental(format!(
            "demand eligibility: {refusal:?}"
        ))),
    }
}

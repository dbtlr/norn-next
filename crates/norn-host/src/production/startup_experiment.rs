//! Opt-in phase experiments over disposable vault copies. Ordinary readiness
//! remains closed until the complete store and watcher have converged.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use norn_store::{ContentModel, SnapshotReader, startup_experiment::Catalogue};

/// Query checkpoints are experimental capabilities, not ordinary trust states.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Stage {
    RequestedDocument,
    /// Complete requested record from a disposable dependency closure; paused simulation.
    CanonicalDemand,
    /// Ordinary heal and transactional health have completed.
    CanonicalHealth,
    Frontmatter,
    RichFacts,
    AllFacts,
    LinkHealth,
}

/// Store reads at a checkpoint prove query cost without claiming host admission.
pub trait Observer: Send + Sync {
    fn declined(&self, _reason: &str) -> Result<(), String> {
        Ok(())
    }
    fn catalogue(&self, catalogue: &Catalogue, model: &ContentModel) -> Result<(), String>;
    fn indexed(
        &self,
        stage: Stage,
        reader: Arc<SnapshotReader>,
        model: Arc<ContentModel>,
    ) -> Result<(), String>;
}

/// The experiment is explicitly supplied by an isolated measurement worker.
pub struct Experiment {
    pub get_target: String,
    pub observer: Arc<dyn Observer>,
    started: AtomicBool,
    separate_frontmatter: bool,
    canonical: bool,
}

impl Experiment {
    pub fn new(get_target: impl Into<String>, observer: Arc<dyn Observer>) -> Arc<Self> {
        Arc::new(Self {
            get_target: get_target.into(),
            observer,
            started: AtomicBool::new(false),
            separate_frontmatter: true,
            canonical: false,
        })
    }

    pub fn facts_once(get_target: impl Into<String>, observer: Arc<dyn Observer>) -> Arc<Self> {
        Arc::new(Self {
            get_target: get_target.into(),
            observer,
            started: AtomicBool::new(false),
            separate_frontmatter: false,
            canonical: false,
        })
    }

    /// Frozen-copy simulation only. The disposable store never becomes the lasting store.
    pub fn canonical_demand(
        get_target: impl Into<String>,
        observer: Arc<dyn Observer>,
    ) -> Arc<Self> {
        Arc::new(Self {
            get_target: get_target.into(),
            observer,
            started: AtomicBool::new(false),
            separate_frontmatter: false,
            canonical: true,
        })
    }

    pub(super) fn take_initial_attempt(&self) -> bool {
        !self.started.swap(true, Ordering::SeqCst)
    }
}

/// The same document reader used by normal gets, for a checkpoint's snapshot.
pub fn get(
    snapshot: &norn_store::Snapshot,
    params: &norn_wire::GetParams,
    model: &ContentModel,
) -> Result<norn_store::Gotten, norn_store::PageRefusal> {
    snapshot.get(params, model, &crate::text::TextLayer)
}

use super::HealScope;
use super::{
    Account, Declaration, JobFailure, Pending, ProductionAttachment, ProductionPolicy, close_job,
    count_document_derived, count_increment_work, document_path, effect, is_markdown,
    plan_document, store_effect,
};
use crate::Healing;
use crate::derivation::plan_base_document;
use norn_fs::walk::inventory::InventoryWalk;
use norn_store::Change;
use std::path::{Path, PathBuf};

/// The regular attach prologue already owns coverage, maintainership and the pin.
pub(super) fn run(
    attachment: &mut ProductionAttachment,
    exclusions: &[PathBuf],
    policy: ProductionPolicy,
    progress: &Healing<'_, ProductionAttachment>,
    experiment: &Experiment,
) -> Result<(), JobFailure> {
    if experiment.canonical {
        return canonical::run(attachment, exclusions, policy, progress, experiment);
    }
    let catalogue_timing = norn_store::profiling::span("startup.catalogue");
    let root = attachment.covered_root.clone();
    let source = norn_fs::Vault::open(&root, exclusions).map_err(effect)?;
    let mut inventory = InventoryWalk::from_vault(&source).map_err(effect)?;
    let store = &mut attachment.store;
    let temp_settings = store
        .begin_request()
        .start_startup_experiment()
        .map_err(store_effect)?;
    let declared = Declaration::read(store)?;
    let model = Arc::clone(declared.model.shared_content_model());
    let directory = attachment.shadows.directory().join("startup-catalogue");
    let mut catalogue = Catalogue::open(&directory).map_err(store_effect)?;
    store
        .begin_request()
        .prepare_inventory()
        .map_err(store_effect)?;
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
            store
                .begin_request()
                .inventory_record(&record.key, &record.value)
                .map_err(store_effect)?;
        }
        if page.complete {
            break;
        }
    }
    catalogue.finish_inventory().map_err(store_effect)?;
    drop(catalogue_timing);
    {
        let _probe = norn_store::profiling::span("startup.probes");
        experiment
            .observer
            .catalogue(&catalogue, &model)
            .map_err(JobFailure::Environmental)?;
    }
    let reader = Arc::new(store.reader_pool(&attachment.readers));
    let _retire_on_exit = RetireReader(Arc::clone(&reader));
    let mut account = Account::default();
    let candidates = catalogue
        .candidates(&experiment.get_target, model.ambiguity_ignore())
        .map_err(store_effect)?
        .expect("completed catalogue");
    if candidates.total != 1 || !is_markdown(Path::new(&candidates.paths[0])) {
        return Err(JobFailure::Environmental(
            "the experiment's requested slug is not a unique catalogue candidate".into(),
        ));
    }
    {
        let _timing = norn_store::profiling::span("startup.requested_document");
        let fact = source
            .fact(Path::new(&candidates.paths[0]))
            .map_err(effect)?;
        let mut pending = Pending::new(
            store,
            policy.changeset_size,
            &root,
            exclusions,
            &mut account,
        )?;
        load_fact(&mut pending, fact, true, true)?;
        pending.flush_for_mode(true)?;
    }
    notify(experiment, Stage::RequestedDocument, &reader, &model)?;
    if experiment.separate_frontmatter {
        let base_timing = norn_store::profiling::span("startup.base");
        replay_phase(
            store,
            &inventory,
            &root,
            exclusions,
            policy,
            &mut account,
            true,
            true,
            progress,
        )?;
        drop(base_timing);
        notify(experiment, Stage::Frontmatter, &reader, &model)?;
    }
    let rich_timing = norn_store::profiling::span("startup.rich");
    replay_phase(
        store,
        &inventory,
        &root,
        exclusions,
        policy,
        &mut account,
        false,
        !experiment.separate_frontmatter,
        progress,
    )?;
    drop(rich_timing);
    notify(
        experiment,
        if experiment.separate_frontmatter {
            Stage::RichFacts
        } else {
            Stage::AllFacts
        },
        &reader,
        &model,
    )?;
    let health_timing = norn_store::profiling::span("startup.final_health");
    account.walked(HealScope::Vault, store.path_order());
    close_job(store, &root, exclusions, policy, &mut account)?;
    {
        let mut request = store.begin_request();
        request
            .finish_startup_health(&model)
            .map_err(store_effect)?;
        count_increment_work(request.counters(), request.read_steps());
    }
    drop(health_timing);
    notify(experiment, Stage::LinkHealth, &reader, &model)?;
    let _finalize = norn_store::profiling::span("startup.finalize");
    reader.retire();
    store
        .begin_request()
        .reset_inventory()
        .map_err(store_effect)?;
    store
        .begin_request()
        .complete_startup_experiment(temp_settings)
        .map_err(store_effect)?;
    attachment.skipped_links = account.links;
    Ok(())
}

struct RetireReader(Arc<SnapshotReader>);

impl Drop for RetireReader {
    fn drop(&mut self) {
        self.0.retire();
    }
}

fn notify(
    experiment: &Experiment,
    stage: Stage,
    reader: &Arc<SnapshotReader>,
    model: &Arc<ContentModel>,
) -> Result<(), JobFailure> {
    let _probe = norn_store::profiling::span("startup.probes");
    experiment
        .observer
        .indexed(stage, Arc::clone(reader), Arc::clone(model))
        .map_err(JobFailure::Environmental)
}

#[allow(clippy::too_many_arguments)] // Each pass uses the same bounded queue and ordinary pending writer.
fn replay_phase(
    store: &mut norn_store::Store,
    inventory: &InventoryWalk,
    root: &Path,
    exclusions: &[PathBuf],
    policy: ProductionPolicy,
    account: &mut Account,
    base: bool,
    files: bool,
    progress: &Healing<'_, ProductionAttachment>,
) -> Result<(), JobFailure> {
    let mut after: Option<Vec<u8>> = None;
    let mut processed = 0;
    let mut pending = Pending::new(store, policy.changeset_size, root, exclusions, account)?;
    loop {
        let page = pending
            .store
            .begin_request()
            .inventory_after(after.as_deref(), policy.store_page_size)
            .map_err(store_effect)?;
        if page.is_empty() {
            break;
        }
        for (_, record) in &page {
            let fact = inventory.replay(record).map_err(effect)?;
            load_fact(&mut pending, fact, base, files)?;
            processed += 1;
            if pending.is_full() {
                pending.flush_for_mode(true)?;
            }
        }
        pending.flush_for_mode(true)?;
        after = page.last().map(|(key, _)| key.clone());
        progress.report(processed, None);
    }
    Ok(())
}

fn load_fact(
    pending: &mut Pending<'_>,
    fact: norn_fs::WalkFact,
    base: bool,
    files: bool,
) -> Result<Option<String>, JobFailure> {
    match fact {
        norn_fs::WalkFact::Skipped(skip) => {
            if files {
                pending.passed_over(skip.path().as_path(), skip.reason());
            } else if !skip.reason().stands() {
                pending.account.withheld.absorb(skip.path().as_path());
            }
        }
        norn_fs::WalkFact::File(file) if !is_markdown(file.path().as_path()) => {
            if files
                && let Ok(path) = document_path(file.path().as_path())
                && path.is_vault_file()
            {
                pending.push(Change::FileBirth { path });
            }
        }
        norn_fs::WalkFact::File(file) => {
            let path = file.path().as_path().to_owned();
            let Some(spelling) = path.to_str() else {
                if let Err(quarantine) = document_path(&path) {
                    pending.quarantine(&path, quarantine);
                }
                return Ok(None);
            };
            let Some(read) = file.read_optional().map_err(effect)? else {
                pending.withhold_entry(&path);
                return Ok(None);
            };
            pending.account.bytes_read += read.bytes().len() as u64;
            let stored = match document_path(&path) {
                Ok(path) => pending
                    .store
                    .begin_request()
                    .stored_document(&path)
                    .map_err(store_effect)?
                    .map(|row| row.path),
                Err(_) => None,
            };
            let hash = read.content_hash().to_string();
            count_document_derived();
            let plan = if base {
                plan_base_document(
                    &path,
                    spelling,
                    read.bytes(),
                    hash.clone(),
                    stored.as_ref(),
                    &pending.declared.model,
                    pending.store.path_order().glob_case(),
                )
            } else {
                plan_document(
                    &path,
                    spelling,
                    read.bytes(),
                    hash.clone(),
                    stored.as_ref(),
                    &pending.declared.model,
                    pending.store.path_order().glob_case(),
                )
            };
            pending.record_plan(plan, hash.clone());
            return Ok(Some(hash));
        }
    }
    Ok(None)
}

/// Harness-only, induced-failure cache option, independent of loading order and
/// health mode. Each worker gets at most one attempt, on its first `Created` or
/// `RebuiltFromZero` store if empty. `Reused` stores skip without spending the attempt.
/// Diagnostic reads must run separately from accepted timing samples.
pub struct InitialImportCache {
    size: norn_store::startup_experiment::WriterCacheSize,
    diagnostics: bool,
    started: AtomicBool,
    receipt: std::sync::Mutex<Option<InitialImportCacheReceipt>>,
}

/// Harness-only receipt of a restored initial-import window under induced-failure.
/// This records the one worker attempt, not warm-performance or RSS qualification.
#[derive(Clone, Copy, Debug)]
pub struct InitialImportCacheReceipt {
    pub writer: norn_store::startup_experiment::WriterCacheReceipt,
    pub import_succeeded: bool,
}

impl InitialImportCache {
    /// Configure one isolated worker's induced-failure window (1..=8192 KiB).
    /// Enable extra diagnostic reads only in runs separate from timing samples.
    pub fn new(kib: u32, diagnostics: bool) -> Result<Arc<Self>, norn_store::StoreError> {
        Ok(Arc::new(Self {
            size: norn_store::startup_experiment::WriterCacheSize::new(kib)?,
            diagnostics,
            started: AtomicBool::new(false),
            receipt: std::sync::Mutex::new(None),
        }))
    }

    /// Read the harness-only receipt after successful restoration of this worker's
    /// one window. None means it was skipped or restoration has not succeeded.
    pub fn receipt(&self) -> Option<InitialImportCacheReceipt> {
        *self
            .receipt
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Own the unpublished store so restoration failure closes or replaces its writer.
/// This runs only in attach, never in warm heals, requests or recovery.
pub(super) fn initial_heal(
    mut attachment: ProductionAttachment,
    cache: Option<&InitialImportCache>,
    heal: impl FnOnce(&mut ProductionAttachment) -> Result<(), JobFailure>,
) -> Result<(ProductionAttachment, Result<(), JobFailure>), JobFailure> {
    let Some(cache) = cache.filter(|cache| {
        !matches!(
            attachment.store.open_outcome(),
            norn_store::OpenOutcome::Reused
        ) && !cache.started.swap(true, Ordering::SeqCst)
    }) else {
        let result = heal(&mut attachment);
        return Ok((attachment, result));
    };
    let order = attachment.store.path_order();
    let empty = match attachment
        .store
        .begin_request()
        .stored_documents_after_ordered(None, 1, order)
    {
        Ok(documents) => documents.is_empty(),
        Err(error) => return Ok((attachment, Err(store_effect(error)))),
    };
    if !empty {
        let result = heal(&mut attachment);
        return Ok((attachment, result));
    }
    let window = match attachment
        .store
        .begin_request()
        .start_initial_import_cache(cache.size, cache.diagnostics)
    {
        Ok(window) => window,
        // Start may have changed the budget before its diagnostic and restore
        // both failed. Only attach's error/rebuild arms may own this writer now.
        Err(error) => return Ok((attachment, Err(store_effect(error)))),
    };
    let mut guarded = InitialCacheGuard {
        attachment: Some(attachment),
        window: Some(window),
        cache,
    };
    let result = heal(guarded.attachment.as_mut().expect("unpublished attachment"));
    guarded.finish(result)
}

struct InitialCacheGuard<'a> {
    attachment: Option<ProductionAttachment>,
    window: Option<norn_store::startup_experiment::WriterCacheWindow>,
    cache: &'a InitialImportCache,
}

impl InitialCacheGuard<'_> {
    fn finish(
        mut self,
        result: Result<(), JobFailure>,
    ) -> Result<(ProductionAttachment, Result<(), JobFailure>), JobFailure> {
        let restored = self.restore(result.is_ok()).map_err(store_effect);
        let result = match (result, restored) {
            // Damage must reach attach's rung-three decision even when restore
            // fails differently: that rung replaces this writer before Ready.
            (Err(failure @ JobFailure::StoreDamaged(_)), _)
            | (_, Err(failure @ JobFailure::StoreDamaged(_))) => Err(failure),
            (_, Err(failure)) => return Err(failure),
            (result, Ok(())) => result,
        };
        // Either restore succeeded or rung three owns replacement. Disarm before
        // transferring the attachment so Drop can never restore a missing writer.
        self.window = None;
        Ok((self.attachment.take().expect("owned attachment"), result))
    }

    fn restore(&mut self, import_succeeded: bool) -> Result<(), norn_store::StoreError> {
        if let Some(window) = &self.window {
            let writer = self
                .attachment
                .as_mut()
                .expect("unpublished attachment")
                .store
                .begin_request()
                .restore_initial_import_cache(window)?;
            *self
                .cache
                .receipt
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                Some(InitialImportCacheReceipt {
                    writer,
                    import_succeeded,
                });
            self.window = None;
        }
        Ok(())
    }
}

impl Drop for InitialCacheGuard<'_> {
    fn drop(&mut self) {
        // Unwinding cannot publish this owned attachment. A failed restoration
        // still closes the writer when the attachment below drops.
        let _ = self.restore(false);
    }
}

mod canonical;

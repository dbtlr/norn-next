//! Fresh-store construction and bounded checkpoints. Canonical derivation stays ordinary.
use super::*;
use crate::lifecycle::loading::{Diagnostics, Operation};
use norn_fs::walk::inventory::InventoryWalk;
use norn_fs::watch::FirstIndexObservation;
use norn_store::loading::{
    Catalogue, GetBuilder, IntakeBuilder, TemporaryCleanupReceipt, TemporaryCleanupStatus,
};
#[cfg(feature = "induced-failure")]
use std::os::unix::ffi::OsStrExt;

pub(super) mod closure;
pub(super) mod facts;

#[cfg(feature = "induced-failure")]
#[derive(Clone, Copy, Debug)]
pub enum LoadingBoundary<'a> {
    AuthorityCaptured,
    BeforeCatalogue,
    CatalogueRecord(&'a Path),
    CatalogueComplete,
    GetLoaded(&'a str),
    GetPublished(&'a str),
    MetadataRecord(&'a Path),
    MetadataComplete,
    CanonicalPage,
    CanonicalComplete,
    QueryEstablished,
    QueryBuilt,
    SourceCreated { role: &'a str, paths: &'a [PathBuf] },
}

/// Deterministic construction timing only; hands out no query capability.
#[cfg(feature = "induced-failure")]
pub trait LoadingProbe: Send + Sync {
    fn at(&self, boundary: LoadingBoundary<'_>);
}

/// Whether a from-zero build may publish early reads from the store it fills.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum EarlyReads {
    /// The attach leg that created the store: [`Construction`] begins when its
    /// own proof holds (created, empty, first-index authority).
    FirstAttach,
    /// Rung 3, recovery and reload: nothing is published early.
    Declined,
}

pub(super) struct CleanupRecord {
    role: String,
    receipt: TemporaryCleanupReceipt,
}

impl ProductionEntryOps {
    fn observe_loading(&self, role: &str, receipt: &TemporaryCleanupReceipt) {
        self.loading_cleanup
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(CleanupRecord {
                role: role.into(),
                receipt: receipt.clone(),
            });
        #[cfg(feature = "induced-failure")]
        self.loading_boundary(LoadingBoundary::SourceCreated {
            role,
            paths: receipt.paths(),
        });
    }

    pub(super) fn collect_loading_cleanup(&self) {
        let mut records = self
            .loading_cleanup
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        records.retain(|record| match record.receipt.status() {
            TemporaryCleanupStatus::Pending => true,
            TemporaryCleanupStatus::Removed => false,
            TemporaryCleanupStatus::Failed(error) => {
                let detail = format!(
                    "{} source {:?}: {error}",
                    record.role,
                    record.receipt.paths()
                );
                self.record_cleanup_failure(detail);
                false
            }
        });
    }

    #[cfg(feature = "induced-failure")]
    pub(super) fn loading_boundary(&self, boundary: LoadingBoundary<'_>) {
        if let Some(probe) = &self.loading_probe {
            probe.at(boundary);
        }
    }

    #[cfg(feature = "induced-failure")]
    pub fn with_loading_probe(mut self, probe: Arc<dyn LoadingProbe>) -> Self {
        self.loading_probe = Some(probe);
        self
    }

    /// Projections fail independently of the canonical store. Keep every physical cause.
    fn loading_failed(&self, role: &str, error: &JobFailure) {
        self.record_cleanup_failure(format!("{role} construction: {error:?}"));
    }

    /// The one construction path from empty to complete: every derivation into a
    /// store under construction runs here, and the mark clears when it ends.
    ///
    /// The routes are an attach after a create or an open-time rebuild, the
    /// inline rung 3 and the `rebuild` leg, and the recovery or reload that
    /// pins a readable declaration over a store an undeclarable attach left
    /// empty. The ordinary heal maintains only a finished store.
    ///
    /// **The mark clears only when the derivation has completed**, after the
    /// watcher coverage window has closed cleanly, so a failure of any kind —
    /// damage, the environment, lost maintainership, a watcher failure, a full
    /// disk — leaves it for the next open to rebuild over. This is its own
    /// transaction until the five steps give the build a final transaction to
    /// carry it.
    ///
    /// `admission` is who may publish early reads from the store being built.
    /// Only the attach after a create does, as [`Construction`] always has;
    /// rung 3, recovery and reload build with [`EarlyReads::Declined`] even
    /// though a rebuilt store is created and empty, because the claim that
    /// makes publication safe belongs to the attach leg. Early reads for every
    /// from-zero build are the later layer's.
    pub(super) fn build_from_zero(
        &self,
        attachment: &mut ProductionAttachment,
        progress: &ProgressReporter<ProductionAttachment>,
        admission: EarlyReads,
    ) -> Result<(), JobFailure> {
        debug_assert!(
            attachment.store.under_construction(),
            "a from-zero build ran over a finished store"
        );
        self.evidence.count_from_zero_build();
        Self::under_coverage(attachment, |attachment| {
            self.derive_into_empty(attachment, progress, admission)
        })?;
        attachment.store.finish_construction().map_err(store_effect)
    }

    /// Derive the vault into the store the caller holds: a build from zero if
    /// the store is under construction, the ordinary heal if it is finished.
    pub(super) fn derive_vault(
        &self,
        attachment: &mut ProductionAttachment,
        progress: &ProgressReporter<ProductionAttachment>,
    ) -> Result<(), JobFailure> {
        if attachment.store.under_construction() {
            self.build_from_zero(attachment, progress, EarlyReads::Declined)
        } else {
            self.heal_under_coverage(attachment, progress)
        }
    }

    fn derive_into_empty(
        &self,
        attachment: &mut ProductionAttachment,
        progress: &ProgressReporter<ProductionAttachment>,
        admission: EarlyReads,
    ) -> Result<(), JobFailure> {
        if !attachment
            .maintainership
            .still_current()
            .map_err(data_dir_effect)?
        {
            return Err(JobFailure::LostMaintainership);
        }
        let healing = progress.healing();
        let order = attachment.store.path_order();
        // These reads belong to the canonical store. Their damage must reach
        // the ordinary rung instead of being filed as disposable construction.
        let fresh = admission == EarlyReads::FirstAttach
            && matches!(
                attachment.store.open_outcome(),
                norn_store::OpenOutcome::Created
            )
            && HealScope::Vault
                .page(&mut attachment.store, None, self.policy, order)?
                .is_empty();
        let pin = if fresh {
            attachment
                .store
                .begin_request()
                .vault_schema_pin()
                .map_err(store_effect)?
        } else {
            None
        };
        let mut loading = match admission {
            EarlyReads::Declined => {
                progress.decline_loading();
                None
            }
            EarlyReads::FirstAttach => {
                match Construction::begin(self, attachment, progress, fresh, pin) {
                    Ok(loading) => loading,
                    Err(error) => {
                        self.loading_failed("catalogue", &error);
                        progress.loading_failed(
                            ReaderUnavailable::new("first-index catalogue construction failed"),
                            false,
                        );
                        None
                    }
                }
            }
        };
        if let Some(construction) = loading.as_mut() {
            construction.checkpoint();
            if let Err(error) = construction.metadata() {
                self.loading_failed("metadata", &error);
                progress.loading_failed(
                    ReaderUnavailable::new("first-index metadata construction failed"),
                    true,
                );
            }
        }
        if !attachment
            .maintainership
            .still_current()
            .map_err(data_dir_effect)?
        {
            return Err(JobFailure::LostMaintainership);
        }
        let exclusions = exclusions_at(
            &attachment.registration,
            &attachment.shadows,
            &attachment.covered_root,
        );
        attachment.skipped_links = heal_documents_with_checkpoint(
            &mut attachment.store,
            &attachment.covered_root,
            &exclusions,
            self.policy,
            &healing,
            &mut || {
                if let Some(construction) = loading.as_mut() {
                    construction.checkpoint();
                }
                #[cfg(feature = "induced-failure")]
                self.loading_boundary(LoadingBoundary::CanonicalPage);
            },
        )?;
        #[cfg(feature = "induced-failure")]
        self.loading_boundary(LoadingBoundary::CanonicalComplete);
        // Publications live in the entry, independently of this construction owner.
        drop(loading);
        self.collect_loading_cleanup();
        Ok(())
    }
}

struct Construction<'a> {
    ops: &'a ProductionEntryOps,
    progress: &'a ProgressReporter<ProductionAttachment>,
    authority: FirstIndexObservation,
    source: norn_fs::Vault,
    catalogue: Catalogue,
    root: PathBuf,
    exclusions: Vec<PathBuf>,
    directory: PathBuf,
    order: StoredPathOrder,
    group: norn_store::ReadGroup,
    pin: Option<norn_store::VaultSchemaPin>,
    model: Arc<ContentModel>,
}

impl<'a> Construction<'a> {
    fn begin(
        ops: &'a ProductionEntryOps,
        attachment: &mut ProductionAttachment,
        progress: &'a ProgressReporter<ProductionAttachment>,
        fresh: bool,
        pin: Option<norn_store::VaultSchemaPin>,
    ) -> Result<Option<Self>, JobFailure> {
        // A rebuild, recovery or reused database can never earn this proof.
        if !fresh {
            progress.decline_loading();
            return Ok(None);
        }
        let order = attachment.store.path_order();
        let Some(authority) = attachment
            .subscription
            .as_ref()
            .and_then(Subscription::first_index_observation)
        else {
            progress.decline_loading();
            return Ok(None);
        };
        #[cfg(feature = "induced-failure")]
        ops.loading_boundary(LoadingBoundary::AuthorityCaptured);
        if !authority.check_root() || !progress.begin_loading(authority.clone()) {
            progress.decline_loading();
            return Ok(None);
        }
        #[cfg(feature = "induced-failure")]
        ops.loading_boundary(LoadingBoundary::BeforeCatalogue);
        let root = attachment.covered_root.clone();
        let exclusions = exclusions_at(&attachment.registration, &attachment.shadows, &root);
        let source = norn_fs::Vault::open(&root, &exclusions).map_err(effect)?;
        let directory = Store::loading_directory(attachment.store.path());
        let model = Arc::clone(attachment.pinned.shared_content_model());
        let mut catalogue = Catalogue::open_observed(&directory, |receipt| {
            ops.observe_loading("catalogue", receipt)
        })
        .map_err(store_effect)?;
        let mut inventory = InventoryWalk::from_vault(&source).map_err(effect)?;
        loop {
            if !authority.admits() {
                progress.decline_loading();
                return Ok(None);
            }
            let page = inventory
                .advance(ops.policy.store_page_size)
                .map_err(effect)?;
            for record in page.records {
                #[cfg(feature = "induced-failure")]
                ops.loading_boundary(LoadingBoundary::CatalogueRecord(Path::new(
                    std::ffi::OsStr::from_bytes(&record.value),
                )));
                let fact = inventory.replay(&record.value).map_err(effect)?;
                if let norn_fs::WalkFact::Skipped(skip) = &fact
                    && !skip.reason().stands()
                {
                    authority.invalidate();
                }
                if let norn_fs::WalkFact::File(file) = fact
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
        #[cfg(feature = "induced-failure")]
        ops.loading_boundary(LoadingBoundary::CatalogueComplete);
        Ok(Some(Self {
            ops,
            progress,
            authority,
            source,
            catalogue,
            root,
            exclusions,
            directory,
            order,
            group: attachment.store.read_group(),
            pin,
            model,
        }))
    }

    fn pin(&self, store: &mut Store) -> Result<(), JobFailure> {
        if let Some(pin) = &self.pin {
            store
                .begin_request()
                .pin_vault_schema(&pin.bytes, &pin.fingerprint)
                .map_err(store_effect)?;
        }
        Ok(())
    }

    /// At most one preparation is spent at each metadata or canonical page checkpoint.
    fn checkpoint(&mut self) {
        if !self.authority.admits() {
            self.progress.decline_loading();
            return;
        }
        let Some(target) = self.progress.next_loading_target() else {
            return;
        };
        let result = self.get(&target);
        self.progress
            .loading_target_finished(result.as_ref().err().map(|_| {
                ReaderUnavailable::new(
                    "the requested first-index Get source could not be constructed",
                )
            }));
        if let Err(error) = result {
            self.ops.loading_failed(&format!("Get {target}"), &error);
        }
    }

    fn get(&self, target: &str) -> Result<(), JobFailure> {
        let mut builder = GetBuilder::create_in_observed(
            &self.directory,
            self.order,
            crate::DERIVATION_VERSION,
            |receipt| self.ops.observe_loading(&format!("Get {target}"), receipt),
        )
        .map_err(store_effect)?;
        let prepared = builder.write(|store| {
            self.pin(store)?;
            closure::prepare(
                store,
                &self.catalogue,
                &self.source,
                &self.root,
                &self.exclusions,
                self.ops.policy,
                target,
                |_name| {
                    #[cfg(feature = "induced-failure")]
                    self.ops.loading_boundary(LoadingBoundary::GetLoaded(_name));
                    Ok(())
                },
            )
        })?;
        if prepared == closure::Preparation::SourceChanged {
            self.authority.invalidate();
        }
        if prepared != closure::Preparation::Ready || !self.authority.check_root() {
            return Ok(());
        }
        let physical = builder.cleanup_receipt().paths()[0].clone();
        let reader = builder
            .finish_in_group(&self.ops.readers, &self.group)
            .map_err(store_effect)?;
        self.progress.publish_loading(
            reader,
            Arc::clone(&self.model),
            self.authority.clone(),
            Operation::Get(target.into()),
            Diagnostics {
                description: format!("Get {target} source {}", physical.display()),
                failures: Arc::clone(&self.ops.cleanup_error),
            },
        );
        #[cfg(feature = "induced-failure")]
        self.ops
            .loading_boundary(LoadingBoundary::GetPublished(target));
        Ok(())
    }

    fn metadata(&mut self) -> Result<(), JobFailure> {
        if !self.authority.admits() {
            return Ok(());
        }
        let mut builder = IntakeBuilder::create_in_observed(
            &self.directory,
            self.order,
            crate::DERIVATION_VERSION,
            |receipt| self.ops.observe_loading("metadata", receipt),
        )
        .map_err(store_effect)?;
        builder.write(|store| self.pin(store))?;
        let mut inventory = InventoryWalk::from_vault(&self.source).map_err(effect)?;
        loop {
            if !self.authority.admits() {
                return Ok(());
            }
            let page = inventory
                .advance(self.ops.policy.store_page_size)
                .map_err(effect)?;
            let complete = builder.write(|store| {
                let mut account = Account::default();
                let mut pending = Pending::new(
                    store,
                    self.ops.policy.changeset_size,
                    &self.root,
                    &self.exclusions,
                    &mut account,
                )?;
                pending.metadata = true;
                for record in page.records {
                    facts::load_fact(
                        &mut pending,
                        inventory.replay(&record.value).map_err(effect)?,
                        facts::Depth::Metadata,
                        false,
                    )?;
                    #[cfg(feature = "induced-failure")]
                    self.ops
                        .loading_boundary(LoadingBoundary::MetadataRecord(Path::new(
                            std::ffi::OsStr::from_bytes(&record.value),
                        )));
                    if pending.is_full() {
                        pending.flush()?;
                    }
                }
                pending.flush()?;
                drop(pending);
                Ok::<_, JobFailure>(account.withheld.roots.is_empty() && !account.withheld.rendered)
            })?;
            if !complete {
                self.authority.invalidate();
                return Ok(());
            }
            self.checkpoint();
            if page.complete {
                break;
            }
        }
        if !self.authority.check_root() {
            return Ok(());
        }
        let physical = builder.cleanup_receipt().paths()[0].clone();
        let reader = builder
            .finish_in_group(&self.ops.readers, &self.group)
            .map_err(store_effect)?;
        self.progress.publish_loading(
            reader,
            Arc::clone(&self.model),
            self.authority.clone(),
            Operation::Find,
            Diagnostics {
                description: format!("metadata source {}", physical.display()),
                failures: Arc::clone(&self.ops.cleanup_error),
            },
        );
        #[cfg(feature = "induced-failure")]
        self.ops.loading_boundary(LoadingBoundary::MetadataComplete);
        Ok(())
    }
}

#[cfg(feature = "induced-failure")]
impl crate::Host<ProductionEntryOps> {
    /// Actual budget resources, including retired sources still held by a read.
    pub fn read_budget_census(&self) -> norn_store::ReadBudgetCensus {
        self.ops().read_budget_census()
    }

    /// Collect final loading cleanup outcomes without retaining their owners.
    pub fn last_reader_cleanup_error(&self) -> Option<String> {
        self.ops().last_reader_cleanup_error()
    }
}

//! From-zero names, content and structure, followed by one link-health judgment and full text.
use super::*;
use crate::lifecycle::loading::{Begun, BuildToken};
use norn_fs::watch::BuildAuthority;
use norn_store::Step;

pub(super) mod steps;

#[cfg(feature = "induced-failure")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoadingBoundary {
    /// The build captured its watcher authority; its construction has not begun.
    AuthorityCaptured,
    CanonicalPage,
    StepComplete(norn_store::Step),
    /// The build published the end of this step to its early reads.
    Published(norn_store::Step),
    /// The build published requested Get targets ready, after the commit
    /// holding their documents.
    TargetReady,
    /// The build drained the requests pending at a chunk boundary.
    RequestsDrained,
    LinkHealthChunk,
    FullTextBatch,
    CanonicalComplete,
    Reconciling,
    /// A requeue call that could take, before it takes: one per call, whether
    /// or not anything is pending.
    RequeueCalled,
    RequeueStarted,
    Requeued,
    QueryEstablished,
    QueryBuilt,
}

/// Deterministic construction timing only; hands out no query capability.
#[cfg(feature = "induced-failure")]
pub trait LoadingProbe: Send + Sync {
    fn at(&self, boundary: LoadingBoundary);
}

/// The construction a from-zero build publishes its steps into: the claim's
/// reporter, and the build and store the entry bound when the build began.
/// A build that could not capture watcher authority publishes nothing.
pub(super) struct Publisher<'a> {
    ops: &'a ProductionEntryOps,
    progress: &'a ProgressReporter<ProductionAttachment>,
    begun: Option<(BuildToken, String)>,
    authority: Option<BuildAuthority>,
}

impl Publisher<'_> {
    /// Publish that `step` has ended, admitting the reads its rows complete.
    pub(super) fn publish(&self, step: Step) {
        let Some((token, epoch)) = &self.begun else {
            return;
        };
        if self.progress.publish_step(*token, epoch, step) {
            self.ops.evidence.count_construction_publication();
            #[cfg(feature = "induced-failure")]
            self.ops.loading_boundary(LoadingBoundary::Published(step));
        }
    }

    /// The targets live reads wait on that this build has not yet published
    /// ready, for its next drain.
    pub(super) fn pending_targets(&self) -> Vec<String> {
        self.begun
            .as_ref()
            .map(|(token, _)| self.progress.pending_targets(*token))
            .unwrap_or_default()
    }

    /// Publish `targets` ready: the group writing their documents has committed.
    pub(super) fn publish_targets(&self, targets: &[String]) {
        let Some((token, epoch)) = &self.begun else {
            return;
        };
        if !targets.is_empty() && self.progress.publish_targets(*token, epoch, targets) {
            self.ops.evidence.count_construction_publication();
            #[cfg(feature = "induced-failure")]
            self.ops.loading_boundary(LoadingBoundary::TargetReady);
        }
    }

    /// Publish a committed requeue and invalidate ready candidate resolutions.
    pub(super) fn publish_requeued(&self, taken: &norn_fs::watch::TakenRoots) {
        if let Some((token, epoch)) = &self.begun {
            self.progress
                .publish_requeued(*token, epoch, &taken.roots, taken.through);
        }
    }

    /// Close this build's construction: it failed, and nothing it wrote is admitted again.
    pub(super) fn close(&self) {
        if let Some((token, _)) = &self.begun {
            self.progress.close_construction(*token);
        }
    }
}

impl ProductionEntryOps {
    #[cfg(feature = "induced-failure")]
    pub(super) fn loading_boundary(&self, boundary: LoadingBoundary) {
        if let Some(probe) = &self.loading_probe {
            probe.at(boundary);
        }
    }

    #[cfg(feature = "induced-failure")]
    pub fn with_loading_probe(mut self, probe: Arc<dyn LoadingProbe>) -> Self {
        self.loading_probe = Some(probe);
        self
    }

    /// The one construction path from empty to complete: every derivation into a
    /// store under construction runs here, and the mark clears when it ends.
    ///
    /// The routes are an attach after a create or an open-time rebuild, the
    /// inline rung 3 and the `rebuild` leg, and the recovery or reload that
    /// pins a readable declaration over a store an undeclarable attach left
    /// empty. The ordinary heal maintains only a finished store. Every route
    /// publishes its steps to early reads ([`Self::construct_from_zero`]).
    ///
    /// **The mark clears after derivation, link-health judgment and full text**, after the
    /// watcher coverage window has closed cleanly, so a failure of any kind —
    /// damage, the environment, lost maintainership, a watcher failure, a full
    /// disk — leaves it for the next open to rebuild over. Rung 3 alone clears
    /// it later still, after its integrity verification
    /// ([`Self::construct_from_zero`]). The closing transaction installs the
    /// full-text triggers, checks the schema digest and clears the mark. The
    /// construction checkpoint follows, before the ordinary reconcile.
    pub(super) fn build_from_zero(
        &self,
        attachment: &mut ProductionAttachment,
        progress: &ProgressReporter<ProductionAttachment>,
    ) -> Result<(), JobFailure> {
        let built = self
            .construct_from_zero(attachment, progress)
            .and_then(|publisher| self.finish_published(&mut attachment.store, &publisher));
        // A failed build's store may stay parked for the recovery it owes,
        // which discards it rather than building over it ([`Self::derive_vault`]).
        if built.is_err()
            && let Err(error) = attachment.store.restore_construction_settings()
        {
            self.record_cleanup_failure(format!("construction settings: {error}"));
        }
        built
    }

    /// Run the closing transaction, then publish that full text is complete.
    /// A closing transaction that fails closes the build's construction.
    pub(super) fn finish_published(
        &self,
        store: &mut Store,
        publisher: &Publisher<'_>,
    ) -> Result<(), JobFailure> {
        let finished = self.finish_construction(store);
        if finished.is_ok() {
            publisher.publish(Step::FullTextComplete);
        } else {
            publisher.close();
        }
        finished
    }

    pub(super) fn finish_construction(&self, store: &mut Store) -> Result<(), JobFailure> {
        let finished = store.finish_construction().map_err(store_effect)?;
        if finished.checkpoint_busy {
            self.evidence.count_construction_checkpoint_busy();
        }
        Ok(())
    }

    /// Count a from-zero build and derive the vault into the store under
    /// construction, leaving the under-construction mark standing and FTS triggers absent.
    ///
    /// The one construction path: [`Self::build_from_zero`] clears the mark
    /// after it, and a caller that owes the result a verification first
    /// (rung 3) clears it only once that passes, so a store that fails the
    /// verification is rebuilt again by the next open rather than adopted. Stored
    /// bodies are fully indexed before returning, so that verification covers full text.
    ///
    /// **Every from-zero build begins its own construction** once its steps
    /// have begun and before its walk: a fresh watcher authority, the store's
    /// zero-I/O reader pool and its pinned model, bound under the claim. The
    /// returned [`Publisher`] publishes names, content and structure as each
    /// ends, relations after the one link-health judgment, and full text
    /// after the closing transaction the caller runs. A build that fails
    /// closes its construction before returning.
    pub(super) fn construct_from_zero<'a>(
        &'a self,
        attachment: &mut ProductionAttachment,
        progress: &'a ProgressReporter<ProductionAttachment>,
    ) -> Result<Publisher<'a>, JobFailure> {
        debug_assert!(
            attachment.store.under_construction(),
            "a from-zero build ran over a finished store"
        );
        attachment.store.defer_link_health().map_err(store_effect)?;
        attachment.store.begin_steps().map_err(store_effect)?;
        self.evidence.count_from_zero_build();
        let mut publisher = None;
        let built = Self::under_coverage(attachment, |attachment| {
            let begun = self.begin_construction(attachment, progress);
            let result = self.derive_into_empty(attachment, progress, &begun);
            publisher = Some(begun);
            result
        });
        let publisher = publisher.ok_or_else(|| {
            built
                .clone()
                .err()
                .unwrap_or_else(|| environmental("construction did not begin under coverage"))
        })?;
        if built.is_err() {
            publisher.close();
        }
        built.map(|()| publisher)
    }

    /// Capture this build's watcher authority and begin its construction.
    /// The root check runs here, outside every lock.
    fn begin_construction<'a>(
        &'a self,
        attachment: &ProductionAttachment,
        progress: &'a ProgressReporter<ProductionAttachment>,
    ) -> Publisher<'a> {
        let schema = Self::schema_path_at(&attachment.registration, &attachment.covered_root);
        let control: Vec<_> = [
            Some(Path::new(norn_config::IN_VAULT_CONFIG_PATH)),
            schema.strip_prefix(&attachment.covered_root).ok(),
        ]
        .into_iter()
        .flatten()
        .filter_map(|path| {
            attachment
                .subscription
                .as_ref()?
                .normalize_build_control(path)
                .ok()
        })
        .collect();
        let authority = attachment
            .subscription
            .as_ref()
            .and_then(|subscription| subscription.capture_build_authority(&control));
        #[cfg(feature = "induced-failure")]
        self.loading_boundary(LoadingBoundary::AuthorityCaptured);
        if let Some(authority) = &authority {
            authority.observe(self.evidence.clone());
        }
        let store_epoch = attachment.store.epoch().to_owned();
        let publication_authority = authority.clone();
        let begun = authority
            .filter(BuildAuthority::check_root)
            .map(|authority| Begun {
                evidence: self.evidence.clone(),
                pool: attachment.store.reader_pool(&self.readers),
                model: Arc::clone(attachment.pinned.shared_content_model()),
                authority,
                store_epoch: store_epoch.clone(),
            });
        let token = progress.begin_construction(begun);
        Publisher {
            ops: self,
            progress,
            begun: token.map(|token| (token, store_epoch)),
            authority: publication_authority,
        }
    }

    /// Derive the vault into the store the caller holds: a build from zero if
    /// the store is under construction, the ordinary heal if it is finished.
    ///
    /// A store whose from-zero build began and failed holds that build's
    /// partial rows and full text, and the attachment kept it parked for this
    /// recovery or reload. No build runs over it: it owes rung 3, which
    /// discards it and builds from an empty store under the controls the leg
    /// has just taken.
    pub(super) fn derive_vault(
        &self,
        attachment: &mut ProductionAttachment,
        progress: &ProgressReporter<ProductionAttachment>,
    ) -> Result<(), JobFailure> {
        if attachment.store.construction_began() {
            Err(JobFailure::StoreDamaged(
                "a from-zero build that failed left a partial store, which is rebuilt from empty"
                    .into(),
            ))
        } else if attachment.store.under_construction() {
            self.build_from_zero(attachment, progress)
        } else {
            self.heal_under_coverage(attachment, progress)
        }
    }

    fn derive_into_empty(
        &self,
        attachment: &mut ProductionAttachment,
        progress: &ProgressReporter<ProductionAttachment>,
        publisher: &Publisher<'_>,
    ) -> Result<(), JobFailure> {
        if !attachment
            .maintainership
            .still_current()
            .map_err(data_dir_effect)?
        {
            return Err(JobFailure::LostMaintainership);
        }
        let healing = progress.healing();
        let exclusions = exclusions_at(
            &attachment.registration,
            &attachment.shadows,
            &attachment.covered_root,
        );
        attachment.skipped_links = steps::documents(
            self,
            &mut attachment.store,
            &attachment.covered_root,
            &exclusions,
            &healing,
            publisher,
            attachment.subscription.as_ref(),
        )?;
        let declared = Declaration::read(&mut attachment.store)?;
        let mut request = attachment.store.begin_request();
        let result =
            request.judge_link_health_deferred(declared.model.content_model(), &mut || {
                #[cfg(feature = "induced-failure")]
                self.loading_boundary(LoadingBoundary::LinkHealthChunk);
            });
        let steps = request.read_steps();
        crate::evidence::count_increment_work(&request.finish(), steps);
        result.map_err(store_effect)?;
        // The document steps' job ends have run, so nothing prunes against this
        // account; only the links the requeue skipped are carried on.
        let mut relations = Account::default();
        steps::requeue(
            self,
            &mut attachment.store,
            &attachment.covered_root,
            &exclusions,
            &healing,
            publisher,
            attachment.subscription.as_ref(),
            false,
            &mut relations,
        )?;
        let order = attachment.store.path_order();
        attachment.skipped_links.merge(relations.links, order);
        attachment.skipped_links.settle();
        // A requeue that took a link out of a list the bound had cut may have
        // left out a link the bound dropped, which only a walk names again.
        if !attachment.skipped_links.exact() {
            self.evidence.count_link_rewalk();
            attachment.skipped_links =
                walk_skipped_links(&attachment.covered_root, &exclusions, order)?;
        }
        attachment.store.begin_request().end_link_health_deferral();
        publisher.publish(Step::Relations);
        loop {
            let mut request = attachment.store.begin_request();
            let result = request.full_text_batch(self.policy.changeset_size);
            let steps = request.read_steps();
            crate::evidence::count_increment_work(&request.finish(), steps);
            let complete = result.map_err(store_effect)?;
            #[cfg(feature = "induced-failure")]
            self.loading_boundary(LoadingBoundary::FullTextBatch);
            if complete {
                break;
            }
        }
        #[cfg(feature = "induced-failure")]
        self.loading_boundary(LoadingBoundary::CanonicalComplete);
        Ok(())
    }
}

#[cfg(feature = "induced-failure")]
impl crate::Host<ProductionEntryOps> {
    /// Actual budget resources, including retired pools still held by a read.
    pub fn read_budget_census(&self) -> norn_store::ReadBudgetCensus {
        self.ops().read_budget_census()
    }

    /// The bounded history of cleanup failures this host recorded.
    pub fn last_reader_cleanup_error(&self) -> Option<String> {
        self.ops().last_reader_cleanup_error()
    }
}

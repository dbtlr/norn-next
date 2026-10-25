//! Request-specific admission during the first attach. No unrestricted early hold escapes.
use super::*;
use norn_fs::watch::FirstIndexObservation;
use norn_wire::{FindParams, GetParams};
use std::sync::Weak;

pub(crate) const TARGET_LIMIT: usize = 8;

#[derive(Clone, Copy)]
pub(crate) enum Request<'a> {
    Canonical,
    Get(&'a GetParams),
    Find(&'a FindParams),
}

impl Request<'_> {
    pub(super) fn eligible(self) -> bool {
        match self {
            Self::Canonical => false,
            Self::Get(params) => whole_get(params),
            Self::Find(params) => norn_store::loading::supports_find(params),
        }
    }
}

fn whole_get(params: &GetParams) -> bool {
    params.target.anchor().is_none()
        && params.columns.is_empty()
        && params.collection.is_none()
        && params.limit.is_none()
        && params.after.is_none()
}

pub(crate) enum Operation {
    Get(String),
    Find,
}

/// Cleanup and disposable-source causes share a bounded history across the host lifetime.
#[derive(Default)]
pub(crate) struct FailureHistory {
    recent: std::collections::VecDeque<String>,
    dropped: u64,
}

impl FailureHistory {
    pub(crate) fn record(&mut self, detail: String) {
        if self.recent.len() == 8 {
            self.recent.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.recent.push_back(detail);
    }

    pub(crate) fn summary(&self) -> Option<String> {
        if self.recent.is_empty() {
            return None;
        }
        let recent = self.recent.iter().cloned().collect::<Vec<_>>().join("; ");
        Some(if self.dropped == 0 {
            recent
        } else {
            format!("{} older failures dropped; {recent}", self.dropped)
        })
    }
}

pub(crate) struct Diagnostics {
    pub(crate) description: String,
    pub(crate) failures: Arc<Mutex<FailureHistory>>,
}

pub(super) struct Source<R: ReadSource> {
    pub(super) reader: Arc<R>,
    pub(super) model: Arc<ContentModel>,
    pub(super) authority: FirstIndexObservation,
    pub(super) operation: Operation,
    pub(super) claim: u64,
    damaged: AtomicBool,
    diagnostics: Diagnostics,
    reported: AtomicBool,
    waker: Option<std::task::Waker>,
}

impl<R: ReadSource> Source<R> {
    fn new(
        reader: R,
        model: Arc<ContentModel>,
        authority: FirstIndexObservation,
        operation: Operation,
        claim: u64,
        diagnostics: Diagnostics,
        waker: Option<std::task::Waker>,
    ) -> Self {
        Self {
            reader: Arc::new(reader),
            model,
            authority,
            operation,
            claim,
            damaged: AtomicBool::new(false),
            reported: AtomicBool::new(false),
            diagnostics,
            waker,
        }
    }

    pub(super) fn accepts(&self, request: Request<'_>) -> bool {
        match (&self.operation, request) {
            (Operation::Get(target), Request::Get(params)) => {
                whole_get(params) && params.target.address() == target
            }
            (Operation::Find, Request::Find(params)) => norn_store::loading::supports_find(params),
            _ => false,
        }
    }

    pub(super) fn admits(&self) -> bool {
        !self.damaged.load(Ordering::Acquire) && self.authority.admits()
    }

    fn held_is_current(&self) -> bool {
        !self.damaged.load(Ordering::Acquire) && self.authority.held_is_current()
    }

    pub(super) fn damage(&self) {
        self.damaged.store(true, Ordering::Release);
        self.reader.retire();
        if let Some(waker) = &self.waker {
            waker.wake_by_ref();
        }
    }

    fn record_failure(&self, detail: &str) {
        if !self.reported.swap(true, Ordering::AcqRel) {
            let mut failures = self
                .diagnostics
                .failures
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let detail = format!("{}: {detail}", self.diagnostics.description);
            failures.record(detail);
        }
    }

    pub(super) fn unavailable(&self, error: ReaderUnavailable) -> ReaderUnavailable {
        self.record_failure(error.detail());
        if error.damaged || error.source_lost {
            self.damage();
        }
        error
    }
}

struct Interest {
    target: String,
    lease: Weak<()>,
}

/// Bounded by spent preparations plus live queue reservations. Spent work never renews.
pub(super) struct Loading<R: ReadSource> {
    pub(super) claim: u64,
    authority: Option<FirstIndexObservation>,
    queue: Vec<Interest>,
    spent: Vec<String>,
    building: Option<String>,
    failures: Vec<(String, ReaderUnavailable)>,
    metadata_failure: Option<ReaderUnavailable>,
    failure: Option<ReaderUnavailable>,
    pub(super) sources: Vec<Arc<Source<R>>>,
    pub(super) revision: u64,
}

impl<R: ReadSource> Loading<R> {
    pub(super) fn provisional(claim: u64) -> Self {
        Self {
            claim,
            authority: None,
            queue: Vec::new(),
            spent: Vec::new(),
            building: None,
            failures: Vec::new(),
            metadata_failure: None,
            failure: None,
            sources: Vec::new(),
            revision: 0,
        }
    }

    pub(super) fn withdrawn(&self) -> bool {
        self.authority
            .as_ref()
            .is_some_and(|authority| !authority.admits())
    }

    pub(super) fn interested(&mut self, request: Request<'_>) -> Option<Arc<()>> {
        let Request::Get(params) = request else {
            return None;
        };
        if !whole_get(params) {
            return None;
        }
        let target = params.target.address();
        self.queue
            .retain(|interest| interest.lease.strong_count() > 0);
        if self.spent.iter().any(|spent| spent == target) {
            return None;
        }
        if let Some(interest) = self.queue.iter().find(|interest| interest.target == target) {
            return interest.lease.upgrade();
        }
        if self.queue.len() + self.spent.len() >= TARGET_LIMIT {
            return None;
        }
        let lease = Arc::new(());
        self.queue.push(Interest {
            target: target.into(),
            lease: Arc::downgrade(&lease),
        });
        Some(lease)
    }

    fn next(&mut self) -> Option<String> {
        self.queue
            .retain(|interest| interest.lease.strong_count() > 0);
        if self.queue.is_empty() {
            return None;
        }
        let interest = self.queue.remove(0);
        self.spent.push(interest.target.clone());
        self.building = Some(interest.target.clone());
        Some(interest.target)
    }

    pub(super) fn select(&self, request: Request<'_>) -> Option<Arc<Source<R>>> {
        self.sources
            .iter()
            .find(|source| source.accepts(request) && source.admits())
            .cloned()
    }
}

impl<A: SnapshotSource> ProgressReporter<A> {
    /// Bind fresh coverage to this initial attach before enumeration starts.
    pub(crate) fn begin_loading(&self, authority: FirstIndexObservation) -> bool {
        let Some(entry) = self.entry.upgrade() else {
            return false;
        };
        let waker = entry.gate.stance_waker();
        self.with_claimed_state(|state| {
            if let Some(loading) = state
                .loading
                .as_mut()
                .filter(|loading| loading.claim == self.epoch)
                && authority.admits()
            {
                authority.register_waker(&waker);
                loading.authority = Some(authority);
                loading.revision += 1;
                true
            } else {
                false
            }
        })
        .unwrap_or(false)
    }

    pub(crate) fn decline_loading(&self) {
        self.with_claimed_state(EntryState::close_loading);
    }

    pub(crate) fn next_loading_target(&self) -> Option<String> {
        self.with_claimed_state(|state| {
            state
                .loading
                .as_mut()
                .filter(|loading| {
                    loading.claim == self.epoch
                        && loading
                            .authority
                            .as_ref()
                            .is_some_and(FirstIndexObservation::admits)
                })
                .and_then(Loading::next)
        })
        .flatten()
    }

    pub(crate) fn loading_target_finished(&self, failure: Option<ReaderUnavailable>) {
        self.with_claimed_state(|state| {
            if let Some(loading) = state.loading.as_mut() {
                if let Some(target) = loading.building.take()
                    && let Some(failure) = failure
                {
                    loading.failures.push((target, failure));
                }
                loading.revision += 1;
            }
        });
    }

    pub(crate) fn loading_failed(&self, failure: ReaderUnavailable, metadata_only: bool) {
        self.with_claimed_state(|state| {
            if let Some(loading) = &mut state.loading {
                if metadata_only {
                    loading.metadata_failure = Some(failure);
                } else {
                    loading.failure = Some(failure);
                }
                loading.revision += 1;
            }
        });
    }

    pub(crate) fn publish_loading(
        &self,
        reader: A::Reader,
        model: Arc<ContentModel>,
        authority: FirstIndexObservation,
        operation: Operation,
        diagnostics: Diagnostics,
    ) {
        // All physical construction and the eventual last-owner drop are outside the gate.
        let waker = self.entry.upgrade().map(|entry| entry.gate.stance_waker());
        let source = Arc::new(Source::new(
            reader,
            model,
            authority,
            operation,
            self.epoch,
            diagnostics,
            waker,
        ));
        let published = self
            .with_claimed_state(|state| {
                if state.stands_on_a_cause() || state.owes_a_rung() || !source.admits() {
                    return false;
                }
                let Some(loading) = state
                    .loading
                    .as_mut()
                    .filter(|loading| loading.claim == self.epoch)
                else {
                    return false;
                };
                loading.sources.push(Arc::clone(&source));
                loading.revision += 1;
                true
            })
            .unwrap_or(false);
        if !published {
            source.reader.retire();
        }
    }
}

impl<A: SnapshotSource> EntryState<A> {
    pub(super) fn close_loading(&mut self) {
        if let Some(loading) = self.loading.take() {
            for source in loading.sources {
                source.reader.retire();
                // The deferred pool reference prevents any last file/connection close here.
                self.retired_readers.push(Arc::clone(&source.reader));
            }
        }
    }

    pub(super) fn retire_damaged_loading_sources(&mut self) {
        if let Some(loading) = &mut self.loading {
            loading.sources.retain(|source| {
                if source.damaged.load(Ordering::Acquire) {
                    self.retired_readers.push(Arc::clone(&source.reader));
                    loading.revision += 1;
                    false
                } else {
                    true
                }
            });
        }
    }

    pub(super) fn loading_source(&self, request: Request<'_>) -> Option<Arc<Source<A::Reader>>> {
        self.loading
            .as_ref()
            .filter(|loading| self.claim.stands_at(loading.claim))?
            .select(request)
    }

    pub(super) fn loading_failure(&self, request: Request<'_>) -> Option<ReaderUnavailable> {
        let loading = self
            .loading
            .as_ref()
            .filter(|loading| self.claim.stands_at(loading.claim))?;
        loading.failure.clone().or_else(|| match request {
            Request::Get(params) => loading
                .failures
                .iter()
                .find(|(target, _)| target == params.target.address())
                .map(|(_, failure)| failure.clone()),
            Request::Find(_) => loading.metadata_failure.clone(),
            Request::Canonical => None,
        })
    }

    pub(super) fn waits_for_loading(&self, request: Request<'_>) -> bool {
        request.eligible()
            && !self.stands_on_a_cause()
            && !self.owes_a_rung()
            && self.loading.as_ref().is_some_and(|loading| {
                self.claim.stands_at(loading.claim)
                    && loading
                        .authority
                        .as_ref()
                        .is_none_or(FirstIndexObservation::admits)
                    && match request {
                        Request::Get(params) => {
                            self.loading_failure(request).is_some()
                                || loading.building.as_deref() == Some(params.target.address())
                                || loading
                                    .queue
                                    .iter()
                                    .any(|interest| interest.target == params.target.address())
                        }
                        Request::Find(_) => true,
                        Request::Canonical => false,
                    }
            })
    }
}

/// Owns a possibly restricted hold. It exposes operation execution, reading and counters only.
pub(crate) struct QueryHold<O: EntryOps> {
    hold: ReadHold<O>,
    source: Option<Arc<Source<<O::Attachment as SnapshotSource>::Reader>>>,
}

impl<O: EntryOps> QueryHold<O> {
    pub(super) fn new(
        hold: ReadHold<O>,
        source: Option<Arc<Source<<O::Attachment as SnapshotSource>::Reader>>>,
    ) -> Self {
        Self { hold, source }
    }

    pub(super) fn into_canonical(self) -> ReadHold<O> {
        assert!(
            self.source.is_none(),
            "a restricted source cannot become an unrestricted hold"
        );
        self.hold
    }
}

impl<O: EntryOps> QueryHold<O>
where
    <O::Attachment as SnapshotSource>::Reader: ReadSource<Snapshot = norn_store::Snapshot>,
{
    #[cfg(feature = "induced-failure")]
    pub(crate) fn query_boundary(&self, host: &Host<O>, completed: bool) {
        if self.source.is_some() {
            host.shared.ops.loading_read_boundary(completed);
        }
    }

    pub(crate) fn reading(&self) -> &HoldReading {
        self.hold.reading()
    }
    pub(crate) fn counters(&self) -> norn_store::SnapshotCounters {
        self.hold.snapshot().counters()
    }

    pub(crate) fn get(
        &self,
        params: &GetParams,
    ) -> Result<norn_store::Gotten, norn_store::PageRefusal> {
        assert!(
            self.source
                .as_ref()
                .is_none_or(|source| source.accepts(Request::Get(params)))
        );
        self.hold
            .snapshot()
            .get(params, self.hold.content_model(), &crate::text::TextLayer)
    }

    pub(crate) fn find(
        &self,
        params: &FindParams,
    ) -> Result<norn_store::Found, norn_store::PageRefusal> {
        assert!(
            self.source
                .as_ref()
                .is_none_or(|source| source.accepts(Request::Find(params)))
        );
        self.hold.snapshot().find(params, self.hold.content_model())
    }

    pub(crate) fn record_page_failure(&self, refusal: &norn_store::PageRefusal) {
        if let Some(source) = &self.source
            && let norn_store::PageRefusal::Store(error) = refusal
        {
            source.record_failure(&error.to_string());
        }
    }

    pub(crate) fn current(&self) -> bool {
        self.source
            .as_ref()
            .is_none_or(|source| source.authority.check_root() && source.held_is_current())
    }

    pub(crate) fn damage(&self, host: &Host<O>, detail: String) -> ReadRefusal {
        if let Some(source) = &self.source {
            ReadRefusal::ReaderUnavailable(source.unavailable(ReaderUnavailable::damaged(detail)))
        } else {
            host.withdraw_for_read_damage(&self.hold, detail)
        }
    }
}

#[cfg(all(test, feature = "induced-failure"))]
mod tests;

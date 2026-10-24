//! Private, sequential FrozenInput experiment. No attachment escapes this owner.
use super::super::ProductionEntryOps;
use super::*;
use crate::ProgressReporter;
use norn_config::registry::Entry as Registration;

#[derive(Debug)]
pub struct HelperComplete {
    retired_paths: Vec<PathBuf>,
    budget: norn_store::ReadBudgetCensus,
}
impl HelperComplete {
    pub fn retired_paths(&self) -> &[PathBuf] {
        &self.retired_paths
    }
    pub fn budget(&self) -> norn_store::ReadBudgetCensus {
        self.budget
    }
}

#[derive(Debug)]
pub enum HelperCause {
    Ineligible,
    Cancelled,
    Operation(JobFailure),
    RequiredGetUnavailable,
    SourceChanged,
    OutstandingReaders(norn_store::ReadBudgetCensus),
    Cleanup,
}

/// Cleanup failures survive beside the original operation or cancellation cause.
#[derive(Debug)]
pub struct HelperFailure {
    pub cause: HelperCause,
    pub cleanup: Vec<String>,
}

/// The ACK transport owns waking a blocked callback before cancelling its run.
/// Cancellation does not preempt reads, callbacks, or their ordinary lifetimes.
#[derive(Default)]
pub struct HelperControl(AtomicBool);
impl HelperControl {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    fn cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Run a single bounded attempt under ordinary ownership, then release it.
/// The caller owns its registration and, in native tests, the shared watcher lease.
/// A completed value proves this synchronous owner ended; process EOF and exit
/// must still be observed by the controller before starting another executable.
pub fn run_prefix_helper(
    ops: &ProductionEntryOps,
    registration: &Registration,
    experiment: &Experiment,
    control: &HelperControl,
) -> Result<HelperComplete, HelperFailure> {
    let fail = |cause| HelperFailure {
        cause,
        cleanup: Vec::new(),
    };
    let Some(frozen) = &experiment.frozen_prefix else {
        return Err(fail(HelperCause::Ineligible));
    };
    if !experiment.canonical
        || experiment.intake
        || ops.semantic.is_some()
        || ops.initial_import_cache.is_some()
        || !experiment.take_initial_attempt()
    {
        return Err(fail(HelperCause::Ineligible));
    }
    if control.cancelled() {
        return Err(fail(HelperCause::Cancelled));
    }
    let _job = ops.evidence.attributing();
    let progress = ProgressReporter::disconnected();
    let mut attachment = ops
        .attach_prologue(registration, &progress)
        .map_err(|error| fail(HelperCause::Operation(error)))?;
    let result = (|| {
        attachment
            .store
            .ensure_startup_complete()
            .map_err(store_effect)?;
        if attachment.controls.undeclarable().is_some()
            || matches!(
                attachment.store.open_outcome(),
                norn_store::OpenOutcome::Reused
            )
        {
            return Ok(None);
        }
        let order = attachment.store.path_order();
        if !HealScope::Vault
            .page(&mut attachment.store, None, ops.policy, order)?
            .is_empty()
        {
            return Ok(None);
        }
        // This namespace is intentionally never a complete ordinary index.
        // Its guard remains after successful stop as well as interruption.
        attachment
            .store
            .begin_request()
            .start_startup_experiment()
            .map_err(store_effect)?;
        let exclusions = super::super::exclusions_at(
            registration,
            &attachment.shadows,
            &attachment.covered_root,
        );
        ProductionEntryOps::under_coverage(&mut attachment, |attachment| {
            canonical::prepare_prefix(attachment, &exclusions, ops.policy, experiment).map(Some)
        })
    })();
    // Drains retired idle connections only. Active readers are never preempted.
    ops.readers.trim(std::time::Instant::now());
    let mut cause = None;
    let mut checks = Vec::new();
    match result {
        Err(error) => cause = Some(HelperCause::Operation(error)),
        Ok(None) => cause = Some(HelperCause::Ineligible),
        Ok(Some(phase)) => {
            checks = phase.checks;
            if let Some(error) = phase.prefix_failure {
                cause = Some(HelperCause::Operation(error));
            } else if !phase.demand_served {
                cause = Some(HelperCause::RequiredGetUnavailable);
            } else if phase.demand_reader.strong_count() != 0 {
                cause = Some(HelperCause::OutstandingReaders(ops.readers.census()));
            }
        }
    }
    if !attachment.heal_observed.is_empty() || !frozen.intact() {
        frozen.invalidate();
        cause = Some(HelperCause::SourceChanged);
    }
    let mut cleanup = Vec::new();
    // The observer checkpoint is evidence only, never the successful terminal.
    if cause.is_none() {
        if let Err(error) = experiment.observer.prefix_retired() {
            cause = Some(HelperCause::Operation(JobFailure::Environmental(error)));
        }
    }
    // Same order as ordinary release, with errors preserved for this owner.
    if let Some(subscription) = attachment.subscription.take()
        && let Err(error) = subscription.close_joined()
    {
        cleanup.push(error.to_string());
    }
    if let Err(error) = attachment.store.close() {
        cleanup.push(error.to_string());
    }
    drop(attachment.maintainership);
    ops.readers.trim(std::time::Instant::now());
    let budget = ops.readers.census();
    if budget.live_connections != 0 || budget.waiting_reads != 0 {
        cause = Some(HelperCause::OutstandingReaders(budget));
    }
    if let Some(error) = ops.last_reader_cleanup_error() {
        cleanup.push(error);
    }
    let mut retired_paths = Vec::new();
    for check in checks {
        match check.verify() {
            Ok(paths) => retired_paths.extend_from_slice(paths),
            Err(error) => cleanup.push(error.to_string()),
        }
    }
    if !frozen.intact() {
        cause = Some(HelperCause::SourceChanged);
    }
    if control.cancelled() {
        cause = Some(HelperCause::Cancelled);
    }
    if !cleanup.is_empty() && cause.is_none() {
        cause = Some(HelperCause::Cleanup);
    }
    if let Some(cause) = cause {
        return Err(HelperFailure { cause, cleanup });
    }
    Ok(HelperComplete {
        retired_paths,
        budget,
    })
}

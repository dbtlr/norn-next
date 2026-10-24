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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HelperCause {
    Ineligible,
    Cancelled,
    Operation(JobFailure),
    RequiredGetUnavailable,
    SourceChanged,
    OutstandingReaders(norn_store::ReadBudgetCensus),
    Cleanup,
}

/// The first observed operation leads, followed by source, eligibility, resource,
/// cleanup and cancellation causes in that order. Every other observed cause survives.
#[derive(Debug)]
pub struct HelperFailure {
    pub cause: HelperCause,
    pub secondary: Vec<HelperCause>,
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
        secondary: Vec::new(),
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
    let Some(mut attachment) = ops
        .attach_prologue_if(registration, &progress, |path| {
            norn_store::startup_experiment::fresh_namespace(path).map_err(store_effect)
        })
        .map_err(|error| fail(HelperCause::Operation(error)))?
    else {
        return Err(fail(HelperCause::Ineligible));
    };
    let mut evidence = canonical::PrefixEvidence::default();
    let mut coverage_failures = Vec::new();
    let result = (|| {
        attachment
            .store
            .ensure_startup_complete()
            .map_err(store_effect)?;
        if attachment.controls.undeclarable().is_some()
            || !matches!(
                attachment.store.open_outcome(),
                norn_store::OpenOutcome::Created
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
        ProductionEntryOps::under_coverage_recording(
            &mut attachment,
            |attachment| {
                canonical::prepare_prefix(
                    attachment,
                    &exclusions,
                    ops.policy,
                    experiment,
                    &mut evidence,
                )
                .map(Some)
            },
            &mut coverage_failures,
        )
    })();
    // Drains retired idle connections only. Active readers are never preempted.
    ops.readers.trim(std::time::Instant::now());
    let mut causes = Vec::new();
    let phase_completed = matches!(&result, Ok(Some(_)));
    match result {
        Err(error) if !coverage_failures.contains(&error) => coverage_failures.push(error),
        Ok(None) => causes.push(HelperCause::Ineligible),
        _ => (),
    }
    causes.extend(
        evidence
            .failures
            .into_iter()
            .chain(coverage_failures)
            .map(HelperCause::Operation),
    );
    if evidence.demand_reader.strong_count() != 0 {
        causes.push(HelperCause::OutstandingReaders(ops.readers.census()));
    }
    if phase_completed && !evidence.demand_served {
        causes.push(HelperCause::RequiredGetUnavailable);
    }
    if !attachment.heal_observed.is_empty() || !frozen.intact() {
        frozen.invalidate();
        causes.push(HelperCause::SourceChanged);
    }
    let mut cleanup = Vec::new();
    // The observer checkpoint is evidence only, never the successful terminal.
    if causes.is_empty()
        && let Err(error) = experiment.observer.prefix_retired()
    {
        causes.push(HelperCause::Operation(JobFailure::Environmental(error)));
    }
    cleanup.extend(super::super::release_owned(attachment, |subscription| {
        if let Some(subscription) = subscription {
            subscription
                .close_joined()
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }));
    ops.readers.trim(std::time::Instant::now());
    let budget = ops.readers.census();
    if budget.live_connections != 0 || budget.waiting_reads != 0 {
        causes.push(HelperCause::OutstandingReaders(budget));
    }
    if let Some(error) = ops.last_reader_cleanup_error() {
        cleanup.push(error);
    }
    let mut retired_paths = Vec::new();
    for check in evidence.checks {
        match check.verify() {
            Ok(paths) => retired_paths.extend_from_slice(paths),
            Err(error) => cleanup.push(error.to_string()),
        }
    }
    if !frozen.intact() {
        causes.push(HelperCause::SourceChanged);
    }
    if control.cancelled() {
        causes.push(HelperCause::Cancelled);
    }
    if !cleanup.is_empty() {
        causes.push(HelperCause::Cleanup);
    }
    causes.sort_by_key(|cause| match cause {
        HelperCause::Operation(_) => 0,
        HelperCause::SourceChanged => 1,
        HelperCause::Ineligible => 2,
        HelperCause::RequiredGetUnavailable => 3,
        HelperCause::OutstandingReaders(_) => 4,
        HelperCause::Cleanup => 5,
        HelperCause::Cancelled => 6,
    });
    // Repeated source checks describe one invalidated authority.
    causes.dedup_by(|later, earlier| {
        matches!(
            (later, earlier),
            (HelperCause::SourceChanged, HelperCause::SourceChanged)
        )
    });
    if !causes.is_empty() {
        let cause = causes.remove(0);
        return Err(HelperFailure {
            cause,
            secondary: causes,
            cleanup,
        });
    }
    Ok(HelperComplete {
        retired_paths,
        budget,
    })
}

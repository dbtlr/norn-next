//! The closed hold's administrative lifetime, complementary to public request tests.
use super::*;
use crate::{LoadingBoundary, LoadingProbe, ProductionEntryOps, ProductionPolicy};
use norn_testkit::{
    isolation,
    scratch::Scratch,
    wait::{Budget, Observed, wait_until},
};
use std::sync::Condvar;

#[derive(Default)]
struct Pause {
    before_catalogue: bool,
    reached: AtomicBool,
    released: Mutex<bool>,
    changed: Condvar,
}
impl Pause {
    fn release(&self) {
        *self.released.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.changed.notify_all();
    }
}
impl LoadingProbe for Pause {
    fn at(&self, boundary: LoadingBoundary<'_>) {
        if !matches!(
            (self.before_catalogue, boundary),
            (true, LoadingBoundary::BeforeCatalogue) | (false, LoadingBoundary::MetadataComplete)
        ) {
            return;
        }
        self.reached.store(true, Ordering::Release);
        let released = self.released.lock().unwrap();
        let (_released, timeout) = self
            .changed
            .wait_timeout_while(released, Duration::from_secs(30), |released| !*released)
            .unwrap();
        assert!(!timeout.timed_out(), "metadata boundary was never released");
    }
}
struct Release<'a>(&'a Pause);
impl Drop for Release<'_> {
    fn drop(&mut self) {
        self.0.release();
    }
}

#[test]
#[cfg(feature = "induced-failure")]
#[allow(clippy::disallowed_methods)] // This unit owns its filesystem fixture.
fn an_established_closed_hold_survives_host_destruction_without_retaining_coverage() {
    let budget = Budget::new(Duration::from_secs(15), Duration::from_millis(100));
    let scratch = Scratch::new("closed-loading-host-destruction");
    let root = scratch.join("vault");
    std::fs::create_dir_all(root.join(".norn")).unwrap();
    std::fs::write(root.join(".norn/schema.yaml"), "version: 1\n").unwrap();
    std::fs::write(root.join("subject.md"), "# Subject\nWhole body.\n").unwrap();
    let _watcher = isolation::Lease::hold(
        isolation::REAL_WATCHER,
        isolation::acquisition_budget(budget),
    );
    let name = VaultName::new("notes").unwrap();
    let registration = Registration::new(
        name.clone(),
        norn_config::registry::VaultRoot::new(&root).unwrap(),
    );
    let pause = Arc::new(Pause::default());
    let host = Host::new(
        RegistryRead::from_entries([registration]),
        ProductionEntryOps::new(
            norn_config::ConfigDirs::new(scratch.join("config"), scratch.join("data")).unwrap(),
            ProductionPolicy::new(2, 2).unwrap(),
        )
        .with_loading_probe(pause.clone()),
        LifecyclePolicy {
            idle_after: Duration::from_secs(3600),
            worker_slots: 1,
            watch_poll_interval: Duration::from_millis(25),
            read_settle_bound: Duration::from_secs(5),
        },
    )
    .unwrap();
    let _release = Release(&pause);
    let params = GetParams::new(
        norn_wire::VaultAddress::name(name.clone()),
        norn_wire::ResolutionTarget::new("subject").unwrap(),
    );
    let (hold, report) = std::thread::scope(|scope| {
        let first = scope.spawn(|| host.get(&params).unwrap());
        wait_until("metadata completion", budget, || {
            if pause.reached.load(Ordering::Acquire) {
                Observed::Met(())
            } else {
                Observed::pending("metadata not complete")
            }
        })
        .unwrap();
        let first = first.join().unwrap();
        let held = host
            .begin_query_read(&name, norn_store::ReadNeed::Source, Request::Get(&params))
            .unwrap();
        pause.release();
        wait_until("canonical promotion", budget, || {
            if host.state(&name).unwrap() == TrustState::Ready {
                Observed::Met(())
            } else {
                Observed::pending("canonical construction continues")
            }
        })
        .unwrap();
        (held, first.answer.report)
    });
    let ops = Arc::clone(&host.shared.ops);
    let authority = hold.source.as_ref().unwrap().authority.clone();
    drop(host);
    assert!(
        !authority.admits(),
        "administrative closure forbids new admission"
    );
    assert!(
        authority.held_is_current(),
        "administrative unsubscription preserves earned authority"
    );
    assert_eq!(ops.read_budget_census().live_connections, 1);
    assert!(hold.current());
    assert_eq!(hold.get(&params).unwrap().report, report);
    assert!(ops.last_reader_cleanup_error().is_none());
    drop(hold);
    assert_eq!(ops.read_budget_census().live_connections, 0);
    assert!(ops.last_reader_cleanup_error().is_none());
}

#[test]
#[allow(clippy::disallowed_methods)] // This unit owns its filesystem fixture.
fn watcher_withdrawal_moves_the_host_signal_without_a_dispatcher_or_gate_retake() {
    let budget = Budget::new(Duration::from_secs(15), Duration::from_millis(100));
    let scratch = Scratch::new("loading-watcher-signal-binding");
    let root = scratch.join("vault");
    std::fs::create_dir_all(root.join(".norn")).unwrap();
    std::fs::write(root.join(".norn/schema.yaml"), "version: 1\n").unwrap();
    std::fs::write(root.join("subject.md"), "# Subject\n").unwrap();
    let _watcher = isolation::Lease::hold(
        isolation::REAL_WATCHER,
        isolation::acquisition_budget(budget),
    );
    let name = VaultName::new("notes").unwrap();
    let pause = Arc::new(Pause {
        before_catalogue: true,
        ..Pause::default()
    });
    let mut host = Host::new(
        RegistryRead::from_entries([Registration::new(
            name.clone(),
            norn_config::registry::VaultRoot::new(&root).unwrap(),
        )]),
        ProductionEntryOps::new(
            norn_config::ConfigDirs::new(scratch.join("config"), scratch.join("data")).unwrap(),
            ProductionPolicy::new(2, 2).unwrap(),
        )
        .with_loading_probe(pause.clone()),
        LifecyclePolicy {
            idle_after: Duration::from_secs(3600),
            worker_slots: 1,
            watch_poll_interval: Duration::from_millis(25),
            read_settle_bound: Duration::from_secs(5),
        },
    )
    .unwrap();
    host.dispatcher_stop.send(()).unwrap();
    host.dispatcher.take().unwrap().join().unwrap();
    std::thread::scope(|scope| {
        let _release = Release(&pause);
        let params = FindParams::new(norn_wire::VaultAddress::name(name.clone()));
        let host_ref = &host;
        let request = scope.spawn(move || host_ref.find(&params));
        wait_until(
            "registered authority and waiting public Find",
            budget,
            || {
                if pause.reached.load(Ordering::Acquire) && host.read_evidence().settle_waits > 0 {
                    Observed::Met(())
                } else {
                    Observed::pending("construction or request not paused")
                }
            },
        )
        .unwrap();
        let entry = host.shared.entries.get(&name).unwrap();
        let state = entry.gate.lock().unwrap();
        let authority = state.loading.as_ref().unwrap().authority.as_ref().unwrap();
        let seen = entry.gate.stance_generation();
        let takes = entry.gate.times_taken();
        // Invalidate the actual bound authority while the gate is unavailable.
        // A watcher wake must touch only the signal and cannot rely on end_hold.
        authority.invalidate();
        assert_eq!(entry.gate.stance_generation(), seen.wrapping_add(1));
        assert_eq!(entry.gate.times_taken(), takes);
        drop(state);
        let refusal = request.join().unwrap().unwrap_err();
        assert_eq!(refusal.code(), &norn_wire::ReasonCode::HostEntryNotReady);
        assert_eq!(host.read_evidence().settle_expiries, 0);
        pause.release();
    });
}

#[test]
fn a_stale_loading_decline_cannot_close_a_successor() {
    let entry = Arc::new(
        Entry::<crate::production::ProductionAttachment>::unattached(
            Registration::new(
                VaultName::new("notes").unwrap(),
                norn_config::registry::VaultRoot::new("/mock/vault").unwrap(),
            ),
            3,
            true,
        ),
    );
    let stale = ProgressReporter {
        entry: Arc::downgrade(&entry),
        epoch: 3,
    };
    {
        let mut state = entry.gate.lock().unwrap();
        let successor = state.claim.supersede();
        state.loading = Some(Loading::provisional(successor));
    }
    stale.decline_loading();
    assert_eq!(
        entry.gate.lock().unwrap().loading.as_ref().unwrap().claim,
        4
    );
    let current = ProgressReporter {
        entry: Arc::downgrade(&entry),
        epoch: 4,
    };
    current.decline_loading();
    assert!(entry.gate.lock().unwrap().loading.is_none());
}

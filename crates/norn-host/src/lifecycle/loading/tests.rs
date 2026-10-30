//! The construction state's guards and lifetimes, complementary to public request tests.
use super::*;
use crate::{LoadingBoundary, LoadingProbe, ProductionEntryOps, ProductionPolicy};
use norn_testkit::{
    isolation,
    scratch::Scratch,
    wait::{Budget, Observed, wait_until},
};
use std::sync::Condvar;

const BUDGET: Budget = Budget::new(Duration::from_secs(15), Duration::from_millis(100));

/// Holds construction at the first boundary `at` matches.
struct Pause {
    at: fn(LoadingBoundary) -> bool,
    reached: AtomicBool,
    released: Mutex<bool>,
    changed: Condvar,
}
impl Pause {
    fn new(at: fn(LoadingBoundary) -> bool) -> Arc<Self> {
        Arc::new(Self {
            at,
            reached: AtomicBool::new(false),
            released: Mutex::new(false),
            changed: Condvar::new(),
        })
    }
    fn release(&self) {
        *self.released.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.changed.notify_all();
    }
}
impl LoadingProbe for Pause {
    fn at(&self, boundary: LoadingBoundary) {
        if !(self.at)(boundary) || self.reached.swap(true, Ordering::AcqRel) {
            return;
        }
        let released = self.released.lock().unwrap();
        let (_released, timeout) = self
            .changed
            .wait_timeout_while(released, Duration::from_secs(30), |released| !*released)
            .unwrap();
        assert!(
            !timeout.timed_out(),
            "construction boundary was never released"
        );
    }
}
struct Release<'a>(&'a Pause);
impl Drop for Release<'_> {
    fn drop(&mut self) {
        self.0.release();
    }
}

#[allow(clippy::disallowed_methods)] // This unit owns its filesystem fixture.
fn host_over(scratch: &Scratch, pause: Arc<Pause>) -> (VaultName, Host<ProductionEntryOps>) {
    let root = scratch.join("vault");
    std::fs::create_dir_all(root.join(".norn")).unwrap();
    std::fs::write(root.join(".norn/schema.yaml"), "version: 1\n").unwrap();
    std::fs::write(root.join("subject.md"), "# Subject\nWhole body.\n").unwrap();
    // FSEvents can deliver the fixture's writes after the host attaches.
    norn_fs::watch::settle_prior_changes(scratch.root(), BUDGET.work()).unwrap();
    let name = VaultName::new("notes").unwrap();
    let host = Host::new(
        RegistryRead::from_entries([Registration::new(
            name.clone(),
            norn_config::registry::VaultRoot::new(&root).unwrap(),
        )]),
        ProductionEntryOps::new(
            norn_config::ConfigDirs::new(scratch.join("config"), scratch.join("data")).unwrap(),
            ProductionPolicy::new(2, 2).unwrap(),
        )
        .with_loading_probe(pause),
        LifecyclePolicy {
            idle_after: Duration::from_secs(3600),
            worker_slots: 1,
            watch_poll_interval: Duration::from_millis(25),
            read_settle_bound: Duration::from_secs(5),
        },
    )
    .unwrap();
    (name, host)
}

#[test]
#[cfg(feature = "induced-failure")]
fn an_established_early_hold_survives_host_destruction_without_retaining_coverage() {
    let scratch = Scratch::new("early-hold-host-destruction");
    let _watcher = isolation::Lease::hold(
        isolation::REAL_WATCHER,
        isolation::acquisition_budget(BUDGET),
    );
    let pause = Pause::new(|boundary| {
        matches!(
            boundary,
            LoadingBoundary::StepComplete(norn_store::Step::Structure)
        )
    });
    let (name, host) = host_over(&scratch, pause.clone());
    let _release = Release(&pause);
    let params = GetParams::new(
        norn_wire::VaultAddress::name(name.clone()),
        norn_wire::ResolutionTarget::new("subject").unwrap(),
    );
    let (hold, report) = std::thread::scope(|scope| {
        let first = scope.spawn(|| host.get(&params).unwrap());
        wait_until("structure published", BUDGET, || {
            if pause.reached.load(Ordering::Acquire) {
                Observed::Met(())
            } else {
                Observed::pending("structure not complete")
            }
        })
        .unwrap();
        let first = first.join().unwrap();
        let held = host
            .begin_query_read(&name, norn_store::ReadNeed::Source, Request::Get(&params))
            .unwrap();
        assert!(held.early.as_ref().unwrap().relations_loading);
        pause.release();
        wait_until("canonical promotion", BUDGET, || {
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
    let authority = hold.early.as_ref().unwrap().authority.clone();
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
    drop(hold);
    assert_eq!(ops.read_budget_census().live_connections, 0);
}

#[test]
fn watcher_withdrawal_moves_the_host_signal_without_a_dispatcher_or_gate_retake() {
    watcher_signal(false);
}

#[test]
fn a_recorded_fact_wakes_nothing_and_withdrawal_does() {
    watcher_signal(true);
}

#[allow(clippy::disallowed_methods)] // The test owns the vault fixture.
fn watcher_signal(record_first: bool) {
    let scratch = Scratch::new("construction-watcher-signal-binding");
    let _watcher = isolation::Lease::hold(
        isolation::REAL_WATCHER,
        isolation::acquisition_budget(BUDGET),
    );
    let pause = Pause::new(|boundary| matches!(boundary, LoadingBoundary::CanonicalPage));
    let (name, mut host) = host_over(&scratch, pause.clone());
    host.dispatcher_stop.send(()).unwrap();
    host.dispatcher.take().unwrap().join().unwrap();
    std::thread::scope(|scope| {
        let _release = Release(&pause);
        let params = FindParams::new(norn_wire::VaultAddress::name(name.clone()));
        let host_ref = &host;
        let request = scope.spawn(move || host_ref.find(&params));
        wait_until("a begun construction and a waiting Find", BUDGET, || {
            if pause.reached.load(Ordering::Acquire) && host.read_evidence().settle_waits > 0 {
                Observed::Met(())
            } else {
                Observed::pending("construction or request not paused")
            }
        })
        .unwrap();
        let entry = host.shared.entries.get(&name).unwrap();
        let state = entry.gate.lock().unwrap();
        let Some(Construction::Building(building)) = state.construction.as_ref() else {
            panic!("the build began its construction before its walk");
        };
        let seen = entry.gate.stance_generation();
        let takes = entry.gate.times_taken();
        if record_first {
            let sequence = building.authority.sequence();
            std::fs::write(scratch.join("vault/late.md"), "# Late\n").unwrap();
            wait_until("accepted fact without the entry gate", BUDGET, || {
                if building.authority.sequence() > sequence {
                    Observed::Met(())
                } else {
                    Observed::pending("fact not accepted")
                }
            })
            .unwrap();
            assert_eq!(entry.gate.stance_generation(), seen);
            assert!(building.authority.admits());
        }
        // Invalidate the actual bound authority while the gate is unavailable.
        // A watcher wake must touch only the signal and cannot rely on end_hold.
        building.authority.invalidate();
        assert_eq!(entry.gate.stance_generation(), seen.wrapping_add(1));
        assert_eq!(entry.gate.times_taken(), takes);
        drop(state);
        let refusal = request.join().unwrap().unwrap_err();
        assert_eq!(refusal.code(), &norn_wire::ReasonCode::HostEntryNotReady);
        assert_eq!(host.read_evidence().settle_expiries, 0);
        pause.release();
    });
}

fn entry_at(epoch: u64) -> Arc<Entry<crate::production::ProductionAttachment>> {
    Arc::new(Entry::unattached(
        Registration::new(
            VaultName::new("notes").unwrap(),
            norn_config::registry::VaultRoot::new("/mock/vault").unwrap(),
        ),
        epoch,
        CanonicalStoreObservation::Absent,
    ))
}

fn reporter(
    entry: &Arc<Entry<crate::production::ProductionAttachment>>,
    epoch: u64,
) -> ProgressReporter<crate::production::ProductionAttachment> {
    ProgressReporter {
        entry: Arc::downgrade(entry),
        epoch,
    }
}

#[test]
fn a_stale_reporter_cannot_close_a_successor() {
    let entry = entry_at(3);
    let stale = reporter(&entry, 3);
    {
        let mut state = entry.gate.lock().unwrap();
        let successor = state.claim.supersede();
        state.construction = Some(Construction::Provisional { claim: successor });
    }
    assert!(stale.begin_construction(None).is_none());
    assert!(matches!(
        entry.gate.lock().unwrap().construction,
        Some(Construction::Provisional { claim: 4 })
    ));
    assert!(reporter(&entry, 4).begin_construction(None).is_none());
    assert!(entry.gate.lock().unwrap().construction.is_none());
}

/// A real build's begin: a fresh authority over live coverage, and the zero-I/O
/// pool of a store in the fixture.
#[allow(clippy::disallowed_methods)] // This unit owns its filesystem fixture.
fn begun(
    scratch: &Scratch,
) -> (
    norn_fs::Subscription,
    norn_store::Store,
    Arc<norn_store::ReadBudget>,
) {
    let root = scratch.join("vault");
    std::fs::create_dir_all(root.join(".norn")).unwrap();
    std::fs::write(root.join(".norn/schema.yaml"), "version: 1\n").unwrap();
    norn_fs::watch::settle_prior_changes(scratch.root(), BUDGET.work()).unwrap();
    let (subscription, _own) =
        norn_fs::watch::watch(&root, &root.join(".norn/schema.yaml")).unwrap();
    subscription.synchronize(BUDGET.work()).unwrap();
    let store = norn_store::Store::open(
        scratch.join("store.sqlite3"),
        crate::production::stored_path_order(subscription.case_sensitivity()),
        crate::DERIVATION_VERSION,
    )
    .unwrap();
    let budget = norn_store::ReadBudget::new(ProductionPolicy::new(2, 2).unwrap().read_budget);
    (subscription, store, budget)
}

fn begin(
    progress: &ProgressReporter<crate::production::ProductionAttachment>,
    subscription: &norn_fs::Subscription,
    store: &norn_store::Store,
    budget: &Arc<norn_store::ReadBudget>,
) -> BuildToken {
    progress
        .begin_construction(Some(Begun {
            evidence: Default::default(),
            pool: store.reader_pool(budget),
            model: Arc::new(ContentModel::none()),
            authority: subscription.capture_build_authority(&[]).unwrap(),
            store_epoch: store.epoch().to_owned(),
        }))
        .expect("a standing claim begins its construction")
}

fn published(entry: &Entry<crate::production::ProductionAttachment>) -> Option<Step> {
    match &entry.gate.lock().unwrap().construction {
        Some(Construction::Building(building)) => building.published,
        _ => None,
    }
}

#[test]
fn stale_publication_is_refused() {
    let scratch = Scratch::new("construction-stale-publication");
    let _watcher = isolation::Lease::hold(
        isolation::REAL_WATCHER,
        isolation::acquisition_budget(BUDGET),
    );
    let (subscription, store, budget) = begun(&scratch);
    let epoch = store.epoch().to_owned();
    let entry = entry_at(3);
    let progress = reporter(&entry, 3);
    let token = begin(&progress, &subscription, &store, &budget);
    let revision = entry.gate.lock().unwrap().construction_revision;
    assert!(progress.publish_step(token, &epoch, Step::Names));
    assert_eq!(published(&entry), Some(Step::Names));
    {
        let state = entry.gate.lock().unwrap();
        assert_eq!(state.construction_revision, revision + 1);
        assert_eq!(
            state.trust,
            TrustState::Unattached,
            "publication never publishes Ready"
        );
        assert!(!state.trust_unbroken_since_ready);
    }
    assert!(
        !progress.publish_step(token, "another store", Step::Content),
        "a publication names the store its build began over"
    );
    // A park, a release, an out-of-service entry and known damage each
    // refuse publication while they stand.
    type Standing = fn(&mut EntryState<crate::production::ProductionAttachment>, bool);
    let guards: [Standing; 4] = [
        |state, on| state.identity_refused = on.then(|| "parked".into()),
        |state, on| state.detach_in_flight = on,
        |state, on| {
            state.service = if on {
                Service::Withdrawn
            } else {
                Service::Served
            }
        },
        |state, on| state.damage_met_under_a_claim = on.then(|| "damaged".into()),
    ];
    for guard in guards {
        let token = begin(&progress, &subscription, &store, &budget);
        guard(&mut entry.gate.lock().unwrap(), true);
        assert!(!progress.publish_step(token, &epoch, Step::Names));
        assert!(
            progress
                .begin_construction(Some(Begun {
                    evidence: Default::default(),
                    pool: store.reader_pool(&budget),
                    model: Arc::new(ContentModel::none()),
                    authority: subscription.capture_build_authority(&[]).unwrap(),
                    store_epoch: epoch.clone(),
                }))
                .is_none(),
            "no build begins under the same guard"
        );
        guard(&mut entry.gate.lock().unwrap(), false);
    }
    // A successor build under the same claim retires the earlier token.
    let successor = begin(&progress, &subscription, &store, &budget);
    assert!(!progress.publish_step(token, &epoch, Step::Content));
    assert!(progress.publish_step(successor, &epoch, Step::Names));
    // A withdrawn authority publishes nothing more.
    subscription.capture_build_authority(&[]).unwrap();
    assert!(!progress.publish_step(successor, &epoch, Step::Content));
    // A changed claim publishes nothing.
    let successor = begin(&progress, &subscription, &store, &budget);
    entry.gate.lock().unwrap().claim.supersede();
    assert!(!progress.publish_step(successor, &epoch, Step::Names));
    let mut state = entry.gate.lock().unwrap();
    assert!(
        state.construction.is_none(),
        "a claim change closes construction"
    );
    state.retired_readers.clear();
}

#[test]
fn counters_never_wake_a_waiter_and_publication_does() {
    let scratch = Scratch::new("construction-counter-wake");
    let _watcher = isolation::Lease::hold(
        isolation::REAL_WATCHER,
        isolation::acquisition_budget(BUDGET),
    );
    let (subscription, store, budget) = begun(&scratch);
    let epoch = store.epoch().to_owned();
    let entry = entry_at(3);
    entry.gate.lock().unwrap().trust = TrustState::warming(WarmingPhase::Healing, 0, None);
    let progress = reporter(&entry, 3);
    let token = begin(&progress, &subscription, &store, &budget);
    let seen = entry.gate.stance_generation();
    progress.healing().report(5, Some(10));
    assert_eq!(
        entry.gate.lock().unwrap().trust,
        TrustState::warming(WarmingPhase::Healing, 5, Some(10))
    );
    assert_eq!(
        entry.gate.stance_generation(),
        seen,
        "counters move no waiter"
    );
    assert!(progress.publish_step(token, &epoch, Step::Names));
    assert_ne!(
        entry.gate.stance_generation(),
        seen,
        "publication wakes waiters"
    );
    entry.gate.lock().unwrap().close_construction();
}

#[test]
#[allow(clippy::disallowed_methods)] // This test owns its vault fixture.
fn a_replacement_construction_takes_a_fresh_meeting() {
    let scratch = Scratch::new("replacement-construction-meeting");
    let _watcher = isolation::Lease::hold(
        isolation::REAL_WATCHER,
        isolation::acquisition_budget(BUDGET),
    );
    let (subscription, store, budget) = begun(&scratch);
    subscription.begin_heal();
    let entry = entry_at(3);
    let progress = reporter(&entry, 3);
    begin(&progress, &subscription, &store, &budget);
    std::fs::write(scratch.join("vault/changed.md"), "# Changed\n").unwrap();
    wait_until("construction records fact", BUDGET, || {
        let state = entry.gate.lock().unwrap();
        match &state.construction {
            Some(Construction::Building(building)) if building.authority.sequence() > 0 => {
                Observed::Met(())
            }
            _ => Observed::pending("no fact yet"),
        }
    })
    .unwrap();
    let params = norn_wire::FindParams::new(norn_wire::VaultAddress::name(
        VaultName::new("notes").unwrap(),
    ));
    let mut meeting = None;
    {
        let state = entry.gate.lock().unwrap();
        assert!(matches!(
            state.early(Request::Find(&params), &mut meeting),
            Early::Waits
        ));
        assert!(meeting.as_ref().unwrap().sequence > 0);
    }
    let first = meeting.as_ref().unwrap().token;
    let replacement = begin(&progress, &subscription, &store, &budget);
    {
        let state = entry.gate.lock().unwrap();
        assert!(matches!(
            state.early(Request::Find(&params), &mut meeting),
            Early::Waits
        ));
        let Some(Construction::Building(building)) = &state.construction else {
            panic!("the replacement construction stands");
        };
        // Late reports of the earlier write may reach the new authority, so the
        // fresh meeting is judged against that authority, never against zero.
        let meeting = meeting.as_ref().unwrap();
        assert_ne!(meeting.token, first);
        assert_eq!(meeting.token, replacement);
        assert_eq!(meeting.sequence, building.authority.sequence());
    }
    subscription.finish_heal().unwrap();
}

#[test]
#[allow(clippy::disallowed_methods)] // This test owns its vault fixture.
fn a_get_meeting_before_a_fact_admits_with_relations_loading() {
    let scratch = Scratch::new("relations-at-admission");
    let _watcher = isolation::Lease::hold(
        isolation::REAL_WATCHER,
        isolation::acquisition_budget(BUDGET),
    );
    let (subscription, store, budget) = begun(&scratch);
    subscription.begin_heal();
    let entry = entry_at(3);
    let progress = reporter(&entry, 3);
    let token = begin(&progress, &subscription, &store, &budget);
    let params = norn_wire::GetParams::new(
        norn_wire::VaultAddress::name(VaultName::new("notes").unwrap()),
        norn_wire::ResolutionTarget::new("/subject").unwrap(),
    );
    let mut meeting = None;
    assert!(matches!(
        entry
            .gate
            .lock()
            .unwrap()
            .early(Request::Get(&params), &mut meeting),
        Early::Waits
    ));
    std::fs::write(scratch.join("vault/elsewhere.md"), "# Elsewhere\n").unwrap();
    wait_until("construction records fact", BUDGET, || {
        let state = entry.gate.lock().unwrap();
        match &state.construction {
            Some(Construction::Building(building)) if building.authority.sequence() > 0 => {
                Observed::Met(())
            }
            _ => Observed::pending("no fact yet"),
        }
    })
    .unwrap();
    assert!(progress.publish_step(token, store.epoch(), Step::Relations));
    let state = entry.gate.lock().unwrap();
    let Early::Admitted(admitted) = state.early(Request::Get(&params), &mut meeting) else {
        panic!("an unrelated fact does not refuse this rooted Get");
    };
    assert!(admitted.relations_loading);
    let params = norn_wire::FindParams::new(norn_wire::VaultAddress::name(
        VaultName::new("notes").unwrap(),
    ));
    let mut stale_meeting = Some(Meeting {
        token,
        sequence: 0,
        touch: None,
        held_by_fact: false,
    });
    assert!(matches!(
        state.early(Request::Find(&params), &mut stale_meeting),
        Early::Withdrawn
    ));
    let Some(Construction::Building(building)) = &state.construction else {
        panic!("building");
    };
    assert_eq!(
        building.evidence.read().early_final_checks_refused_by_fact,
        1
    );

    drop(state);
    subscription.finish_heal().unwrap();
}

#[test]
#[allow(clippy::disallowed_methods)]
fn a_report_after_admission_refuses_even_after_derivation() {
    let scratch = Scratch::new("derived-after-admission");
    let _watcher = isolation::Lease::hold(
        isolation::REAL_WATCHER,
        isolation::acquisition_budget(BUDGET),
    );
    let (subscription, store, budget) = begun(&scratch);
    subscription.begin_heal();
    let entry = entry_at(3);
    let progress = reporter(&entry, 3);
    let token = begin(&progress, &subscription, &store, &budget);
    assert!(progress.publish_step(token, store.epoch(), Step::Structure));
    let params = norn_wire::GetParams::new(
        norn_wire::VaultAddress::name(VaultName::new("notes").unwrap()),
        norn_wire::ResolutionTarget::new("/subject").unwrap(),
    );
    let Early::Admitted(admitted) = entry
        .gate
        .lock()
        .unwrap()
        .early(Request::Get(&params), &mut None)
    else {
        panic!("structure admits the target");
    };
    std::fs::write(scratch.join("vault/subject.md"), "# New\n").unwrap();
    wait_until("accepted report after admission", BUDGET, || {
        if admitted.authority.sequence() > admitted.admitted_at {
            Observed::Met(())
        } else {
            Observed::pending("no report")
        }
    })
    .unwrap();
    // Retirement freezes acceptance history as Ready does, while an established
    // hold keeps that history. Later backend reports cannot mask this predicate.
    admitted.authority.retire();
    let taken = subscription.take_build_roots().unwrap();
    admitted.authority.mark_derived(&taken.roots, taken.through);
    admitted.authority.visit_roots(|_, mut roots| {
        assert!(Iterator::all(&mut roots, |(_, _, owed, _)| owed.is_none()));
    });
    assert!(
        !admitted.facts_current(),
        "the held snapshot predates even a derived report"
    );
    subscription.finish_heal().unwrap();
}

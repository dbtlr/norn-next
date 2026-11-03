//! The background scheduler's lifecycle cases: claim-bound maintenance starts
//! at a wake or a deadline and never at a watcher poll, a due slice over a
//! held claim starts at the claim's release, an entry without coverage keeps
//! no clock, and a maintenance leg ends within its budget plus one unit.
//!
//! The hosts here run no ambient dispatcher tick inside a case — their watcher
//! poll is a minute away — so anything that ran did so without one.

use super::*;

/// A host over one vault whose dispatcher ticks once a minute, running each
/// maintenance leg under `budget`, holding the vault `Ready`.
fn a_ready_vault_under(
    ops: &Arc<FakeOps>,
    budget: Duration,
) -> (Host<Arc<FakeOps>>, VaultName, DemandLease<Arc<FakeOps>>) {
    let name = VaultName::new("notes").unwrap();
    let host = Host::new(
        Roots::Absent(&[&name]).registry(),
        Arc::clone(ops),
        LifecyclePolicy {
            background_turn_budget: budget,
            ..no_ambient_poll_policy(1)
        },
    )
    .unwrap();
    let lease = host.demand(&name).unwrap();
    wait_for_state(&host, &name, TrustState::Ready);
    (host, name, lease)
}

fn a_ready_vault(ops: &Arc<FakeOps>) -> (Host<Arc<FakeOps>>, VaultName, DemandLease<Arc<FakeOps>>) {
    a_ready_vault_under(ops, crate::BACKGROUND_TURN_BUDGET)
}

/// Wait until the fake has run `count` maintenance legs.
fn wait_for_maintenances(ops: &FakeOps, count: usize) {
    wait_until(
        "the maintenance legs to run",
        lifecycle_wait_budget(),
        || {
            let ran = ops.maintenances.load(Ordering::SeqCst);
            if ran >= count {
                Observed::Met(())
            } else {
                Observed::pending(format!("{ran} of {count} maintenance legs ran"))
            }
        },
    )
    .unwrap_or_else(|failure| panic!("{failure}"));
}

/// Wait until the scheduler has marked `name` as owing maintenance.
fn wait_for_the_mark(host: &Host<Arc<FakeOps>>, name: &VaultName) {
    let entry = host.shared.entries.get(name).unwrap();
    wait_until(
        "the scheduler to mark the entry",
        lifecycle_wait_budget(),
        || {
            if entry.gate.lock().unwrap().claim.owes_maintenance() {
                Observed::Met(())
            } else {
                Observed::pending("the entry is not marked")
            }
        },
    )
    .unwrap_or_else(|failure| panic!("{failure}"));
}

/// **Owed maintenance starts at its wake, with no poll tick.** The fake owes
/// maintenance on a flag, as a commit owing semantic work does, and says so
/// by waking the scheduler: the leg runs although the watcher poll that used
/// to start it is a minute away.
#[test]
fn owed_maintenance_starts_at_its_wake_without_a_poll_tick() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    arrange_for(&ops.maintenance_due_at, &name);
    host.wake_background();
    wait_for_maintenances(&ops, 1);
    drop((lease, host));
}

/// **A clock that comes due starts maintenance at its deadline, with no poll
/// tick and no wake.** The fake's clock runs from the attach, and nothing but
/// the scheduler's own deadline is left to start the leg.
#[test]
fn a_due_clock_starts_maintenance_at_its_deadline_without_a_poll_tick() {
    let ops = Arc::new(FakeOps::default());
    *ops.maintenance_period.lock().unwrap() = Some(Duration::from_millis(30));
    let (host, _name, lease) = a_ready_vault(&ops);
    wait_for_maintenances(&ops, 1);
    assert!(
        host.scheduler_reading().deadlines >= 1,
        "the leg started without the scheduler reaching a deadline"
    );
    drop((lease, host));
}

/// **On a vault no leg touches, the second deadline after attach starts a
/// maintenance leg, as the first did.** The scheduler gives up the entry's
/// deadline while the leg it scheduled runs, so the leg's release is what
/// hands the entry's next deadline back to it.
#[test]
fn the_second_deadline_after_attach_starts_a_maintenance_leg_as_the_first_did() {
    let ops = Arc::new(FakeOps::default());
    *ops.maintenance_period.lock().unwrap() = Some(Duration::from_millis(30));
    let (host, _name, lease) = a_ready_vault(&ops);
    wait_for_maintenances(&ops, 3);
    drop((lease, host));
}

/// **A watcher poll never schedules a maintenance leg.** The entry owes
/// maintenance and nothing has announced it. A poll that reports nothing
/// takes the claim and gives it back with nothing scheduled, and a poll
/// carrying facts schedules their reconcile; only the scheduler's wake
/// starts the maintenance.
#[test]
fn a_watcher_poll_never_schedules_a_maintenance_leg() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    wait_for_no_claim(&host, &name);
    arrange_for(&ops.maintenance_due_at, &name);

    poll_watchers(&host.shared);
    wait_for_no_claim(&host, &name);
    assert_eq!(
        ops.maintenances.load(Ordering::SeqCst),
        0,
        "a poll that reported nothing scheduled maintenance"
    );

    ops.off_thread_fact_poll_batches.store(1, Ordering::SeqCst);
    poll_watchers(&host.shared);
    wait_until(
        "the polled facts to be reconciled",
        lifecycle_wait_budget(),
        || {
            if ops.reconciles.load(Ordering::SeqCst) >= 1 {
                Observed::Met(())
            } else {
                Observed::pending("no reconcile ran")
            }
        },
    )
    .unwrap_or_else(|failure| panic!("{failure}"));
    // The reconcile's end hands the claim on to the maintenance it found
    // due, so maintenance follows the facts' reconcile rather than leading.
    wait_for_maintenances(&ops, 1);
    let applies_or_not = ops.reconciled_batches.lock().unwrap().len();
    assert!(
        applies_or_not >= 1,
        "the poll's facts were not the first leg"
    );
    drop((lease, host));
}

/// **A due slice over a held claim starts at the claim's release**, here an
/// apply's, which hands on no reconcile and so asks no clock at its end. The
/// scheduler marks the entry while the apply holds the claim, and the
/// release of the marked claim wakes it.
#[test]
fn a_due_slice_over_a_held_claim_starts_at_the_claims_release() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    ops.block_apply.store(true, Ordering::SeqCst);
    let pending = host
        .admit_apply(&name, PlanDocument::resolved(an_empty_plan(&name)))
        .expect("admitted");
    wait_for_flag("apply_started", &ops.apply_started);

    arrange_for(&ops.maintenance_due_at, &name);
    host.wake_background();
    wait_for_the_mark(&host, &name);
    assert_eq!(
        ops.maintenances.load(Ordering::SeqCst),
        0,
        "maintenance ran under a claim the apply holds"
    );

    ops.apply_release.store(true, Ordering::SeqCst);
    pending.wait().expect("the apply applied");
    wait_for_maintenances(&ops, 1);
    drop((lease, host));
}

/// **The scheduler runs its ask at most once per wake or deadline it
/// receives, and a marked entry contributes no deadline.** A clock past due
/// over a claim a leg holds cannot wake the scheduler again and again for as
/// long as the leg runs.
#[test]
fn the_scheduler_asks_once_per_wake_or_deadline_and_never_spins_on_a_held_claim() {
    let ops = Arc::new(FakeOps::default());
    // The clock runs from the attach and comes due behind the claim the
    // apply below takes at once.
    *ops.maintenance_period.lock().unwrap() = Some(Duration::from_millis(300));
    let (host, name, lease) = a_ready_vault(&ops);
    ops.block_apply.store(true, Ordering::SeqCst);
    let pending = host
        .admit_apply(&name, PlanDocument::resolved(an_empty_plan(&name)))
        .expect("admitted");
    wait_for_flag("apply_started", &ops.apply_started);
    wait_for_the_mark(&host, &name);
    let marked = host.scheduler_reading();
    thread::sleep(Duration::from_millis(50));
    let held = host.scheduler_reading();
    assert_eq!(
        held, marked,
        "the scheduler asked again while nothing woke it and the entry was marked"
    );
    assert!(
        held.asks <= held.wakes + held.deadlines,
        "the scheduler asked more often than it was woken or reached a deadline: {held:?}"
    );

    ops.apply_release.store(true, Ordering::SeqCst);
    pending.wait().expect("the apply applied");
    wait_for_maintenances(&ops, 1);
    let ended = host.scheduler_reading();
    assert!(
        ended.asks <= ended.wakes + ended.deadlines,
        "the scheduler asked more often than it was woken or reached a deadline: {ended:?}"
    );
    drop((lease, host));
}

/// **A detached entry contributes no deadline and gets no maintenance leg.**
/// The release that left the entry unattached ended its clocks: the fake's
/// clock comes due again and again after the detach, and nothing runs or
/// wakes for it.
#[test]
fn a_detached_entry_contributes_no_deadline_and_gets_no_maintenance() {
    let ops = Arc::new(FakeOps::default());
    *ops.maintenance_period.lock().unwrap() = Some(Duration::from_millis(20));
    let (host, name, lease) = a_ready_vault(&ops);
    wait_for_maintenances(&ops, 1);
    drop(lease);
    host.reap_idle(Instant::now() + Duration::from_secs(61))
        .unwrap();
    wait_for_state(&host, &name, TrustState::Unattached);
    // A turn that begins after the release reads the entry with no coverage,
    // and every earlier turn has ended by then: what the scheduler sleeps
    // until from here is what the detached entry contributes.
    let before = host.scheduler_reading().asks;
    host.wake_background();
    wait_until(
        "the scheduler to ask after the release",
        lifecycle_wait_budget(),
        || {
            let reading = host.scheduler_reading();
            if reading.asks > before {
                Observed::Met(())
            } else {
                Observed::pending(format!("{reading:?}"))
            }
        },
    )
    .unwrap_or_else(|failure| panic!("{failure}"));
    let detached = || {
        (
            ops.maintenances.load(Ordering::SeqCst),
            host.scheduler_reading().deadlines,
        )
    };
    let at_detach = detached();
    *ops.maintenance_clock.lock().unwrap() = Some(Instant::now());
    thread::sleep(Duration::from_millis(80));
    assert_eq!(
        detached(),
        at_detach,
        "a detached entry kept a deadline or ran maintenance"
    );
    drop(host);
}

/// **An entry whose release is in flight gets no maintenance leg.** The
/// scheduler marks it rather than scheduling over a release, and the
/// release, which leaves it unattached, ends its clocks.
#[test]
fn an_entry_whose_release_is_in_flight_gets_no_maintenance() {
    let ops = Arc::new(FakeOps::default());
    // The clock runs from the attach and comes due while the release below
    // holds the entry's coverage.
    *ops.maintenance_period.lock().unwrap() = Some(Duration::from_millis(300));
    let (host, name, lease) = a_ready_vault(&ops);
    drop(lease);
    ops.block_detach.store(true, Ordering::SeqCst);
    host.reap_idle(Instant::now() + Duration::from_secs(61))
        .unwrap();
    wait_for_flag("detach_started", &ops.detach_started);
    wait_for_the_mark(&host, &name);
    ops.detach_release.store(true, Ordering::SeqCst);
    wait_for_state(&host, &name, TrustState::Unattached);
    let entry = host.shared.entries.get(&name).unwrap();
    wait_until(
        "the release's wake to clear the mark",
        lifecycle_wait_budget(),
        || {
            if entry.gate.lock().unwrap().claim.owes_maintenance() {
                Observed::pending("the entry is still marked")
            } else {
                Observed::Met(())
            }
        },
    )
    .unwrap_or_else(|failure| panic!("{failure}"));
    assert_eq!(ops.maintenances.load(Ordering::SeqCst), 0);
    drop(host);
}

/// **A maintenance leg ends within its budget plus one unit.** The fake owes
/// far more 10 ms units than a 30 ms budget holds, so the leg runs more than
/// one and stops at the first unit boundary past the budget.
#[test]
fn a_maintenance_leg_ends_within_its_budget_plus_one_unit() {
    let ops = Arc::new(FakeOps::default());
    ops.maintenance_units_owed.store(1_000, Ordering::SeqCst);
    *ops.maintenance_unit_cost.lock().unwrap() = Duration::from_millis(10);
    let (host, name, lease) = a_ready_vault_under(&ops, Duration::from_millis(30));
    arrange_for(&ops.maintenance_due_at, &name);
    host.wake_background();
    wait_for_maintenances(&ops, 1);
    wait_for_no_claim(&host, &name);
    let reading = host.maintenance_reading();
    assert_eq!(reading.legs, 1, "{reading:?}");
    // The first unit is the fake's maintain; three 10 ms units at most fill
    // a 30 ms budget, and the boundary past it ends the leg.
    assert!(
        (2..=4).contains(&reading.most_units_in_a_leg),
        "the leg ran {} units under a 30 ms budget of 10 ms units",
        reading.most_units_in_a_leg
    );
    drop((lease, host));
}

/// **A budget too small for any unit runs exactly one unit per leg.**
#[test]
fn a_budget_too_small_for_any_unit_runs_exactly_one_unit_per_leg() {
    let ops = Arc::new(FakeOps::default());
    ops.maintenance_units_owed.store(1_000, Ordering::SeqCst);
    let (host, name, lease) = a_ready_vault_under(&ops, Duration::ZERO);
    for leg in 1..=3 {
        arrange_for(&ops.maintenance_due_at, &name);
        host.wake_background();
        wait_for_maintenances(&ops, leg);
        wait_for_no_claim(&host, &name);
    }
    let reading = host.maintenance_reading();
    assert_eq!((reading.legs, reading.units), (3, 3), "{reading:?}");
    drop((lease, host));
}

/// A resolved plan of no transitions for `name`, which the fake applies as it
/// stands.
fn an_empty_plan(name: &VaultName) -> norn_wire::ResolvedPlan {
    norn_wire::ResolvedPlan::new(
        norn_wire::VaultAddress::name(name.clone()),
        norn_wire::RootIdentity::from_device_and_inode(1, 1),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
}

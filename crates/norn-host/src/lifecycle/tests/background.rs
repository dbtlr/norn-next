//! The background scheduler's lifecycle cases: claim-bound maintenance starts
//! at a wake or a deadline and never at a watcher poll, a due slice over a
//! held claim starts at the claim's release, an entry without coverage keeps
//! no clock, and a maintenance leg ends within its budget plus one unit.
//!
//! The hosts here run no ambient dispatcher tick inside a case — their watcher
//! poll is a minute away — so anything that ran did so without one.

use super::*;

/// A host over one vault whose dispatcher ticks once a minute, running each
/// maintenance leg under `budget`, holding the vault `Ready` and quiet: the
/// attach's leg has ended and the scheduler has answered its wakes.
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
    wait_for_a_quiet_entry(&host, &name);
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
    assert_eq!(
        ops.reconcile_and_maintenance_order.lock().unwrap()[..2],
        ["reconcile", "maintenance"],
        "the poll's reconcile was not the first leg"
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
/// The release that left the entry unattached ended its clocks: the clock the
/// entry last read off its coverage, still ahead at the detach, comes due
/// after it, and no deadline reaches the scheduler for it and nothing runs.
#[test]
fn a_detached_entry_contributes_no_deadline_and_gets_no_maintenance() {
    let ops = Arc::new(FakeOps::default());
    *ops.maintenance_period.lock().unwrap() = Some(Duration::from_millis(100));
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
    thread::sleep(Duration::from_millis(250));
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
/// one and stops at the first unit boundary past the budget: the leg's clock,
/// as the host records it, reaches the budget and stays inside the budget
/// plus the longest unit the leg ran.
#[test]
fn a_maintenance_leg_ends_within_its_budget_plus_one_unit() {
    let ops = Arc::new(FakeOps::default());
    ops.maintenance_units_owed.store(1_000, Ordering::SeqCst);
    *ops.maintenance_unit_cost.lock().unwrap() = Duration::from_millis(10);
    let budget = Duration::from_millis(30);
    let (host, name, lease) = a_ready_vault_under(&ops, budget);
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
    // The leg stopped for its budget, not for want of units, so its clock
    // reached the budget; it began no unit past it, so the clock stays
    // inside the budget plus one unit. The margin is the leg's own
    // bookkeeping between its last unit and the reading.
    let longest_unit = *ops.longest_maintenance_unit.lock().unwrap();
    assert!(
        reading.longest_leg >= budget,
        "the leg ended at {:?}, short of its {budget:?} budget",
        reading.longest_leg
    );
    assert!(
        reading.longest_leg < budget + longest_unit + Duration::from_millis(5),
        "the leg took {:?}, past its {budget:?} budget plus one unit of {longest_unit:?}",
        reading.longest_leg
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

/// **An unwind out of the scheduler's dispatch of the leg it scheduled leaves
/// every other vault scheduled.** A write left for alpha's next hold panics,
/// and it is left from inside the scheduler's own hold of alpha's gate, so
/// the hold it lands in is the dispatch of the leg that ask scheduled. The
/// unwind poisons alpha's gate and ends nothing else: beta's owed
/// maintenance still starts at its wake.
#[test]
fn an_unwind_in_the_schedulers_dispatch_leaves_every_other_vault_scheduled() {
    let ops = Arc::new(FakeOps::default());
    let alpha = VaultName::new("alpha").unwrap();
    let beta = VaultName::new("beta").unwrap();
    let host = Host::new(
        Roots::Absent(&[&alpha, &beta]).registry(),
        Arc::clone(&ops),
        no_ambient_poll_policy(2),
    )
    .unwrap();
    let leases = (host.demand(&alpha).unwrap(), host.demand(&beta).unwrap());
    for name in [&alpha, &beta] {
        wait_for_state(&host, name, TrustState::Ready);
        wait_for_a_quiet_entry(&host, name);
    }

    let entry = host.shared.entries.get(&alpha).unwrap();
    let gate = Arc::clone(&entry);
    *ops.when_maintenance_owed_is_read.lock().unwrap() = Some((
        alpha.clone(),
        Box::new(move || {
            gate.gate
                .run_under_the_next_hold(|_| panic!("a write left for the next hold unwinds"));
        }),
    ));
    arrange_for(&ops.maintenance_due_at, &alpha);
    host.wake_background();
    wait_until(
        "the scheduler's dispatch to unwind under alpha's gate",
        lifecycle_wait_budget(),
        || {
            if entry.gate.is_poisoned() {
                Observed::Met(())
            } else {
                Observed::pending("alpha's gate is not poisoned")
            }
        },
    )
    .unwrap_or_else(|failure| panic!("{failure}"));
    assert_eq!(ops.maintenances.load(Ordering::SeqCst), 0);

    arrange_for(&ops.maintenance_due_at, &beta);
    host.wake_background();
    wait_for_maintenances(&ops, 1);

    // The poison is cleared so the case can tear its fixture down: the
    // host's destruction takes every entry's gate.
    entry.gate.clear_poison();
    drop((leases, host));
}

/// **An unwind out of one vault's dispatch still asks the vaults after it in
/// the same turn.** Alpha and beta are both due by clock at one wake.
/// Alpha's dispatch unwinds under its gate, and beta, asked later in that same
/// turn, starts its leg with no further wake: an unwind caught only at the
/// turn would end the turn before beta's ask, and with no deadline to wait
/// for, beta would stall until some unrelated wake.
#[test]
fn an_unwind_in_one_vaults_dispatch_still_asks_the_vaults_after_it_in_that_turn() {
    let ops = Arc::new(FakeOps::default());
    let alpha = VaultName::new("alpha").unwrap();
    let beta = VaultName::new("beta").unwrap();
    let host = Host::new(
        Roots::Absent(&[&alpha, &beta]).registry(),
        Arc::clone(&ops),
        no_ambient_poll_policy(2),
    )
    .unwrap();
    let leases = (host.demand(&alpha).unwrap(), host.demand(&beta).unwrap());
    for name in [&alpha, &beta] {
        wait_for_state(&host, name, TrustState::Ready);
        wait_for_a_quiet_entry(&host, name);
    }

    let entry = host.shared.entries.get(&alpha).unwrap();
    let gate = Arc::clone(&entry);
    *ops.when_maintenance_owed_is_read.lock().unwrap() = Some((
        alpha.clone(),
        Box::new(move || {
            gate.gate
                .run_under_the_next_hold(|_| panic!("a write left for the next hold unwinds"));
        }),
    ));
    // One clock past due stands for both vaults, so both are due at the one
    // wake and neither has an owed flag a later ask could be woken for.
    *ops.maintenance_clock.lock().unwrap() = Some(Instant::now());
    host.wake_background();
    wait_for_maintenances(&ops, 1);
    assert!(
        entry.gate.is_poisoned(),
        "alpha's dispatch did not unwind, so the case did not test what it names"
    );

    // The poison is cleared so the case can tear its fixture down: the
    // host's destruction takes every entry's gate.
    entry.gate.clear_poison();
    drop((leases, host));
}

/// **A scheduler turn that unwinds whole ends no later turn.** The armed ask
/// unwinds before it reads any entry; the next wake is still asked, and the
/// maintenance it finds owed starts.
#[test]
fn a_scheduler_turn_that_unwinds_ends_no_later_turn() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    host.shared
        .panic_in_scheduler_ask
        .store(true, Ordering::SeqCst);
    host.wake_background();
    wait_until("the armed ask to unwind", lifecycle_wait_budget(), || {
        if host.shared.panic_in_scheduler_ask.load(Ordering::SeqCst) {
            Observed::pending("the armed ask has not run")
        } else {
            Observed::Met(())
        }
    })
    .unwrap_or_else(|failure| panic!("{failure}"));
    owe_maintenance(&host, &ops, &name);
    wait_for_maintenances(&ops, 1);
    drop((lease, host));
}

/// Whether the entry is marked, whether anything holds its claim, and how
/// many maintenance legs the fake has run, read under one hold.
fn mark_claim_and_legs(
    host: &Host<Arc<FakeOps>>,
    ops: &FakeOps,
    name: &VaultName,
) -> (bool, bool, usize) {
    let entry = host.shared.entries.get(name).unwrap();
    let state = entry.gate.lock().unwrap();
    (
        state.claim.owes_maintenance(),
        state.claim.is_held(),
        ops.maintenances.load(Ordering::SeqCst),
    )
}

/// **A stale arrival that drops its marker over a marked entry wakes the
/// scheduler.** The scheduler finds maintenance owed while a job the entry
/// has moved past still holds the gate, and marks the entry. That job's
/// arrival at a worker drops its marker and opens the gate, and that is the
/// only wake the case sends: the maintenance starts from it.
#[test]
fn a_stale_arrival_dropping_its_marker_wakes_the_scheduler_for_a_marked_entry() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    wait_for_no_claim(&host, &name);
    let entry = host.shared.entries.get(&name).unwrap();
    let stale = {
        let mut state = entry.gate.lock().unwrap();
        let stale = state.claim.epoch();
        // The job in the channel, and the entry moved on from it.
        state.claim.mark(Job::Reconcile(name.clone(), stale));
        state.claim.supersede();
        stale
    };
    owe_maintenance(&host, &ops, &name);
    wait_for_the_mark(&host, &name);
    assert_eq!(ops.maintenances.load(Ordering::SeqCst), 0);

    run_job(&host.shared, Job::Reconcile(name.clone(), stale));
    wait_for_maintenances(&ops, 1);
    drop((lease, host));
}

/// **No maintenance leg is scheduled over an entry that owes a heal rung**,
/// though its coverage is in hand and its claim is free: the entry is
/// marked, and the rung's release asks it again. The watcher's loss leaves
/// a recovery owed over coverage the entry keeps, and a demand runs it.
#[test]
fn an_entry_owing_a_rung_is_marked_and_its_maintenance_starts_at_the_rungs_release() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    *ops.terminal_poll.lock().unwrap() = Some(WatchError::Backend("lost".into()));
    poll_watchers(&host.shared);
    wait_for_state(&host, &name, backend_lost());
    wait_for_no_claim(&host, &name);
    {
        let entry = host.shared.entries.get(&name).unwrap();
        let state = entry.gate.lock().unwrap();
        assert!(state.owes_a_rung() && state.coverage.in_hand());
    }

    // A leg scheduled here would hold the claim beside the mark.
    arrange_for(&ops.block_maintenance_at, &name);
    owe_maintenance(&host, &ops, &name);
    wait_for_the_mark(&host, &name);
    assert_eq!(
        mark_claim_and_legs(&host, &ops, &name),
        (true, false, 0),
        "a maintenance leg was scheduled over an entry owing a rung"
    );

    *ops.block_maintenance_at.lock().unwrap() = None;
    let recovering = host.demand(&name).unwrap();
    wait_for_maintenances(&ops, 1);
    drop((recovering, lease, host));
}

/// **No maintenance leg is scheduled over an entry whose teardown is due.**
/// The case stands the teardown due over coverage in hand and a free claim,
/// the instant between the reap marking it due and scheduling it, and holds
/// it there: the entry is only marked.
#[test]
fn no_maintenance_leg_is_scheduled_over_an_entry_whose_teardown_is_due() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    drop(lease);
    wait_for_no_claim(&host, &name);
    {
        let entry = host.shared.entries.get(&name).unwrap();
        let mut state = entry.gate.lock().unwrap();
        state.detach_due = true;
        assert!(state.coverage.in_hand() && !state.claim.is_held());
    }

    // A leg scheduled here would hold the claim beside the mark.
    arrange_for(&ops.block_maintenance_at, &name);
    owe_maintenance(&host, &ops, &name);
    wait_for_the_mark(&host, &name);
    assert_eq!(
        mark_claim_and_legs(&host, &ops, &name),
        (true, false, 0),
        "a maintenance leg was scheduled over an entry whose teardown is due"
    );
    *ops.block_maintenance_at.lock().unwrap() = None;
    drop(host);
}

/// **A hold that returns the read stance from refuse to serve wakes the
/// scheduler**, with no attach and no other wake. Every route back to serve
/// in this host follows a recovery's rescan with a reconcile, whose end hands
/// owed maintenance on itself, so the case moves the stance at the gate: one
/// hold withdraws trust, maintenance comes to be owed with nothing announcing
/// it, and the hold that publishes `Ready` again is what starts it.
#[test]
fn a_hold_returning_the_stance_from_refuse_to_serve_wakes_the_scheduler() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    wait_for_no_claim(&host, &name);
    let entry = host.shared.entries.get(&name).unwrap();
    {
        let mut state = entry.gate.lock().unwrap();
        state.withdraw_trust(UntrustedReason::environmental_refusal("refused"));
        assert_eq!(state.read_stance(), ReadStance::Refuse);
    }

    arrange_for(&ops.maintenance_due_at, &name);
    // The entry's own count of stance wakes, not the scheduler's count of
    // every wake: another wake (a late release, a tick's poll) may land in
    // the same window and must not stand in for this one.
    let stance_wakes = entry.gate.lock().unwrap().stance_wakes;
    entry.gate.lock().unwrap().trust = TrustState::Ready;
    assert_eq!(
        entry.gate.lock().unwrap().stance_wakes,
        stance_wakes + 1,
        "the return to serve sent no wake"
    );
    wait_for_maintenances(&ops, 1);
    drop((lease, host));
}

/// **The commit of an engine delivery wakes the scheduler**, with no attach
/// and no other wake: the engine is ready to initialize from that commit.
/// Every leg that commits a delivery either installs coverage, which wakes
/// too, or follows a recovery's rescan with a reconcile, so the case runs the
/// publishing hold's commit itself, over coverage parked back by the leg the
/// case stands in for.
///
/// The wakes are counted inside that one hold. The commit's wake starts an
/// ask, which finds the arranged maintenance due and schedules a leg whose
/// release wakes the scheduler again, and on a busy machine all of that can
/// land before a reading taken after the hold. Every wake site but a case's
/// own [`Host::wake_background`] runs under an entry's gate, and this host
/// has one entry, so a count read on both sides of the commit, under the
/// gate, moves by the commit's wake alone.
#[test]
fn the_commit_of_an_engine_delivery_wakes_the_scheduler() {
    use crate::reload::EngineConfigReceiver;
    use crate::semantic::SemanticEngines;

    let scratch = std::env::temp_dir().join("norn-host-delivery-wake");
    let engines = SemanticEngines::new(
        norn_config::ConfigDirs::new(scratch.join("config"), scratch.join("data")).unwrap(),
    );
    let ops = Arc::new(FakeOps {
        semantic: Some(Arc::clone(&engines)),
        ..FakeOps::default()
    });
    let (host, name, lease) = a_ready_vault(&ops);
    engines.receive(&name, None);

    // What the engine owes once its delivery is committed.
    arrange_for(&ops.maintenance_due_at, &name);
    {
        let entry = host.shared.entries.get(&name).unwrap();
        let mut state = entry.gate.lock().unwrap();
        let wakes = host.scheduler_reading().wakes;
        let leg = state.claim.epoch();
        let coverage = state.coverage.take(leg).expect("the coverage is parked");
        record_active_declaration(&mut state, &*host.shared.ops, &name, &coverage);
        state.coverage.park_by(leg, coverage);
        assert!(
            state.delivered_engine.is_some(),
            "no delivery was committed"
        );
        assert_eq!(
            host.scheduler_reading().wakes,
            wakes + 1,
            "the delivery's commit sent no wake"
        );
    }
    wait_for_maintenances(&ops, 1);
    drop((lease, host));
}

/// **An ask that finds nothing due clears the mark, and the entry then
/// contributes its next deadline.** The entry stands marked over a clock
/// still ahead; the wake's ask clears the mark, and the clock's deadline is
/// what next asks it and starts the maintenance.
#[test]
fn an_ask_with_nothing_due_clears_the_mark_and_the_entry_contributes_its_deadline() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    wait_for_no_claim(&host, &name);
    let entry = host.shared.entries.get(&name).unwrap();
    {
        let mut state = entry.gate.lock().unwrap();
        state.claim.mark_owing_maintenance();
        *ops.maintenance_clock.lock().unwrap() = Some(Instant::now() + Duration::from_millis(500));
    }
    let deadlines = host.scheduler_reading().deadlines;
    host.wake_background();
    wait_until("the ask to clear the mark", lifecycle_wait_budget(), || {
        if entry.gate.lock().unwrap().claim.owes_maintenance() {
            Observed::pending("the entry is still marked")
        } else {
            Observed::Met(())
        }
    })
    .unwrap_or_else(|failure| panic!("{failure}"));
    assert_eq!(ops.maintenances.load(Ordering::SeqCst), 0);

    wait_for_maintenances(&ops, 1);
    assert!(
        host.scheduler_reading().deadlines > deadlines,
        "the maintenance started without the entry's deadline being reached"
    );
    drop((lease, host));
}

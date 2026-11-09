//! The background lane's lifecycle cases: owed lane work starts at a wake, a
//! background turn is bounded by the background turn budget with one unit as
//! its floor, one store's backlog cannot starve another's, no turn feeds
//! while its entry's read stance is refuse, a store left owed by a refused or
//! unwound unit is retried at a deadline, and damage a feed read meets is
//! judged for the store lifetime it read.
//!
//! The hosts here run no ambient dispatcher tick inside a case — their watcher
//! poll is a minute away — so anything that ran did so without one.

use super::*;

/// A host over `names`, whose dispatcher ticks once a minute, running each
/// background turn under `budget`, with every vault demanded, `Ready` and
/// quiet.
fn ready_vaults_under(
    ops: &Arc<FakeOps>,
    names: &[&VaultName],
    budget: Duration,
) -> (Host<Arc<FakeOps>>, Vec<DemandLease<Arc<FakeOps>>>) {
    let host = Host::new(
        Roots::Absent(names).registry(),
        Arc::clone(ops),
        LifecyclePolicy {
            background_turn_budget: budget,
            ..no_ambient_poll_policy(1)
        },
    )
    .unwrap();
    let leases = names
        .iter()
        .map(|name| {
            let lease = host.demand(name).unwrap();
            wait_for_state(&host, name, TrustState::Ready);
            lease
        })
        .collect();
    // Quiet once every vault is ready: a later vault's attach wakes the
    // scheduler, which asks every entry again.
    for name in names {
        wait_for_a_quiet_entry(&host, name);
    }
    (host, leases)
}

/// Owe `units` lane units on `name`.
fn owe(ops: &FakeOps, name: &VaultName, units: usize) {
    ops.lane_owed.lock().unwrap().insert(name.clone(), units);
}

/// Wait until the lane has run `count` units across every vault.
fn wait_for_lane_units(ops: &FakeOps, count: usize) {
    wait_until("the lane units to run", lifecycle_wait_budget(), || {
        let ran = ops.lane_ran.lock().unwrap().len();
        if ran >= count {
            Observed::Met(())
        } else {
            Observed::pending(format!("{ran} of {count} lane units ran"))
        }
    })
    .unwrap_or_else(|failure| panic!("{failure}"));
}

/// **Owed lane work starts at its wake, with no poll tick.** The vault owes
/// three units, and nothing but the scheduler's wake announces them: the
/// lane runs all three although the watcher poll is a minute away, and no
/// maintenance leg carries any of them.
#[test]
fn owed_lane_work_starts_at_its_wake_without_a_poll_tick() {
    let ops = Arc::new(FakeOps::default());
    let name = VaultName::new("notes").unwrap();
    let (host, leases) = ready_vaults_under(&ops, &[&name], crate::BACKGROUND_TURN_BUDGET);
    owe(&ops, &name, 3);
    host.wake_background();
    wait_for_lane_units(&ops, 3);
    assert_eq!(host.lane_reading().units, 3, "{:?}", host.lane_reading());
    assert_eq!(ops.maintenances.load(Ordering::SeqCst), 0);
    drop((leases, host));
}

/// **A background turn ends within its budget plus one unit.** The vault owes
/// far more 10 ms units than a 30 ms budget holds. Each turn stops at the
/// first unit boundary at or past the budget, checked against the units'
/// own measured durations rather than a count fixed for an unloaded machine:
///
/// - a turn that is not the last reached the budget on its own clock, so a
///   lane that stopped early (one unit per turn while units fit) fails;
/// - the units before its last one finished inside the budget, so a lane that
///   ran past the boundary fails;
/// - the budget check that let its last unit through read a clock still
///   inside the budget, so a lane that took another unit after the budget
///   was spent fails. That is the reading the decision itself used, so no
///   scheduling delay after it can move it past a boundary the lane honoured;
///   a turn's first unit runs with no check and reads zero.
///
/// A runner that oversleeps a 10 ms unit past the budget ends every turn
/// after one unit, which these properties allow.
#[test]
fn a_background_turn_ends_within_its_budget_plus_one_unit() {
    let ops = Arc::new(FakeOps::default());
    *ops.lane_unit_cost.lock().unwrap() = Duration::from_millis(10);
    let budget = Duration::from_millis(30);
    let name = VaultName::new("notes").unwrap();
    let (host, leases) = ready_vaults_under(&ops, &[&name], budget);
    owe(&ops, &name, 12);
    host.wake_background();
    wait_for_lane_units(&ops, 12);
    wait_until(
        "the last turn to be recorded",
        lifecycle_wait_budget(),
        || {
            let reading = host.lane_reading();
            if reading.units == 12 {
                Observed::Met(())
            } else {
                Observed::pending(format!("{reading:?}"))
            }
        },
    )
    .unwrap_or_else(|failure| panic!("{failure}"));
    let reading = host.lane_reading();
    let turns = host.shared.lane.turn_log();
    let took = ops.lane_unit_took.lock().unwrap().clone();
    assert_eq!(turns.len() as u64, reading.turns, "{reading:?}");
    assert_eq!(took.len(), 12, "{took:?}");
    let mut next_unit = 0;
    for (index, turn) in turns.iter().copied().enumerate() {
        let (units, clock) = (turn.units as usize, turn.took);
        let mine = &took[next_unit..next_unit + units];
        next_unit += units;
        let before_last: Duration = mine[..units - 1].iter().sum();
        let describe = format!("turn {index} ran {units} units {mine:?} in {clock:?}; {turns:?}");
        // The lane checks the budget only between units, so every unit but
        // the last began with the clock still inside it.
        assert!(before_last < budget, "past the boundary: {describe}");
        if index + 1 < turns.len() {
            // The store was still owed, so only the budget ended the turn.
            assert!(clock >= budget, "stopped short of the budget: {describe}");
        }
        assert!(
            turn.last_began < budget,
            "let a unit through past the budget at {:?}: {describe}",
            turn.last_began
        );
    }
    drop((leases, host));
}

/// **A budget too small for any unit runs exactly one unit per background
/// turn**, which the unit counter beside the turn's clock shows.
#[test]
fn a_budget_too_small_for_any_unit_runs_exactly_one_unit_per_turn() {
    let ops = Arc::new(FakeOps::default());
    let name = VaultName::new("notes").unwrap();
    let (host, leases) = ready_vaults_under(&ops, &[&name], Duration::ZERO);
    owe(&ops, &name, 5);
    host.wake_background();
    wait_for_lane_units(&ops, 5);
    wait_until(
        "the last turn to be recorded",
        lifecycle_wait_budget(),
        || {
            let reading = host.lane_reading();
            if reading.units == 5 {
                Observed::Met(())
            } else {
                Observed::pending(format!("{reading:?}"))
            }
        },
    )
    .unwrap_or_else(|failure| panic!("{failure}"));
    let reading = host.lane_reading();
    assert_eq!(
        (reading.turns, reading.most_units_in_a_turn),
        (5, 1),
        "{reading:?}"
    );
    drop((leases, host));
}

/// **One store with a large backlog cannot starve another.** The large
/// store owes fifty units and the small one three, and each turn runs one
/// unit: a store still owed goes to the back of the set, so the turns
/// alternate and the small backlog is done within its first three turns
/// rather than after the large one. The large store's first unit is held
/// until the small store is in the set: a turn costs microseconds here, and
/// fairness is among the stores in the set, so without the hold the lane can
/// drain the large store before the scheduler's ask reaches the small one.
#[test]
fn one_store_with_a_large_backlog_cannot_starve_another() {
    let ops = Arc::new(FakeOps::default());
    let large = VaultName::new("large").unwrap();
    let small = VaultName::new("small").unwrap();
    let (host, leases) = ready_vaults_under(&ops, &[&large, &small], Duration::ZERO);
    ops.block_lane_unit.store(true, Ordering::SeqCst);
    ops.lane_owed.lock().unwrap().insert(large.clone(), 50);
    host.wake_background();
    wait_until(
        "the large store's first unit to start",
        lifecycle_wait_budget(),
        || {
            if ops.lane_unit_started.load(Ordering::SeqCst) {
                Observed::Met(())
            } else {
                Observed::pending("no unit has started")
            }
        },
    )
    .unwrap_or_else(|failure| panic!("{failure}"));

    // The small store comes to be owed while the large store's unit runs.
    // One scheduler thread asks in turn, so once a second ask has begun the
    // first, which put the small store in the set, has ended.
    ops.lane_owed.lock().unwrap().insert(small.clone(), 3);
    for _ in 0..2 {
        let asks = host.scheduler_reading().asks;
        host.wake_background();
        wait_until("the scheduler to ask", lifecycle_wait_budget(), || {
            if host.scheduler_reading().asks > asks {
                Observed::Met(())
            } else {
                Observed::pending("the scheduler has not asked")
            }
        })
        .unwrap_or_else(|failure| panic!("{failure}"));
    }
    ops.lane_unit_release.store(true, Ordering::SeqCst);

    wait_for_lane_units(&ops, 53);
    let ran = ops.lane_ran.lock().unwrap().clone();
    let small_at: Vec<usize> = ran
        .iter()
        .enumerate()
        .filter(|(_, name)| **name == small)
        .map(|(at, _)| at)
        .collect();
    assert_eq!(small_at.len(), 3);
    assert!(
        small_at.iter().all(|at| *at < 7),
        "the small store waited behind the large one: its units ran at {small_at:?}"
    );
    drop((leases, host));
}

/// Publish a damage verdict over `name`: its read stance is refuse.
fn publish_damage(host: &Host<Arc<FakeOps>>, name: &VaultName) {
    let entry = host.shared.entries.get(name).unwrap();
    let mut state = entry.gate.lock().unwrap();
    state.withdraw_trust(UntrustedReason::store_damaged_rebuilding(
        "the store is damaged",
    ));
    assert_eq!(state.read_stance(), ReadStance::Refuse);
}

/// **No background turn feeds while its entry's stance is refuse, and a
/// published damage verdict pauses the turns until the entry serves again.**
/// A turn in flight when the verdict is published stops at its next unit
/// boundary; the wakes that follow put nothing of that store on the lane,
/// while the vault beside it, serving, is fed; and the publication that
/// returns the entry to serve wakes the scheduler, which resumes the store.
#[test]
fn no_background_turn_feeds_while_the_entry_refuses() {
    let ops = Arc::new(FakeOps::default());
    let damaged = VaultName::new("damaged").unwrap();
    let serving = VaultName::new("serving").unwrap();
    let (host, leases) =
        ready_vaults_under(&ops, &[&damaged, &serving], crate::BACKGROUND_TURN_BUDGET);
    ops.block_lane_unit.store(true, Ordering::SeqCst);
    owe(&ops, &damaged, 5);
    host.wake_background();
    wait_for_marker(
        "the damaged store's first unit",
        &ops.lane_unit_started,
        lifecycle_wait_budget(),
    );

    // The verdict lands while that unit is in flight.
    publish_damage(&host, &damaged);
    ops.lane_unit_release.store(true, Ordering::SeqCst);
    owe(&ops, &serving, 2);
    host.wake_background();
    wait_for_lane_units(&ops, 3);
    let ran = ops.lane_ran.lock().unwrap().clone();
    assert_eq!(
        ran.iter().filter(|name| **name == damaged).count(),
        1,
        "a turn fed past a published damage verdict: {ran:?}"
    );
    assert_eq!(ran.iter().filter(|name| **name == serving).count(), 2);
    let owed = ops.lane_owed.lock().unwrap().get(&damaged).copied();
    assert_eq!(owed, Some(4), "the damaged store's work is still owed");

    // The rebuild's publication returns the entry to serve.
    {
        let entry = host.shared.entries.get(&damaged).unwrap();
        entry.gate.lock().unwrap().trust = TrustState::Ready;
    }
    wait_for_lane_units(&ops, 7);
    let ran = ops.lane_ran.lock().unwrap().clone();
    assert_eq!(ran.iter().filter(|name| **name == damaged).count(), 5);
    drop((leases, host));
}

/// Wait until `count` lane units were refused, unwound or met damage.
fn wait_for_units_not_run(ops: &FakeOps, count: usize) {
    wait_until("the lane units to fail", lifecycle_wait_budget(), || {
        let failed = ops.lane_units_not_run.load(Ordering::SeqCst);
        if failed >= count {
            Observed::Met(())
        } else {
            Observed::pending(format!("{failed} of {count} lane units failed"))
        }
    })
    .unwrap_or_else(|failure| panic!("{failure}"));
}

/// **A unit its environment refused is retried at a deadline, with no wake.**
/// The refusal ends the turn and leaves the work owed; nothing but the
/// scheduler's own retry deadline asks the store again — the watcher poll is
/// a minute away and no leg commits — and the retried turn runs the unit.
#[test]
fn a_refused_unit_is_retried_at_a_deadline_without_a_wake() {
    let ops = Arc::new(FakeOps::default());
    let name = VaultName::new("notes").unwrap();
    let (host, leases) = ready_vaults_under(&ops, &[&name], crate::BACKGROUND_TURN_BUDGET);
    ops.refuse_lane_unit.store(true, Ordering::SeqCst);
    owe(&ops, &name, 1);
    host.wake_background();
    wait_for_units_not_run(&ops, 1);
    let wakes = host.scheduler_reading().wakes;
    wait_for_lane_units(&ops, 1);
    assert_eq!(
        host.scheduler_reading().wakes,
        wakes,
        "a wake, not the retry deadline, asked the store again"
    );
    assert!(host.scheduler_reading().deadlines >= 1);
    drop((leases, host));
}

/// An ask finishing after a refusal cannot overwrite the lane's retry deadline.
#[test]
fn an_ask_finishing_after_a_refusal_keeps_its_retry_deadline() {
    let ops = Arc::new(FakeOps::default());
    let name = VaultName::new("notes").unwrap();
    let (host, leases) = ready_vaults_under(&ops, &[&name], crate::BACKGROUND_TURN_BUDGET);
    let (entered, entering) = std::sync::mpsc::channel();
    let (resume, resumed) = std::sync::mpsc::channel();
    host.shared.scheduler.run_before_next_sleep(move || {
        entered.send(()).unwrap();
        resumed.recv_timeout(Duration::from_secs(10)).unwrap();
    });
    let began = Instant::now();
    ops.refuse_lane_unit.store(true, Ordering::SeqCst);
    owe(&ops, &name, 1);
    let turns = host.lane_reading().turns;
    host.wake_background();
    entering.recv_timeout(Duration::from_secs(10)).unwrap();
    wait_for_units_not_run(&ops, 1);
    wait_for_lane_turns(&host, turns + 1);
    let wakes = host.scheduler_reading().wakes;
    let deadlines = host.scheduler_reading().deadlines;
    assert!(began.elapsed() < super::super::lane::BACKGROUND_RETRY_AFTER);
    resume.send(()).unwrap();
    wait_for_lane_units(&ops, 1);
    assert_eq!(host.scheduler_reading().wakes, wakes);
    assert!(host.scheduler_reading().deadlines > deadlines);
    drop((leases, host));
}

/// **A store whose turn unwound is queued again.** The unwind is caught at
/// the lane's seam and takes none of the work owed; the store is asked again
/// at the retry deadline, with no wake, and its unit runs.
#[test]
fn a_store_whose_turn_unwound_is_queued_again() {
    let ops = Arc::new(FakeOps::default());
    let name = VaultName::new("notes").unwrap();
    let (host, leases) = ready_vaults_under(&ops, &[&name], crate::BACKGROUND_TURN_BUDGET);
    ops.panic_in_lane_unit.store(true, Ordering::SeqCst);
    owe(&ops, &name, 1);
    host.wake_background();
    wait_for_units_not_run(&ops, 1);
    let wakes = host.scheduler_reading().wakes;
    wait_for_lane_units(&ops, 1);
    assert_eq!(
        host.scheduler_reading().wakes,
        wakes,
        "a wake, not the retry deadline, asked the store again"
    );
    drop((leases, host));
}

/// Record that `name`'s entry stands on the store lifetime `epoch`, as a leg
/// committing lane-1 work records it.
fn stand_on(host: &Host<Arc<FakeOps>>, name: &VaultName, epoch: &str) {
    let entry = host.shared.entries.get(name).unwrap();
    entry.gate.lock().unwrap().store_reading = Some(norn_store::StoreReading::of(epoch, 3));
}

/// The state a feed read's damage verdict publishes.
fn damaged_by_the_feed() -> TrustState {
    TrustState::untrusted(UntrustedReason::store_damaged_rebuilding(
        "the feed read met damage",
    ))
}

/// Wait until the lane has recorded `count` turns: the judgment of what a
/// turn's units met runs before its turn is recorded.
fn wait_for_lane_turns(host: &Host<Arc<FakeOps>>, count: u64) {
    wait_until("the lane turns to end", lifecycle_wait_budget(), || {
        let reading = host.lane_reading();
        if reading.turns >= count {
            Observed::Met(())
        } else {
            Observed::pending(format!("{reading:?}"))
        }
    })
    .unwrap_or_else(|failure| panic!("{failure}"));
}

/// **Damage a feed read meets is judged for the store lifetime it read, and
/// schedules the rebuild once.** The entry stands on the lifetime the unit
/// read: the verdict is published and the rebuild runs, and it runs once.
#[test]
fn damage_a_feed_read_meets_schedules_the_rebuild_once() {
    let ops = Arc::new(FakeOps::default());
    let name = VaultName::new("notes").unwrap();
    let (host, leases) = ready_vaults_under(&ops, &[&name], crate::BACKGROUND_TURN_BUDGET);
    stand_on(&host, &name, "lifetime-a");
    ops.block_rebuild.store(true, Ordering::SeqCst);
    *ops.lane_unit_damage.lock().unwrap() = Some("lifetime-a".to_string());
    owe(&ops, &name, 1);
    host.wake_background();

    wait_for_flag("rebuild_started", &ops.rebuild_started);
    assert_eq!(host.state(&name), answered(damaged_by_the_feed()));
    ops.rebuild_release.store(true, Ordering::SeqCst);
    wait_for_state(&host, &name, TrustState::Ready);
    wait_for_lane_units(&ops, 1);
    assert_eq!(ops.rebuilds.load(Ordering::SeqCst), 1);
    drop((leases, host));
}

/// **A verdict over a store lifetime the entry no longer stands on schedules
/// nothing.** The unit read a lifetime a rebuild has since replaced: its turn
/// ends with the entry still serving and no rebuild scheduled.
#[test]
fn damage_a_feed_read_meets_in_an_old_lifetime_schedules_nothing() {
    let ops = Arc::new(FakeOps::default());
    let name = VaultName::new("notes").unwrap();
    let (host, leases) = ready_vaults_under(&ops, &[&name], crate::BACKGROUND_TURN_BUDGET);
    stand_on(&host, &name, "lifetime-b");
    *ops.lane_unit_damage.lock().unwrap() = Some("lifetime-a".to_string());
    owe(&ops, &name, 1);
    host.wake_background();

    wait_for_lane_turns(&host, 1);
    assert_eq!(host.state(&name), answered(TrustState::Ready));
    assert_eq!(ops.rebuilds.load(Ordering::SeqCst), 0);
    drop((leases, host));
}

/// **Where the entry records no store reading, the ops answer for the
/// lifetime.** A host that composes no engine records none, and its store's
/// verification still reads one lifetime: the ops' own binding of it decides.
/// A verdict over the lifetime the ops hold schedules the rebuild once; one
/// over another lifetime schedules nothing.
#[test]
fn damage_a_unit_meets_is_judged_by_the_ops_lifetime_where_the_entry_records_none() {
    for (held, rebuilds) in [("lifetime-a", 1), ("lifetime-b", 0)] {
        let ops = Arc::new(FakeOps::default());
        let name = VaultName::new("notes").unwrap();
        let (host, leases) = ready_vaults_under(&ops, &[&name], crate::BACKGROUND_TURN_BUDGET);
        *ops.lifetime_held_by_the_ops.lock().unwrap() = Some(held.to_string());
        *ops.lane_unit_damage.lock().unwrap() = Some("lifetime-a".to_string());
        owe(&ops, &name, 1);
        host.wake_background();

        wait_for_lane_turns(&host, 1);
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.rebuilds.load(Ordering::SeqCst), rebuilds, "held {held}");
        drop((leases, host));
    }
}

/// **A verdict a unit in flight brings back after its entry's stance moved to
/// refuse schedules nothing.** The unit read the lifetime the entry still
/// stands on, and no rebuild is owed or held: the entry's trust was withdrawn
/// for an environmental refusal while the unit ran, so only the refusing
/// stance tells the judgment that the verdict is not this entry's to act on.
#[test]
fn damage_a_feed_read_meets_after_the_stance_refuses_schedules_nothing() {
    let ops = Arc::new(FakeOps::default());
    let name = VaultName::new("notes").unwrap();
    let (host, leases) = ready_vaults_under(&ops, &[&name], crate::BACKGROUND_TURN_BUDGET);
    stand_on(&host, &name, "lifetime-a");
    ops.block_lane_unit.store(true, Ordering::SeqCst);
    *ops.lane_unit_damage.lock().unwrap() = Some("lifetime-a".to_string());
    owe(&ops, &name, 1);
    host.wake_background();
    wait_for_flag("lane_unit_started", &ops.lane_unit_started);
    let refused = TrustState::untrusted(UntrustedReason::environmental_refusal(
        "the environment refused the store",
    ));
    {
        let entry = host.shared.entries.get(&name).unwrap();
        let mut state = entry.gate.lock().unwrap();
        state.trust = refused.clone();
        assert_eq!(state.read_stance(), ReadStance::Refuse);
        assert!(!state.owes_a_rung());
    }

    ops.lane_unit_release.store(true, Ordering::SeqCst);
    wait_for_lane_turns(&host, 1);
    assert_eq!(host.state(&name), answered(refused));
    assert_eq!(ops.rebuilds.load(Ordering::SeqCst), 0);
    let entry = host.shared.entries.get(&name).unwrap();
    assert!(
        entry
            .gate
            .lock()
            .unwrap()
            .damage_met_under_a_claim
            .is_none(),
        "the verdict was carried for a later publication"
    );
    drop((leases, host));
}

/// **A verdict a unit in flight brings back while the rebuild runs schedules
/// no second rebuild.** The unit read the lifetime the entry still records —
/// a rebuild in flight has not yet recorded the store that replaces it — so
/// only the entry's refusing stance tells the verdict is stale: the rebuild
/// ends at `Ready`, and it is the only one.
#[test]
fn damage_a_feed_read_meets_during_a_rebuild_schedules_no_second_rebuild() {
    let ops = Arc::new(FakeOps::default());
    let name = VaultName::new("notes").unwrap();
    let (host, leases) = ready_vaults_under(&ops, &[&name], crate::BACKGROUND_TURN_BUDGET);
    stand_on(&host, &name, "lifetime-a");
    ops.block_lane_unit.store(true, Ordering::SeqCst);
    *ops.lane_unit_damage.lock().unwrap() = Some("lifetime-a".to_string());
    owe(&ops, &name, 1);
    host.wake_background();
    wait_for_flag("lane_unit_started", &ops.lane_unit_started);

    // A read meets the damage first, and its rebuild is under way.
    ops.block_rebuild.store(true, Ordering::SeqCst);
    let hold = host
        .begin_read(&name)
        .expect("a ready entry answers a read");
    host.withdraw_for_read_damage(&hold, "the store is damaged".to_string());
    drop(hold);
    wait_for_flag("rebuild_started", &ops.rebuild_started);

    ops.lane_unit_release.store(true, Ordering::SeqCst);
    wait_for_lane_turns(&host, 1);
    ops.rebuild_release.store(true, Ordering::SeqCst);
    wait_for_state(&host, &name, TrustState::Ready);
    assert_eq!(
        ops.rebuilds.load(Ordering::SeqCst),
        1,
        "a stale verdict scheduled a second rebuild"
    );
    let entry = host.shared.entries.get(&name).unwrap();
    assert!(
        entry
            .gate
            .lock()
            .unwrap()
            .damage_met_under_a_claim
            .is_none(),
        "a stale verdict is carried to the end of the rebuild"
    );
    drop((leases, host));
}

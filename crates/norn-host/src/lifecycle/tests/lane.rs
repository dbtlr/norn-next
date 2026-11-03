//! The background lane's lifecycle cases: owed lane work starts at a wake, a
//! background turn is bounded by the background turn budget with one unit as
//! its floor, one store's backlog cannot starve another's, and no turn feeds
//! while its entry's read stance is refuse.
//!
//! The hosts here run no ambient dispatcher tick inside a case — their watcher
//! poll is a minute away — so anything that ran did so without one.

use super::*;

/// A host over `names`, whose dispatcher ticks once a minute, running each
/// background turn under `budget`, with every vault demanded and `Ready`.
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
/// far more 10 ms units than a 30 ms budget holds, so a turn runs more than
/// one and stops at the first unit boundary past the budget: its clock, as
/// the lane records it, reaches the budget and stays inside the budget plus
/// the longest unit it ran. The store, still owed, takes the next turn.
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
    assert!(reading.turns >= 3, "{reading:?}");
    assert!(
        (2..=4).contains(&reading.most_units_in_a_turn),
        "a turn ran {} units under a 30 ms budget of 10 ms units",
        reading.most_units_in_a_turn
    );
    let longest_unit = *ops.longest_lane_unit.lock().unwrap();
    assert!(
        reading.longest_turn >= budget,
        "the longest turn ended at {:?}, short of its {budget:?} budget",
        reading.longest_turn
    );
    assert!(
        reading.longest_turn < budget + longest_unit + Duration::from_millis(5),
        "a turn took {:?}, past its {budget:?} budget plus one unit of {longest_unit:?}",
        reading.longest_turn
    );
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

/// **One store with a large backlog cannot starve another.** Both owe work
/// at the same wake, one fifty units and the other three, and each turn runs
/// one unit: the store still owed goes to the back of the set, so the turns
/// alternate and the small backlog is done within its first three turns
/// rather than after the large one.
#[test]
fn one_store_with_a_large_backlog_cannot_starve_another() {
    let ops = Arc::new(FakeOps::default());
    let large = VaultName::new("large").unwrap();
    let small = VaultName::new("small").unwrap();
    let (host, leases) = ready_vaults_under(&ops, &[&large, &small], Duration::ZERO);
    {
        let mut owed = ops.lane_owed.lock().unwrap();
        owed.insert(large.clone(), 50);
        owed.insert(small.clone(), 3);
    }
    host.wake_background();
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
        small_at.iter().all(|at| *at < 6),
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

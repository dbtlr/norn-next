//! The reload admission's lifecycle cases: a reload of either mode waits
//! behind the vault's own routine work — a watcher poll or a maintenance leg —
//! and refuses at once on explicit work.
//!
//! The hosts here run no ambient dispatcher tick inside a case, so the poll or
//! the leg a case holds open is the one the case drove, and a waiting reload
//! is read off the entry's claim rather than inferred from a reply that has
//! not come yet.

use super::*;

/// What a reload's asker is answered.
type Answer = Result<ReloadJudgment, ReloadRefusal>;

/// The answer `vault/reload-busy` renders from: the entry still publishes
/// `Ready` and explicit work holds it.
fn busy() -> Answer {
    Err(ReloadRefusal::Unavailable(Demand::State(TrustState::Ready)))
}

/// The answer a reload that ran over the fake's attachment gets.
fn judged() -> Answer {
    Ok(fake_judgment(ReloadOutcome::ConfigOnly))
}

/// A host the case shares with the thread a reload is asked on.
type SharedHost = Arc<Host<Arc<FakeOps>>>;

/// A host over one vault with no ambient tick, holding the vault `Ready` and
/// quiet over an attachment that holds a reload.
fn a_ready_vault(ops: &Arc<FakeOps>) -> (SharedHost, VaultName, DemandLease<Arc<FakeOps>>) {
    ops.reload_supported.store(true, Ordering::SeqCst);
    let name = VaultName::new("notes").unwrap();
    let host = Arc::new(host_without_ambient_polling(
        Arc::clone(ops),
        Roots::Absent(&[&name]),
        1,
    ));
    let lease = host.demand(&name).unwrap();
    wait_for_state(&host, &name, TrustState::Ready);
    wait_for_a_quiet_entry(&host, &name);
    (host, name, lease)
}

/// A resolved plan of no transitions for `name`: the fake applies it as it
/// stands.
fn a_plan(name: &VaultName) -> PlanDocument {
    PlanDocument::resolved(norn_wire::ResolvedPlan::new(
        norn_wire::VaultAddress::name(name.clone()),
        norn_wire::RootIdentity::from_device_and_inode(1, 1),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    ))
}

/// Hold a maintenance leg open over `name`, answering once it runs.
fn hold_a_maintenance_leg(host: &Host<Arc<FakeOps>>, ops: &FakeOps, name: &VaultName) {
    // The block stands before the owe: the owe wakes the background
    // scheduler, which may start the leg before the next statement runs.
    arrange_for(&ops.block_maintenance_at, name);
    owe_maintenance(host, ops, name);
    wait_for_flag("maintenance_started", &ops.maintenance_started);
}

/// Hold a watcher poll open over the host's one vault, on a thread of its
/// own, answering once the poll holds the claim.
fn hold_a_poll(host: &Host<Arc<FakeOps>>, ops: &FakeOps) -> thread::JoinHandle<()> {
    ops.block_poll.store(true, Ordering::SeqCst);
    let shared = Arc::clone(&host.shared);
    let poll = thread::spawn(move || poll_watchers(&shared));
    wait_for_flag("poll_started", &ops.poll_started);
    poll
}

/// Ask a reload of `name` — a dry run where `dry_run` — on a thread of its
/// own, and answer its receiver once the entry records it waiting. A reload
/// answered at once fails the case with what it was answered.
fn a_waiting_reload(host: &SharedHost, name: &VaultName, dry_run: bool) -> mpsc::Receiver<Answer> {
    let (sent, answer) = mpsc::channel();
    let asking = Arc::clone(host);
    let asked = name.clone();
    thread::spawn(move || {
        let _ = sent.send(if dry_run {
            asking.judge_reload(&asked)
        } else {
            asking.reload(&asked)
        });
    });
    let entry = host.shared.entries.get(name).expect("the vault is served");
    wait_until("the reload to wait", lifecycle_wait_budget(), || {
        if let Ok(answered) = answer.try_recv() {
            return Observed::Met(Err(answered));
        }
        if entry.gate.lock().unwrap().claim.reload_waits() {
            Observed::Met(Ok(()))
        } else {
            Observed::pending("the reload is not recorded waiting")
        }
    })
    .unwrap_or_else(|failure| panic!("{failure}"))
    .unwrap_or_else(|answered| panic!("the reload was answered at once: {answered:?}"));
    answer
}

/// The answer a waiting reload's asker got, on the one budget.
fn answer_of(answer: &mpsc::Receiver<Answer>) -> Answer {
    wait_until(
        "the reload to be answered",
        lifecycle_wait_budget(),
        || match answer.try_recv() {
            Ok(answered) => Observed::Met(answered),
            Err(_) => Observed::pending("no answer yet"),
        },
    )
    .unwrap_or_else(|failure| panic!("{failure}"))
}

/// Wait for the fake to have run `expected` maintenance legs.
fn wait_for_maintenances(ops: &FakeOps, expected: usize) {
    wait_until(
        "the maintenance legs to run",
        lifecycle_wait_budget(),
        || {
            let ran = ops.maintenances.load(Ordering::SeqCst);
            if ran == expected {
                Observed::Met(())
            } else {
                Observed::pending(format!("{ran} maintenance legs so far"))
            }
        },
    )
    .unwrap_or_else(|failure| panic!("{failure}"));
}

/// **A reload asked while a maintenance leg runs waits for the leg and then
/// runs**, in either mode.
#[test]
fn a_reload_asked_while_a_maintenance_leg_runs_waits_and_runs_after_it() {
    for dry_run in [false, true] {
        let ops = Arc::new(FakeOps::default());
        let (host, name, lease) = a_ready_vault(&ops);
        hold_a_maintenance_leg(&host, &ops, &name);

        let answer = a_waiting_reload(&host, &name, dry_run);
        ops.maintenance_release.store(true, Ordering::SeqCst);

        assert_eq!(answer_of(&answer), judged(), "dry run: {dry_run}");
        assert_eq!(ops.maintenances.load(Ordering::SeqCst), 1);
        drop((lease, host));
    }
}

/// **A reload asked while a watcher poll holds the entry waits for the poll
/// and then runs.**
#[test]
fn a_reload_asked_while_a_watcher_poll_holds_the_claim_waits_and_runs_after_it() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    let poll = hold_a_poll(&host, &ops);

    let answer = a_waiting_reload(&host, &name, false);
    ops.poll_release.store(true, Ordering::SeqCst);

    assert_eq!(answer_of(&answer), judged());
    poll.join().unwrap();
    drop((lease, host));
}

/// **A reload over a maintenance leg scheduled and not yet running takes the
/// claim from it, and the pass stays due**: the scheduler's mark wakes it at
/// the reload's release, and the leg runs then.
#[test]
fn a_reload_over_a_scheduled_maintenance_leg_takes_the_claim_and_the_pass_runs_after() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    let entry = host.shared.entries.get(&name).unwrap();
    {
        let mut state = entry.gate.lock().unwrap();
        state
            .claim
            .schedule(|epoch| Job::Maintenance(name.clone(), epoch));
        state.claim.mark_owing_maintenance();
    }
    arrange_for(&ops.maintenance_due_at, &name);

    assert_eq!(host.reload(&name), judged());
    wait_for_maintenances(&ops, 1);
    drop((entry, lease, host));
}

/// **While a reload waits, a maintenance leg that ends with maintenance still
/// due hands the claim to the reload rather than to another maintenance
/// leg**; the pass it owes runs after the reload.
#[test]
fn while_a_reload_waits_a_maintenance_leg_hands_on_to_no_other_maintenance_leg() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    hold_a_maintenance_leg(&host, &ops, &name);
    let answer = a_waiting_reload(&host, &name, false);

    // The leg's end finds maintenance due again, and the reload is held open
    // where it runs, so what ran before it is read while it holds the claim.
    arrange_for(&ops.maintenance_due_at, &name);
    ops.block_reload.store(true, Ordering::SeqCst);
    ops.maintenance_release.store(true, Ordering::SeqCst);
    wait_for_flag("reload_started", &ops.reload_started);
    assert_eq!(
        ops.maintenances.load(Ordering::SeqCst),
        1,
        "a second maintenance leg ran ahead of the waiting reload"
    );

    ops.reload_release.store(true, Ordering::SeqCst);
    assert_eq!(answer_of(&answer), judged());
    wait_for_maintenances(&ops, 2);
    drop((lease, host));
}

/// **A maintenance leg that ends with watcher facts left hands on to their
/// reconcile, and the waiting reload is answered with the warming that
/// reconcile publishes**, not as busy.
#[test]
fn a_maintenance_leg_leaving_facts_answers_the_waiting_reload_with_the_reconciles_warming() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    hold_a_maintenance_leg(&host, &ops, &name);
    let answer = a_waiting_reload(&host, &name, false);

    // The leg's own drain meets the fact.
    ops.facts_on_next_polls.store(1, Ordering::SeqCst);
    ops.maintenance_release.store(true, Ordering::SeqCst);

    let answered = answer_of(&answer);
    assert!(
        matches!(
            answered,
            Err(ReloadRefusal::Unavailable(Demand::State(
                TrustState::Warming { .. }
            )))
        ),
        "the waiting reload was answered {answered:?}"
    );
    wait_for_reconciles(&ops, 1, "the reconcile the facts are owed to run");
    wait_for_state(&host, &name, TrustState::Ready);
    drop((lease, host));
}

/// **A watcher poll that reports facts schedules their reconcile, and the
/// reload waiting behind the poll is answered with the warming it publishes.**
#[test]
fn a_poll_reporting_facts_answers_the_waiting_reload_with_the_reconciles_warming() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    let poll = hold_a_poll(&host, &ops);
    let answer = a_waiting_reload(&host, &name, false);

    ops.facts_on_next_polls.store(1, Ordering::SeqCst);
    ops.poll_release.store(true, Ordering::SeqCst);

    assert_eq!(
        answer_of(&answer),
        Err(ReloadRefusal::Unavailable(Demand::State(
            TrustState::warming(WarmingPhase::Healing, 0, None)
        )))
    );
    poll.join().unwrap();
    wait_for_reconciles(&ops, 1, "the reconcile the facts are owed to run");
    wait_for_state(&host, &name, TrustState::Ready);
    drop((lease, host));
}

/// **Damage a read met while a reload waited schedules its rebuild where the
/// routine work ends, and the waiting reload is answered with that state.**
#[test]
fn damage_a_read_met_during_the_wait_schedules_the_rebuild_and_answers_the_reload_with_it() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    let poll = hold_a_poll(&host, &ops);
    let answer = a_waiting_reload(&host, &name, false);
    let detail = "the store is damaged";
    host.shared
        .entries
        .get(&name)
        .unwrap()
        .gate
        .lock()
        .unwrap()
        .damage_met_under_a_claim = Some(detail.into());

    ops.poll_release.store(true, Ordering::SeqCst);

    assert_eq!(
        answer_of(&answer),
        Err(ReloadRefusal::Unavailable(Demand::State(
            TrustState::untrusted(UntrustedReason::store_damaged_rebuilding(detail))
        )))
    );
    poll.join().unwrap();
    wait_for_state(&host, &name, TrustState::Ready);
    assert_eq!(ops.rebuilds.load(Ordering::SeqCst), 1);
    drop((lease, host));
}

/// **An apply asked while a reload waits queues behind the reload**: the
/// reload takes the claim where the routine work ends, and the apply runs at
/// the reload's end.
#[test]
fn an_apply_asked_while_a_reload_waits_runs_after_the_reload() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    hold_a_maintenance_leg(&host, &ops, &name);
    let answer = a_waiting_reload(&host, &name, false);
    let pending = host.admit_apply(&name, a_plan(&name)).expect("admitted");

    ops.block_reload.store(true, Ordering::SeqCst);
    ops.maintenance_release.store(true, Ordering::SeqCst);
    wait_for_flag("reload_started", &ops.reload_started);
    assert!(
        ops.applies_ran.lock().unwrap().is_empty(),
        "the apply ran ahead of the reload waiting before it"
    );

    ops.reload_release.store(true, Ordering::SeqCst);
    assert_eq!(answer_of(&answer), judged());
    pending.wait().expect("the apply applied");
    drop((lease, host));
}

/// **A reload asked while another waits refuses at once as busy**, in either
/// mode, and the waiting one still runs.
#[test]
fn a_second_reload_while_one_waits_refuses_busy_at_once() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    hold_a_maintenance_leg(&host, &ops, &name);
    let answer = a_waiting_reload(&host, &name, true);

    assert_eq!(host.reload(&name), busy());
    assert_eq!(host.judge_reload(&name), busy());
    assert_eq!(
        reload_refused(&host, &reload_params(&name, false)),
        ErrorDetail::reload_busy()
    );

    ops.maintenance_release.store(true, Ordering::SeqCst);
    assert_eq!(answer_of(&answer), judged());
    drop((lease, host));
}

/// **A reload asked while an apply is queued refuses at once as busy**, in
/// either mode, though routine work is what holds the claim.
#[test]
fn a_reload_while_an_apply_is_queued_refuses_busy_at_once() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    hold_a_maintenance_leg(&host, &ops, &name);
    let pending = host.admit_apply(&name, a_plan(&name)).expect("admitted");

    assert_eq!(host.reload(&name), busy());
    assert_eq!(host.judge_reload(&name), busy());

    ops.maintenance_release.store(true, Ordering::SeqCst);
    pending.wait().expect("the apply applied");
    drop((lease, host));
}

/// **A reload asked while an apply runs refuses at once as busy.**
#[test]
fn a_reload_while_an_apply_runs_refuses_busy_at_once() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    ops.block_apply.store(true, Ordering::SeqCst);
    let pending = host.admit_apply(&name, a_plan(&name)).expect("admitted");
    wait_for_flag("apply_started", &ops.apply_started);

    assert_eq!(host.reload(&name), busy());

    ops.apply_release.store(true, Ordering::SeqCst);
    pending.wait().expect("the apply applied");
    drop((lease, host));
}

/// **A reload asked while an idle detach is scheduled refuses at once as
/// busy.**
#[test]
fn a_reload_while_a_detach_is_scheduled_refuses_busy_at_once() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    drop(lease);
    let entry = host.shared.entries.get(&name).unwrap();
    {
        let mut state = entry.gate.lock().unwrap();
        state.detach_due = true;
        assert!(
            schedule_due_detach(&mut state, &name).is_some(),
            "the idle detach was not scheduled"
        );
    }

    assert_eq!(host.reload(&name), busy());
    drop((entry, host));
}

/// **A reload waiting when the host begins shutting down is answered that
/// the host stopped**, rather than left waiting.
#[test]
fn a_waiting_reload_answers_host_stopped_once_the_host_shuts_down() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    hold_a_maintenance_leg(&host, &ops, &name);
    let answer = a_waiting_reload(&host, &name, false);

    host.shared.shutting_down.store(true, Ordering::SeqCst);
    ops.maintenance_release.store(true, Ordering::SeqCst);

    assert_eq!(answer_of(&answer), Err(ReloadRefusal::HostStopped));
    drop((lease, host));
}

/// **An unregistration asked while a reload waits is refused as held**, as
/// over any attached entry, and the waiting reload still runs.
#[test]
fn an_unregistration_while_a_reload_waits_is_refused_and_the_reload_runs() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    hold_a_maintenance_leg(&host, &ops, &name);
    let answer = a_waiting_reload(&host, &name, false);

    let refused = host.unregister(&name, true);
    assert!(
        matches!(
            refused,
            Err(RegistrationRefusal::Serving(ServingRefusal::Held))
        ),
        "the unregistration answered {refused:?}"
    );

    ops.maintenance_release.store(true, Ordering::SeqCst);
    assert_eq!(answer_of(&answer), judged());
    drop((lease, host));
}

/// **A reload waiting behind a watcher poll that unwinds answers the untrusted
/// reading the unwind publishes**, not the release it passes through.
#[test]
fn a_reload_waiting_behind_a_poll_that_unwinds_answers_the_unwinds_reading() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    let poll = hold_a_poll(&host, &ops);
    let answer = a_waiting_reload(&host, &name, false);

    *ops.panic_in_poll_at.lock().unwrap() = Some(name.clone());
    ops.poll_release.store(true, Ordering::SeqCst);

    assert_eq!(
        answer_of(&answer),
        Err(ReloadRefusal::Unavailable(Demand::State(unwound(
            POLL_PANIC
        ))))
    );
    poll.join().unwrap();
    drop((lease, host));
}

/// **A reload waiting behind a maintenance leg that unwinds answers the
/// untrusted reading the unwind publishes**, not the release it passes
/// through.
#[test]
fn a_reload_waiting_behind_a_maintenance_leg_that_unwinds_answers_the_unwinds_reading() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    hold_a_maintenance_leg(&host, &ops, &name);
    let answer = a_waiting_reload(&host, &name, false);

    ops.panic_in_maintenance.store(true, Ordering::SeqCst);
    ops.maintenance_release.store(true, Ordering::SeqCst);

    assert_eq!(
        answer_of(&answer),
        Err(ReloadRefusal::Unavailable(Demand::State(unwound(
            MAINTENANCE_PANIC
        ))))
    );
    drop((lease, host));
}

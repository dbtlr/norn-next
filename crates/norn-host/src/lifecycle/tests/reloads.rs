//! The reload admission's lifecycle cases: a reload of either mode runs over
//! a vault that serves or settles a change, waits behind the vault's own
//! routine work — a watcher poll, a maintenance leg or a reconcile turn —
//! folds the facts that work left before it reads its candidate, publishes
//! `Ready` only where its end leaves nothing, and refuses at once on explicit
//! work.
//!
//! The hosts here run no ambient dispatcher tick inside a case, so the poll or
//! the leg a case holds open is the one the case drove, and a waiting reload
//! is read off the entry's claim rather than inferred from a reply that has
//! not come yet.

use super::*;

/// What a reload's asker is answered.
type Answer = Result<ReloadJudgment, ReloadRefusal>;

/// The answer `vault/reload-busy` renders from: the entry serves or settles
/// a change, and explicit work holds it.
fn busy() -> Answer {
    Err(ReloadRefusal::Busy)
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
    a_ready_vault_settling_within(ops, crate::READ_SETTLE_BOUND)
}

/// [`a_ready_vault`], whose reads settle within `read_settle_bound`: a case
/// whose read must answer before its bound names one past the case's own
/// wait budget, so a read that answers only at its bound fails the case.
fn a_ready_vault_settling_within(
    ops: &Arc<FakeOps>,
    read_settle_bound: Duration,
) -> (SharedHost, VaultName, DemandLease<Arc<FakeOps>>) {
    ops.reload_supported.store(true, Ordering::SeqCst);
    let (host, name) = fixture_settling_within(Arc::clone(ops), read_settle_bound);
    let host = Arc::new(host);
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

/// **A reload over a reconcile turn scheduled and not yet begun takes the
/// claim from it, and its fold derives the facts the turn was scheduled
/// for**, in either mode: the reload reads its candidate over the fold's
/// commit, and the displaced turn does not run after it.
#[test]
fn a_reload_over_a_scheduled_reconcile_turn_takes_the_claim_and_its_fold_derives_the_facts() {
    for dry_run in [false, true] {
        let ops = Arc::new(FakeOps::default());
        let (host, name, lease) = a_ready_vault(&ops);
        let entry = host.shared.entries.get(&name).unwrap();
        {
            // A poll's end, its marker held undispatched: the fact it took
            // in is pending, the vault settles it, and the turn is scheduled.
            let mut state = entry.gate.lock().unwrap();
            state.pending.merge(a_fact());
            state.publish_pending_reconcile();
            state
                .claim
                .schedule(|epoch| Job::Reconcile(name.clone(), epoch));
        }

        let reloaded = if dry_run {
            host.judge_reload(&name)
        } else {
            host.reload(&name)
        };

        assert_eq!(reloaded, judged(), "dry run: {dry_run}");
        assert_eq!(
            *ops.reconciles_at_reload.lock().unwrap(),
            vec![1],
            "dry run: {dry_run}: the candidate was not read over the fold's commit"
        );
        assert!(
            !ops.reconciled_batches.lock().unwrap()[0].is_empty(),
            "dry run: {dry_run}: the fold derived nothing"
        );
        assert_eq!(host.state(&name), answered(TrustState::Ready));
        assert_eq!(
            ops.reconciles.load(Ordering::SeqCst),
            1,
            "dry run: {dry_run}: the displaced turn ran after the reload"
        );
        drop((entry, lease, host));
    }
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

/// **A maintenance leg that ends with watcher facts left gives the waiting
/// reload the claim, and the reload's fold derives them** before it reads
/// its candidate, rather than a reconcile ahead of it answering it warming.
#[test]
fn a_maintenance_leg_leaving_facts_gives_the_waiting_reload_the_claim_and_its_fold_derives_them() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    hold_a_maintenance_leg(&host, &ops, &name);
    let answer = a_waiting_reload(&host, &name, false);

    // The leg's own drain meets the fact.
    ops.facts_on_next_polls.store(1, Ordering::SeqCst);
    ops.maintenance_release.store(true, Ordering::SeqCst);

    assert_eq!(answer_of(&answer), judged());
    assert_eq!(*ops.reconciles_at_reload.lock().unwrap(), vec![1]);
    wait_for_state(&host, &name, TrustState::Ready);
    assert_eq!(ops.reconciles.load(Ordering::SeqCst), 1);
    drop((lease, host));
}

/// **A watcher poll that reports facts gives the reload waiting behind it the
/// claim, and the reload's fold derives them** in place of the reconcile the
/// poll would have scheduled.
#[test]
fn a_poll_reporting_facts_gives_the_waiting_reload_the_claim_and_its_fold_derives_them() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    let poll = hold_a_poll(&host, &ops);
    let answer = a_waiting_reload(&host, &name, false);

    ops.facts_on_next_polls.store(1, Ordering::SeqCst);
    ops.poll_release.store(true, Ordering::SeqCst);

    assert_eq!(answer_of(&answer), judged());
    assert_eq!(*ops.reconciles_at_reload.lock().unwrap(), vec![1]);
    poll.join().unwrap();
    wait_for_state(&host, &name, TrustState::Ready);
    assert_eq!(ops.reconciles.load(Ordering::SeqCst), 1);
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

/// **A maintenance leg a leg's end handed on, not yet running, holds a reload
/// as a running one does**: the reload waits, an apply asked after it queues,
/// and where the leg begins it runs rather than yielding to the apply, so the
/// reload takes the claim at its end and the apply runs after the reload.
#[test]
fn a_reload_waiting_behind_a_handed_on_maintenance_leg_runs_before_an_apply_asked_after_it() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    let entry = host.shared.entries.get(&name).unwrap();
    // The hand-on a leg's end makes, with the send held back so the leg has
    // not begun.
    let handed_on = {
        let mut state = entry.gate.lock().unwrap();
        let job = state
            .claim
            .hand_on(|epoch| Job::Maintenance(name.clone(), epoch));
        state.claim.take_slot(job.epoch());
        job
    };
    let answer = a_waiting_reload(&host, &name, false);
    let pending = host.admit_apply(&name, a_plan(&name)).expect("admitted");

    ops.block_reload.store(true, Ordering::SeqCst);
    dispatch_followup(&host.shared, handed_on);
    wait_for_flag("reload_started", &ops.reload_started);
    assert_eq!(ops.maintenances.load(Ordering::SeqCst), 1);
    assert!(
        ops.applies_ran.lock().unwrap().is_empty(),
        "the apply ran ahead of the reload waiting before it"
    );

    ops.reload_release.store(true, Ordering::SeqCst);
    assert_eq!(answer_of(&answer), judged());
    pending.wait().expect("the apply applied");
    drop((entry, lease, host));
}

/// **A reload waiting behind a watcher poll that meets an environmental
/// refusal is answered with the untrusted state the poll published**, and
/// does not run.
#[test]
fn a_reload_waiting_behind_a_poll_refused_by_its_environment_answers_untrusted_and_does_not_run() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    let poll = hold_a_poll(&host, &ops);
    let answer = a_waiting_reload(&host, &name, false);

    ops.block_reload.store(true, Ordering::SeqCst);
    ops.environmental_poll.store(true, Ordering::SeqCst);
    ops.poll_release.store(true, Ordering::SeqCst);

    let answered = answer_of(&answer);
    assert!(
        matches!(
            answered,
            Err(ReloadRefusal::Unavailable(Demand::State(
                TrustState::Untrusted {
                    reason: UntrustedReason::EnvironmentalRefusal { .. },
                    ..
                }
            )))
        ),
        "the waiting reload was answered {answered:?}"
    );
    poll.join().unwrap();
    assert!(
        !ops.reload_started.load(Ordering::SeqCst),
        "the reload ran over an entry the poll left untrusted"
    );
    ops.reload_release.store(true, Ordering::SeqCst);
    drop((lease, host));
}

/// **An apply asked while a reload waits behind a watcher poll queues behind
/// the reload**: the poll's end gives the reload the claim, and the apply
/// runs at the reload's end.
#[test]
fn an_apply_asked_while_a_reload_waits_behind_a_poll_runs_after_the_reload() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    let poll = hold_a_poll(&host, &ops);
    let answer = a_waiting_reload(&host, &name, false);
    let pending = host.admit_apply(&name, a_plan(&name)).expect("admitted");

    ops.block_reload.store(true, Ordering::SeqCst);
    ops.poll_release.store(true, Ordering::SeqCst);
    wait_for_flag("reload_started", &ops.reload_started);
    assert!(
        ops.applies_ran.lock().unwrap().is_empty(),
        "the apply ran ahead of the reload waiting before it"
    );

    ops.reload_release.store(true, Ordering::SeqCst);
    assert_eq!(answer_of(&answer), judged());
    pending.wait().expect("the apply applied");
    poll.join().unwrap();
    drop((lease, host));
}

/// The answer a reload waiting behind routine work that lost maintainership
/// gets while the release that loss opened is still giving the coverage back.
fn releasing() -> Answer {
    Err(ReloadRefusal::Unavailable(Demand::State(
        TrustState::warming(WarmingPhase::ReleasingCoverage, 0, None),
    )))
}

/// **A reload waiting behind a watcher poll that loses maintainership is
/// answered with the release the loss opens**, rather than left waiting for
/// work that will not end in its favour.
#[test]
fn a_reload_waiting_behind_a_poll_that_loses_maintainership_answers_the_release() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    let poll = hold_a_poll(&host, &ops);
    let answer = a_waiting_reload(&host, &name, false);

    // The detach is held, so the release stands where the loss opened it.
    ops.block_detach.store(true, Ordering::SeqCst);
    ops.lost_poll.store(true, Ordering::SeqCst);
    ops.poll_release.store(true, Ordering::SeqCst);

    assert_eq!(answer_of(&answer), releasing());
    ops.detach_release.store(true, Ordering::SeqCst);
    poll.join().unwrap();
    wait_for_state(&host, &name, TrustState::Ready);
    drop((lease, host));
}

/// **A reload waiting behind a maintenance leg that loses maintainership is
/// answered with the release the loss opens**, rather than left waiting for
/// work that will not end in its favour.
#[test]
fn a_reload_waiting_behind_a_maintenance_leg_that_loses_maintainership_answers_the_release() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    hold_a_maintenance_leg(&host, &ops, &name);
    let answer = a_waiting_reload(&host, &name, false);

    // The detach is held, so the release stands where the loss opened it.
    ops.block_detach.store(true, Ordering::SeqCst);
    ops.lost_maintenance.store(true, Ordering::SeqCst);
    ops.maintenance_release.store(true, Ordering::SeqCst);

    assert_eq!(answer_of(&answer), releasing());
    ops.detach_release.store(true, Ordering::SeqCst);
    wait_for_state(&host, &name, TrustState::Ready);
    drop((lease, host));
}

/// **A reload waiting behind a watcher poll that a root refusal moves past is
/// answered with the park the refusal raises**, though the poll still holds
/// the coverage and runs on.
#[test]
fn a_reload_waiting_behind_a_poll_a_root_refusal_moves_past_answers_the_park() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    let poll = hold_a_poll(&host, &ops);
    let answer = a_waiting_reload(&host, &name, false);

    refuse_identity_error(&host.shared, &name, "the root cannot be read".into());
    let parked = host
        .shared
        .entries
        .get(&name)
        .unwrap()
        .gate
        .lock()
        .unwrap()
        .published_demand();
    assert_ne!(
        parked,
        Demand::State(TrustState::Ready),
        "no park was raised"
    );

    assert_eq!(answer_of(&answer), Err(ReloadRefusal::Unavailable(parked)));
    ops.poll_release.store(true, Ordering::SeqCst);
    poll.join().unwrap();
    drop((lease, host));
}

/// The healing a `Ready` vault settles a change under.
fn healing() -> TrustState {
    TrustState::warming(WarmingPhase::Healing, 0, None)
}

/// The answer a reload whose drain met a watcher overflow gets: where the
/// entry then stands, its trust withdrawn until the rescan is reread.
fn overflowed() -> Answer {
    Err(ReloadRefusal::Unavailable(Demand::State(
        TrustState::untrusted(UntrustedReason::WatcherOverflow),
    )))
}

/// Hold a `Ready` vault settling an edit: a driven poll reports a fact, and
/// the reconcile turn it schedules is held open inside the fake. Only that
/// turn is held; the turns after it run.
fn hold_a_settling_turn(host: &SharedHost, ops: &Arc<FakeOps>, name: &VaultName) {
    let turn = ops.reconciles.load(Ordering::SeqCst) + 1;
    ops.block_reconcile_at.store(turn, Ordering::SeqCst);
    report_through_a_driven_poll(ops, host, name, &ops.off_thread_fact_poll_batches);
    wait_for_flag("reconcile_started", &ops.reconcile_started);
    assert_eq!(
        host.state(name),
        answered(healing()),
        "the edit did not leave the vault settling"
    );
}

/// Let the held turn end, its own drain meeting one more fact: the turn
/// leaves that fact pending, for whatever takes the claim at its end.
fn end_the_turn_leaving_a_fact(ops: &FakeOps) {
    ops.facts_on_next_polls.store(1, Ordering::SeqCst);
    ops.reconcile_release.store(true, Ordering::SeqCst);
}

/// Ask a reload of `name` — a dry run where `dry_run` — on a thread of its
/// own, answering its receiver at once.
fn ask_a_reload(host: &SharedHost, name: &VaultName, dry_run: bool) -> mpsc::Receiver<Answer> {
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
    answer
}

/// **A reload asked while a `Ready` vault settles an edit waits for the
/// reconcile turn and folds what that turn left**, in either mode: it reads
/// its candidate only once its fold has derived the fact, and the vault ends
/// `Ready` with nothing derived twice.
#[test]
fn a_reload_over_a_vault_settling_an_edit_waits_for_the_turn_and_folds_what_it_left() {
    for dry_run in [false, true] {
        let ops = Arc::new(FakeOps::default());
        let (host, name, lease) = a_ready_vault(&ops);
        hold_a_settling_turn(&host, &ops, &name);

        let answer = a_waiting_reload(&host, &name, dry_run);
        end_the_turn_leaving_a_fact(&ops);

        assert_eq!(answer_of(&answer), judged(), "dry run: {dry_run}");
        assert_eq!(
            *ops.reconciles_at_reload.lock().unwrap(),
            vec![2],
            "dry run: {dry_run}: the candidate was not read over the turn and the fold's commit"
        );
        assert!(
            !ops.reconciled_batches.lock().unwrap()[1].is_empty(),
            "dry run: {dry_run}: the fold derived nothing"
        );
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(
            ops.reconciles.load(Ordering::SeqCst),
            2,
            "dry run: {dry_run}"
        );
        drop((lease, host));
    }
}

/// **A reload over a quiet `Ready` vault commits nothing and drains the
/// watcher twice**: its fold finds no fact, and its end drains once more
/// before it publishes. That is the whole of what the fold adds to a reload
/// over a quiet vault.
#[test]
fn a_reload_over_a_quiet_ready_vault_commits_nothing_and_drains_twice() {
    for dry_run in [false, true] {
        let ops = Arc::new(FakeOps::default());
        let (host, name, lease) = a_ready_vault(&ops);
        let polled = polls_of(&ops, &name);

        let reloaded = if dry_run {
            host.judge_reload(&name)
        } else {
            host.reload(&name)
        };

        assert_eq!(reloaded, judged(), "dry run: {dry_run}");
        assert_eq!(
            ops.reconciles.load(Ordering::SeqCst),
            0,
            "dry run: {dry_run}"
        );
        assert_eq!(
            polls_of(&ops, &name) - polled,
            2,
            "dry run: {dry_run}: the fold's drain and the end's drain"
        );
        assert_eq!(host.state(&name), answered(TrustState::Ready));
        drop((lease, host));
    }
}

/// **A reload publishes `Ready` only where its end's drain leaves nothing**,
/// in either mode: the fold leaves the vault healing while the candidate is
/// read, and a fact arriving meanwhile is taken in by the end's drain and
/// handed on to its reconcile rather than left behind a `Ready` publication.
#[test]
fn a_reload_over_a_settling_vault_publishes_ready_only_where_its_end_drain_leaves_nothing() {
    for dry_run in [false, true] {
        let ops = Arc::new(FakeOps::default());
        let (host, name, lease) = a_ready_vault(&ops);
        hold_a_settling_turn(&host, &ops, &name);
        let answer = a_waiting_reload(&host, &name, dry_run);

        ops.block_reload.store(true, Ordering::SeqCst);
        end_the_turn_leaving_a_fact(&ops);
        wait_for_flag("reload_started", &ops.reload_started);
        assert_eq!(
            host.state(&name),
            answered(healing()),
            "dry run: {dry_run}: the fold published Ready over the candidate's reading"
        );

        ops.facts_on_next_polls.store(1, Ordering::SeqCst);
        ops.reload_release.store(true, Ordering::SeqCst);

        assert_eq!(answer_of(&answer), judged(), "dry run: {dry_run}");
        assert_eq!(
            ops.facts_on_next_polls.load(Ordering::SeqCst),
            0,
            "dry run: {dry_run}: the end's drain left the fact with the watcher"
        );
        wait_for_reconciles(&ops, 3, "the reconcile the end's drain handed on to");
        wait_for_state(&host, &name, TrustState::Ready);
        drop((lease, host));
    }
}

/// **A read that met the change a reload folds answers once the fold commits
/// it**, from the reader standing then and under the healing that still
/// stands, never from a snapshot without the change.
#[test]
fn a_read_that_met_the_change_a_reload_folds_answers_once_the_fold_commits() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    hold_a_settling_turn(&host, &ops, &name);
    let answer = a_waiting_reload(&host, &name, false);

    // The fold's commit is held, and so is the reload's own step after it.
    ops.hold_reconciles_from.store(2, Ordering::SeqCst);
    ops.block_reload.store(true, Ordering::SeqCst);
    end_the_turn_leaving_a_fact(&ops);
    wait_for_reconciles(&ops, 2, "the fold's commit to begin");

    let before = host.read_evidence();
    thread::scope(|scope| {
        let reading = scope.spawn(|| host.begin_read(&name));
        wait_for_settle_waits(&host, before, 1);
        assert!(
            !reading.is_finished(),
            "the read answered before the fold committed the change it met"
        );
        ops.later_reconcile_release.store(true, Ordering::SeqCst);
        let hold = reading
            .join()
            .expect("the read finished")
            .expect("a read that met the folded change was refused");
        assert_eq!(hold.reading().published(), &Demand::State(healing()));
    });

    ops.reload_release.store(true, Ordering::SeqCst);
    assert_eq!(answer_of(&answer), judged());
    drop((lease, host));
}

/// **A watcher overflow the turn a reload waits for meets withdraws trust,
/// and the reload answers with it**, its candidate not read; the rescan
/// then runs.
#[test]
fn an_overflow_during_the_wait_answers_the_reload_untrusted_and_owes_the_rescan() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    hold_a_settling_turn(&host, &ops, &name);
    let answer = a_waiting_reload(&host, &name, false);

    ops.handoff_rescan_poll_batches.store(1, Ordering::SeqCst);
    ops.reconcile_release.store(true, Ordering::SeqCst);

    assert_eq!(answer_of(&answer), overflowed());
    assert!(ops.reconciles_at_reload.lock().unwrap().is_empty());
    wait_for_state(&host, &name, TrustState::Ready);
    drop((lease, host));
}

/// **A watcher overflow the fold's drain meets withdraws trust, and the
/// reload answers with it**, in either mode, its candidate not read; the
/// rescan then runs.
#[test]
fn an_overflow_at_the_folds_drain_answers_the_reload_untrusted_and_owes_the_rescan() {
    for dry_run in [false, true] {
        let ops = Arc::new(FakeOps::default());
        let (host, name, lease) = a_ready_vault(&ops);
        let poll = hold_a_poll(&host, &ops);
        let answer = a_waiting_reload(&host, &name, dry_run);

        // The poll reports an edit, and the fold's drain meets an overflow.
        ops.facts_on_next_polls.store(1, Ordering::SeqCst);
        ops.handoff_rescan_poll_batches.store(1, Ordering::SeqCst);
        ops.poll_release.store(true, Ordering::SeqCst);

        assert_eq!(answer_of(&answer), overflowed(), "dry run: {dry_run}");
        assert!(
            ops.reconciles_at_reload.lock().unwrap().is_empty(),
            "dry run: {dry_run}: the candidate was read over an overflow"
        );
        poll.join().unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        drop((lease, host));
    }
}

/// **A watcher overflow the end's drain meets withdraws trust, and the reload
/// answers with it**, in either mode, though it read its candidate; the
/// rescan then runs.
#[test]
fn an_overflow_at_the_end_drain_answers_the_reload_untrusted_and_owes_the_rescan() {
    for dry_run in [false, true] {
        let ops = Arc::new(FakeOps::default());
        let (host, name, lease) = a_ready_vault(&ops);
        ops.block_reload.store(true, Ordering::SeqCst);
        let answer = ask_a_reload(&host, &name, dry_run);
        wait_for_flag("reload_started", &ops.reload_started);

        ops.handoff_rescan_poll_batches.store(1, Ordering::SeqCst);
        ops.reload_release.store(true, Ordering::SeqCst);

        // The leg read its candidate, so its answer is spelled as one that
        // may have; an untrusted entry renders alike either way.
        assert_eq!(
            answer_of(&answer),
            Err(ReloadRefusal::Unreported(Demand::State(
                TrustState::untrusted(UntrustedReason::WatcherOverflow)
            ))),
            "dry run: {dry_run}"
        );
        wait_for_state(&host, &name, TrustState::Ready);
        drop((lease, host));
    }
}

/// **An apply asked while a reload waits behind a reconcile turn queues
/// behind the reload**: the turn's end gives the reload the claim, and the
/// apply runs at the reload's end.
#[test]
fn an_apply_asked_while_a_reload_waits_behind_a_turn_runs_after_the_reload() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    hold_a_settling_turn(&host, &ops, &name);
    let answer = a_waiting_reload(&host, &name, false);
    let pending = host.admit_apply(&name, a_plan(&name)).expect("admitted");

    ops.block_reload.store(true, Ordering::SeqCst);
    end_the_turn_leaving_a_fact(&ops);
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

/// **A candidate failure over a settling vault keeps what the fold derived
/// and withdraws no trust**, in either mode: the vault is `Ready` once
/// nothing is left, and no recovery is owed.
#[test]
fn a_candidate_failure_over_a_settling_vault_keeps_the_folds_derivation_and_its_trust() {
    for dry_run in [false, true] {
        let ops = Arc::new(FakeOps::default());
        let (host, name, lease) = a_ready_vault(&ops);
        hold_a_settling_turn(&host, &ops, &name);
        let answer = a_waiting_reload(&host, &name, dry_run);

        ops.reload_candidate_unparsable
            .store(true, Ordering::SeqCst);
        end_the_turn_leaving_a_fact(&ops);

        let refused = answer_of(&answer);
        assert!(
            matches!(
                refused,
                Err(ReloadRefusal::Core(ReloadError::SchemaParse(_)))
            ),
            "dry run: {dry_run}: the reload answered {refused:?}"
        );
        assert_eq!(*ops.reconciles_at_reload.lock().unwrap(), vec![2]);
        assert_eq!(
            host.state(&name),
            answered(TrustState::Ready),
            "dry run: {dry_run}: the candidate's failure withdrew trust"
        );
        assert_eq!(ops.recovers.load(Ordering::SeqCst), 0, "dry run: {dry_run}");
        assert_eq!(
            ops.reconciles.load(Ordering::SeqCst),
            2,
            "dry run: {dry_run}"
        );
        drop((lease, host));
    }
}

/// **A runtime failure the fold meets answers `vault/reload-failed` saying
/// the fold met it**, its candidate not read, and is handled as a runtime
/// failure the reload's own steps meet: trust is withdrawn, the recovery is
/// owed, and no fact the fold took is lost, since the recovery rereads the
/// whole vault. That holds for a refusing environment and for a control file
/// the fold's derivation could not read alike.
#[test]
fn a_failure_the_fold_meets_answers_reload_failed_saying_the_fold_met_it() {
    for control_file in [false, true] {
        let ops = Arc::new(FakeOps::default());
        let (host, name, lease) = a_ready_vault(&ops);
        hold_a_settling_turn(&host, &ops, &name);
        let answer = a_waiting_reload(&host, &name, false);

        ops.hold_reconciles_from.store(2, Ordering::SeqCst);
        end_the_turn_leaving_a_fact(&ops);
        wait_for_reconciles(&ops, 2, "the fold's commit to begin");
        if control_file {
            ops.control_file_reconcile.store(true, Ordering::SeqCst);
        } else {
            ops.environmental_reconcile.store(true, Ordering::SeqCst);
        }
        ops.later_reconcile_release.store(true, Ordering::SeqCst);

        let refusal = answer_of(&answer).expect_err("the fold's failure was answered as success");
        assert!(
            ops.reconciles_at_reload.lock().unwrap().is_empty(),
            "control file: {control_file}: the candidate was read over the fold's failure"
        );
        let envelope = refusal
            .answer(&name)
            .expect("a live host answers with a code");
        if !control_file {
            assert_eq!(
                envelope.detail(),
                &ErrorDetail::reload_failed(norn_wire::ReloadFailure::environmental("refused"))
            );
        }
        assert_eq!(
            envelope.message(),
            "this vault's reload met a failure while it took in the change the vault held, so \
             its control files were not read",
            "control file: {control_file}"
        );

        // No ambient tick runs here, so the entry stands as the failure's
        // policy left it until a demand asks for the recovery it owes.
        {
            let entry = host.shared.entries.get(&name).unwrap();
            let state = entry.gate.lock().unwrap();
            assert!(
                matches!(state.trust, TrustState::Untrusted { .. }),
                "control file: {control_file}: the fold's failure left the trust at {:?}",
                state.trust
            );
            assert!(
                state.recovery_required,
                "control file: {control_file}: the fold's failure owes no recovery"
            );
            assert!(
                state.pending.has_rescans(),
                "control file: {control_file}: nothing rereads the facts the fold took"
            );
        }
        drop(host.demand(&name).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(
            ops.recovers.load(Ordering::SeqCst),
            1,
            "control file: {control_file}"
        );
        drop((lease, host));
    }
}

/// **A schema-changing reload over a settling vault whose end drain finds
/// nothing answers with `Ready`**: its fold derived what the turn it waited
/// for left, and nothing arrived while it activated.
#[test]
fn a_schema_changing_reload_over_a_settling_vault_answers_at_ready() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    ops.reload_schema_changed.store(true, Ordering::SeqCst);
    hold_a_settling_turn(&host, &ops, &name);
    let answer = a_waiting_reload(&host, &name, false);

    end_the_turn_leaving_a_fact(&ops);

    assert_eq!(
        answer_of(&answer),
        Ok(fake_judgment(ReloadOutcome::SchemaChanged))
    );
    assert_eq!(host.state(&name), answered(TrustState::Ready));
    drop((lease, host));
}

/// **Explicit work refuses a reload busy at once over a `Ready` and a
/// settling vault alike**, in one spelling that says the control files were
/// not read.
#[test]
fn explicit_work_refuses_a_reload_busy_alike_over_a_ready_and_a_settling_vault() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    ops.block_apply.store(true, Ordering::SeqCst);
    let pending = host.admit_apply(&name, a_plan(&name)).expect("admitted");
    wait_for_flag("apply_started", &ops.apply_started);
    let over_ready = host
        .reload(&name)
        .expect_err("a reload ran beside an apply")
        .answer(&name)
        .expect("a live host answers with a code");
    ops.apply_release.store(true, Ordering::SeqCst);
    pending.wait().expect("the apply applied");
    wait_for_state(&host, &name, TrustState::Ready);
    wait_for_a_quiet_entry(&host, &name);

    hold_a_settling_turn(&host, &ops, &name);
    let answer = a_waiting_reload(&host, &name, true);
    let over_settling = host
        .reload(&name)
        .expect_err("a second reload was admitted")
        .answer(&name)
        .expect("a live host answers with a code");
    let dry_over_settling = host
        .judge_reload(&name)
        .expect_err("a second dry run was admitted")
        .answer(&name)
        .expect("a live host answers with a code");

    assert_eq!(over_ready.detail(), &ErrorDetail::reload_busy());
    assert_eq!(
        over_ready.message(),
        "this vault is already being worked over, so its control files were not read"
    );
    assert_eq!(over_settling, over_ready);
    assert_eq!(dry_over_settling, over_ready);

    ops.reconcile_release.store(true, Ordering::SeqCst);
    assert_eq!(answer_of(&answer), judged());
    drop((lease, host));
}

/// **While a reload waits, a reconcile turn that ends with maintenance due
/// hands the claim to the reload rather than to a maintenance leg**: the
/// reload's fold is the only commit after the turn's own, and the pass runs
/// after the reload.
#[test]
fn while_a_reload_waits_a_reconcile_turn_ending_with_maintenance_due_hands_on_to_the_reload() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    hold_a_settling_turn(&host, &ops, &name);
    let answer = a_waiting_reload(&host, &name, false);
    arrange_for(&ops.maintenance_due_at, &name);

    ops.block_reload.store(true, Ordering::SeqCst);
    end_the_turn_leaving_a_fact(&ops);
    wait_for_flag("reload_started", &ops.reload_started);
    assert_eq!(
        ops.maintenances.load(Ordering::SeqCst),
        0,
        "a maintenance leg ran ahead of the waiting reload"
    );
    assert_eq!(
        ops.reconciles.load(Ordering::SeqCst),
        2,
        "the turn's commit and the fold's"
    );

    ops.reload_release.store(true, Ordering::SeqCst);
    assert_eq!(answer_of(&answer), judged());
    drop((lease, host));
}

/// Arm the fake so the held turn over the named vault fails one way.
type ArmAFailure = fn(&FakeOps, &VaultName);

/// **Every runtime failure of the turn a reload waits for answers the
/// reload**, never busy, its candidate not read: a refusing environment, a
/// terminal watch, a lost or contended maintainership, and damaged derived
/// state.
#[test]
fn every_failure_of_the_turn_a_reload_waits_for_answers_the_reload_with_its_candidate_unread() {
    let failures: [(&str, ArmAFailure); 5] = [
        ("environmental", |ops, _| {
            ops.environmental_reconcile.store(true, Ordering::SeqCst);
        }),
        ("terminal watch", |ops, _| {
            ops.terminal_reconcile.store(true, Ordering::SeqCst);
        }),
        ("lost maintainership", |ops, _| {
            ops.lost_reconcile.store(true, Ordering::SeqCst);
        }),
        ("contended maintainership", |ops, _| {
            ops.contend_reconcile.store(true, Ordering::SeqCst);
        }),
        ("damaged store", |ops, name| {
            arrange_for(&ops.damaged_reconcile_at, name);
        }),
    ];
    for (kind, arm) in failures {
        let ops = Arc::new(FakeOps::default());
        let (host, name, lease) = a_ready_vault(&ops);
        hold_a_settling_turn(&host, &ops, &name);
        let answer = a_waiting_reload(&host, &name, false);

        arm(&ops, &name);
        ops.block_detach.store(true, Ordering::SeqCst);
        ops.reconcile_release.store(true, Ordering::SeqCst);

        let answered = answer_of(&answer);
        assert!(answered.is_err(), "{kind}: the reload ran over the failure");
        assert_ne!(answered, busy(), "{kind}");
        assert!(
            ops.reconciles_at_reload.lock().unwrap().is_empty(),
            "{kind}: the candidate was read"
        );
        ops.detach_release.store(true, Ordering::SeqCst);
        drop((lease, host));
    }
}

/// **A reload waiting behind a reconcile turn that unwinds is answered**,
/// and does not run.
#[test]
fn a_reload_waiting_behind_a_reconcile_turn_that_unwinds_is_answered_and_does_not_run() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    hold_a_settling_turn(&host, &ops, &name);
    let answer = a_waiting_reload(&host, &name, false);

    ops.panic_in_reconcile.store(true, Ordering::SeqCst);
    ops.reconcile_release.store(true, Ordering::SeqCst);

    assert!(answer_of(&answer).is_err());
    assert!(ops.reconciles_at_reload.lock().unwrap().is_empty());
    drop((lease, host));
}

/// **An edit stream does not lengthen a reload's wait**: the reload runs
/// after the one turn it waited for and its own fold, answers while the
/// stream keeps the vault settling, and the vault reaches `Ready` once the
/// stream stops.
#[test]
fn a_reload_under_an_edit_stream_waits_one_turn_and_answers_while_the_vault_settles() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    hold_a_settling_turn(&host, &ops, &name);
    let answer = a_waiting_reload(&host, &name, false);
    arrange_for(&ops.continuous_fact_handoff_for, &name);

    ops.block_reload.store(true, Ordering::SeqCst);
    ops.reconcile_release.store(true, Ordering::SeqCst);
    wait_for_flag("reload_started", &ops.reload_started);
    assert_eq!(
        ops.reconciles.load(Ordering::SeqCst),
        2,
        "the turn's commit and the fold's"
    );
    assert_eq!(host.state(&name), answered(healing()));

    ops.reload_release.store(true, Ordering::SeqCst);
    assert_eq!(answer_of(&answer), judged());
    assert_ne!(
        host.state(&name),
        answered(TrustState::Ready),
        "Ready published under an edit stream"
    );
    *ops.continuous_fact_handoff_for.lock().unwrap() = None;
    wait_for_state(&host, &name, TrustState::Ready);
    drop((lease, host));
}

/// **A reload whose end drain leaves facts hands the claim to the apply
/// queued behind it**, which runs and whose intake derives them; the vault
/// then reaches `Ready`.
#[test]
fn a_reload_whose_end_drain_leaves_facts_hands_the_claim_to_the_apply_queued_behind_it() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    hold_a_settling_turn(&host, &ops, &name);
    let answer = a_waiting_reload(&host, &name, false);
    let pending = host.admit_apply(&name, a_plan(&name)).expect("admitted");

    ops.block_reload.store(true, Ordering::SeqCst);
    end_the_turn_leaving_a_fact(&ops);
    wait_for_flag("reload_started", &ops.reload_started);
    assert!(ops.applies_ran.lock().unwrap().is_empty());

    ops.facts_on_next_polls.store(1, Ordering::SeqCst);
    ops.reload_release.store(true, Ordering::SeqCst);
    assert_eq!(answer_of(&answer), judged());
    pending.wait().expect("the apply applied");
    wait_for_state(&host, &name, TrustState::Ready);
    drop((lease, host));
}

/// **The reload leg's observation gap is the same whether or not its fold
/// found facts**: after a fold that derived facts, a read begun while the
/// candidate is read answers at once under the healing the fold published,
/// though an edit made after the fold's drain is still with the watcher.
/// The end's drain takes that edit in before the leg ends.
#[test]
fn a_read_while_a_reload_reads_its_candidate_after_a_fold_that_found_facts_can_miss_a_new_edit() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    hold_a_settling_turn(&host, &ops, &name);
    let answer = a_waiting_reload(&host, &name, false);

    ops.block_reload.store(true, Ordering::SeqCst);
    end_the_turn_leaving_a_fact(&ops);
    wait_for_flag("reload_started", &ops.reload_started);
    ops.facts_on_next_polls.store(1, Ordering::SeqCst);

    let hold = host
        .begin_read(&name)
        .expect("a read in the reload leg's gap was refused");
    assert_eq!(hold.reading().published(), &Demand::State(healing()));
    assert_eq!(
        ops.facts_on_next_polls.load(Ordering::SeqCst),
        1,
        "the edit was taken in before the read answered"
    );
    drop(hold);

    ops.reload_release.store(true, Ordering::SeqCst);
    assert_eq!(answer_of(&answer), judged());
    assert_eq!(
        ops.facts_on_next_polls.load(Ordering::SeqCst),
        0,
        "the end's drain left the edit with the watcher"
    );
    drop((lease, host));
}

/// **A reload asked while the reconcile an earlier reload's end handed on to
/// runs waits for that turn rather than refusing busy**, and runs after it.
#[test]
fn a_reload_over_the_reconcile_an_earlier_reloads_end_handed_on_to_waits_and_runs() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    ops.block_reload.store(true, Ordering::SeqCst);
    let first = ask_a_reload(&host, &name, false);
    wait_for_flag("reload_started", &ops.reload_started);
    assert_eq!(
        host.reload(&name),
        busy(),
        "a second reload while the first runs"
    );

    let turn = ops.reconciles.load(Ordering::SeqCst) + 1;
    ops.block_reconcile_at.store(turn, Ordering::SeqCst);
    ops.facts_on_next_polls.store(1, Ordering::SeqCst);
    ops.reload_release.store(true, Ordering::SeqCst);
    assert_eq!(answer_of(&first), judged());
    wait_for_flag("reconcile_started", &ops.reconcile_started);

    ops.block_reload.store(false, Ordering::SeqCst);
    let second = a_waiting_reload(&host, &name, true);
    ops.reconcile_release.store(true, Ordering::SeqCst);
    assert_eq!(answer_of(&second), judged());
    wait_for_state(&host, &name, TrustState::Ready);
    drop((lease, host));
}

/// The reader `name`'s entry holds now, where one stands.
fn standing_reader(host: &SharedHost, name: &VaultName) -> Option<Arc<FakeReader>> {
    let entry = host.shared.entries.get(name).expect("the vault is served");
    let state = entry.gate.lock().unwrap();
    state.reader.as_ref().map(Arc::clone)
}

/// Hold a schema-changing activation of `name` open once it has closed the
/// reader, asked on a thread of its own; the reload's receiver comes back.
fn a_held_schema_activation(
    host: &SharedHost,
    ops: &FakeOps,
    name: &VaultName,
) -> mpsc::Receiver<Answer> {
    ops.reload_schema_changed.store(true, Ordering::SeqCst);
    ops.block_schema_reload.store(true, Ordering::SeqCst);
    let answer = ask_a_reload(host, name, false);
    wait_for_flag("reload_started", &ops.reload_started);
    assert!(
        standing_reader(host, name).is_none(),
        "the activation left the reader it closed standing"
    );
    answer
}

/// End a held schema activation with one fact for its end's drain, the
/// reconcile turn that follows held at its own release: answers that turn's
/// number once it has begun.
fn end_the_activation_leaving_a_fact(ops: &FakeOps) -> usize {
    let next_turn = ops.reconciles.load(Ordering::SeqCst) + 1;
    ops.hold_reconciles_from.store(next_turn, Ordering::SeqCst);
    ops.facts_on_next_polls.store(1, Ordering::SeqCst);
    ops.reload_release.store(true, Ordering::SeqCst);
    next_turn
}

/// **A schema-changing reload whose end drain leaves facts answers its asker
/// as its leg ends**, with the new declaration's fingerprints, and installs
/// the new pool there with trust unbroken: the entry settles under healing,
/// not `Ready`, and an ordinary reconcile turn derives what the drain left.
#[test]
fn a_schema_reload_leaving_facts_answers_as_its_leg_ends_and_an_ordinary_reconcile_follows() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    let answer = a_held_schema_activation(&host, &ops, &name);

    let next_turn = end_the_activation_leaving_a_fact(&ops);

    assert_eq!(
        answer_of(&answer),
        Ok(fake_judgment(ReloadOutcome::SchemaChanged)),
        "the reload was not answered as its leg ended"
    );
    wait_for_reconciles(&ops, next_turn, "the reconcile the leg's end handed on to");
    assert_eq!(host.state(&name), answered(healing()));
    assert!(
        standing_reader(&host, &name).is_some(),
        "the leg ended with no pool installed"
    );
    {
        let entry = host.shared.entries.get(&name).unwrap();
        let state = entry.gate.lock().unwrap();
        assert_eq!(
            state.read_stance(),
            ReadStance::Settle,
            "installing the pool over the store the entry kept broke trust"
        );
    }
    ops.later_reconcile_release.store(true, Ordering::SeqCst);
    wait_for_state(&host, &name, TrustState::Ready);
    drop((lease, host));
}

/// **A read that met a schema reload's change before its activation closed
/// the reader answers from the pool the leg installs as it ends**, under the
/// healing that stands where the end drain left facts. The activation
/// derived every fact the read met, and the install is what wakes it: it
/// answers while the next reconcile turn is still held, not at that turn's
/// commit or at its bound.
#[test]
fn a_read_that_met_the_change_before_the_activation_answers_from_the_pool_its_leg_installs() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault_settling_within(&ops, LONG_SETTLE);
    ops.reload_schema_changed.store(true, Ordering::SeqCst);
    hold_a_settling_turn(&host, &ops, &name);
    let entry = host.shared.entries.get(&name).unwrap();
    let (paused, pause) = mpsc::channel();
    let (resume, resumed) = mpsc::channel::<()>();
    entry.gate.when_a_read_lets_go(move || {
        paused.send(()).expect("the case waits for the pause");
        resumed.recv().expect("the case resumes the read");
    });

    let before = host.read_evidence();
    thread::scope(|scope| {
        let reading = scope.spawn(|| host.begin_read(&name));
        pause
            .recv()
            .expect("the read paused after letting the gate go");
        let answer = a_waiting_reload(&host, &name, false);
        ops.block_schema_reload.store(true, Ordering::SeqCst);
        ops.reconcile_release.store(true, Ordering::SeqCst);
        wait_for_flag("reload_started", &ops.reload_started);
        resume.send(()).expect("the read is paused");
        assert!(
            !settle_waits_or_an_answer(&host, before, 2, &reading),
            "a read answered while the activation held the entry with no reader"
        );

        let next_turn = end_the_activation_leaving_a_fact(&ops);
        assert_eq!(
            answer_of(&answer),
            Ok(fake_judgment(ReloadOutcome::SchemaChanged))
        );
        wait_until(
            "the read to answer as the leg installs the pool",
            lifecycle_wait_budget(),
            || {
                if reading.is_finished() {
                    Observed::Met(())
                } else {
                    Observed::pending("the read is still waiting")
                }
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));
        let hold = reading
            .join()
            .expect("the read finished")
            .expect("a read the activation derived was refused");
        assert_eq!(hold.reading().published(), &Demand::State(healing()));
        let installed = standing_reader(&host, &name).expect("the leg installed a pool");
        assert!(
            Arc::ptr_eq(&hold.reader, &installed),
            "the read answered from a handle other than the one the leg installed"
        );
        // The turn the leg's end handed on to is still held, so nothing but
        // the install could have woken the read.
        wait_for_reconciles(&ops, next_turn, "the reconcile the leg's end handed on to");
        drop(hold);
        ops.later_reconcile_release.store(true, Ordering::SeqCst);
    });
    wait_for_state(&host, &name, TrustState::Ready);
    drop((entry, lease, host));
}

/// **A read that first meets the entry while the reader the activation
/// closed is not yet replaced waits for the facts the leg's end drain takes
/// in**: it does not answer as the leg installs the pool, and answers once
/// the reconcile turn that follows derives the drain's facts, never waiting
/// for `Ready`.
#[test]
fn a_read_that_met_the_activation_answers_once_the_next_intake_derives_the_end_drain() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    let answer = a_held_schema_activation(&host, &ops, &name);

    let before = host.read_evidence();
    thread::scope(|scope| {
        let reading = scope.spawn(|| host.begin_read(&name));
        wait_for_settle_waits(&host, before, 1);

        let next_turn = end_the_activation_leaving_a_fact(&ops);
        assert_eq!(
            answer_of(&answer),
            Ok(fake_judgment(ReloadOutcome::SchemaChanged))
        );
        wait_for_reconciles(&ops, next_turn, "the reconcile the leg's end handed on to");
        assert!(
            !settle_waits_or_an_answer(&host, before, 2, &reading),
            "a read that met the activation answered without the end drain's facts"
        );

        // Facts keep arriving at every drain from here, and the turn after
        // this one is held, so the entry is still healing when this turn
        // commits what the leg's drain left.
        arrange_for(&ops.continuous_fact_handoff_for, &name);
        ops.block_reconcile_at
            .store(next_turn + 1, Ordering::SeqCst);
        ops.later_reconcile_release.store(true, Ordering::SeqCst);
        let hold = reading
            .join()
            .expect("the read finished")
            .expect("a read whose facts were derived was refused");
        assert_eq!(
            hold.reading().published(),
            &Demand::State(healing()),
            "the read waited past the intake that derived its facts"
        );
        drop(hold);
        *ops.continuous_fact_handoff_for.lock().unwrap() = None;
        ops.reconcile_release.store(true, Ordering::SeqCst);
    });
    wait_for_state(&host, &name, TrustState::Ready);
    drop((lease, host));
}

/// **A schema-changing reload over a quiet vault answers with `Ready`**, from
/// a pool installed in place of the one its activation closed.
#[test]
fn a_schema_reload_over_a_quiet_vault_answers_with_ready_over_a_new_pool() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    ops.reload_schema_changed.store(true, Ordering::SeqCst);
    let closed = standing_reader(&host, &name).expect("a ready entry holds a reader");

    assert_eq!(
        host.reload(&name),
        Ok(fake_judgment(ReloadOutcome::SchemaChanged))
    );
    assert_eq!(host.state(&name), answered(TrustState::Ready));
    let installed = standing_reader(&host, &name).expect("the reload installed a pool");
    assert!(
        !Arc::ptr_eq(&installed, &closed),
        "the reload left the reader its activation closed standing"
    );
    drop((lease, host));
}

/// **A schema-changing activation over a settling vault that fails after it
/// began replacing the derived state withdraws trust and owes the
/// recovery**, though the fold published healing before it began: whether it
/// had begun is what the leg recorded, never the trust label.
#[test]
fn a_schema_activation_failing_after_it_began_over_a_settling_vault_withdraws_trust() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    ops.reload_schema_changed.store(true, Ordering::SeqCst);
    ops.reload_schema_apply_failure
        .store(true, Ordering::SeqCst);
    hold_a_settling_turn(&host, &ops, &name);
    let answer = a_waiting_reload(&host, &name, false);
    ops.block_recover.store(true, Ordering::SeqCst);

    end_the_turn_leaving_a_fact(&ops);

    let answered = answer_of(&answer);
    assert!(
        matches!(answered, Err(ReloadRefusal::Core(_))),
        "the reload answered {answered:?}"
    );
    let entry = host.shared.entries.get(&name).unwrap();
    {
        let state = entry.gate.lock().unwrap();
        assert!(
            matches!(state.trust, TrustState::Untrusted { .. }),
            "the failed activation left the trust at {:?}",
            state.trust
        );
        assert!(state.recovery_required, "no recovery is owed");
    }
    ops.recover_release.store(true, Ordering::SeqCst);
    drop((entry, lease, host));
}

/// Damage the end drain meets after a failed activation owes a rebuild,
/// while the reload still answers with the candidate's own error.
#[test]
fn a_failed_schema_activation_whose_end_drain_meets_damage_owes_a_rebuild() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    ops.reload_schema_changed.store(true, Ordering::SeqCst);
    ops.reload_schema_apply_failure
        .store(true, Ordering::SeqCst);
    ops.block_rebuild.store(true, Ordering::SeqCst);
    let damaged = name.clone();
    ops.once_the_candidate_is_read(move |ops| {
        arrange_for(&ops.damaged_poll_at, &damaged);
    });

    let answer = host.reload(&name);
    let state = host.state(&name);
    // Let any dispatched rebuild finish even if an assertion fails.
    ops.rebuild_release.store(true, Ordering::SeqCst);
    assert_eq!(
        answer,
        Err(ReloadRefusal::Core(ReloadError::SchemaApply(
            "the candidate could not be pinned".into()
        )))
    );
    assert_eq!(
        state,
        answered(TrustState::untrusted(
            UntrustedReason::store_damaged_rebuilding(
                "the watcher cursor points into a truncated page"
            )
        ))
    );
    wait_for_one_rebuild_to_ready(&host, &name, &ops);
    assert_eq!(ops.recovers.load(Ordering::SeqCst), 0);
    drop((lease, host));
}

/// **A fold over a quiet `Ready` vault whose drain meets an edit publishes
/// healing while it derives it**, so a read meanwhile waits for the edit
/// rather than answering `Ready` without it.
#[test]
fn a_fold_over_a_ready_vault_meeting_an_edit_publishes_healing_while_it_derives_it() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    let fold = ops.reconciles.load(Ordering::SeqCst) + 1;
    ops.hold_reconciles_from.store(fold, Ordering::SeqCst);
    ops.facts_on_next_polls.store(1, Ordering::SeqCst);

    let answer = ask_a_reload(&host, &name, false);
    wait_for_reconciles(&ops, fold, "the fold's commit to begin");
    assert_eq!(
        host.state(&name),
        answered(healing()),
        "Ready while the fold derives an edit"
    );

    ops.later_reconcile_release.store(true, Ordering::SeqCst);
    assert_eq!(answer_of(&answer), judged());
    drop((lease, host));
}

/// **A reload whose end drain is saturated publishes no `Ready`**, though it
/// delivered nothing: the facts left with the watcher go to the reconcile
/// it hands on to, and the vault reaches `Ready` once they are derived.
#[test]
fn a_reload_whose_end_drain_is_saturated_publishes_no_ready() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    ops.block_reload.store(true, Ordering::SeqCst);
    let answer = ask_a_reload(&host, &name, false);
    wait_for_flag("reload_started", &ops.reload_started);

    arrange_for(&ops.continuous_handoff_poll_for, &name);
    ops.reload_release.store(true, Ordering::SeqCst);

    assert_eq!(answer_of(&answer), judged());
    let after = host.state(&name);
    *ops.continuous_handoff_poll_for.lock().unwrap() = None;
    assert_ne!(
        after,
        answered(TrustState::Ready),
        "Ready published over a saturated end drain"
    );
    wait_for_state(&host, &name, TrustState::Ready);
    drop((lease, host));
}

/// **Damage a read carried to the claim while the fold derived answers the
/// reload before its candidate is read**, in either mode: the damage is
/// published with the rebuild it owes, and the rebuild returns the vault to
/// `Ready`.
#[test]
fn damage_a_read_met_while_the_fold_derives_answers_the_reload_before_its_candidate() {
    for dry_run in [false, true] {
        let ops = Arc::new(FakeOps::default());
        let (host, name, lease) = a_ready_vault(&ops);
        hold_a_settling_turn(&host, &ops, &name);
        let answer = a_waiting_reload(&host, &name, dry_run);

        ops.hold_reconciles_from.store(2, Ordering::SeqCst);
        end_the_turn_leaving_a_fact(&ops);
        wait_for_reconciles(&ops, 2, "the fold's commit to begin");
        host.shared
            .entries
            .get(&name)
            .unwrap()
            .gate
            .lock()
            .unwrap()
            .damage_met_under_a_claim = Some("the database disk image is malformed".into());
        ops.later_reconcile_release.store(true, Ordering::SeqCst);

        assert!(answer_of(&answer).is_err(), "dry run: {dry_run}");
        assert!(
            ops.reconciles_at_reload.lock().unwrap().is_empty(),
            "dry run: {dry_run}: the candidate was read over the damage"
        );
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.rebuilds.load(Ordering::SeqCst), 1, "dry run: {dry_run}");
        drop((lease, host));
    }
}

/// **A maintenance leg that ends with facts left for the waiting reload
/// publishes healing at its end**, so between the leg's end and the reload's
/// fold the vault does not stand `Ready` with an edit it has not derived.
#[test]
fn a_maintenance_leg_leaving_facts_for_the_waiting_reload_publishes_healing_at_its_end() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    hold_a_maintenance_leg(&host, &ops, &name);
    let answer = a_waiting_reload(&host, &name, false);

    // The leg's own drain meets the fact and then an empty poll; the fold's
    // drain, the poll after those, is held.
    let fold_drain = polls_of(&ops, &name) + 3;
    ops.block_poll_at.store(fold_drain, Ordering::SeqCst);
    ops.facts_on_next_polls.store(1, Ordering::SeqCst);
    ops.maintenance_release.store(true, Ordering::SeqCst);
    wait_for_flag("poll_started", &ops.poll_started);
    assert_eq!(
        host.state(&name),
        answered(healing()),
        "the leg's end left the vault Ready with a fact pending"
    );

    ops.poll_release.store(true, Ordering::SeqCst);
    assert_eq!(answer_of(&answer), judged());
    wait_for_state(&host, &name, TrustState::Ready);
    drop((lease, host));
}

/// **An apply admitted while a reload waits for a reconcile turn a leg's end
/// handed on runs after the reload**, though it was queued before the turn
/// began: the turn runs and hands the claim to the reload, never to the
/// apply ahead of it.
#[test]
fn an_apply_queued_behind_a_reload_waiting_for_a_handed_on_turn_runs_after_the_reload() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    hold_a_settling_turn(&host, &ops, &name);

    // The held turn ends leaving a fact, and the turn it hands on to is held
    // at its top, before it takes the gate.
    let next_turn = ops.reconcile_boundaries.load(Ordering::SeqCst) + 1;
    ops.block_reconcile_boundary_at
        .store(next_turn, Ordering::SeqCst);
    end_the_turn_leaving_a_fact(&ops);
    wait_for_flag(
        "reconcile_boundary_started",
        &ops.reconcile_boundary_started,
    );

    let answer = a_waiting_reload(&host, &name, false);
    let pending = host.admit_apply(&name, a_plan(&name)).expect("admitted");
    ops.block_reload.store(true, Ordering::SeqCst);
    ops.reconcile_boundary_release.store(true, Ordering::SeqCst);

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

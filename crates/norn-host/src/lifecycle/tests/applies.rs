//! The apply seam's lifecycle cases: how an apply is admitted onto its
//! entry, when the head of the queue takes the claim, what it takes in, and
//! how its caller is answered.
//!
//! Each case is one of the seam's named lifecycle cases, run against the
//! fake ops: the apply job's own work — planning, the applier, the changeset
//! — is the ops', and what is pinned here is the choreography around it. The
//! hosts here run no ambient dispatcher tick inside a case, so anything that
//! ran did so without one.

use super::*;
use norn_wire::{
    ApplyReport, ChangesetOutcome, ReasonCode, ResolvedPlan, RootIdentity, VaultAddress,
};

/// A resolved plan of no transitions for `name`: the fake applies it as it
/// stands.
fn a_plan(name: &VaultName) -> PlanDocument {
    PlanDocument::resolved(ResolvedPlan::new(
        VaultAddress::name(name.clone()),
        RootIdentity::from_device_and_inode(1, 1),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    ))
}

/// The resolved plan inside [`a_plan`].
fn the_plan(name: &VaultName) -> ResolvedPlan {
    match a_plan(name) {
        PlanDocument::Resolved(plan) => plan,
        PlanDocument::Operations(_) => unreachable!("a_plan is resolved"),
    }
}

/// A host over one vault with no ambient tick, holding the vault `Ready`.
fn a_ready_vault(ops: &Arc<FakeOps>) -> (Host<Arc<FakeOps>>, VaultName, DemandLease<Arc<FakeOps>>) {
    let name = VaultName::new("notes").unwrap();
    let host = host_without_ambient_polling(Arc::clone(ops), Roots::Absent(&[&name]), 1);
    let lease = host.demand(&name, AttachMode::Durable).unwrap();
    wait_for_state(&host, &name, TrustState::Ready);
    (host, name, lease)
}

/// The applies the fake ran, read once one has.
fn wait_for_an_apply(ops: &FakeOps) -> AppliedWhen {
    wait_until("the apply to run", lifecycle_wait_budget(), || {
        match ops.applies_ran.lock().unwrap().first() {
            Some(when) => Observed::Met(*when),
            None => Observed::pending("no apply ran"),
        }
    })
    .unwrap_or_else(|failure| panic!("{failure}"))
}

/// The report an answer that applied carries.
fn applied(answer: ApplyAnswer) -> ApplyReport {
    answer.expect("the apply applied").report
}

/// **Case 1: admission over a free claim takes it at admission.** The entry
/// is `Ready` and nothing holds it, so the apply is sent the moment it is
/// admitted: the claim is held before `admit_apply` returns, and the apply
/// runs with no dispatcher tick — the host here ticks once a minute.
#[test]
fn admission_over_a_free_claim_takes_the_claim_at_admission() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    let entry = host.shared.entries.get(&name).unwrap();
    assert!(
        !entry.gate.lock().unwrap().claim.is_held(),
        "something held the claim before the apply arrived"
    );
    ops.block_apply.store(true, Ordering::SeqCst);

    let pending = host.admit_apply(&name, a_plan(&name)).expect("admitted");
    assert!(
        entry.gate.lock().unwrap().claim.is_held(),
        "the apply was admitted over a free claim and did not take it"
    );
    wait_for_flag("apply_started", &ops.apply_started);
    ops.apply_release.store(true, Ordering::SeqCst);

    match applied(pending.wait()) {
        ApplyReport::Applied {
            plan, changeset, ..
        } => {
            assert_eq!(plan, the_plan(&name));
            assert_eq!(changeset, ChangesetOutcome::Committed);
        }
        other => panic!("the apply answered {other:?}"),
    }
    drop((lease, host));
}

/// Wait until the fake has run `expected` reconciles, counting one that is
/// blocked inside the fake.
fn wait_for_reconciles_begun(ops: &FakeOps, expected: usize) {
    wait_until("the reconciles to begin", lifecycle_wait_budget(), || {
        let begun = ops.reconciles.load(Ordering::SeqCst);
        if begun >= expected {
            Observed::Met(())
        } else {
            Observed::pending(format!("{begun} reconciles begun"))
        }
    })
    .unwrap_or_else(|failure| panic!("{failure}"));
}

/// Start a reconcile turn over the ready vault `name` and hold it inside the
/// fake: a watcher poll delivers one fact and schedules the turn.
fn hold_a_reconcile_turn(ops: &FakeOps, host: &Host<Arc<FakeOps>>) {
    ops.block_reconcile.store(true, Ordering::SeqCst);
    ops.facts_on_next_polls.store(1, Ordering::SeqCst);
    poll_watchers(&host.shared);
    wait_for_flag("reconcile_started", &ops.reconcile_started);
}

/// **Case 2: an apply arriving during a reconcile queues with no settle
/// wait, and takes the claim before the next turn and before a hand-on to
/// maintenance.** The turn in flight leaves a fact waiting and finds
/// maintenance due, so without the queue it would hand on to maintenance and
/// take another turn after it. The apply is admitted while the turn holds
/// the claim — admission returns with the turn still blocked — and the next
/// reconcile to run is the apply's own intake: the apply is off the queue and
/// running when it does, and no maintenance scan ran before it.
#[test]
fn an_apply_arriving_during_a_reconcile_takes_the_claim_before_the_next_turn_and_maintenance() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    let entry = host.shared.entries.get(&name).unwrap();
    hold_a_reconcile_turn(&ops, &host);
    ops.facts_on_next_polls.store(1, Ordering::SeqCst);
    arrange_for(&ops.maintenance_due_at, &name);

    let pending = host
        .admit_apply(&name, a_plan(&name))
        .expect("an apply over an entry taking in a change is admitted");
    {
        let state = entry.gate.lock().unwrap();
        assert!(!state.applies.is_empty(), "the apply was not queued");
        assert!(
            state.claim.leg().is_some(),
            "the turn no longer held the claim when admission returned"
        );
    }

    ops.hold_reconciles_from.store(2, Ordering::SeqCst);
    ops.block_reconcile.store(false, Ordering::SeqCst);
    ops.reconcile_release.store(true, Ordering::SeqCst);
    wait_for_reconciles_begun(&ops, 2);
    {
        let state = entry.gate.lock().unwrap();
        assert!(
            state.applies.is_empty() && state.running_apply.is_some(),
            "the reconcile after the turn is not the apply's intake"
        );
    }
    assert_eq!(ops.maintenances.load(Ordering::SeqCst), 0);
    ops.later_reconcile_release.store(true, Ordering::SeqCst);

    applied(pending.wait());
    assert_eq!(
        ops.applies_ran.lock().unwrap().first().copied(),
        Some(AppliedWhen {
            reconciles: 2,
            maintenances: 0,
            reader_stood: true,
        }),
        "the apply ran after another turn or a maintenance scan"
    );
    drop((lease, host));
}

/// **Case 3: under a sustained edit stream a queued apply runs within one
/// turn.** Every drain a reconcile turn takes finds the watcher saturated,
/// so the entry would take turn after turn under one claim no watcher poll
/// looks past. The apply is admitted while the third turn is in flight, and
/// runs straight after it: the one reconcile between that turn and the
/// apply is the apply's own intake.
#[test]
fn under_a_sustained_edit_stream_a_queued_apply_runs_within_one_turn() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    arrange_for(&ops.continuous_fact_handoff_for, &name);
    ops.hold_reconciles_from.store(3, Ordering::SeqCst);
    ops.facts_on_next_polls.store(1, Ordering::SeqCst);
    poll_watchers(&host.shared);
    wait_for_reconciles_begun(&ops, 3);

    let pending = host
        .admit_apply(&name, a_plan(&name))
        .expect("an apply over an entry taking in a stream is admitted");
    ops.later_reconcile_release.store(true, Ordering::SeqCst);
    let when = wait_for_an_apply(&ops);
    // The stream stops before the case ends, so the host it drops is quiet.
    *ops.continuous_fact_handoff_for.lock().unwrap() = None;

    assert_eq!(
        when.reconciles, 4,
        "the apply waited more than the turn in flight and its own intake"
    );
    applied(pending.wait());
    drop((lease, host));
}

/// **Case 12: the intake cutoff.** The apply takes in the fact the watcher
/// delivered before it, as its own reconcile ahead of the apply. A fact
/// delivered while the apply runs is not taken in before its changeset: the
/// claim the apply holds keeps a watcher poll off the entry, so the fact
/// stays with the watcher. A read meanwhile answers at once, from the entry
/// still `Ready` — the state before the apply. And the apply that ends with
/// that fact waiting hands the claim on to the reconcile that derives it.
#[test]
fn a_fact_delivered_during_an_apply_waits_for_its_changeset_and_hands_on_to_the_reconcile() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    ops.facts_on_next_polls.store(1, Ordering::SeqCst);
    ops.block_apply.store(true, Ordering::SeqCst);

    let pending = host.admit_apply(&name, a_plan(&name)).expect("admitted");
    wait_for_flag("apply_started", &ops.apply_started);
    assert_eq!(
        ops.applies_ran
            .lock()
            .unwrap()
            .first()
            .map(|when| when.reconciles),
        Some(1),
        "the apply did not derive the fact delivered before it first"
    );
    assert_eq!(*ops.reconciled_batches.lock().unwrap(), vec![a_fact()]);

    ops.facts_on_next_polls.store(1, Ordering::SeqCst);
    poll_watchers(&host.shared);
    assert_eq!(
        ops.facts_on_next_polls.load(Ordering::SeqCst),
        1,
        "a fact delivered during the apply was taken in before its changeset"
    );
    let hold = host
        .begin_read(&name)
        .expect("a read during an apply answers at once");
    assert_eq!(
        hold.reading().published(),
        &Demand::State(TrustState::Ready),
        "a read during the apply did not answer the state before it"
    );
    drop(hold);

    ops.apply_release.store(true, Ordering::SeqCst);
    applied(pending.wait());
    wait_for_reconciles_begun(&ops, 2);
    wait_for_state(&host, &name, TrustState::Ready);
    assert_eq!(
        *ops.reconciled_batches.lock().unwrap(),
        vec![a_fact(), a_fact()],
        "the fact that waited through the apply was not derived after it"
    );
    drop((lease, host));
}

/// **Case 14, before the mark: an unwind before the publishing mark answers
/// the apply not applied.** The worker unwinds after planning and before
/// publication, so the reply is dropped unanswered; its caller is answered
/// from the progress record with the cause the unwind published over the
/// entry — the one an unanswered reload's asker reads — and the plan
/// planning resolved.
#[test]
fn an_unwind_before_the_publishing_mark_answers_not_applied_with_the_cause_and_the_plan() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    ops.panic_in_apply_before_publishing
        .store(true, Ordering::SeqCst);

    let pending = host.admit_apply(&name, a_plan(&name)).expect("admitted");
    let refused = pending
        .wait()
        .expect_err("an unwound apply answered applied");

    assert_eq!(refused.code(), &ReasonCode::HostApplyNotRun);
    let expected_cause = ReadRefusal::NotServing(Demand::State(unwound(APPLY_PANIC))).answer(&name);
    assert_eq!(
        refused.detail(),
        &ErrorDetail::apply_not_run(expected_cause, Some(the_plan(&name)))
    );
    drop((lease, host));
}

/// **Case 14, answered late: the cause is the one the unwind published,
/// however long after it the caller asks.** The apply's caller asks only once
/// the entry has attached again and stands `Ready`, and is still answered with
/// the unwind: the answer was fixed where the leg's unwind cleanup published,
/// not read off the entry when the caller came to wait.
#[test]
fn an_unwound_apply_asked_late_is_answered_with_the_cause_its_unwind_published() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    ops.panic_in_apply_before_publishing
        .store(true, Ordering::SeqCst);

    let pending = host.admit_apply(&name, a_plan(&name)).expect("admitted");
    wait_for_state(&host, &name, unwound(APPLY_PANIC));
    ops.panic_in_apply_before_publishing
        .store(false, Ordering::SeqCst);
    let again = host.demand(&name, AttachMode::Durable).unwrap();
    wait_for_state(&host, &name, TrustState::Ready);

    let refused = answer_of(pending).expect_err("an unwound apply answered applied");
    let expected_cause = ReadRefusal::NotServing(Demand::State(unwound(APPLY_PANIC))).answer(&name);
    assert_eq!(
        refused.detail(),
        &ErrorDetail::apply_not_run(expected_cause, Some(the_plan(&name)))
    );
    drop((again, lease, host));
}

/// **Case 14, after the mark: an unwind after the publishing mark answers
/// the outcome unknown, with the resolved plan.** Some targets may have
/// landed, so the caller is told so and handed the plan that finishes them.
#[test]
fn an_unwind_after_the_publishing_mark_answers_unknown_with_the_resolved_plan() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    ops.panic_in_apply_after_publishing
        .store(true, Ordering::SeqCst);

    let pending = host.admit_apply(&name, a_plan(&name)).expect("admitted");
    let unknown = pending
        .wait()
        .expect_err("an unwound apply answered applied");

    assert_eq!(unknown.code(), &ReasonCode::HostApplyOutcomeUnknown);
    assert_eq!(
        unknown.detail(),
        &ErrorDetail::apply_outcome_unknown(the_plan(&name))
    );
    drop((lease, host));
}

/// **Case 15: a changeset that cannot commit after every target landed
/// answers applied with the entry healing, and the entry heals.** The answer
/// carries the resolved plan, as every outcome given after planning does,
/// and the paths the apply touched are taken in as facts the reconcile the
/// apply hands on to derives.
#[test]
fn a_changeset_that_cannot_commit_answers_applied_healing_and_the_entry_heals() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    ops.heal_in_apply.store(true, Ordering::SeqCst);

    let pending = host.admit_apply(&name, a_plan(&name)).expect("admitted");
    match applied(pending.wait()) {
        ApplyReport::Applied {
            plan, changeset, ..
        } => {
            assert_eq!(plan, the_plan(&name));
            assert_eq!(changeset, ChangesetOutcome::Healing);
        }
        other => panic!("the apply answered {other:?}"),
    }
    wait_for_reconciles_begun(&ops, 1);
    wait_for_state(&host, &name, TrustState::Ready);
    assert_eq!(
        *ops.reconciled_batches.lock().unwrap(),
        vec![a_fact()],
        "the heal the changeset owes was not derived"
    );
    drop((lease, host));
}

/// **Case 16: a dropped `PendingApply` does not stop the apply.** The caller
/// stops waiting while the apply runs; the apply finishes, gives the claim
/// back, and the entry takes the next apply as it would have.
#[test]
fn a_dropped_pending_apply_does_not_stop_the_apply() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    let entry = host.shared.entries.get(&name).unwrap();
    ops.block_apply.store(true, Ordering::SeqCst);

    drop(host.admit_apply(&name, a_plan(&name)).expect("admitted"));
    wait_for_flag("apply_started", &ops.apply_started);
    ops.apply_release.store(true, Ordering::SeqCst);
    wait_until(
        "the apply nobody waits for to give the claim back",
        lifecycle_wait_budget(),
        || {
            let held = entry.gate.lock().unwrap().claim.is_held();
            if held {
                Observed::pending("the claim is held")
            } else {
                Observed::Met(())
            }
        },
    )
    .unwrap_or_else(|failure| panic!("{failure}"));
    assert_eq!(ops.applies_ran.lock().unwrap().len(), 1);

    ops.block_apply.store(false, Ordering::SeqCst);
    applied(
        host.admit_apply(&name, a_plan(&name))
            .expect("admitted")
            .wait(),
    );
    assert_eq!(ops.applies_ran.lock().unwrap().len(), 2);
    drop((lease, host));
}

/// **An apply admitted over a reconcile waiting in the channel takes the
/// claim, and the queue slot holds its send until the reconcile arrives.**
/// The reconcile a poll scheduled is in the channel, behind the one worker
/// another vault's attach holds, so its marker and its slot both stand.
/// Admission schedules the apply over that marker — the entry's work at its
/// own epoch, with the gate held — and the slot the reconcile holds is what
/// keeps the apply from being sent beside it. When the superseded reconcile
/// arrives it runs nothing and sends the apply, whose intake derives the
/// fact the reconcile was scheduled for.
#[test]
fn an_apply_over_a_reconcile_in_the_channel_waits_on_its_slot_and_is_sent_when_it_arrives() {
    let ops = Arc::new(FakeOps::default());
    let name = VaultName::new("a").unwrap();
    let holding = VaultName::new("b").unwrap();
    let host = host_without_ambient_polling(Arc::clone(&ops), Roots::Absent(&[&name, &holding]), 1);
    let lease = host.demand(&name, AttachMode::Durable).unwrap();
    wait_for_state(&host, &name, TrustState::Ready);
    ops.block_attach.store(true, Ordering::SeqCst);
    let holding_lease = host.demand(&holding, AttachMode::Durable).unwrap();
    wait_for_flag("attach_started", &ops.attach_started);

    ops.facts_on_next_polls.store(1, Ordering::SeqCst);
    poll_watchers(&host.shared);
    let entry = host.shared.entries.get(&name).unwrap();
    let reconcile = {
        let state = entry.gate.lock().unwrap();
        assert!(
            matches!(state.claim.marker(), Some(Job::Reconcile(..))),
            "the poll scheduled no reconcile"
        );
        state.claim.slot().expect("the reconcile is in the channel")
    };

    let pending = host.admit_apply(&name, a_plan(&name)).expect("admitted");
    {
        let state = entry.gate.lock().unwrap();
        assert!(
            matches!(state.claim.marker(), Some(Job::Apply(..))),
            "the apply did not take the claim over the reconcile"
        );
        assert_eq!(
            state.claim.slot(),
            Some(reconcile),
            "the apply was sent beside the reconcile already in the channel"
        );
    }

    ops.block_attach.store(false, Ordering::SeqCst);
    ops.attach_release.store(true, Ordering::SeqCst);
    let when = wait_for_an_apply(&ops);
    applied(pending.wait());
    assert_eq!(
        when.reconciles, 1,
        "the superseded reconcile ran, or the apply did not derive its fact"
    );
    drop((lease, holding_lease, host));
}

/// The answer `pending` gives, waited for within the lifecycle budget, so a
/// queued apply nothing answers fails the case rather than hanging it.
fn answer_of(pending: PendingApply) -> ApplyAnswer {
    let (sent, answer) = mpsc::channel();
    thread::spawn(move || {
        let _ = sent.send(pending.wait());
    });
    wait_until(
        "the apply to be answered",
        lifecycle_wait_budget(),
        || match answer.try_recv() {
            Ok(answered) => Observed::Met(answered),
            Err(_) => Observed::pending("the apply is still waiting"),
        },
    )
    .unwrap_or_else(|failure| panic!("{failure}"))
}

/// The cause an apply answered not applied carries, where it stopped before
/// planning: no plan beside it.
fn not_applied_cause(answer: ApplyAnswer) -> ErrorEnvelope {
    let refused = answer.expect_err("the apply answered as applied");
    match refused.detail() {
        ErrorDetail::ApplyNotRun {
            cause, plan: None, ..
        } => cause.clone(),
        other => panic!("the apply answered {other:?}"),
    }
}

/// **Case 7: a leg's unwind answers every queued apply not applied, with
/// the unwind as its cause.** The apply queues behind a reconcile turn that
/// then panics. The unwind's cleanup releases the entry and publishes the
/// leg's unwind, re-arming nothing, so the apply is answered with that
/// cause rather than left waiting on an entry nothing will serve.
#[test]
fn a_leg_unwind_answers_every_queued_apply_not_applied_with_the_unwind() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    ops.panic_in_reconcile.store(true, Ordering::SeqCst);
    hold_a_reconcile_turn(&ops, &host);
    let first = host.admit_apply(&name, a_plan(&name)).expect("admitted");
    let second = host.admit_apply(&name, a_plan(&name)).expect("admitted");

    ops.reconcile_release.store(true, Ordering::SeqCst);
    let expected = ReadRefusal::NotServing(Demand::State(unwound(RECONCILE_PANIC))).answer(&name);
    assert_eq!(not_applied_cause(answer_of(first)), expected);
    assert_eq!(not_applied_cause(answer_of(second)), expected);
    assert!(ops.applies_ran.lock().unwrap().is_empty());
    drop((lease, host));
}

/// **Case 8: lost trust answers every queued apply not applied, with the
/// trust it lost.** The apply queues behind a reconcile turn whose watcher
/// fails terminally. The turn publishes the entry untrusted and owing a
/// recovery no lease has asked for, and the apply is answered with that
/// cause: it runs over no store the entry no longer vouches for, and does not
/// wait for a recovery nothing has asked for.
#[test]
fn lost_trust_answers_every_queued_apply_not_applied_with_the_loss() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    ops.terminal_reconcile.store(true, Ordering::SeqCst);
    hold_a_reconcile_turn(&ops, &host);
    let pending = host.admit_apply(&name, a_plan(&name)).expect("admitted");

    ops.reconcile_release.store(true, Ordering::SeqCst);
    let lost = TrustState::untrusted(watcher_lost(WatchError::Backend("lost".into())));
    assert_eq!(
        not_applied_cause(answer_of(pending)),
        ReadRefusal::NotServing(Demand::State(lost)).answer(&name)
    );
    assert!(ops.applies_ran.lock().unwrap().is_empty());
    drop((lease, host));
}

/// **Case 9: a failed attach answers every queued apply not applied, with
/// its failure.** The apply is admitted over an unattached vault, so it
/// queues behind the attach its demand owes, and that attach fails: the
/// failure the attach publishes is the apply's answer.
#[test]
fn a_failed_attach_answers_every_queued_apply_not_applied_with_its_failure() {
    let ops = Arc::new(FakeOps::default());
    let name = VaultName::new("notes").unwrap();
    let host = host_without_ambient_polling(Arc::clone(&ops), Roots::Absent(&[&name]), 1);
    ops.block_attach.store(true, Ordering::SeqCst);
    ops.terminal_attach.store(true, Ordering::SeqCst);
    let pending = host
        .admit_apply(&name, a_plan(&name))
        .expect("an apply over an unattached vault is admitted");
    wait_for_flag("attach_started", &ops.attach_started);
    assert!(
        !host
            .shared
            .entries
            .get(&name)
            .unwrap()
            .gate
            .lock()
            .unwrap()
            .applies
            .is_empty(),
        "the apply did not queue behind the attach"
    );

    ops.attach_release.store(true, Ordering::SeqCst);
    let lost = TrustState::untrusted(watcher_lost(WatchError::Backend("lost".into())));
    assert_eq!(
        not_applied_cause(answer_of(pending)),
        ReadRefusal::NotServing(Demand::State(lost)).answer(&name)
    );
    assert!(ops.applies_ran.lock().unwrap().is_empty());
    drop(host);
}

/// **Case 10, a park: a park answers every queued apply not applied at
/// once, with the park.** The apply queues behind a reconcile turn, and the
/// entry is parked while the turn still runs: the apply is answered with the
/// park's own refusal before the turn ends, since no teardown waits for the
/// work it moves the entry past. Each park the registry raises is taken in
/// turn, and so is a maintainer another process holds, which an attach meets.
#[cfg(unix)]
#[test]
fn a_park_answers_every_queued_apply_not_applied_at_once_with_the_park() {
    for park in [Park::Identity, Park::DuplicateRoot] {
        let scratch = temp_base("apply-queued-over-a-park");
        let ops = Arc::new(FakeOps::default());
        let name = VaultName::new("notes").unwrap();
        let root = scratch.root().join("notes");
        let host = host_without_ambient_polling(
            Arc::clone(&ops),
            Roots::Created(&[(&name, root.as_path())]),
            1,
        );
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        hold_a_reconcile_turn(&ops, &host);
        let pending = host.admit_apply(&name, a_plan(&name)).expect("admitted");

        let refused = park_entry(&host, &name, &root, park);
        let cause = not_applied_cause(answer_of(pending));
        assert_eq!(cause.detail(), &refused, "{park:?}");
        ops.reconcile_release.store(true, Ordering::SeqCst);
        assert!(ops.applies_ran.lock().unwrap().is_empty(), "{park:?}");
        drop((lease, host));
    }

    let ops = Arc::new(FakeOps::default());
    let name = VaultName::new("notes").unwrap();
    let host = host_without_ambient_polling(Arc::clone(&ops), Roots::Absent(&[&name]), 1);
    ops.block_attach.store(true, Ordering::SeqCst);
    ops.contend_attach.store(true, Ordering::SeqCst);
    let pending = host.admit_apply(&name, a_plan(&name)).expect("admitted");
    wait_for_flag("attach_started", &ops.attach_started);
    ops.attach_release.store(true, Ordering::SeqCst);
    let cause = not_applied_cause(answer_of(pending));
    assert_eq!(
        cause,
        ReadRefusal::NotServing(Demand::MaintainerContended(MaintainerIdentity::unknown()))
            .answer(&name),
        "a maintainer held elsewhere"
    );
    drop(host);
}

/// **Case 10, the host's destruction: it answers every queued apply not
/// applied at once, and waits for none.** The apply queues behind a
/// reconcile turn, and the host is destroyed while the turn still runs,
/// with a lease outliving it. The apply is answered while the destruction
/// is still waiting for the turn, with the teardown the entry publishes.
#[test]
fn the_hosts_destruction_answers_every_queued_apply_not_applied_at_once() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    hold_a_reconcile_turn(&ops, &host);
    let pending = host.admit_apply(&name, a_plan(&name)).expect("admitted");

    let destroyed = thread::spawn(move || drop(host));
    let cause = not_applied_cause(answer_of(pending));
    assert_eq!(
        cause,
        ReadRefusal::NotServing(Demand::State(TrustState::warming(
            WarmingPhase::ReleasingCoverage,
            0,
            None
        )))
        .answer(&name)
    );
    assert!(
        !destroyed.is_finished(),
        "the destruction did not wait for the turn"
    );
    ops.reconcile_release.store(true, Ordering::SeqCst);
    destroyed.join().expect("the host was destroyed");
    assert!(ops.applies_ran.lock().unwrap().is_empty());
    drop(lease);
}

/// Begin the idle detach of `name`, whose lease has gone, and hold it inside
/// the fake's detach: the release window stands open over the entry.
fn hold_an_idle_detach(ops: &FakeOps, host: &Host<Arc<FakeOps>>, name: &VaultName) {
    ops.block_detach.store(true, Ordering::SeqCst);
    dispatcher_tick(&host.shared, Instant::now() + Duration::from_secs(61));
    wait_for_flag("detach_started", &ops.detach_started);
    assert!(
        host.shared
            .entries
            .get(name)
            .unwrap()
            .gate
            .lock()
            .unwrap()
            .detach_in_flight,
        "no idle detach stands open over the entry"
    );
}

/// **Case 11: an idle detach in flight at admission re-arms the attach and
/// keeps the queue.** The vault's last lease went and its idle detach is
/// giving the coverage back when the apply arrives. The apply is admitted —
/// a write never refuses because its vault went idle — and its demand is
/// what the release honors: the release re-attaches, and the apply runs over
/// the coverage that attach installs.
#[test]
fn an_idle_detach_in_flight_at_admission_re_arms_the_attach_and_keeps_the_queue() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    drop(lease);
    hold_an_idle_detach(&ops, &host, &name);

    let pending = host
        .admit_apply(&name, a_plan(&name))
        .expect("an apply over an entry releasing for idleness is admitted");
    ops.block_detach.store(false, Ordering::SeqCst);
    ops.detach_release.store(true, Ordering::SeqCst);

    applied(answer_of(pending));
    assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);
    assert_eq!(
        ops.attaches.load(Ordering::SeqCst),
        2,
        "the apply did not run over a re-attach"
    );
    drop(host);
}

/// **A release that re-arms nothing answers every queued apply not applied,
/// with what it publishes.** The vault's declaration withholds trust, so the
/// recovery it owes waits for the vault to change and no demand re-arms it.
/// An apply admitted while the vault's idle detach gives its coverage back
/// queues behind a re-attach its demand does not owe, and the release that
/// ends unattached answers it rather than leaving it waiting on an entry
/// nothing will serve.
#[test]
fn a_release_that_re_arms_nothing_answers_every_queued_apply_not_applied() {
    let ops = Arc::new(FakeOps::default());
    ops.withholds_trust.store(true, Ordering::SeqCst);
    let name = VaultName::new("notes").unwrap();
    let host = host_without_ambient_polling(Arc::clone(&ops), Roots::Absent(&[&name]), 1);
    drop(host.demand(&name, AttachMode::Durable).unwrap());
    wait_for_state(
        &host,
        &name,
        TrustState::untrusted(UntrustedReason::schema_unreadable(
            "this fake withholds trust",
        )),
    );
    hold_an_idle_detach(&ops, &host, &name);

    let pending = host.admit_apply(&name, a_plan(&name)).expect("admitted");
    ops.block_detach.store(false, Ordering::SeqCst);
    ops.detach_release.store(true, Ordering::SeqCst);

    assert_eq!(
        not_applied_cause(answer_of(pending)),
        ReadRefusal::NotServing(Demand::State(TrustState::Unattached)).answer(&name)
    );
    assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);
    drop(host);
}

/// **Case 6: read damage carried to a hand-on is published with its rebuild
/// in that hold, and answers the queue.** A read meets damage while a
/// reconcile turn holds the entry, so the verdict is carried to the turn's
/// end; an apply is queued behind the turn, which leaves a fact waiting and
/// finds maintenance due. The turn's end hands the claim to the rebuild the
/// damage owes, publishing the damage as it does, rather than to the
/// maintenance scan: the apply is answered with the damage, no maintenance
/// scan or apply runs over the damaged store, and the rebuild runs next.
#[test]
fn read_damage_carried_to_a_hand_on_is_published_with_its_rebuild_and_answers_the_queue() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    let hold = host
        .begin_read(&name)
        .expect("a ready entry answers a read");
    hold_a_reconcile_turn(&ops, &host);
    ops.facts_on_next_polls.store(1, Ordering::SeqCst);
    arrange_for(&ops.maintenance_due_at, &name);
    let pending = host.admit_apply(&name, a_plan(&name)).expect("admitted");
    let refusal = host.withdraw_for_read_damage(&hold, "the store is damaged".to_string());
    assert!(
        !matches!(
            refusal,
            ReadRefusal::NotServing(Demand::State(TrustState::Untrusted { .. }))
        ),
        "the read published the damage beneath the turn"
    );
    drop(hold);

    ops.block_rebuild.store(true, Ordering::SeqCst);
    ops.reconcile_release.store(true, Ordering::SeqCst);
    let damaged = TrustState::untrusted(UntrustedReason::store_damaged_rebuilding(
        "the store is damaged",
    ));
    assert_eq!(
        not_applied_cause(answer_of(pending)),
        ReadRefusal::NotServing(Demand::State(damaged)).answer(&name)
    );
    wait_for_flag("rebuild_started", &ops.rebuild_started);
    assert_eq!(
        ops.maintenances.load(Ordering::SeqCst),
        0,
        "a maintenance scan ran over the damage the read met"
    );
    assert!(ops.applies_ran.lock().unwrap().is_empty());
    ops.rebuild_release.store(true, Ordering::SeqCst);
    wait_for_state(&host, &name, TrustState::Ready);
    drop((lease, host));
}

/// Hold reconcile `turn` inside the fake, and answer whether the apply
/// queued on `name` is still queued while it runs — that turn is not the
/// apply's own intake — then let it go.
fn the_turn_runs_ahead_of_the_queue(
    ops: &FakeOps,
    host: &Host<Arc<FakeOps>>,
    name: &VaultName,
    turn: usize,
) {
    wait_for_reconciles_begun(ops, turn);
    wait_for_flag("reconcile_started", &ops.reconcile_started);
    {
        let entry = host.shared.entries.get(name).unwrap();
        let state = entry.gate.lock().unwrap();
        assert!(
            !state.applies.is_empty() && state.running_apply.is_none(),
            "the apply took the claim at the turn its entry owed before trust was held"
        );
    }
    ops.reconcile_release.store(true, Ordering::SeqCst);
}

/// **Case 5: an apply queued during an attach, a recovery or a rebuild runs
/// only once a reader stands and trust is held.** An attach and a recovery
/// each end with a fact their drain delivered still to derive, so each hands
/// the claim on to a reconcile before the entry publishes `Ready`. That
/// reconcile is the entry's own work, not the apply's intake: the apply waits
/// through it, and takes the claim only once the entry stands `Ready` over a
/// reader. A rebuild publishes the damage it resolves until it ends, so an
/// apply arriving during one is refused with it rather than queued.
#[test]
fn an_apply_queued_during_an_attach_recovery_or_rebuild_runs_once_trust_is_held() {
    // An attach from nothing, which the apply's own demand schedules.
    let ops = Arc::new(FakeOps::default());
    let name = VaultName::new("notes").unwrap();
    let host = host_without_ambient_polling(Arc::clone(&ops), Roots::Absent(&[&name]), 1);
    ops.block_attach.store(true, Ordering::SeqCst);
    let pending = host.admit_apply(&name, a_plan(&name)).expect("admitted");
    wait_for_flag("attach_started", &ops.attach_started);
    ops.facts_on_next_polls.store(1, Ordering::SeqCst);
    ops.block_reconcile_at.store(1, Ordering::SeqCst);
    ops.attach_release.store(true, Ordering::SeqCst);
    the_turn_runs_ahead_of_the_queue(&ops, &host, &name, 1);
    applied(answer_of(pending));
    assert_eq!(ops.applies_ran.lock().unwrap()[0].reconciles, 1, "attach");
    drop(host);

    // A recovery a demand asks for once a turn lost the watcher.
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    ops.terminal_reconcile.store(true, Ordering::SeqCst);
    ops.facts_on_next_polls.store(1, Ordering::SeqCst);
    poll_watchers(&host.shared);
    wait_for_state(
        &host,
        &name,
        TrustState::untrusted(watcher_lost(WatchError::Backend("lost".into()))),
    );
    ops.block_recover.store(true, Ordering::SeqCst);
    let recovering = host.demand(&name, AttachMode::Durable).unwrap();
    wait_for_flag("recover_started", &ops.recover_started);
    let pending = host.admit_apply(&name, a_plan(&name)).expect("admitted");
    ops.block_reconcile_at.store(2, Ordering::SeqCst);
    ops.recover_release.store(true, Ordering::SeqCst);
    the_turn_runs_ahead_of_the_queue(&ops, &host, &name, 2);
    applied(answer_of(pending));
    assert_eq!(ops.applies_ran.lock().unwrap()[0].reconciles, 2, "recovery");
    drop((recovering, lease, host));

    // A rebuild a turn that met damage hands on to. It publishes the damage
    // throughout, so no apply queues behind it: admission refuses with the
    // damage, and an apply admitted once the rebuild publishes `Ready` runs.
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    arrange_for(&ops.damaged_reconcile_at, &name);
    ops.block_rebuild.store(true, Ordering::SeqCst);
    ops.facts_on_next_polls.store(1, Ordering::SeqCst);
    poll_watchers(&host.shared);
    wait_for_flag("rebuild_started", &ops.rebuild_started);
    let damaged = TrustState::untrusted(UntrustedReason::store_damaged_rebuilding(
        "the database disk image is malformed",
    ));
    assert_eq!(
        host.admit_apply(&name, a_plan(&name))
            .expect_err("an apply was admitted over a rebuild"),
        ReadRefusal::NotServing(Demand::State(damaged)).answer(&name)
    );
    ops.rebuild_release.store(true, Ordering::SeqCst);
    wait_for_state(&host, &name, TrustState::Ready);
    applied(answer_of(
        host.admit_apply(&name, a_plan(&name)).expect("admitted"),
    ));
    drop((lease, host));
}

/// **Case 4: an apply queued during a schema reload runs only after the
/// reload publishes `Ready`.** The reload closed the entry's reader when it
/// began, and its drain delivers a fact, so it hands the claim on to its own
/// next turn before it mints a reader again. The apply queued meanwhile does
/// not take the claim at that hand-on: the reload's turn runs first, and the
/// apply runs over the reader the reload minted.
#[test]
fn an_apply_queued_during_a_schema_reload_runs_only_after_it_publishes_ready() {
    let ops = Arc::new(FakeOps::default());
    ops.reload_supported.store(true, Ordering::SeqCst);
    ops.reload_schema_changed.store(true, Ordering::SeqCst);
    let (host, name, lease) = a_ready_vault(&ops);
    let host = Arc::new(host);
    ops.block_schema_reload.store(true, Ordering::SeqCst);
    let reload = reload_on_a_thread(&host, &name, false);
    wait_for_flag("reload_started", &ops.reload_started);
    let pending = host
        .admit_apply(&name, a_plan(&name))
        .expect("an apply over a reloading entry is admitted");
    ops.facts_on_next_polls.store(1, Ordering::SeqCst);

    ops.reload_release.store(true, Ordering::SeqCst);
    applied(answer_of(pending));
    reload
        .join()
        .unwrap()
        .expect("the host is running")
        .expect("the reload applied");
    let when = ops.applies_ran.lock().unwrap()[0];
    assert!(when.reader_stood, "the apply ran with no reader standing");
    assert_eq!(
        when.reconciles, 1,
        "the reload's own turn did not run ahead of the apply"
    );
    drop(lease);
}

/// What [`an_apply_handed_off_into_a_full_queue`] leaves a case: the host,
/// the vault the apply was admitted against, the apply's handle and the two
/// vaults' leases.
type HandedOff = (
    Host<Arc<FakeOps>>,
    VaultName,
    PendingApply,
    [DemandLease<Arc<FakeOps>>; 2],
);

/// Two ready vaults on one worker, `a` and `b`, with an apply queued on
/// `a` behind a reconcile turn and `b`'s reconcile filling the job queue
/// when that turn ends: the turn's hand-off meets a full queue, and so does
/// the send its leg's end tries again. Answers the host, the two names, the
/// apply's handle and the leases.
fn an_apply_handed_off_into_a_full_queue(ops: &Arc<FakeOps>, maintenance_due: bool) -> HandedOff {
    let a = VaultName::new("a").unwrap();
    let b = VaultName::new("b").unwrap();
    let host = host_without_ambient_polling(Arc::clone(ops), Roots::Absent(&[&a, &b]), 1);
    let leases = [&a, &b].map(|name| {
        let lease = host.demand(name, AttachMode::Durable).unwrap();
        wait_for_state(&host, name, TrustState::Ready);
        lease
    });
    hold_a_reconcile_turn(ops, &host);
    if maintenance_due {
        arrange_for(&ops.maintenance_due_at, &a);
    }
    let pending = host.admit_apply(&a, a_plan(&a)).expect("admitted");
    ops.block_reconcile.store(false, Ordering::SeqCst);
    ops.facts_on_next_polls.store(1, Ordering::SeqCst);
    poll_watchers(&host.shared);
    assert!(
        host.shared
            .entries
            .get(&b)
            .unwrap()
            .gate
            .lock()
            .unwrap()
            .claim
            .slot_taken(),
        "b's reconcile is not waiting in the queue"
    );

    ops.reconcile_release.store(true, Ordering::SeqCst);
    let entry = host.shared.entries.get(&a).unwrap();
    wait_until(
        "a's hand-off to stand on its marker once b's reconcile ran",
        lifecycle_wait_budget(),
        || {
            let state = entry.gate.lock().unwrap();
            if ops.reconciles.load(Ordering::SeqCst) == 2
                && state.claim.marker().is_some()
                && !state.claim.slot_taken()
            {
                Observed::Met(())
            } else {
                Observed::pending("a's hand-off is not yet on its marker")
            }
        },
    )
    .unwrap_or_else(|failure| panic!("{failure}"));
    (host, a, pending, leases)
}

/// **Case 2, the turn's end: a turn in flight hands the claim to the queued
/// apply itself, ahead of the maintenance it found due.** The hand-off meets
/// a full queue, so the job it named stands as the entry's marker, which is
/// read before anything runs it: it is the apply, not the maintenance scan a
/// yield at the scan's own start would still answer for.
#[test]
fn a_turns_end_hands_the_claim_to_the_queued_apply_ahead_of_due_maintenance() {
    let ops = Arc::new(FakeOps::default());
    let (host, a, pending, leases) = an_apply_handed_off_into_a_full_queue(&ops, true);
    assert!(
        matches!(
            host.shared
                .entries
                .get(&a)
                .unwrap()
                .gate
                .lock()
                .unwrap()
                .claim
                .marker(),
            Some(Job::Apply(..))
        ),
        "the turn's end handed the claim to other work than the queued apply"
    );
    retry_pending_dispatches(&host.shared);
    applied(answer_of(pending));
    assert_eq!(ops.maintenances.load(Ordering::SeqCst), 0);
    drop((leases, host));
}

/// **Case 18: a full job queue on a hand-off to an apply retries through the
/// marker.** The turn's hand-off to the queued apply meets a full queue, as
/// does its leg's own retry, so the apply stands as the entry's marker with
/// no queue slot, and it does not run. The next retry of refused dispatches
/// sends it, and it runs.
#[test]
fn a_full_job_queue_on_a_hand_off_to_an_apply_retries_through_the_marker() {
    let ops = Arc::new(FakeOps::default());
    let (host, _a, pending, leases) = an_apply_handed_off_into_a_full_queue(&ops, false);
    assert!(
        ops.applies_ran.lock().unwrap().is_empty(),
        "the apply ran with no retry to send it"
    );
    retry_pending_dispatches(&host.shared);
    applied(answer_of(pending));
    assert_eq!(ops.applies_ran.lock().unwrap().len(), 1);
    drop((leases, host));
}

/// **Case 2, the maintenance scan's start: a scan scheduled while an apply
/// was admitted yields the claim to it before it begins.** The apply
/// arrives while a watcher poll holds the entry, so it queues; the poll then
/// finds maintenance due and schedules the scan. The scan's start is the
/// first point the queue can take the claim, and it does: the apply runs
/// with no maintenance scan run before it.
#[test]
fn a_maintenance_scan_yields_the_claim_to_an_apply_queued_before_it_began() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    *ops.poll_gate.lock().unwrap() = Some(name.clone());
    arrange_for(&ops.maintenance_due_at, &name);
    let shared = Arc::clone(&host.shared);
    let polling = thread::spawn(move || poll_watchers(&shared));
    wait_for_flag("poll_started", &ops.poll_started);

    let pending = host.admit_apply(&name, a_plan(&name)).expect("admitted");
    assert!(
        !host
            .shared
            .entries
            .get(&name)
            .unwrap()
            .gate
            .lock()
            .unwrap()
            .applies
            .is_empty(),
        "the apply did not queue behind the poll"
    );
    *ops.poll_gate.lock().unwrap() = None;
    ops.poll_release.store(true, Ordering::SeqCst);
    polling.join().expect("the poll");
    applied(answer_of(pending));
    assert_eq!(
        ops.applies_ran.lock().unwrap()[0].maintenances,
        0,
        "a maintenance scan ran ahead of the apply queued before it began"
    );
    drop((lease, host));
}

/// Whether `host` refuses the registration changes an apply holds off, and
/// an explicit reload, over `name`: `vault unregister` and `vault set` as
/// held, and the reload as unavailable.
fn registration_changes_refuse_as_held(host: &Host<Arc<FakeOps>>, name: &VaultName) {
    assert_eq!(
        host.unregister(name, false),
        Err(RegistrationRefusal::Serving(ServingRefusal::Held)),
        "vault unregister was not refused as held"
    );
    assert_eq!(
        host.set(&SetParams::new(name.clone())).map(|_| ()),
        Err(RegistrationRefusal::Serving(ServingRefusal::Held)),
        "vault set was not refused as held"
    );
}

/// **Case 13: `vault unregister` and `vault set` refuse as held while an
/// apply is queued or running, and an explicit reload refuses while applies
/// are queued.** The apply first waits in the queue behind a reconcile turn,
/// then runs held inside the fake; both changes refuse as held at each
/// point, and the reload refuses while the apply is queued. Once the apply
/// has answered, the vault still stands registered.
#[test]
fn registration_changes_refuse_as_held_while_an_apply_is_queued_or_running() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    hold_a_reconcile_turn(&ops, &host);
    ops.block_apply.store(true, Ordering::SeqCst);
    let pending = host.admit_apply(&name, a_plan(&name)).expect("admitted");

    registration_changes_refuse_as_held(&host, &name);
    assert!(
        matches!(host.reload(&name), Err(ReloadRefusal::Unavailable(_))),
        "an explicit reload was not refused while an apply was queued"
    );

    ops.reconcile_release.store(true, Ordering::SeqCst);
    wait_for_flag("apply_started", &ops.apply_started);
    registration_changes_refuse_as_held(&host, &name);

    ops.apply_release.store(true, Ordering::SeqCst);
    applied(answer_of(pending));
    assert!(host.shared.entries.get(&name).is_some());
    drop((lease, host));
}

/// A ready vault over a root on disk, which a park can be raised over.
#[cfg(unix)]
fn a_ready_vault_on_disk(
    ops: &Arc<FakeOps>,
    scratch: &Scratch,
) -> (
    Host<Arc<FakeOps>>,
    VaultName,
    std::path::PathBuf,
    DemandLease<Arc<FakeOps>>,
) {
    let name = VaultName::new("notes").unwrap();
    let root = scratch.root().join("notes");
    let host = host_without_ambient_polling(
        Arc::clone(ops),
        Roots::Created(&[(&name, root.as_path())]),
        1,
    );
    let lease = host.demand(&name, AttachMode::Durable).unwrap();
    wait_for_state(&host, &name, TrustState::Ready);
    (host, name, root, lease)
}

/// **Case 17, before publication: a running apply that has not begun
/// publishing stops at a teardown, and answers not applied with the
/// teardown's cause.** The apply has planned and is about to publish when a
/// park moves the entry past its leg. Its check before the first publication
/// finds the leg no longer standing, so it publishes nothing, and its caller
/// is answered with the park and the plan it resolved.
#[cfg(unix)]
#[test]
fn a_running_apply_that_has_not_begun_publishing_stops_at_a_teardown() {
    let scratch = temp_base("apply-stopped-at-a-teardown");
    let ops = Arc::new(FakeOps::default());
    let (host, name, root, lease) = a_ready_vault_on_disk(&ops, &scratch);
    ops.block_apply.store(true, Ordering::SeqCst);
    let pending = host.admit_apply(&name, a_plan(&name)).expect("admitted");
    wait_for_flag("apply_started", &ops.apply_started);

    let parked = park_entry(&host, &name, &root, Park::DuplicateRoot);
    ops.apply_release.store(true, Ordering::SeqCst);
    let refused = answer_of(pending).expect_err("the apply published over a teardown");
    match refused.detail() {
        ErrorDetail::ApplyNotRun {
            cause,
            plan: Some(plan),
            ..
        } => {
            assert_eq!(cause.detail(), &parked);
            assert_eq!(plan, &the_plan(&name));
        }
        other => panic!("the apply answered {other:?}"),
    }
    assert_eq!(ops.applies_stood_down.load(Ordering::SeqCst), 1);
    drop((lease, host));
}

/// **Case 17, after publication began: a publishing apply finishes its
/// publication and its changeset before its leg ends.** A park moves the
/// entry past the apply's leg once the apply has begun publishing. The apply
/// is not stopped: it finishes and answers applied, and the coverage its leg
/// holds goes back only once it has.
#[cfg(unix)]
#[test]
fn a_publishing_apply_finishes_its_publication_and_changeset_before_its_leg_ends() {
    let scratch = temp_base("apply-finishing-over-a-teardown");
    let ops = Arc::new(FakeOps::default());
    let (host, name, root, lease) = a_ready_vault_on_disk(&ops, &scratch);
    ops.block_apply_publishing.store(true, Ordering::SeqCst);
    let pending = host.admit_apply(&name, a_plan(&name)).expect("admitted");
    wait_for_flag("apply_publishing_started", &ops.apply_publishing_started);

    park_entry(&host, &name, &root, Park::DuplicateRoot);
    assert_eq!(
        ops.detaches.load(Ordering::SeqCst),
        0,
        "the park gave the coverage back under a publishing apply"
    );
    ops.apply_publishing_release.store(true, Ordering::SeqCst);
    match applied(answer_of(pending)) {
        ApplyReport::Applied { changeset, .. } => {
            assert_eq!(changeset, ChangesetOutcome::Committed);
        }
        other => panic!("the apply answered {other:?}"),
    }
    assert_eq!(ops.applies_stood_down.load(Ordering::SeqCst), 0);
    wait_until(
        "the leg to give the coverage back",
        lifecycle_wait_budget(),
        || match ops.detaches.load(Ordering::SeqCst) {
            1 => Observed::Met(()),
            detaches => Observed::pending(format!("{detaches} detaches")),
        },
    )
    .unwrap_or_else(|failure| panic!("{failure}"));
    drop((lease, host));
}

/// **Admission over a `Ready` entry whose mint failed does what a read does
/// there: it asks for the mint again.** The entry serves every surface but
/// its read seam, and nothing it publishes afterwards would mint, so an
/// apply queued to wait for a reader would wait for ever. A mint that now
/// succeeds lets the apply take the claim and run.
#[test]
fn an_apply_over_a_ready_entry_whose_mint_failed_mints_the_reader_as_a_read_would() {
    let ops = Arc::new(FakeOps::default());
    ops.reader_mint_fails.store(true, Ordering::SeqCst);
    let (host, name, lease) = a_ready_vault(&ops);
    ops.reader_mint_fails.store(false, Ordering::SeqCst);

    let pending = host.admit_apply(&name, a_plan(&name)).expect("admitted");
    for _ in 0..3 {
        dispatcher_tick(&host.shared, Instant::now());
    }
    applied(answer_of(pending));
    drop((lease, host));
}

/// **And where that mint fails again, admission refuses with the code a
/// read would carry**: the reader is unavailable, for the reason the mint
/// gave, and nothing is queued.
#[test]
fn an_apply_over_a_ready_entry_whose_mint_fails_again_is_refused_as_a_read_would_be() {
    let ops = Arc::new(FakeOps::default());
    ops.reader_mint_fails.store(true, Ordering::SeqCst);
    let (host, name, lease) = a_ready_vault(&ops);

    let refused = host
        .admit_apply(&name, a_plan(&name))
        .expect_err("an apply over an entry no reader can serve was admitted");
    assert_eq!(
        refused,
        ReadRefusal::ReaderUnavailable(ReaderUnavailable::new(
            "this coverage mints no read handle"
        ))
        .answer(&name)
    );
    let state = host.shared.entries.get(&name).unwrap();
    assert!(state.gate.lock().unwrap().applies.is_empty());
    assert!(ops.applies_ran.lock().unwrap().is_empty());
    drop((lease, host));
}

/// **Damage a read meets during an apply's intake stops the apply before it
/// plans.** The read holds the entry's handle while the apply's intake
/// derives a fact; the read meets damage and carries it to the claim the
/// apply holds. The apply is answered not applied with the damage its leg
/// publishes, the rebuild runs, and the apply never ran over the damaged
/// store.
#[test]
fn damage_a_read_meets_during_an_apply_intake_answers_the_apply_not_applied() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    let entry = host.shared.entries.get(&name).unwrap();
    let hold = host
        .begin_read(&name)
        .expect("a ready entry answers a read");
    ops.block_reconcile.store(true, Ordering::SeqCst);
    ops.facts_on_next_polls.store(1, Ordering::SeqCst);
    let pending = host.admit_apply(&name, a_plan(&name)).expect("admitted");
    wait_for_flag("reconcile_started", &ops.reconcile_started);
    assert!(
        entry.gate.lock().unwrap().running_apply.is_some(),
        "the reconcile running is not the apply's intake"
    );

    let _ = host.withdraw_for_read_damage(&hold, "the store is damaged".to_string());
    drop(hold);
    ops.block_rebuild.store(true, Ordering::SeqCst);
    ops.reconcile_release.store(true, Ordering::SeqCst);

    let damaged = TrustState::untrusted(UntrustedReason::store_damaged_rebuilding(
        "the store is damaged",
    ));
    assert_eq!(
        not_applied_cause(answer_of(pending)),
        ReadRefusal::NotServing(Demand::State(damaged)).answer(&name)
    );
    wait_for_flag("rebuild_started", &ops.rebuild_started);
    assert!(
        ops.applies_ran.lock().unwrap().is_empty(),
        "the apply ran over the damage its intake met"
    );
    ops.rebuild_release.store(true, Ordering::SeqCst);
    wait_for_state(&host, &name, TrustState::Ready);
    drop((lease, host));
}

/// **An apply whose one snapshot meets damage is answered as a read meeting
/// it is**: the damage is published with the rebuild it owes, and the apply
/// is answered not applied with it, before anything was planned.
#[test]
fn an_apply_whose_snapshot_meets_damage_publishes_it_and_answers_not_applied() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    ops.damage_in_apply.store(true, Ordering::SeqCst);
    ops.block_rebuild.store(true, Ordering::SeqCst);

    let pending = host.admit_apply(&name, a_plan(&name)).expect("admitted");
    let damaged = TrustState::untrusted(UntrustedReason::store_damaged_rebuilding(
        "the store is damaged",
    ));
    assert_eq!(
        not_applied_cause(answer_of(pending)),
        ReadRefusal::NotServing(Demand::State(damaged)).answer(&name)
    );
    wait_for_flag("rebuild_started", &ops.rebuild_started);
    ops.rebuild_release.store(true, Ordering::SeqCst);
    wait_for_state(&host, &name, TrustState::Ready);
    drop((lease, host));
}

/// **Case 10, destruction over an idle detach: the host's destruction
/// answers an apply queued behind an idle detach at once**, while the detach
/// still holds the coverage, rather than leaving it to the release that
/// destruction waits for.
#[test]
fn the_hosts_destruction_over_an_idle_detach_answers_the_queue_at_once() {
    let ops = Arc::new(FakeOps::default());
    let (host, name, lease) = a_ready_vault(&ops);
    drop(lease);
    hold_an_idle_detach(&ops, &host, &name);
    let pending = host.admit_apply(&name, a_plan(&name)).expect("admitted");

    let destroyed = thread::spawn(move || drop(host));
    let cause = not_applied_cause(answer_of(pending));
    assert_eq!(
        cause,
        ReadRefusal::NotServing(Demand::State(TrustState::warming(
            WarmingPhase::ReleasingCoverage,
            0,
            None
        )))
        .answer(&name)
    );
    assert!(
        !destroyed.is_finished(),
        "the destruction did not wait for the detach"
    );
    ops.block_detach.store(false, Ordering::SeqCst);
    ops.detach_release.store(true, Ordering::SeqCst);
    destroyed.join().expect("the host was destroyed");
    assert!(ops.applies_ran.lock().unwrap().is_empty());
}

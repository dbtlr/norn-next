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
            maintenances: 0
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

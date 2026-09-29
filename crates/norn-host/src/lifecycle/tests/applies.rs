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
use norn_wire::{ApplyReport, ChangesetOutcome, ResolvedPlan, RootIdentity, VaultAddress};

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

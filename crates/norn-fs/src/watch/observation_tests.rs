//! Eligibility observes accepted facts without consuming the heal or delivery.
//!
//! These bookkeeping and lifetime tests inject facts through `ingest` over real
//! polling subscriptions. Their synchronous baseline provides unchanged coverage;
//! native FSEvents may deliver setup/history facts even after becoming live.
//! Native delivery and synchronization contracts remain in `tests/watcher.rs`.
#![allow(clippy::disallowed_methods)]
use super::*;
use norn_testkit::{isolation, scratch::Scratch, wait::Budget};

/// Take the lease before creating the tree. Callers bind in this order so the
/// subscription and scratch tree drop before the lease is released.
fn fixture() -> (isolation::Lease, Scratch, Subscription) {
    let lease = isolation::Lease::hold(
        isolation::REAL_WATCHER,
        isolation::acquisition_budget(Budget::new(
            Duration::from_secs(15),
            Duration::from_millis(250),
        )),
    );
    let scratch = Scratch::under(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/guard-scratch"),
        "watch-observation",
    )
    .unwrap();
    let root = scratch.join("vault");
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let schema = root.join("schema.yaml");
    std::fs::write(&schema, "version: 1\n").unwrap();
    let (watch, _) = watch_polling(&root, &schema).unwrap();
    watch.synchronize(Duration::from_secs(5)).unwrap();
    (lease, scratch, watch)
}

#[test]
fn observation_accepts_unchanged_coverage_and_never_recovers_after_a_held_fact() {
    let (_lease, _scratch, watch) = fixture();
    let root = watch.covered_root().to_path_buf();
    watch.begin_heal();
    let guard = watch
        .unchanged_observation()
        .expect("unchanged live watcher");
    assert!(watch.accepts_observation(&guard));
    assert!(guard.is_current());
    ingest(
        &watch.state,
        Ok(
            Event::new(EventKind::Access(notify::event::AccessKind::Read))
                .add_path(root.join("competing.md")),
        ),
    );
    ingest(
        &watch.state,
        Ok(Event::new(EventKind::Create(CreateKind::File)).add_path(root.join(".norn/tmp/shadow"))),
    );
    assert!(
        guard.is_current(),
        "accesses and excluded non-facts retain eligibility"
    );
    let (foreign, _) = super::watch_polling(&root, &root.join("schema.yaml")).unwrap();
    foreign.synchronize(Duration::from_secs(5)).unwrap();
    let foreign_guard = foreign.unchanged_observation().unwrap();
    assert!(!watch.accepts_observation(&foreign_guard));
    drop(foreign);
    assert!(
        !foreign_guard.is_current(),
        "a token owns no subscription lifetime"
    );

    // The callback's actual intake seam, before suppression or delivery.
    ingest(
        &watch.state,
        Ok(Event::new(EventKind::Create(CreateKind::File)).add_path(root.join("competing.md"))),
    );
    assert!(!watch.holds_a_settled_batch());
    assert!(
        !guard.is_current(),
        "an open heal holds this fact, not the slot"
    );
    assert!(watch.unchanged_observation().is_none());
    assert!(!watch.finish_heal().unwrap().is_empty());
    assert!(watch.try_recv().unwrap().is_none());
    assert!(
        !watch.accepts_observation(&guard),
        "draining cannot restore eligibility"
    );
    drop(watch);
    assert!(!guard.is_current());
}

#[test]
fn observation_stays_invalid_after_terminal_consumption_and_revision_exhaustion() {
    let (_lease, _scratch, watch) = fixture();
    watch.begin_heal();
    let guard = watch.unchanged_observation().unwrap();
    ingest(
        &watch.state,
        Err(notify::Error::generic("observed backend loss")),
    );
    // Deliberately consume State's terminal before control publication, as an
    // attach can do while the callback is between ingest and publish_control.
    assert!(watch.finish_heal().is_err());
    assert!(!guard.is_current());
    assert!(watch.unchanged_observation().is_none());

    let (second, _) = super::watch_polling(
        watch.covered_root(),
        &watch.covered_root().join("schema.yaml"),
    )
    .unwrap();
    second.synchronize(Duration::from_secs(5)).unwrap();
    second.begin_heal();
    let guard = second.unchanged_observation().unwrap();
    second.state.lock().unwrap().observation_revision = Some(u64::MAX);
    ingest(
        &second.state,
        Ok(Event::new(EventKind::Any).add_path(second.covered_root().join("changed.md"))),
    );
    assert!(
        !guard.is_current(),
        "an unrepresentable revision cannot wrap back to eligibility"
    );
    second.finish_heal().unwrap();
    assert!(second.unchanged_observation().is_none());
    drop(second);
    drop(watch);
}

#[test]
fn observation_rejects_overflow_control_loss_and_a_replaced_root() {
    let (_lease, _scratch, watch) = fixture();
    watch.begin_heal();
    let guard = watch.unchanged_observation().unwrap();
    ingest(
        &watch.state,
        Ok(Event::new(EventKind::Other).set_flag(notify::event::Flag::Rescan)),
    );
    assert!(!guard.is_current());
    watch.finish_heal().unwrap();
    assert!(!guard.is_current());

    let (second, _) = super::watch_polling(
        watch.covered_root(),
        &watch.covered_root().join("schema.yaml"),
    )
    .unwrap();
    second.synchronize(Duration::from_secs(5)).unwrap();
    let guard = second.unchanged_observation().unwrap();
    publish_control(
        &second.control,
        SubscriptionState::Terminal(WatchError::SynchronizationExpired),
    );
    assert!(!guard.is_current());

    let (mut third, _) = super::watch_polling(
        watch.covered_root(),
        &watch.covered_root().join("schema.yaml"),
    )
    .unwrap();
    third.synchronize(Duration::from_secs(5)).unwrap();
    // Isolate the explicit anchor observation from backend delivery, just as
    // existing root-anchor tests do: the guard itself must record the loss.
    third.begin_heal();
    third.watcher.take();
    let guard = third.unchanged_observation().unwrap();
    let moved = watch.covered_root().with_file_name("moved");
    std::fs::rename(watch.covered_root(), &moved).unwrap();
    assert!(
        !guard.is_current(),
        "root checking does not wait for backend delivery"
    );
    std::fs::rename(&moved, watch.covered_root()).unwrap();
    assert!(
        !guard.is_current(),
        "returning the root cannot revive a refused token"
    );
    assert!(
        third.unchanged_observation().is_none(),
        "known root loss cannot mint a new authority"
    );
    assert!(matches!(
        third.state(),
        SubscriptionState::Terminal(WatchError::CoverageLost(_))
    ));
    assert!(
        matches!(third.finish_heal(), Err(WatchError::CoverageLost(_))),
        "ordinary continuation retains the loss"
    );
    drop(third);
    drop(second);
    drop(watch);
}

#[test]
fn an_established_first_index_proof_survives_admin_close_but_records_late_contradictions() {
    let (_lease, _scratch, watch) = fixture();
    let proof = watch.first_index_observation().expect("unchanged coverage");
    assert!(proof.check_root());
    assert!(proof.admits());
    let state = Arc::clone(&watch.state);
    let root = watch.covered_root().to_path_buf();
    drop(watch);
    assert!(!proof.admits(), "administrative end closes acquisitions");
    assert!(
        proof.held_is_current(),
        "healthy established read may finish"
    );
    ingest(
        &state,
        Ok(Event::new(EventKind::Create(CreateKind::File)).add_path(root.join("late.md"))),
    );
    assert!(
        !proof.held_is_current(),
        "accepted late contradiction is retained"
    );
    drop(state);
    assert!(!proof.held_is_current());
}

#[test]
fn first_index_checks_separate_root_io_from_admission_and_never_recover() {
    let (_lease, _scratch, mut watch) = fixture();
    watch.begin_heal();
    watch.watcher.take();
    let proof = watch.first_index_observation().unwrap();
    let root = watch.covered_root().to_path_buf();
    let moved = root.with_file_name("moved-for-proof");
    // Admission reads bounded proof/control bookkeeping only, even if the big
    // watcher account is locked and the name on disk no longer resolves. The
    // account is locked before the rename: dropping the watcher does not join
    // its polling thread, and a final scan that classified the rename would
    // withdraw the proof, which is the conservative outcome but not this
    // case's subject.
    let account = watch.state.lock().unwrap();
    std::fs::rename(&root, &moved).unwrap();
    assert!(
        proof.admits(),
        "no filesystem work or State lock under entry gate"
    );
    drop(account);
    assert!(!proof.check_root());
    std::fs::rename(&moved, &root).unwrap();
    assert!(!proof.check_root());
    assert!(!proof.held_is_current());
    assert!(watch.first_index_observation().is_none());
    assert!(matches!(
        watch.finish_heal(),
        Err(WatchError::CoverageLost(_))
    ));
}

#[test]
fn first_index_proofs_observe_accepted_facts_before_delivery_and_notify_late_registration() {
    let (_lease, _scratch, watch) = fixture();
    watch.begin_heal();
    let proof = watch.first_index_observation().unwrap();
    ingest(
        &watch.state,
        Ok(Event::new(EventKind::Create(CreateKind::File))
            .add_path(watch.covered_root().join("known-before-delivery.md"))),
    );
    assert!(!watch.holds_a_settled_batch());
    assert!(!proof.admits());
    assert!(!proof.held_is_current());
    assert!(!watch.finish_heal().unwrap().is_empty());
    assert!(watch.first_index_observation().is_none());
}

#[test]
fn first_index_held_proof_owns_no_watcher_account_and_backend_failure_poison_survives_close() {
    let (lease, scratch, watch) = fixture();
    let proof = watch.first_index_observation().unwrap();
    let weak = Arc::downgrade(&watch.state);
    drop(watch);
    assert!(
        weak.upgrade().is_none(),
        "proof owns no watcher/batch resources"
    );
    assert!(proof.held_is_current());
    proof.invalidate();
    assert!(!proof.held_is_current());

    drop(scratch);
    drop(lease);
    let (_lease2, _other, watch) = fixture();
    let proof = watch.first_index_observation().unwrap();
    ingest(
        &watch.state,
        Err(notify::Error::generic("backend contradiction")),
    );
    drop(watch);
    assert!(!proof.held_is_current());
}

#[test]
fn first_index_withdrawal_wakes_an_existing_condvar_and_racing_registration() {
    struct Signal {
        notified: Mutex<bool>,
        changed: Condvar,
    }
    impl std::task::Wake for Signal {
        fn wake(self: Arc<Self>) {
            self.wake_by_ref();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            *self.notified.lock().unwrap() = true;
            self.changed.notify_all();
        }
    }
    let (_lease, _scratch, watch) = fixture();
    let proof = watch.first_index_observation().unwrap();
    let signal = Arc::new(Signal {
        notified: Mutex::new(false),
        changed: Condvar::new(),
    });
    let waker = std::task::Waker::from(Arc::clone(&signal));
    proof.register_waker(&waker);
    proof.invalidate();
    let (notified, _) = signal
        .changed
        .wait_timeout_while(
            signal.notified.lock().unwrap(),
            Duration::from_secs(1),
            |notified| !*notified,
        )
        .unwrap();
    assert!(*notified, "withdrawal wakes the stance condition variable");
    drop(notified);
    *signal.notified.lock().unwrap() = false;
    proof.register_waker(&waker);
    assert!(
        *signal.notified.lock().unwrap(),
        "registration after withdrawal cannot lose the wake"
    );
    // Race registration with withdrawal on another unchanged coverage, using
    // the same existing stance signal and no bridge worker.
    let (second, _) = super::watch_polling(
        watch.covered_root(),
        &watch.covered_root().join("schema.yaml"),
    )
    .unwrap();
    second.synchronize(Duration::from_secs(5)).unwrap();
    let racing = second.first_index_observation().unwrap();
    *signal.notified.lock().unwrap() = false;
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            barrier.wait();
            racing.register_waker(&waker);
        });
        barrier.wait();
        racing.invalidate();
    });
    assert!(
        *signal.notified.lock().unwrap(),
        "racing registration must wake the existing waiter"
    );
}

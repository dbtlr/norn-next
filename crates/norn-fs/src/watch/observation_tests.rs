//! Eligibility observes accepted facts without consuming the heal or delivery.
#![allow(clippy::disallowed_methods)]
use super::*;
use norn_testkit::{isolation, scratch::Scratch, wait::Budget};

fn fixture() -> (Scratch, Subscription, isolation::Lease) {
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
    let lease = isolation::Lease::hold(
        isolation::REAL_WATCHER,
        isolation::acquisition_budget(Budget::new(
            Duration::from_secs(15),
            Duration::from_millis(250),
        )),
    );
    let (watch, _) = watch(&root, &schema).unwrap();
    watch.synchronize(Duration::from_secs(5)).unwrap();
    (scratch, watch, lease)
}

#[test]
fn observation_accepts_unchanged_coverage_and_never_recovers_after_a_held_fact() {
    let (_scratch, watch, _lease) = fixture();
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
    let (foreign, _) = super::watch(&root, &root.join("schema.yaml")).unwrap();
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
    let (_scratch, watch, _lease) = fixture();
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

    let (second, _) = super::watch(
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
    let (_scratch, watch, _lease) = fixture();
    watch.begin_heal();
    let guard = watch.unchanged_observation().unwrap();
    ingest(
        &watch.state,
        Ok(Event::new(EventKind::Other).set_flag(notify::event::Flag::Rescan)),
    );
    assert!(!guard.is_current());
    watch.finish_heal().unwrap();
    assert!(!guard.is_current());

    let (second, _) = super::watch(
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

    let (mut third, _) = super::watch(
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

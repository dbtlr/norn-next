use super::*;
use crate::lifecycle::MaintenanceTurn;
use crate::semantic::SemanticEngines;
use norn_store::ReadThrough;
use norn_wire::Freshness;

use super::super::death_prune::{DEATH_PRUNE_PERIOD, admitted};

/// The production ops over the fixture's directories, composing the semantic
/// engines, with the death prune's clock running at `period` where one is
/// given.
fn engines_and_ops(
    f: &Fixture,
    period: Option<Duration>,
) -> (Arc<SemanticEngines>, ProductionEntryOps) {
    let dirs = ConfigDirs::new(f.root.join("config"), f.root.join("data")).unwrap();
    let engines = SemanticEngines::new(dirs.clone());
    let mut policy = ProductionPolicy::new(64, 2).unwrap();
    if let Some(period) = period {
        policy.death_prune_period = period;
    }
    let ops = ProductionEntryOps::new(dirs, policy).with_semantic(Arc::clone(&engines));
    (engines, ops)
}

/// Attach the fixture's vault through `ops`, with its watcher taken out so
/// the case's own reconciles are the only ones that run.
fn attach(f: &Fixture, ops: &ProductionEntryOps) -> ProductionAttachment {
    let mut attachment = ops
        .attach(&f.registration(), &ProgressReporter::disconnected())
        .expect("an attach");
    attachment.subscription.take();
    attachment
}

/// Write `documents` into the fixture's vault, each `path` holding `body`.
fn write(f: &Fixture, documents: &[(&str, &str)]) {
    for (path, body) in documents {
        let at = f.vault().join(path);
        fs::create_dir_all(at.parent().unwrap()).unwrap();
        fs::write(at, body).unwrap();
    }
}

/// Remove `paths` from the fixture's vault.
fn remove(f: &Fixture, paths: &[&str]) {
    for path in paths {
        fs::remove_file(f.vault().join(path)).unwrap();
    }
}

/// Reconcile `paths` as the watcher would have reported them: a leg that may
/// commit lane-1 work.
fn reconcile(
    f: &Fixture,
    ops: &ProductionEntryOps,
    name: &VaultName,
    attachment: &mut ProductionAttachment,
    paths: &[&str],
) {
    let normalizer = norn_fs::PathNormalizer::detect(&f.vault()).unwrap();
    let mut batch = norn_fs::Batch::default();
    for path in paths {
        batch.merge(norn_fs::Batch::vault_change(
            normalizer.normalize(Path::new(path)).unwrap(),
        ));
    }
    ops.reconcile(
        name,
        attachment,
        ReconcileWork { batch },
        &ProgressReporter::disconnected(),
    )
    .expect("the scoped increment");
}

/// Run one quiet maintenance leg over `attachment` under a budget no unit
/// here reaches.
fn leg(ops: &ProductionEntryOps, name: &VaultName, attachment: &mut ProductionAttachment) {
    ops.maintain_quiet(
        name,
        attachment,
        true,
        &mut MaintenanceTurn::new(Duration::from_secs(60)),
        &ProgressReporter::disconnected(),
    )
    .expect("a maintenance leg");
}

/// Bring the death prune's clock due now.
fn come_due(attachment: &mut ProductionAttachment) {
    attachment.death_prune.due = Instant::now() - Duration::from_millis(1);
}

/// The deadline the death prune's clock contributes.
fn prune_deadline(
    ops: &ProductionEntryOps,
    name: &VaultName,
    attachment: &ProductionAttachment,
) -> Option<Instant> {
    attachment
        .death_prune
        .deadline(&attachment.store, || ops.read_through(name, attachment))
}

/// The freshness an engine status reports, where it reports one.
fn freshness_of(engine: &norn_wire::EngineStatus) -> Option<Freshness> {
    match engine {
        norn_wire::EngineStatus::On { freshness, .. } => *freshness,
        _ => None,
    }
}

/// Every death the store holds, by path.
fn deaths(attachment: &mut ProductionAttachment) -> Vec<String> {
    attachment
        .store
        .begin_request()
        .stored_tombstones_after(None, norn_store::MAX_PAGE)
        .expect("the deaths")
        .into_iter()
        .map(|death| death.path.as_str().to_string())
        .collect()
}

/// The store's death floor.
fn floor(attachment: &mut ProductionAttachment) -> Option<i64> {
    attachment
        .store
        .begin_request()
        .death_floor()
        .expect("the floor")
}

/// The deaths move-following returns for the wikilink `[[target]]` held at
/// `holder`, read through the routine repair reads through.
fn followed(attachment: &ProductionAttachment, holder: &str, target: &str) -> Vec<String> {
    let snapshot = Arc::new(attachment.store.open_reader().reader.expect("a reader"))
        .try_take()
        .expect("an idle reader")
        .establish()
        .expect("a snapshot");
    snapshot
        .followed_tombstones(
            norn_wire::LinkFamily::Wikilink,
            None,
            target,
            &DocumentPath::new(holder).unwrap(),
        )
        .expect("the move-following reads")
        .into_iter()
        .map(|death| death.path.as_str().to_string())
        .collect()
}

/// Run the background units `name`'s engine owes until it owes none, as the
/// lane would, after publishing its delivery beside the store.
fn settle(engines: &SemanticEngines, name: &VaultName, attachment: &mut ProductionAttachment) {
    let reading = attachment
        .store
        .begin_request()
        .store_reading()
        .expect("the store reading");
    engines.publish_delivery(name, Some(&reading), Some(attachment.store.feed_source()));
    for _ in 0..256 {
        if engines.unit(name) != (crate::BackgroundUnit::Ran { owed: true }) {
            return;
        }
    }
    panic!("the owed units never settle");
}

/// **The clock comes due one period after attach, and a pass is owed again
/// only on new work.** The first period owes a pass; a clock that comes due
/// with nothing new moves a period on and runs no unit, and contributes no
/// deadline until a leg may have committed again; an edit that records no
/// death and moves no broken finding owes nothing; a death owes a pass, and
/// so do a broken finding filed and one discarded.
#[test]
fn the_clock_comes_due_a_period_after_attach_and_a_pass_only_on_new_work() {
    let f = Fixture::new("death-prune-clock");
    write(
        &f,
        &[
            ("docs/a.md", "alpha\n"),
            ("docs/b.md", "beta\n"),
            ("docs/h.md", "plain\n"),
        ],
    );
    let (ops, name) = f.ops(64);
    let before = Instant::now();
    let mut attachment = attach(&f, &ops);
    let after = Instant::now();

    let first = prune_deadline(&ops, &name, &attachment).expect("the first pass is owed");
    assert!(
        first >= before + DEATH_PRUNE_PERIOD && first <= after + DEATH_PRUNE_PERIOD,
        "the clock starts one period after attach"
    );
    assert!(
        ops.maintenance_deadline(&attachment)
            .is_some_and(|deadline| deadline <= first)
    );
    leg(&ops, &name, &mut attachment);
    assert_eq!(ops.death_prune_reading(), crate::DeathPruneReading::default());

    come_due(&mut attachment);
    assert!(crate::lifecycle::maintenance_due(&ops, &name, &attachment));
    leg(&ops, &name, &mut attachment);
    let reading = ops.death_prune_reading();
    assert_eq!(
        (reading.due_checks, reading.passes_begun, reading.passes_finished),
        (1, 1, 1),
        "one leg runs the first pass whole"
    );
    assert_eq!(prune_deadline(&ops, &name, &attachment), None, "no new work");
    assert!(!crate::lifecycle::maintenance_due(&ops, &name, &attachment));

    come_due(&mut attachment);
    let checked = Instant::now();
    leg(&ops, &name, &mut attachment);
    let reading = ops.death_prune_reading();
    assert_eq!((reading.due_checks, reading.passes_begun), (2, 1));
    assert!(
        attachment.death_prune.due >= checked + DEATH_PRUNE_PERIOD,
        "a due clock that finds nothing moves a period on"
    );

    write(&f, &[("docs/a.md", "alpha, edited\n")]);
    reconcile(&f, &ops, &name, &mut attachment, &["docs/a.md"]);
    assert!(
        prune_deadline(&ops, &name, &attachment).is_some(),
        "a leg that may have committed brings the clock back"
    );
    come_due(&mut attachment);
    leg(&ops, &name, &mut attachment);
    let reading = ops.death_prune_reading();
    assert_eq!(
        (reading.due_checks, reading.passes_begun),
        (3, 1),
        "an edit that records no death and moves no broken finding owes nothing"
    );
    assert_eq!(prune_deadline(&ops, &name, &attachment), None);

    remove(&f, &["docs/b.md"]);
    reconcile(&f, &ops, &name, &mut attachment, &["docs/b.md"]);
    assert_eq!(deaths(&mut attachment), ["docs/b.md"]);
    come_due(&mut attachment);
    leg(&ops, &name, &mut attachment);
    assert_eq!(ops.death_prune_reading().passes_finished, 2, "a death owes a pass");
    assert!(deaths(&mut attachment).is_empty());

    write(&f, &[("docs/h.md", "[[nowhere]]\n")]);
    reconcile(&f, &ops, &name, &mut attachment, &["docs/h.md"]);
    come_due(&mut attachment);
    leg(&ops, &name, &mut attachment);
    assert_eq!(
        ops.death_prune_reading().passes_finished,
        3,
        "a broken finding filed owes a pass"
    );

    write(&f, &[("docs/h.md", "plain again\n")]);
    reconcile(&f, &ops, &name, &mut attachment, &["docs/h.md"]);
    come_due(&mut attachment);
    leg(&ops, &name, &mut attachment);
    assert_eq!(
        ops.death_prune_reading().passes_finished,
        4,
        "a broken finding discarded owes a pass"
    );
    ops.detach(&name, attachment);
}

/// **A vault no reader of the death feed stands for keeps no death for lane
/// 2**: deletions are pruned at the next due pass, the floor rises to them
/// and the table shrinks, whether the host composes no engine or the vault's
/// configuration states none. No engine is woken.
#[test]
fn deletions_with_no_engine_are_pruned_at_the_next_due_pass() {
    for composed in [false, true] {
        let f = Fixture::new(&format!("death-prune-no-engine-{composed}"));
        write(
            &f,
            &[
                ("docs/a.md", "alpha\n"),
                ("docs/b.md", "beta\n"),
                ("docs/c.md", "gamma\n"),
            ],
        );
        let (ops, name) = if composed {
            (engines_and_ops(&f, None).1, f.registration().name)
        } else {
            f.ops(64)
        };
        let mut attachment = attach(&f, &ops);
        assert_eq!(ops.read_through(&name, &attachment), None, "composed: {composed}");
        remove(&f, &["docs/a.md", "docs/b.md"]);
        reconcile(&f, &ops, &name, &mut attachment, &["docs/a.md", "docs/b.md"]);
        assert_eq!(deaths(&mut attachment), ["docs/a.md", "docs/b.md"]);
        assert_eq!(floor(&mut attachment), None);
        let died_at = attachment
            .store
            .begin_request()
            .death_prune_marks()
            .unwrap()
            .newest_death
            .expect("the deaths' generation");

        come_due(&mut attachment);
        leg(&ops, &name, &mut attachment);
        assert!(deaths(&mut attachment).is_empty(), "composed: {composed}");
        assert_eq!(floor(&mut attachment), Some(died_at));
        let reading = ops.death_prune_reading();
        assert_eq!(reading.work.deaths_removed, 2);
        assert_eq!(reading.floor_raises, 1);
        assert_eq!(reading.engine_wakes, 0);
        ops.detach(&name, attachment);
    }
}

/// **A configured engine that is behind keeps its unread deaths up to the
/// live-document bound, and lane 2's progress owes the next pass.** Four of
/// six documents die past an engine that has not read them: the pass keeps
/// the newest two, one per live document, removes the oldest, raises the
/// floor past what the engine read and wakes it. A stalled engine makes no
/// pass due; once it has read on, the next due pass removes what it kept,
/// raising the floor to what the engine has read, and wakes nothing.
#[test]
fn an_engine_behind_keeps_its_unread_deaths_up_to_the_bound() {
    let f = Fixture::new("death-prune-behind");
    write(&f, &[(".norn/config.toml", "[engine.semantic]\n")]);
    let documents: Vec<String> = (0..6).map(|index| format!("docs/n{index}.md")).collect();
    for document in &documents {
        write(&f, &[(document, &format!("note {document}\n"))]);
    }
    let (engines, ops) = engines_and_ops(&f, None);
    let name = f.registration().name;
    let mut attachment = attach(&f, &ops);
    settle(&engines, &name, &mut attachment);
    let read = ops
        .read_through(&name, &attachment)
        .expect("a configured engine is a reader");
    assert!(matches!(read, ReadThrough::Through(_)), "{read:?}");
    // The first period's pass, over no deaths.
    come_due(&mut attachment);
    leg(&ops, &name, &mut attachment);

    let dying: Vec<&str> = documents[..4].iter().map(String::as_str).collect();
    remove(&f, &dying);
    reconcile(&f, &ops, &name, &mut attachment, &dying);
    assert_eq!(deaths(&mut attachment).len(), 4);
    assert!(engines.owes_work(&name), "the deletions owe the engine its drain");
    let behind = ops.read_through(&name, &attachment);
    assert_eq!(behind, Some(read), "the engine has not run since");

    come_due(&mut attachment);
    leg(&ops, &name, &mut attachment);
    let standing = attachment
        .store
        .begin_request()
        .stored_tombstones_after(None, norn_store::MAX_PAGE)
        .unwrap();
    assert_eq!(standing.len(), 2, "one unread death per live document stands");
    let raised = floor(&mut attachment).expect("the bound evicted the oldest deaths");
    assert!(standing.iter().all(|death| death.generation >= raised));
    let ReadThrough::Through(read_generation) = read else {
        unreachable!()
    };
    assert!(raised > read_generation, "the floor passed what the engine had read");
    let reading = ops.death_prune_reading();
    assert_eq!((reading.floor_raises, reading.engine_wakes), (1, 1));
    assert_eq!(reading.work.deaths_kept_for_lane_two, 2);

    let store_reading = attachment.store.begin_request().store_reading().unwrap();
    assert_eq!(store_reading.death_floor(), Some(raised));
    assert_eq!(
        freshness_of(&crate::semantic::engine_status(
            engines.status(&name),
            Some(&store_reading)
        )),
        Some(Freshness::rescanning()),
        "an engine below the floor reads as rescanning"
    );

    assert_eq!(
        prune_deadline(&ops, &name, &attachment),
        None,
        "a stalled engine makes no pass due"
    );
    come_due(&mut attachment);
    leg(&ops, &name, &mut attachment);
    assert_eq!(ops.death_prune_reading().passes_begun, reading.passes_begun);

    settle(&engines, &name, &mut attachment);
    let caught_up = attachment.store.begin_request().store_reading().unwrap();
    assert_eq!(
        freshness_of(&crate::semantic::engine_status(
            engines.status(&name),
            Some(&caught_up)
        )),
        Some(Freshness::trailing(0)),
        "an engine that reconciled through the floor reads its lag again"
    );
    assert!(
        prune_deadline(&ops, &name, &attachment).is_some(),
        "lane 2 read past what the last pass kept for it"
    );
    come_due(&mut attachment);
    leg(&ops, &name, &mut attachment);
    assert!(deaths(&mut attachment).is_empty());
    let after = ops.death_prune_reading();
    assert_eq!(after.passes_begun, reading.passes_begun + 1);
    assert_eq!((after.floor_raises, after.engine_wakes), (2, 1), "a raise the engine read through wakes nothing");
    ops.detach(&name, attachment);
}

/// **A currently broken link's deaths stand, and move-following answers the
/// same before and after a pass.** `[[moved]]` lost both its targets, so it
/// keeps both deaths and repair skips it as ambiguous; `[[old]]` keeps its
/// one; a death no broken link reads goes.
#[test]
fn a_broken_links_deaths_stand_and_move_following_answers_the_same() {
    let f = Fixture::new("death-prune-broken");
    write(
        &f,
        &[
            ("a/moved.md", "one\n"),
            ("b/moved.md", "two\n"),
            ("c/old.md", "three\n"),
            ("d/unrelated.md", "four\n"),
            ("h.md", "[[moved]] [[old]]\n"),
        ],
    );
    let (ops, name) = f.ops(64);
    let mut attachment = attach(&f, &ops);
    let dying = ["a/moved.md", "b/moved.md", "c/old.md", "d/unrelated.md"];
    remove(&f, &dying);
    reconcile(&f, &ops, &name, &mut attachment, &dying);
    let before = (
        followed(&attachment, "h.md", "moved"),
        followed(&attachment, "h.md", "old"),
    );
    assert_eq!(before.0, ["a/moved.md", "b/moved.md"]);
    assert_eq!(before.1, ["c/old.md"]);

    come_due(&mut attachment);
    leg(&ops, &name, &mut attachment);
    assert_eq!(
        deaths(&mut attachment),
        ["a/moved.md", "b/moved.md", "c/old.md"]
    );
    assert_eq!(
        (
            followed(&attachment, "h.md", "moved"),
            followed(&attachment, "h.md", "old"),
        ),
        before
    );
    let reading = ops.death_prune_reading();
    assert_eq!(reading.work.broken_links_marked, 2);
    assert_eq!(reading.work.deaths_kept_by_link, 3);
    assert_eq!(reading.work.deaths_removed, 1);
    ops.detach(&name, attachment);
}

/// **No unit runs during a from-zero build or while link health is
/// loading**: over such a store the clock contributes no deadline and a leg
/// runs no unit, however due it is.
#[test]
fn no_unit_runs_during_a_build_or_while_link_health_loads() {
    let f = Fixture::new("death-prune-building");
    write(&f, &[("docs/a.md", "alpha\n")]);
    let (ops, name) = f.ops(64);
    let mut attachment = attach(&f, &ops);
    assert!(admitted(&attachment.store));
    let mut building = Store::open(
        f.root.join("building.sqlite3"),
        attachment.store.path_order(),
        crate::DERIVATION_VERSION,
    )
    .expect("a store from zero");
    building.defer_link_health().expect("deferring link health");
    for step in ["link health deferred", "steps begun"] {
        if step == "steps begun" {
            building.begin_steps().expect("beginning a from-zero build");
        }
        std::mem::swap(&mut attachment.store, &mut building);
        assert!(!admitted(&attachment.store), "{step}");
        come_due(&mut attachment);
        assert_eq!(prune_deadline(&ops, &name, &attachment), None, "{step}");
        leg(&ops, &name, &mut attachment);
        assert_eq!(
            ops.death_prune_reading(),
            crate::DeathPruneReading::default(),
            "{step}"
        );
        std::mem::swap(&mut attachment.store, &mut building);
    }
    leg(&ops, &name, &mut attachment);
    assert_eq!(
        ops.death_prune_reading().passes_finished,
        1,
        "the admitted store's pass runs"
    );
    ops.detach(&name, attachment);
}

/// **A pass's work follows the deaths it examines and the broken links it
/// marks, not the vault.** The same deletions and broken links over 300 and
/// 2,000 documents cost the same rows. No engine stands for this vault, so
/// the pass is handed no read-through generation and skips the live-document
/// count and the bound walk, the reads that would follow the vault.
#[test]
fn a_pass_costs_what_it_examines_at_both_scales() {
    let pass_at = |documents: usize| {
        let f = Fixture::new(&format!("death-prune-cost-{documents}"));
        for index in 0..documents {
            write(&f, &[(&format!("docs/{index:05}.md"), "body\n")]);
        }
        for index in 0..5 {
            write(&f, &[(&format!("holders/{index}.md"), &format!("[[gone-{index}]]\n"))]);
        }
        let (ops, name) = f.ops(64);
        let mut attachment = attach(&f, &ops);
        let dying: Vec<String> = (0..20).map(|index| format!("docs/{index:05}.md")).collect();
        let dying: Vec<&str> = dying.iter().map(String::as_str).collect();
        remove(&f, &dying);
        reconcile(&f, &ops, &name, &mut attachment, &dying);
        come_due(&mut attachment);
        leg(&ops, &name, &mut attachment);
        let reading = ops.death_prune_reading();
        assert_eq!(reading.passes_finished, 1);
        ops.detach(&name, attachment);
        reading.work
    };
    let small = pass_at(300);
    let large = pass_at(2_000);
    for work in [&small, &large] {
        assert_eq!(work.live_documents, None);
        assert_eq!(work.deaths_walked, 0);
    }
    let examined = |work: &norn_store::DeathPruneWork| {
        (
            work.rows_read,
            work.findings_marked,
            work.broken_links_marked,
            work.followed_reads,
            work.deaths_walked,
            work.deaths_examined,
            work.deaths_removed,
            work.kept_rows_cleared,
        )
    };
    assert_eq!(examined(&small), examined(&large));
    assert_eq!(small.broken_links_marked, 5);
    assert_eq!(small.deaths_examined, 20);
    assert_eq!(small.deaths_removed, 20);
}

/// A host over the fixture with the death prune's clock at `period`, polling
/// its watcher often, demanded and `Ready`.
fn host_over(
    f: &Fixture,
    ops: ProductionEntryOps,
) -> (
    crate::Host<ProductionEntryOps>,
    VaultName,
    crate::DemandLease<ProductionEntryOps>,
) {
    let registration = f.registration();
    let name = registration.name.clone();
    let host = crate::Host::new(
        crate::RegistryRead::from_entries([registration]),
        ops,
        crate::LifecyclePolicy {
            idle_after: Duration::from_secs(60),
            worker_slots: 1,
            watch_poll_interval: Duration::from_millis(5),
            read_settle_bound: crate::READ_SETTLE_BOUND,
            background_threads: crate::BACKGROUND_THREADS,
            background_turn_budget: crate::BACKGROUND_TURN_BUDGET,
        },
    )
    .unwrap();
    norn_fs::watch::settle_prior_changes(f.root.root(), lifecycle_budget().work()).unwrap();
    let lease = host.demand(&name).unwrap();
    wait_state(&host, &name, norn_wire::TrustState::Ready);
    (host, name, lease)
}

/// Wait until `name`'s status reports an engine `wanted` accepts.
fn wait_for_engine(
    host: &crate::Host<ProductionEntryOps>,
    name: &VaultName,
    wanted: impl Fn(&norn_wire::EngineStatus) -> bool,
) -> norn_wire::EngineStatus {
    wait_until("the engine a status reports", lifecycle_budget(), || {
        let engine = super::status::status_of(host, name).engine;
        if wanted(&engine) {
            Observed::Met(engine)
        } else {
            Observed::Pending(format!("the engine reports {engine:?}"))
        }
    })
    .unwrap_or_else(|failure| panic!("{failure}"))
}

/// **A floor raise past the engine reaches the status, and wakes the engine,
/// on a running host.** The engine is held while four of six documents die;
/// the pass the deletions owe evicts the oldest past the bound, so the status
/// reads the engine rescanning rather than trailing behind an older reading.
/// Released, the engine reconciles through the floor and catches up.
#[test]
fn a_floor_raise_past_the_engine_reaches_the_status_and_wakes_the_engine() {
    let f = Fixture::new("death-prune-status");
    write(&f, &[(".norn/config.toml", "[engine.semantic]\n")]);
    for index in 0..6 {
        write(&f, &[(&format!("docs/n{index}.md"), &format!("note {index}\n"))]);
    }
    let (engines, ops) = engines_and_ops(&f, Some(Duration::from_millis(20)));
    let (host, name, _lease) = host_over(&f, ops);
    let caught_up = |engine: &norn_wire::EngineStatus| {
        matches!(
            engine,
            norn_wire::EngineStatus::On {
                freshness: Some(Freshness::Trailing { generations: 0, .. }),
                coverage: Some(coverage),
                ..
            } if coverage.embedded == coverage.total
        )
    };
    wait_for_engine(&host, &name, caught_up);
    engines.hold_owed_work();

    remove(&f, &["docs/n0.md", "docs/n1.md", "docs/n2.md", "docs/n3.md"]);
    wait_for_engine(&host, &name, |engine| {
        freshness_of(engine) == Some(Freshness::rescanning())
    });
    let reading = host.ops().death_prune_reading();
    assert!(reading.floor_raises >= 1, "{reading:?}");
    assert!(reading.engine_wakes >= 1, "{reading:?}");

    engines.release_owed_work();
    write(&f, &[("docs/n9.md", "note nine\n")]);
    wait_for_engine(&host, &name, caught_up);
}

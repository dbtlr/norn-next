//! A private initial-build instrument changes only when link health is judged.
#![cfg(feature = "induced-failure")]
#![allow(clippy::disallowed_methods)] // Isolated fixture arrangement.

mod attach;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use norn_host::startup_experiment::{Experiment, Observer, Stage};
use norn_store::{ContentModel, SnapshotReader, startup_experiment::Catalogue};
use norn_wire::{GetParams, ResolutionTarget, TrustState, VaultAddress};

#[derive(Default)]
struct Probe(Mutex<Vec<(bool, BTreeMap<&'static str, u64>)>>);

impl Observer for Probe {
    fn catalogue(&self, _: &Catalogue, _: &ContentModel) -> Result<(), String> {
        panic!("health-once must not construct a catalogue")
    }
    fn indexed(
        &self,
        _: Stage,
        _: Arc<SnapshotReader>,
        _: Arc<ContentModel>,
    ) -> Result<(), String> {
        panic!("health-once must not expose a checkpoint reader")
    }
    fn health_once_work(
        &self,
        finalized: bool,
        work: &BTreeMap<&'static str, u64>,
    ) -> Result<(), String> {
        self.0.lock().unwrap().push((finalized, work.clone()));
        Ok(())
    }
}

fn wait_ready(host: &attach::ServingHost, vault: &attach::Vault) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while host.state(vault.name()).unwrap() != TrustState::Ready
        || !host.work_in_flight(vault.name()).unwrap().is_quiet()
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn semantic_get(mut answer: serde_json::Value) -> serde_json::Value {
    answer.as_object_mut().unwrap().remove("reading");
    // Handles name independent stores; preserve every other rich Get field.
    for finding in answer["report"]["document"]["findings"]["items"]
        .as_array_mut()
        .unwrap()
    {
        finding.as_object_mut().unwrap().remove("id");
        finding.as_object_mut().unwrap().remove("generation");
    }
    answer
}

#[test]
fn health_once_preserves_ordinary_heal_identity_work_and_whole_get() {
    let scratch = norn_testkit::scratch::Scratch::new("health-once-equivalence");
    let root = scratch.join("vault");
    for dir in [".norn", "one", "two"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    std::fs::write(
        root.join(".norn/schema.yaml"),
        "version: 1\nfields:\n  rating: { type: number }\n",
    )
    .unwrap();
    for (path, body) in [
        (
            "first.md",
            "---\nrating: nope\n---\n[[target#Present]] [[target#Absent]] [[target#^block]] [[missing]] [[dupe]]\n#tag\n",
        ),
        ("middle.md", "[[target]] [[target#Present]]\n"),
        ("one/dupe.md", "One\n"),
        ("two/dupe.md", "Two\n"),
        ("target.md", "# Present\nBlock. ^block\n"),
        ("malformed.md", "---\nvalue: [\n---\nBody\n"),
        ("bad\nname.md", "Refused name\n"),
    ] {
        std::fs::write(root.join(path), body).unwrap();
    }
    let vault = attach::Vault::adopt(scratch.root());
    let probe = Arc::new(Probe::default());
    let mut results = vec![];
    for deferred in [false, true] {
        let machine = vault.beside(&scratch.join(if deferred { "once" } else { "ordinary" }));
        let host = machine.host_with_loading_options(
            norn_host::ProductionPolicy::new(2, 2).unwrap(),
            deferred.then(|| Experiment::health_once(probe.clone())),
            None,
        );
        let demand = host.demand(machine.name()).unwrap();
        wait_ready(&host, &machine);
        let params = GetParams::new(
            VaultAddress::name(machine.name().clone()),
            ResolutionTarget::new("first").unwrap(),
        );
        let answer = serde_json::to_value(host.get(&params).unwrap().answer).unwrap();
        let document = &answer["report"]["document"];
        for collection in [
            "fields", "headings", "blocks", "tags", "links", "findings", "body",
        ] {
            assert!(
                document.get(collection).is_some(),
                "whole Get omitted {collection}"
            );
        }
        assert_eq!(document["links"]["total"], 5);
        assert_eq!(document["findings"]["total"], 4);
        let work = host.evidence();
        drop(demand);
        drop(host);
        machine.store().verify_integrity().unwrap();
        let audit =
            norn_store::startup_audit::audit(&machine.database(), |_, _, _| Ok(())).unwrap();
        let db = norn_db::open_read_only(&machine.database())
            .adopted
            .unwrap();
        let mut statement = db
            .connection()
            .prepare("SELECT id,path,generation FROM documents ORDER BY id")
            .unwrap();
        let identities: Vec<(i64, String, i64)> = statement
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        // Finding writes consume global generations too. Preserve document IDs,
        // insertion order and changeset membership, not health-dependent gaps.
        let mut group = 0;
        let mut previous = None;
        let identities: Vec<_> = identities
            .into_iter()
            .map(|(id, path, generation)| {
                if previous != Some(generation) {
                    group += 1;
                    previous = Some(generation);
                }
                (id, path, group)
            })
            .collect();
        results.push((audit, semantic_get(answer), work, identities));
    }
    assert_eq!(
        results[0].0["semantic_sha256"],
        results[1].0["semantic_sha256"]
    );
    assert_eq!(
        results[0].0["document_construction_sha256"],
        results[1].0["document_construction_sha256"]
    );
    assert_eq!(results[0].0["counts"], results[1].0["counts"]);
    assert_eq!(results[0].1, results[1].1);
    assert_eq!(
        results[0].3, results[1].3,
        "document IDs and changeset order moved"
    );
    let a = results[0].2;
    let b = results[1].2;
    assert_eq!(
        (
            a.document_opens,
            a.stats,
            a.walk_dirents,
            a.documents_derived,
            a.changesets_applied,
            a.documents_upserted,
            a.documents_deleted,
            a.tombstones_recorded
        ),
        (
            b.document_opens,
            b.stats,
            b.walk_dirents,
            b.documents_derived,
            b.changesets_applied,
            b.documents_upserted,
            b.documents_deleted,
            b.tombstones_recorded
        )
    );
    let stages = probe.0.lock().unwrap();
    assert_eq!(stages.len(), 2);
    assert!(!stages[0].0);
    assert!(stages[1].0);
    assert_eq!(stages[0].1["links_redecided"], 0);
    assert_eq!(stages[0].1["documents_upserted"], 6);
    assert_eq!(stages[1].1["documents_upserted"], 6);
    assert_eq!(
        stages[1].1["links_redecided"], 7,
        "every link is judged once over full facts"
    );
    assert!(
        a.links_redecided > b.links_redecided,
        "fixture must expose repeated ordinary health work"
    );
}

#[derive(Clone, Copy)]
enum Failure {
    HealthSql,
    Finalization,
    Unwind,
}

struct Fail(Failure);
impl Observer for Fail {
    fn catalogue(&self, _: &Catalogue, _: &ContentModel) -> Result<(), String> {
        unreachable!()
    }
    fn indexed(
        &self,
        _: Stage,
        _: Arc<SnapshotReader>,
        _: Arc<ContentModel>,
    ) -> Result<(), String> {
        unreachable!()
    }
    fn health_once_work(
        &self,
        finalized: bool,
        _: &BTreeMap<&'static str, u64>,
    ) -> Result<(), String> {
        match (&self.0, finalized) {
            (Failure::HealthSql, false) => {
                norn_store::induced_failure::fail_next_meta_read_as_busy()
            }
            (Failure::Finalization, true) => return Err("refuse finalization after health".into()),
            (Failure::Unwind, false) => panic!("unfinished full-facts construction"),
            _ => {}
        }
        Ok(())
    }
}

fn fixture(label: &str) -> (norn_testkit::scratch::Scratch, attach::Vault) {
    let scratch = norn_testkit::scratch::Scratch::new(label);
    let root = scratch.join("vault");
    std::fs::create_dir_all(root.join(".norn")).unwrap();
    std::fs::write(root.join(".norn/schema.yaml"), "version: 1\n").unwrap();
    std::fs::write(root.join("first.md"), "[[later]] [[missing]]\n").unwrap();
    std::fs::write(root.join("later.md"), "# Target\n").unwrap();
    let vault = attach::Vault::adopt(scratch.root());
    (scratch, vault)
}

fn wait_refused(host: &attach::ServingHost, vault: &attach::Vault) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match host.state(vault.name()) {
            Err(_) => break,
            Ok(TrustState::Ready) => panic!("a refused experiment reached Ready"),
            Ok(_) => {}
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn failed_health_finalization_and_unwind_never_publish_or_allow_ordinary_adoption() {
    for (group, failure) in [None, Some(1), Some(16), Some(0)]
        .into_iter()
        .flat_map(|group| {
            [Failure::HealthSql, Failure::Finalization, Failure::Unwind]
                .into_iter()
                .map(move |failure| (group, failure))
        })
    {
        let (_scratch, vault) = fixture("health-once-failed");
        let host = vault.host_with_startup_experiment(
            norn_host::ProductionPolicy::new(1, 1).unwrap(),
            match group {
                None => Experiment::health_once(Arc::new(Fail(failure))),
                Some(group) => {
                    Experiment::health_once_construction(Arc::new(Fail(failure)), group).unwrap()
                }
            },
        );
        let demand = host.demand(vault.name()).unwrap();
        wait_refused(&host, &vault);
        drop(demand);
        drop(host);
        // The schema still matches: refusal must be the persistent incomplete
        // marker, rather than a DDL mismatch or missing document hash.
        let db = norn_db::open_read_only(&vault.database()).adopted.unwrap();
        let marker: i64 = db
            .connection()
            .query_row(
                "SELECT value FROM meta WHERE key='startup_experiment_incomplete'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(marker, 1);
        if group.is_some_and(|group| group != 1) && !matches!(failure, Failure::Finalization) {
            let committed: i64 = db
                .connection()
                .query_row("SELECT count(*) FROM documents", [], |r| r.get(0))
                .unwrap();
            assert_eq!(
                committed, 0,
                "the uncommitted construction group survived failure"
            );
        }
        drop(db);
        let host = vault.host();
        let _demand = host.demand(vault.name()).unwrap();
        wait_refused(&host, &vault);
    }
}

#[test]
fn health_once_refuses_existing_empty_warm_and_completed_experimental_stores() {
    for (existing, group) in ["empty", "ordinary", "health-once"]
        .into_iter()
        .flat_map(|existing| {
            [None, Some(1), Some(16), Some(0)]
                .into_iter()
                .map(move |group| (existing, group))
        })
    {
        let (_scratch, vault) = fixture("health-once-existing");
        let probe = Arc::new(Probe::default());
        if existing == "empty" {
            drop(vault.store());
        } else {
            let host = vault.host_with_loading_options(
                norn_host::ProductionPolicy::new(1, 1).unwrap(),
                (existing == "health-once").then(|| Experiment::health_once(probe.clone())),
                None,
            );
            let demand = host.demand(vault.name()).unwrap();
            wait_ready(&host, &vault);
            drop(demand);
            drop(host);
        }
        let before = norn_store::startup_audit::audit(&vault.database(), |_, _, _| Ok(())).unwrap();
        let attempted = Arc::new(Probe::default());
        let host = vault.host_with_startup_experiment(
            norn_host::ProductionPolicy::new(1, 1).unwrap(),
            match group {
                None => Experiment::health_once(attempted.clone()),
                Some(group) => {
                    Experiment::health_once_construction(attempted.clone(), group).unwrap()
                }
            },
        );
        let demand = host.demand(vault.name()).unwrap();
        wait_refused(&host, &vault);
        assert!(attempted.0.lock().unwrap().is_empty());
        drop(demand);
        drop(host);
        let after = norn_store::startup_audit::audit(&vault.database(), |_, _, _| Ok(())).unwrap();
        assert_eq!(
            before["canonical_identity_sha256"],
            after["canonical_identity_sha256"]
        );
        let host = vault.host();
        let _demand = host.demand(vault.name()).unwrap();
        wait_ready(&host, &vault);
        if existing != "empty" {
            assert_eq!(
                host.evidence().documents_derived,
                0,
                "ordinary restart re-derived a complete store"
            );
        }
    }
}

#[test]
fn full_facts_cannot_be_read_or_ready_before_final_health() {
    struct Pause(Arc<std::sync::Barrier>);
    impl Observer for Pause {
        fn catalogue(&self, _: &Catalogue, _: &ContentModel) -> Result<(), String> {
            unreachable!()
        }
        fn indexed(
            &self,
            _: Stage,
            _: Arc<SnapshotReader>,
            _: Arc<ContentModel>,
        ) -> Result<(), String> {
            unreachable!()
        }
        fn health_once_work(
            &self,
            finalized: bool,
            work: &BTreeMap<&'static str, u64>,
        ) -> Result<(), String> {
            if !finalized {
                assert_eq!(work["documents_upserted"], 2);
                assert_eq!(work["links_redecided"], 0);
                self.0.wait();
                self.0.wait();
            }
            Ok(())
        }
    }
    for group in [None, Some(1), Some(16), Some(0)] {
        let (_scratch, vault) = fixture("health-once-unpublished");
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let host = vault.host_with_startup_experiment(
            norn_host::ProductionPolicy::new(1, 1).unwrap(),
            match group {
                None => Experiment::health_once(Arc::new(Pause(barrier.clone()))),
                Some(group) => {
                    Experiment::health_once_construction(Arc::new(Pause(barrier.clone())), group)
                        .unwrap()
                }
            },
        );
        let _demand = host.demand(vault.name()).unwrap();
        barrier.wait();
        let params = GetParams::new(
            VaultAddress::name(vault.name().clone()),
            ResolutionTarget::new("first").unwrap(),
        );
        let state = host.state(vault.name());
        let read = host.get(&params);
        // Always release the worker before an assertion can unwind the host.
        barrier.wait();
        assert_ne!(state.unwrap(), TrustState::Ready);
        assert!(
            read.is_err(),
            "full facts without final health were readable"
        );
        wait_ready(&host, &vault);
        assert!(host.get(&params).is_ok());
    }
}

#[test]
fn construction_groups_preserve_health_once_identity_work_and_rich_answers() {
    let (scratch, vault) = fixture("construction-groups-equivalence");
    for index in 0..35 {
        std::fs::write(
            scratch.join(&format!("vault/note-{index:03}.md")),
            "[[first]] [[missing]]\n",
        )
        .unwrap();
    }
    let mut results = vec![];
    for group in [None, Some(1), Some(16), Some(0)] {
        let machine = vault.beside(&scratch.join(&format!("arm-{group:?}")));
        let probe = Arc::new(Probe::default());
        let experiment = match group {
            None => Experiment::health_once(probe.clone()),
            Some(group) => Experiment::health_once_construction(probe.clone(), group).unwrap(),
        };
        let host = machine.host_with_startup_experiment(
            norn_host::ProductionPolicy::new(1, 1).unwrap(),
            experiment,
        );
        let demand = host.demand(machine.name()).unwrap();
        wait_ready(&host, &machine);
        let params = GetParams::new(
            VaultAddress::name(machine.name().clone()),
            ResolutionTarget::new("first").unwrap(),
        );
        let answer = semantic_get(serde_json::to_value(host.get(&params).unwrap().answer).unwrap());
        let work = host.evidence();
        drop(demand);
        drop(host);
        machine.store().verify_integrity().unwrap();
        let audit =
            norn_store::startup_audit::audit(&machine.database(), |_, _, _| Ok(())).unwrap();
        let stages = probe.0.lock().unwrap();
        assert_eq!(stages.len(), 2);
        if let Some(group) = group {
            let receipt = &stages[1].1;
            assert_eq!(receipt["construction_group"], group as u64);
            assert_eq!(
                receipt["construction_document_changesets"],
                stages[0].1["changesets_applied"]
            );
            assert_eq!(receipt["construction_health_generation_groups"], 37);
            assert_eq!(receipt["construction_health_resets"], 1);
            assert_eq!(
                receipt["construction_commits"],
                match group {
                    1 => 75,
                    16 => 5,
                    0 => 1,
                    _ => unreachable!(),
                }
            );
            assert_eq!(
                receipt["construction_commit_attempts"],
                receipt["construction_commits"]
            );
        }
        results.push((audit, answer, work));
        let host = machine.host();
        let _demand = host.demand(machine.name()).unwrap();
        wait_ready(&host, &machine);
        assert_eq!(host.evidence().documents_derived, 0);
    }
    for result in &results[1..] {
        for key in ["semantic_sha256", "document_construction_sha256", "counts"] {
            assert_eq!(results[0].0[key], result.0[key], "{key}");
        }
        assert_eq!(results[0].1, result.1);
        assert_eq!(
            results[0].2, result.2,
            "all existing work counters remain identical"
        );
    }
}

#[test]
fn source_changes_held_during_construction_drain_after_commit_and_equal_a_fresh_build() {
    struct ChangeDuringHeal {
        root: std::path::PathBuf,
        drain: Mutex<Option<BTreeMap<&'static str, u64>>>,
    }
    impl Observer for ChangeDuringHeal {
        fn catalogue(&self, _: &Catalogue, _: &ContentModel) -> Result<(), String> {
            unreachable!()
        }
        fn indexed(
            &self,
            _: Stage,
            _: Arc<SnapshotReader>,
            _: Arc<ContentModel>,
        ) -> Result<(), String> {
            unreachable!()
        }
        fn health_once_work(
            &self,
            finalized: bool,
            _: &BTreeMap<&'static str, u64>,
        ) -> Result<(), String> {
            if !finalized {
                std::fs::write(self.root.join("first.md"), "[[arrived#Present]] [[gone]]\n")
                    .unwrap();
                std::fs::rename(self.root.join("later.md"), self.root.join("arrived.md")).unwrap();
                std::fs::write(self.root.join("arrived.md"), "# Present\n").unwrap();
                // Keep the heal window open across the polling interval and
                // coalescer's maximum batch age. The assertion below requires
                // actual pre-publication drain commits, not eventual repair.
                std::thread::sleep(Duration::from_secs(1));
            }
            Ok(())
        }
        fn construction_complete(&self, work: &BTreeMap<&'static str, u64>) -> Result<(), String> {
            *self.drain.lock().unwrap() = Some(work.clone());
            Ok(())
        }
    }
    for group in [1, 16, 0] {
        let (scratch, vault) = fixture("construction-held-changes");
        let probe = Arc::new(ChangeDuringHeal {
            root: scratch.join("vault"),
            drain: Mutex::new(None),
        });
        let host = vault.host_with_startup_experiment(
            norn_host::ProductionPolicy::new(1, 1).unwrap(),
            Experiment::health_once_construction(probe.clone(), group).unwrap(),
        );
        let demand = host.demand(vault.name()).unwrap();
        wait_ready(&host, &vault);
        let receipt = probe.drain.lock().unwrap().clone().unwrap();
        assert!(
            receipt["construction_commits_drain"] > 0,
            "changes were not drained before publication"
        );
        assert_eq!(
            receipt["construction_commits_drain"],
            receipt["construction_drain_changesets"]
        );
        drop(demand);
        drop(host);
        let mut maintained_store = vault.store();
        norn_testkit::equivalence::assert_operationally_valid(
            &mut maintained_store,
            "maintained after held changes",
        );
        let maintained =
            norn_testkit::equivalence::StoreProjection::read(&mut maintained_store).unwrap();
        assert_eq!(
            norn_testkit::equivalence::tombstones(&mut maintained_store)
                .unwrap()
                .len(),
            1
        );
        drop(maintained_store);
        let fresh = vault.beside(&scratch.join("fresh-final-tree"));
        let host = fresh.host();
        let demand = host.demand(fresh.name()).unwrap();
        wait_ready(&host, &fresh);
        drop(demand);
        drop(host);
        let mut rebuilt_store = fresh.store();
        norn_testkit::equivalence::assert_operationally_valid(
            &mut rebuilt_store,
            "fresh final tree",
        );
        let rebuilt = norn_testkit::equivalence::StoreProjection::read(&mut rebuilt_store).unwrap();
        maintained.assert_equivalent(&rebuilt, "held source changes match a from-zero final tree");
    }
}

#[test]
fn construction_experiment_is_first_attempt_only_even_on_a_second_created_store() {
    for group in [1, 16, 0] {
        let (scratch, vault) = fixture("construction-second-attempt");
        let probe = Arc::new(Probe::default());
        let experiment = Experiment::health_once_construction(probe.clone(), group).unwrap();
        let host = vault.host_with_startup_experiment(
            norn_host::ProductionPolicy::new(1, 1).unwrap(),
            experiment.clone(),
        );
        let demand = host.demand(vault.name()).unwrap();
        wait_ready(&host, &vault);
        drop(demand);
        drop(host);
        let other = vault.beside(&scratch.join("second-created-store"));
        let host = other.host_with_startup_experiment(
            norn_host::ProductionPolicy::new(1, 1).unwrap(),
            experiment,
        );
        let demand = host.demand(other.name()).unwrap();
        wait_refused(&host, &other);
        assert_eq!(probe.0.lock().unwrap().len(), 2);
        drop(demand);
        drop(host);
    }
}

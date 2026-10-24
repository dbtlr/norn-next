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
    for failure in [Failure::HealthSql, Failure::Finalization, Failure::Unwind] {
        let (_scratch, vault) = fixture("health-once-failed");
        let host = vault.host_with_startup_experiment(
            norn_host::ProductionPolicy::new(1, 1).unwrap(),
            Experiment::health_once(Arc::new(Fail(failure))),
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
        drop(db);
        let host = vault.host();
        let _demand = host.demand(vault.name()).unwrap();
        wait_refused(&host, &vault);
    }
}

#[test]
fn health_once_refuses_existing_empty_warm_and_completed_experimental_stores() {
    for existing in ["empty", "ordinary", "health-once"] {
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
            Experiment::health_once(attempted.clone()),
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
    let (_scratch, vault) = fixture("health-once-unpublished");
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let host = vault.host_with_startup_experiment(
        norn_host::ProductionPolicy::new(1, 1).unwrap(),
        Experiment::health_once(Arc::new(Pause(barrier.clone()))),
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

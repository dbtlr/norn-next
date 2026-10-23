//! Canonical-demand is a paused simulation, checked against the ordinary final store.
#![cfg(feature = "induced-failure")]
#![allow(clippy::disallowed_methods)]
mod attach;
use norn_host::startup_experiment::{Experiment, Observer, Stage};
use norn_store::{ContentModel, ReadNeed, SnapshotReader, startup_experiment::Catalogue};
use norn_wire::{GetParams, ResolutionTarget, VaultAddress, VaultName};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Probe {
    target: String,
    answers: Mutex<Vec<serde_json::Value>>,
}
impl Observer for Probe {
    fn catalogue(&self, _: &Catalogue, _: &ContentModel) -> Result<(), String> {
        Ok(())
    }
    fn indexed(
        &self,
        stage: Stage,
        reader: Arc<SnapshotReader>,
        model: Arc<ContentModel>,
    ) -> Result<(), String> {
        if !matches!(stage, Stage::CanonicalDemand | Stage::CanonicalHealth) {
            return Ok(());
        }
        let turn = reader
            .acquire(ReadNeed::Source, Instant::now() + Duration::from_secs(5))
            .unwrap()
            .unwrap();
        let snapshot = turn.establish().unwrap();
        let params = GetParams::new(
            VaultAddress::name(VaultName::new("notes").unwrap()),
            ResolutionTarget::new(&self.target).unwrap(),
        );
        let answer = match norn_host::startup_experiment::get(&snapshot, &params, &model) {
            Ok(gotten) => {
                let (unsatisfied, report) = gotten.into_report();
                assert!(unsatisfied.is_empty());
                serde_json::to_value(report).unwrap()
            }
            Err(refusal) => serde_json::json!({"refusal":format!("{refusal:?}")}),
        };
        self.answers.lock().unwrap().push(answer);
        Ok(())
    }
}
fn wait_ready(host: &norn_host::Host<norn_host::ProductionEntryOps>, name: &VaultName) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while host.state(name).unwrap() != norn_wire::TrustState::Ready
        || !host.work_in_flight(name).unwrap().is_quiet()
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
}
#[test]
fn canonical_default_get_has_links_anchors_and_schema_findings() {
    let scratch = norn_testkit::scratch::Scratch::new("canonical-demand-whole");
    let root = scratch.join("vault");
    std::fs::create_dir_all(root.join(".norn")).unwrap();
    std::fs::write(
        root.join(".norn/schema.yaml"),
        "version: 1\nfields:\n  rating: { type: number }\n",
    )
    .unwrap();
    std::fs::write(root.join("first.md"), "---\nrating: nope\n---\n[[target]] [[missing]] [[target#Present]] [[target#Absent]] [[target#^block]] [[target#^absent]]\n").unwrap();
    std::fs::write(root.join("target.md"), "# Present\nBlock text. ^block\n").unwrap();
    let vault = attach::Vault::adopt(scratch.root());
    let probe = Arc::new(Probe {
        target: "first".into(),
        answers: Mutex::new(vec![]),
    });
    let host = vault.host_with_startup_experiment(
        norn_host::ProductionPolicy::new(128, 128).unwrap(),
        Experiment::canonical_demand("first", probe.clone()),
    );
    let demand = host.demand(vault.name()).unwrap();
    wait_ready(&host, vault.name());
    let answers = probe.answers.lock().unwrap();
    assert_eq!(answers.len(), 2);
    assert_eq!(
        semantic_answer(answers[0].clone()),
        semantic_answer(answers[1].clone())
    );
    assert_eq!(answers[0]["document"]["links"]["total"], 6);
    assert_eq!(answers[0]["document"]["findings"]["total"], 4);
    drop(answers);
    drop(demand);
    drop(host);
    vault.store().verify_integrity().unwrap();
}

// Finding handles are scoped to each store epoch. Keep all authored and
// derived content, including complete ordered findings and link candidates.
fn semantic_answer(mut answer: serde_json::Value) -> serde_json::Value {
    if let Some(findings) = answer["document"]["findings"]["items"].as_array_mut() {
        for finding in findings {
            let object = finding.as_object_mut().unwrap();
            object.remove("id");
            object.remove("generation");
        }
    }
    answer
}

#[test]
fn canonical_demand_uses_eligible_candidates_and_exact_ambiguity() {
    for (target, files, wanted_path, ambiguous) in [
        (
            "first",
            vec![
                ("first.md", vec![0xff]),
                ("folder/first.md", b"[[target]]".to_vec()),
                ("target.md", b"# Target".to_vec()),
            ],
            Some("folder/first.md"),
            false,
        ),
        (
            "dupe",
            vec![
                ("one/dupe.md", b"One".to_vec()),
                ("two/Dupe.md", b"Two".to_vec()),
                ("three/dupe.md", b"Three".to_vec()),
                ("four/dupe.md", b"Four".to_vec()),
                ("five/dupe.md", b"Five".to_vec()),
                ("six/dupe.md", b"Six".to_vec()),
                ("dupe.md", vec![0xff]),
            ],
            None,
            true,
        ),
        (
            "first",
            vec![
                (
                    "first.md",
                    b"[[folder/target]] [[dupe]] [block](./folder/target.md#^id) [[photo.png]]"
                        .to_vec(),
                ),
                ("folder/target.md", b"# Heading\nA block. ^id".to_vec()),
                ("elsewhere/target.md", b"Other".to_vec()),
                ("one/dupe.md", b"One".to_vec()),
                ("two/dupe.md", b"Two".to_vec()),
                ("photo.png", vec![0xff]),
            ],
            Some("first.md"),
            false,
        ),
    ] {
        let scratch = norn_testkit::scratch::Scratch::new("canonical-demand-candidates");
        let root = scratch.join("vault");
        std::fs::create_dir_all(root.join(".norn")).unwrap();
        std::fs::write(root.join(".norn/schema.yaml"), "version: 1\n").unwrap();
        for (path, bytes) in files {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        let vault = attach::Vault::adopt(scratch.root());
        let probe = Arc::new(Probe {
            target: target.into(),
            answers: Mutex::new(vec![]),
        });
        let host = vault.host_with_startup_experiment(
            norn_host::ProductionPolicy::new(128, 128).unwrap(),
            Experiment::canonical_demand(target, probe.clone()),
        );
        let demand = host.demand(vault.name()).unwrap();
        wait_ready(&host, vault.name());
        let answers = probe.answers.lock().unwrap();
        assert_eq!(answers.len(), 2);
        assert_eq!(
            semantic_answer(answers[0].clone()),
            semantic_answer(answers[1].clone())
        );
        if let Some(path) = wanted_path {
            assert_eq!(answers[0]["document"]["path"], path);
        }
        if ambiguous {
            assert!(
                answers[0]["refusal"]
                    .as_str()
                    .unwrap()
                    .contains("AmbiguousTarget")
            );
        }
        drop(answers);
        drop(demand);
        drop(host);
        vault.store().verify_integrity().unwrap();
    }
}

#[test]
fn canonical_final_layout_and_ids_match_an_ordinary_heal() {
    let scratch = norn_testkit::scratch::Scratch::new("canonical-demand-final");
    let root = scratch.join("vault");
    std::fs::create_dir_all(root.join(".norn")).unwrap();
    std::fs::write(
        root.join(".norn/schema.yaml"),
        "version: 1\nfields:\n  rating: { type: number }\n",
    )
    .unwrap();
    // Cross several ordinary changesets; priority subject sorts near the end.
    for index in 0..270 {
        std::fs::write(
            root.join(format!("note-{index:04}.md")),
            "---\nrating: nope\n---\n[[last]] [[missing]]\n# Heading\n",
        )
        .unwrap();
    }
    std::fs::write(root.join("last.md"), "[[note-0269#Heading]] [[missing]]").unwrap();
    let vault = attach::Vault::adopt(scratch.root());
    let mut audits = vec![];
    for canonical in [false, true] {
        let machine = vault.beside(&scratch.join(if canonical { "canonical" } else { "ordinary" }));
        let probe = Arc::new(Probe {
            target: "note-0269".into(),
            answers: Mutex::new(vec![]),
        });
        let policy = norn_host::ProductionPolicy::new(128, 128).unwrap();
        let host = if canonical {
            machine.host_with_startup_experiment(
                policy,
                Experiment::canonical_demand("note-0269", probe),
            )
        } else {
            machine.host_with_policy(policy)
        };
        let demand = host.demand(machine.name()).unwrap();
        wait_ready(&host, machine.name());
        drop(demand);
        drop(host);
        machine.store().verify_integrity().unwrap();
        audits
            .push(norn_store::startup_audit::audit(&machine.database(), |_, _, _| Ok(())).unwrap());
    }
    for key in [
        "semantic_sha256",
        "counts",
        "write_generation",
        "canonical_identity_sha256",
        "page_map_sha256",
    ] {
        assert!(!audits[0][key].is_null(), "missing diagnostic {key}");
        assert_eq!(audits[0][key], audits[1][key], "{key}");
    }
}

struct MoveCandidate {
    root: std::path::PathBuf,
    remove: bool,
    probe: Probe,
    declined: Mutex<bool>,
}
impl Observer for MoveCandidate {
    fn catalogue(&self, _: &Catalogue, _: &ContentModel) -> Result<(), String> {
        if self.remove {
            std::fs::remove_file(self.root.join("first.md")).unwrap();
        } else {
            std::fs::write(
                self.root.join("first.md"),
                "Edited [[target#Present]] [[missing]]",
            )
            .unwrap();
        }
        Ok(())
    }
    fn declined(&self, _: &str) -> Result<(), String> {
        *self.declined.lock().unwrap() = true;
        Ok(())
    }
    fn indexed(
        &self,
        stage: Stage,
        reader: Arc<SnapshotReader>,
        model: Arc<ContentModel>,
    ) -> Result<(), String> {
        self.probe.indexed(stage, reader, model)
    }
}
#[test]
fn a_removed_or_edited_inventory_candidate_never_returns_stale_bytes() {
    for remove in [false, true] {
        let scratch = norn_testkit::scratch::Scratch::new("canonical-demand-moved");
        let root = scratch.join("vault");
        std::fs::create_dir_all(root.join(".norn")).unwrap();
        std::fs::write(root.join(".norn/schema.yaml"), "version: 1\n").unwrap();
        std::fs::write(root.join("first.md"), "Old bytes").unwrap();
        std::fs::write(root.join("target.md"), "# Present").unwrap();
        let vault = attach::Vault::adopt(scratch.root());
        let observer = Arc::new(MoveCandidate {
            root,
            remove,
            probe: Probe {
                target: "first".into(),
                answers: Mutex::new(vec![]),
            },
            declined: Mutex::new(false),
        });
        let host = vault.host_with_startup_experiment(
            norn_host::ProductionPolicy::new(128, 128).unwrap(),
            Experiment::canonical_demand("first", observer.clone()),
        );
        let demand = host.demand(vault.name()).unwrap();
        wait_ready(&host, vault.name());
        let answers = observer.probe.answers.lock().unwrap();
        if remove {
            assert!(*observer.declined.lock().unwrap());
            assert_eq!(answers.len(), 1);
        } else {
            assert!(
                answers.last().unwrap()["document"]["body"]["text"]
                    .as_str()
                    .unwrap()
                    .starts_with("Edited")
            );
            if answers.len() == 2 {
                assert_eq!(
                    semantic_answer(answers[0].clone()),
                    semantic_answer(answers[1].clone())
                );
            } else {
                assert!(*observer.declined.lock().unwrap());
            }
        }
        drop(answers);
        drop(demand);
        drop(host);
        vault.store().verify_integrity().unwrap();
    }
}

#[test]
fn a_candidate_head_larger_than_the_closure_bound_declines_instead_of_guessing() {
    struct Decline(Mutex<bool>);
    impl Observer for Decline {
        fn catalogue(&self, _: &Catalogue, _: &ContentModel) -> Result<(), String> {
            Ok(())
        }
        fn declined(&self, _: &str) -> Result<(), String> {
            *self.0.lock().unwrap() = true;
            Ok(())
        }
        fn indexed(
            &self,
            stage: Stage,
            _: Arc<SnapshotReader>,
            _: Arc<ContentModel>,
        ) -> Result<(), String> {
            assert_ne!(stage, Stage::CanonicalDemand);
            Ok(())
        }
    }
    let scratch = norn_testkit::scratch::Scratch::new("canonical-demand-bound");
    let root = scratch.join("vault");
    std::fs::create_dir_all(root.join(".norn")).unwrap();
    std::fs::write(root.join(".norn/schema.yaml"), "version: 1\n").unwrap();
    for i in 0..257 {
        let directory = root.join(format!("folder-{i}"));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("same.md"), "Document").unwrap();
    }
    let vault = attach::Vault::adopt(scratch.root());
    let observer = Arc::new(Decline(Mutex::new(false)));
    let host = vault.host_with_startup_experiment(
        norn_host::ProductionPolicy::new(128, 128).unwrap(),
        Experiment::canonical_demand("same", observer.clone()),
    );
    let demand = host.demand(vault.name()).unwrap();
    wait_ready(&host, vault.name());
    assert!(*observer.0.lock().unwrap());
    drop(demand);
    drop(host);
    vault.store().verify_integrity().unwrap();
}

#[test]
fn retiring_the_projection_keeps_an_active_snapshot_but_closes_new_reads() {
    struct Retained(Mutex<Option<(Arc<SnapshotReader>, norn_store::Snapshot)>>);
    impl Observer for Retained {
        fn catalogue(&self, _: &Catalogue, _: &ContentModel) -> Result<(), String> {
            Ok(())
        }
        fn indexed(
            &self,
            stage: Stage,
            reader: Arc<SnapshotReader>,
            model: Arc<ContentModel>,
        ) -> Result<(), String> {
            if stage == Stage::CanonicalDemand {
                let turn = reader
                    .acquire(ReadNeed::Source, Instant::now() + Duration::from_secs(10))
                    .unwrap()
                    .unwrap();
                *self.0.lock().unwrap() = Some((reader, turn.establish().unwrap()));
            } else if stage == Stage::CanonicalHealth {
                let mut held = self.0.lock().unwrap();
                let (reader, snapshot) = held.as_ref().unwrap();
                assert!(
                    reader
                        .acquire(ReadNeed::Source, Instant::now() + Duration::from_secs(5))
                        .unwrap()
                        .is_none()
                );
                let params = GetParams::new(
                    VaultAddress::name(VaultName::new("notes").unwrap()),
                    ResolutionTarget::new("first").unwrap(),
                );
                let (_, report) = norn_host::startup_experiment::get(snapshot, &params, &model)
                    .unwrap()
                    .into_report();
                assert_eq!(
                    serde_json::to_value(report).unwrap()["document"]["body"]["text"],
                    "Retained body"
                );
                held.take();
            }
            Ok(())
        }
    }
    let scratch = norn_testkit::scratch::Scratch::new("canonical-demand-retirement");
    let root = scratch.join("vault");
    std::fs::create_dir_all(root.join(".norn")).unwrap();
    std::fs::write(root.join(".norn/schema.yaml"), "version: 1\n").unwrap();
    std::fs::write(root.join("first.md"), "Retained body").unwrap();
    let vault = attach::Vault::adopt(scratch.root());
    let observer = Arc::new(Retained(Mutex::new(None)));
    let host = vault.host_with_startup_experiment(
        norn_host::ProductionPolicy::new(128, 128).unwrap(),
        Experiment::canonical_demand("first", observer.clone()),
    );
    let demand = host.demand(vault.name()).unwrap();
    wait_ready(&host, vault.name());
    assert!(observer.0.lock().unwrap().is_none());
    drop(demand);
    drop(host);
    assert_no_projection_files(scratch.root());
}

fn assert_no_projection_files(root: &std::path::Path) {
    for entry in std::fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            if entry.file_name() == "canonical-demand" {
                assert_eq!(std::fs::read_dir(entry.path()).unwrap().count(), 0);
            } else {
                assert_no_projection_files(&entry.path());
            }
        }
    }
}

#[test]
fn an_observer_failure_removes_projection_files_and_leaves_an_ordinary_recoverable_store() {
    struct Fail;
    impl Observer for Fail {
        fn catalogue(&self, _: &Catalogue, _: &ContentModel) -> Result<(), String> {
            Ok(())
        }
        fn indexed(
            &self,
            stage: Stage,
            _: Arc<SnapshotReader>,
            _: Arc<ContentModel>,
        ) -> Result<(), String> {
            assert_eq!(stage, Stage::CanonicalDemand);
            Err("induced observer failure".into())
        }
    }
    let scratch = norn_testkit::scratch::Scratch::new("canonical-demand-failure");
    let root = scratch.join("vault");
    std::fs::create_dir_all(root.join(".norn")).unwrap();
    std::fs::write(root.join(".norn/schema.yaml"), "version: 1\n").unwrap();
    std::fs::write(root.join("first.md"), "[[missing]]").unwrap();
    let vault = attach::Vault::adopt(scratch.root());
    let host = vault.host_with_startup_experiment(
        norn_host::ProductionPolicy::new(128, 128).unwrap(),
        Experiment::canonical_demand("first", Arc::new(Fail)),
    );
    let demand = host.demand(vault.name()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while host.state(vault.name()).is_ok() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    drop(demand);
    drop(host);
    assert_no_projection_files(scratch.root());
    let host = vault.host();
    let demand = host.demand(vault.name()).unwrap();
    wait_ready(&host, vault.name());
    drop(demand);
    drop(host);
    vault.store().verify_integrity().unwrap();
}

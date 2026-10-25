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
        Experiment::canonical_intake("first", probe.clone()),
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
    struct WorkProbe {
        probe: Probe,
        work: Mutex<Option<std::collections::BTreeMap<&'static str, u64>>>,
    }
    impl Observer for WorkProbe {
        fn catalogue(&self, catalogue: &Catalogue, model: &ContentModel) -> Result<(), String> {
            self.probe.catalogue(catalogue, model)
        }
        fn indexed(
            &self,
            stage: Stage,
            reader: Arc<SnapshotReader>,
            model: Arc<ContentModel>,
        ) -> Result<(), String> {
            self.probe.indexed(stage, reader, model)
        }
        fn canonical_work(
            &self,
            work: &std::collections::BTreeMap<&'static str, u64>,
        ) -> Result<(), String> {
            *self.work.lock().unwrap() = Some(work.clone());
            Ok(())
        }
    }
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
    let mut works = vec![];
    for (mode, cached) in [
        ("ordinary", false),
        ("ordinary", true),
        ("canonical-demand", false),
        ("canonical-demand", true),
        ("canonical-intake", false),
        ("canonical-intake", true),
    ] {
        let machine = vault.beside(&scratch.join(format!("{mode}-{cached}")));
        let probe = Arc::new(WorkProbe {
            probe: Probe {
                target: "note-0269".into(),
                answers: Mutex::new(vec![]),
            },
            work: Mutex::new(None),
        });
        let policy = norn_host::ProductionPolicy::new(128, 128).unwrap();
        let experiment = match mode {
            "ordinary" => None,
            "canonical-demand" => Some(Experiment::canonical_demand("note-0269", probe.clone())),
            "canonical-intake" => Some(Experiment::canonical_intake("note-0269", probe.clone())),
            _ => unreachable!(),
        };
        let cache = cached
            .then(|| norn_host::startup_experiment::InitialImportCache::new(8192, true).unwrap());
        if mode == "ordinary" {
            // Rebuilding a deliberately superseded empty store selects ordinary
            // canonical construction, including the cache lane, without public
            // Created-only metadata work in this canonical-phase comparison.
            let order = machine.store().path_order();
            let version = norn_store::DerivationVersion::new(
                norn_host::DERIVATION_VERSION.get().checked_add(1).unwrap(),
            );
            drop(norn_store::Store::open(machine.database(), order, version).unwrap());
        }
        let host = machine.host_with_loading_options(policy, experiment, cache.clone());
        let demand = host.demand(machine.name()).unwrap();
        wait_ready(&host, machine.name());
        if let Some(cache) = cache {
            let receipt = cache.receipt().expect("initial cache window ran");
            assert!(receipt.import_succeeded);
            assert_eq!(receipt.writer.during, Some(-8192));
            assert_eq!(receipt.writer.after, Some(receipt.writer.before));
        }
        if mode == "ordinary" {
            let e = host.evidence();
            works.push(std::collections::BTreeMap::from([
                ("documents_derived", e.documents_derived),
                ("changesets_applied", e.changesets_applied),
                ("documents_upserted", e.documents_upserted),
                ("documents_deleted", e.documents_deleted),
                ("tombstones_recorded", e.tombstones_recorded),
                ("findings_written", e.findings_written),
                ("findings_discarded", e.findings_discarded),
                ("links_redecided", e.links_redecided),
                ("link_health_keys_resolved", e.link_health_keys_resolved),
                ("link_health_candidates_read", e.link_health_candidates_read),
                ("changeset_read_steps", e.changeset_read_steps),
            ]));
        } else {
            works.push(
                probe
                    .work
                    .lock()
                    .unwrap()
                    .clone()
                    .expect("ordinary work checkpoint"),
            );
        }
        drop(demand);
        drop(host);
        machine.store().verify_integrity().unwrap();
        audits
            .push(norn_store::startup_audit::audit(&machine.database(), |_, _, _| Ok(())).unwrap());
        if mode == "canonical-intake" {
            let before = probe.probe.answers.lock().unwrap().len();
            let host = machine.host_with_startup_experiment(
                policy,
                Experiment::canonical_intake("note-0269", probe.clone()),
            );
            let demand = host.demand(machine.name()).unwrap();
            wait_ready(&host, machine.name());
            assert_eq!(host.evidence().documents_derived, 0);
            assert_eq!(
                probe.probe.answers.lock().unwrap().len(),
                before,
                "restart entered the initial experiment"
            );
            drop(demand);
            drop(host);
            let restarted =
                norn_store::startup_audit::audit(&machine.database(), |_, _, _| Ok(())).unwrap();
            for key in [
                "store_epoch",
                "write_generation",
                "semantic_sha256",
                "canonical_identity_sha256",
                "page_map_sha256",
            ] {
                assert_eq!(audits.last().unwrap()[key], restarted[key], "restart {key}");
            }
        }
    }
    for key in [
        "semantic_sha256",
        "counts",
        "write_generation",
        "canonical_identity_sha256",
        "page_map_sha256",
    ] {
        assert!(!audits[0][key].is_null(), "missing diagnostic {key}");
        for audit in &audits[1..] {
            assert_eq!(audits[0][key], audit[key], "{key}");
        }
    }
    assert_eq!(works[0], works[1]);
    assert_eq!(works[0], works[2]);
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
    std::fs::write(root.join("invalid.md"), [0xff]).unwrap();
    std::fs::write(root.join("miss.md"), "Present outside the requested suffix").unwrap();
    let vault = attach::Vault::adopt(scratch.root());
    for target in ["same", "absent", "invalid", "absent/miss"] {
        let vault = vault.beside(&scratch.join(target));
        let observer = Arc::new(Decline(Mutex::new(false)));
        let host = vault.host_with_startup_experiment(
            norn_host::ProductionPolicy::new(128, 128).unwrap(),
            Experiment::canonical_intake(target, observer.clone()),
        );
        let demand = host.demand(vault.name()).unwrap();
        wait_ready(&host, vault.name());
        assert!(*observer.0.lock().unwrap());
        drop(demand);
        drop(host);
        vault.store().verify_integrity().unwrap();
    }
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
            if entry.file_name() == "canonical-demand" || entry.file_name() == "canonical-intake" {
                fn no_files(path: &std::path::Path) {
                    for child in std::fs::read_dir(path).unwrap() {
                        let child = child.unwrap();
                        assert!(
                            child.file_type().unwrap().is_dir(),
                            "projection residue: {:?}",
                            child.path()
                        );
                        no_files(&child.path());
                    }
                }
                no_files(&entry.path());
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
        fn intake(
            &self,
            _: Arc<norn_host::startup_experiment::IntakeReader>,
        ) -> Result<(), String> {
            Err("induced intake observer failure".into())
        }
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
            Ok(())
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
        Experiment::canonical_intake("first", Arc::new(Fail)),
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

#[test]
fn intake_answers_sparse_zero_and_unknown_fields_before_ordinary_heal() {
    struct IntakeProbe {
        answers: Mutex<Vec<serde_json::Value>>,
        drains: Mutex<Vec<serde_json::Value>>,
        retired: Mutex<Option<Arc<norn_host::startup_experiment::IntakeReader>>>,
        held: Mutex<
            Option<(
                Arc<norn_host::startup_experiment::IntakeReader>,
                norn_host::startup_experiment::IntakeSnapshot,
            )>,
        >,
    }
    fn requests() -> Vec<norn_wire::FindParams> {
        [
            norn_wire::Predicate::equal_to("bucket", "rare"),
            norn_wire::Predicate::equal_to("bucket", "absent"),
            norn_wire::Predicate::equal_to("unknown", "absent"),
        ]
        .into_iter()
        .map(|predicate| {
            norn_wire::FindParams::new(VaultAddress::name(VaultName::new("notes").unwrap()))
                .with_predicates([predicate])
                .with_columns([norn_wire::Column::fields()])
        })
        .collect()
    }
    impl Observer for IntakeProbe {
        fn catalogue(&self, _: &Catalogue, _: &ContentModel) -> Result<(), String> {
            Ok(())
        }
        fn intake(
            &self,
            reader: Arc<norn_host::startup_experiment::IntakeReader>,
        ) -> Result<(), String> {
            let snapshot = reader
                .snapshot(Instant::now() + Duration::from_secs(5))
                .unwrap()
                .unwrap();
            let address = VaultAddress::name(VaultName::new("notes").unwrap());
            for predicate in [
                norn_wire::Predicate::matches("Last"),
                norn_wire::Predicate::tag("tag"),
                norn_wire::Predicate::links_to(ResolutionTarget::new("first").unwrap()),
                norn_wire::Predicate::resolves(ResolutionTarget::new("first").unwrap()),
                norn_wire::Predicate::has_finding(norn_wire::FindingKind::Broken),
            ] {
                assert!(
                    snapshot
                        .find(
                            &norn_wire::FindParams::new(address.clone())
                                .with_predicates([predicate.clone()])
                        )
                        .is_err(),
                    "incomplete predicate must decline: {predicate:?}"
                );
                assert!(
                    snapshot
                        .count(
                            &norn_wire::CountParams::new(address.clone())
                                .with_predicates([predicate])
                        )
                        .is_err()
                );
            }
            for column in [
                norn_wire::Column::body(),
                norn_wire::Column::links(),
                norn_wire::Column::headings(),
                norn_wire::Column::blocks(),
                norn_wire::Column::tags(),
                norn_wire::Column::findings(),
            ] {
                assert!(
                    snapshot
                        .find(
                            &norn_wire::FindParams::new(address.clone())
                                .with_columns([column.clone()])
                        )
                        .is_err(),
                    "incomplete column must decline: {column:?}"
                );
            }
            assert!(
                snapshot
                    .count(
                        &norn_wire::CountParams::new(address).with_by([norn_wire::GroupKey::tag()])
                    )
                    .is_err()
            );
            let answers = requests()
                .iter()
                .map(|params| {
                    serde_json::to_value(snapshot.find(params).unwrap().into_report()).unwrap()
                })
                .collect();
            *self.answers.lock().unwrap() = answers;
            *self.drains.lock().unwrap() = metadata_drains(
                |params| snapshot.find(params).unwrap(),
                |params| snapshot.count(params).unwrap(),
                snapshot.reading(),
            );
            *self.held.lock().unwrap() = Some((reader, snapshot));
            Ok(())
        }
        fn indexed(
            &self,
            stage: Stage,
            reader: Arc<SnapshotReader>,
            model: Arc<ContentModel>,
        ) -> Result<(), String> {
            if stage == Stage::CanonicalHealth {
                let turn = reader
                    .acquire(
                        ReadNeed::SourceAndScratch,
                        Instant::now() + Duration::from_secs(5),
                    )
                    .unwrap()
                    .unwrap();
                let snapshot = turn.establish().unwrap();
                let (retired, active) = self.held.lock().unwrap().take().unwrap();
                *self.retired.lock().unwrap() = Some(retired.clone());
                assert!(
                    retired
                        .snapshot(Instant::now() + Duration::from_secs(5))
                        .unwrap()
                        .is_none()
                );
                assert_eq!(
                    *self.drains.lock().unwrap(),
                    metadata_drains(
                        |params| active.find(params).unwrap(),
                        |params| active.count(params).unwrap(),
                        active.reading()
                    )
                );
                assert_eq!(
                    *self.drains.lock().unwrap(),
                    metadata_drains(
                        |params| snapshot.find(params, &model).unwrap(),
                        |params| snapshot.count(params, &model).unwrap(),
                        snapshot.reading()
                    )
                );
                let answers = self.answers.lock().unwrap();
                assert_eq!(
                    answers.len(),
                    3,
                    "metadata checkpoint must precede ordinary completion"
                );
                for (params, early) in requests().iter().zip(answers.iter()) {
                    assert_eq!(
                        *early,
                        serde_json::to_value(snapshot.find(params, &model).unwrap().into_report())
                            .unwrap()
                    );
                }
                assert_eq!(answers[0][2]["page"]["rows"].as_array().unwrap().len(), 1);
                assert_eq!(answers[1][2]["page"]["rows"].as_array().unwrap().len(), 0);
            }
            Ok(())
        }
    }
    let scratch = norn_testkit::scratch::Scratch::new("canonical-intake-fields");
    let root = scratch.join("vault");
    rich_metadata_fixture(&root);
    let vault = attach::Vault::adopt(scratch.root());
    let probe = Arc::new(IntakeProbe {
        answers: Mutex::new(vec![]),
        drains: Mutex::new(vec![]),
        retired: Mutex::new(None),
        held: Mutex::new(None),
    });
    let host = vault.host_with_startup_experiment(
        norn_host::ProductionPolicy::new(2, 2).unwrap(),
        Experiment::canonical_intake("first", probe.clone()),
    );
    let demand = host.demand(vault.name()).unwrap();
    wait_ready(&host, vault.name());
    assert_eq!(probe.answers.lock().unwrap().len(), 3);
    drop(demand);
    drop(host);
    assert_no_projection_files(scratch.root());
    vault.store().verify_integrity().unwrap();
}

fn rich_metadata_fixture(root: &std::path::Path) {
    std::fs::create_dir_all(root.join(".norn")).unwrap();
    std::fs::write(
        root.join(".norn/schema.yaml"),
        "version: 1\nfields:\n  rank: { type: number }\n  due: { type: date }\n",
    )
    .unwrap();
    std::fs::write(
        root.join("first.md"),
        "---\nbucket: common\n---\n[[missing]]",
    )
    .unwrap();
    std::fs::write(root.join("last.md"), "---\nbucket: rare\n---\nLast").unwrap();
    std::fs::write(root.join("invalid.md"), [0xff]).unwrap();
    for (name, text) in [
        (
            "A.md",
            "rank: 2\nlabels: [red, blue]\ndue: 2026-01-01\nnothing: null",
        ),
        ("b.md", "rank: 10\nlabels: [blue, blue]\ndue: nope"),
        ("c.md", "rank: null\nlabels: []"),
        ("d.md", "rank: nope\nlabels: [null, red]"),
        ("e.md", "labels: red\nlate_only: yes"),
        ("malformed.md", "value: ["),
    ] {
        std::fs::write(
            root.join(name),
            format!("---\n{text}\n---\n# tag #bodytag [[missing]]"),
        )
        .unwrap();
    }
}

// Drain through each store's own cursors. Compare cursor keys and every answer
// field, checking local provenance before excluding epoch/generation/retention IDs.
fn metadata_drains(
    find: impl Fn(&norn_wire::FindParams) -> norn_store::Found,
    count: impl Fn(&norn_wire::CountParams) -> norn_store::Counted,
    reading: &norn_store::StoreReading,
) -> Vec<serde_json::Value> {
    use norn_wire::{CountParams, GroupKey};
    let address = VaultAddress::name(VaultName::new("notes").unwrap());
    let mut answers = metadata_find_drains(|params| {
        let found = find(params);
        assert_eq!(found.snapshot.epoch, reading.epoch());
        assert_eq!(found.snapshot.generation as i64, reading.write_generation());
        found
    });
    for by in [
        vec![],
        vec![GroupKey::field("labels")],
        vec![GroupKey::field("rank")],
        vec![GroupKey::field("unknown")],
        vec![GroupKey::field("labels"), GroupKey::field("rank")],
    ] {
        let mut params = CountParams::new(address.clone()).with_by(by).with_limit(2);
        for page in 0..20 {
            let (unsatisfied, advisories, report) = count(&params).into_report();
            assert_eq!(report.counted_at.epoch, reading.epoch());
            assert_eq!(
                report.counted_at.generation as i64,
                reading.write_generation()
            );
            assert!(report.moved.is_empty());
            answers.push(serde_json::json!({"unsatisfied":unsatisfied,"advisories":advisories,"rows":report.rows,"next_key":report.next.as_ref().map(|c| c.key())}));
            params.after = report.next;
            if params.after.is_none() {
                break;
            }
            assert!(page < 19, "count did not terminate");
        }
    }
    answers
}

fn metadata_find_drains(
    find: impl Fn(&norn_wire::FindParams) -> norn_store::Found,
) -> Vec<serde_json::Value> {
    use norn_wire::{Column, Direction, FindParams, Predicate, Sort, SortKey};
    let address = VaultAddress::name(VaultName::new("notes").unwrap());
    let mut selections = vec![FindParams::new(address.clone())];
    for predicate in [
        Predicate::equal_to("bucket", "rare"),
        Predicate::equal_to("bucket", "absent"),
        Predicate::equal_to("unknown", "absent"),
        Predicate::has("nothing"),
        Predicate::missing("nothing"),
        Predicate::in_any("labels", ["red".into(), "blue".into()]),
        Predicate::not_equal_to("labels", "red"),
        Predicate::before("rank", "5"),
        Predicate::after("due", "2025-01-01"),
        Predicate::before("labels", "z"),
        Predicate::path("*.md"),
        Predicate::has("late_only"),
    ] {
        selections.push(FindParams::new(address.clone()).with_predicates([predicate]));
    }
    for key in [
        SortKey::path(),
        SortKey::field("rank"),
        SortKey::field("labels"),
        SortKey::field("unknown"),
    ] {
        for direction in [Direction::Ascending, Direction::Descending] {
            selections.push(
                FindParams::new(address.clone()).with_sort(Sort::new(key.clone(), direction)),
            );
        }
    }
    let mut answers = Vec::new();
    for mut params in selections {
        params.limit = Some(2);
        params.columns = vec![Column::path(), Column::field("rank"), Column::fields()];
        let mut seen = std::collections::BTreeSet::new();
        for page in 0..20 {
            let found = find(&params);
            assert!(!found.snapshot.epoch.is_empty());
            assert!(found.snapshot.generation > 0);
            if let Some(next) = &found.next {
                assert_eq!(next.snapshot(), &found.snapshot);
            }
            let hashes = found.content_hashes.clone();
            assert_eq!(hashes.len(), found.rows.len());
            let (unsatisfied, advisories, report) = found.into_report();
            assert!(report.page.moved.is_empty());
            for row in &report.page.rows {
                assert!(
                    seen.insert(row.path.clone()),
                    "duplicate document in a drain"
                );
            }
            answers.push(serde_json::json!({"unsatisfied":unsatisfied,"advisories":advisories,"rows":report.page.rows,"next_key": report.page.next.as_ref().map(|c| c.key()), "rules":report.rule_sets,"hashes":hashes}));
            params.after = report.page.next;
            if params.after.is_none() {
                break;
            }
            assert!(page < 19, "find did not terminate");
        }
    }
    answers
}

#[test]
fn live_complete_metadata_uses_existing_find_and_retires_before_ready() {
    use norn_host::startup_experiment::{LiveFindReader, LiveFindRefusal};
    use norn_wire::{Column, FindParams};
    struct LiveProbe {
        get: Mutex<Option<serde_json::Value>>,
        drains: Mutex<Vec<serde_json::Value>>,
        first: Mutex<Option<norn_store::Found>>,
        retained: Mutex<Option<Arc<LiveFindReader>>>,
    }
    impl Observer for LiveProbe {
        fn catalogue(&self, _: &Catalogue, _: &ContentModel) -> Result<(), String> {
            Ok(())
        }
        fn live_get(
            &self,
            reader: Arc<norn_host::startup_experiment::LiveGetReader>,
        ) -> Result<(), String> {
            let params = GetParams::new(
                VaultAddress::name(VaultName::new("notes").unwrap()),
                ResolutionTarget::new("first").unwrap(),
            );
            let got = reader
                .get(&params, Instant::now() + Duration::from_secs(5))
                .unwrap();
            assert!(got.unsatisfied.is_empty());
            let report = serde_json::to_value(got.report).unwrap();
            for finding in report["document"]["findings"]["items"].as_array().unwrap() {
                assert!(finding["id"].as_u64().unwrap() > 0);
                assert!(finding["generation"].as_u64().unwrap() <= got.snapshot.generation);
                assert!(finding["generation"].as_u64().unwrap() > 0);
            }
            *self.get.lock().unwrap() = Some(semantic_answer(report));
            Ok(())
        }
        fn live_find(&self, reader: Arc<LiveFindReader>) -> Result<(), String> {
            assert!(
                self.get.lock().unwrap().is_some(),
                "Get must precede complete metadata"
            );
            *self.drains.lock().unwrap() = metadata_find_drains(|params| {
                reader
                    .find(params, Instant::now() + Duration::from_secs(5))
                    .unwrap()
            });
            let params = FindParams::new(VaultAddress::name(VaultName::new("notes").unwrap()))
                .with_columns([Column::path(), Column::fields()])
                .with_sort(norn_wire::Sort::new(
                    norn_wire::SortKey::field("rank"),
                    norn_wire::Direction::Ascending,
                ))
                .with_limit(2);
            let found = reader
                .find(&params, Instant::now() + Duration::from_secs(5))
                .unwrap();
            assert_eq!(found.rows.len(), 2);
            let cursor = found
                .next
                .as_ref()
                .expect("complete metadata has more pages");
            assert_eq!(cursor.snapshot(), &found.snapshot);
            *self.first.lock().unwrap() = Some(found);
            *self.retained.lock().unwrap() = Some(reader);
            Ok(())
        }
        fn intake(
            &self,
            _: Arc<norn_host::startup_experiment::IntakeReader>,
        ) -> Result<(), String> {
            panic!("live metadata must not expose frozen snapshot acquisition")
        }
        fn indexed(
            &self,
            stage: Stage,
            reader: Arc<SnapshotReader>,
            model: Arc<ContentModel>,
        ) -> Result<(), String> {
            assert_eq!(stage, Stage::CanonicalHealth);
            let snapshot = reader
                .acquire(ReadNeed::Source, Instant::now() + Duration::from_secs(5))
                .unwrap()
                .unwrap()
                .establish()
                .unwrap();
            assert_eq!(
                *self.drains.lock().unwrap(),
                metadata_find_drains(|params| snapshot.find(params, &model).unwrap())
            );
            Ok(())
        }
    }
    let scratch = norn_testkit::scratch::Scratch::under(
        std::path::Path::new(env!("CARGO_TARGET_TMPDIR")),
        "live-metadata",
    )
    .unwrap();
    let root = scratch.join("vault");
    rich_metadata_fixture(&root);
    let vault = attach::Vault::adopt(scratch.root());
    let probe = Arc::new(LiveProbe {
        get: Mutex::new(None),
        drains: Mutex::new(vec![]),
        first: Mutex::new(None),
        retained: Mutex::new(None),
    });
    let host = vault.host_with_startup_experiment(
        norn_host::ProductionPolicy::new(2, 2).unwrap(),
        Experiment::live_metadata("first", probe.clone()),
    );
    let demand = host.demand(vault.name()).unwrap();
    wait_ready(&host, vault.name());
    let found = probe
        .first
        .lock()
        .unwrap()
        .take()
        .expect("early Find offered");
    let params = FindParams::new(VaultAddress::name(vault.name().clone()))
        .with_columns([Column::path(), Column::fields()])
        .with_sort(norn_wire::Sort::new(
            norn_wire::SortKey::field("rank"),
            norn_wire::Direction::Ascending,
        ))
        .with_limit(2);
    let gotten = host
        .get(&GetParams::new(
            params.vault.clone(),
            ResolutionTarget::new("first").unwrap(),
        ))
        .unwrap();
    assert_eq!(
        probe.get.lock().unwrap().take().unwrap(),
        semantic_answer(serde_json::to_value(gotten.answer.report).unwrap())
    );
    assert_eq!(
        found.rows,
        host.find(&params).unwrap().answer.report.page.rows
    );
    let counted = host
        .count(&norn_wire::CountParams::new(params.vault.clone()))
        .unwrap()
        .answer
        .report;
    assert_eq!(counted.rows, [norn_wire::Tally::new([], 8)]);
    assert_ne!(counted.counted_at.epoch, found.snapshot.epoch);
    let cursor = found.next.clone().unwrap();
    let mut continued = params.clone();
    continued.after = Some(cursor.clone());
    let next = host.find(&continued).unwrap().answer.report.page;
    assert_eq!(next.moved, [norn_wire::Moved::Epoch]);
    assert_eq!(
        continued.after.as_ref().unwrap(),
        &cursor,
        "never rewrite early authority"
    );
    let mut actual_suffix = next.rows;
    continued.after = next.next;
    while continued.after.is_some() {
        let page = host.find(&continued).unwrap().answer.report.page;
        assert!(page.moved.is_empty());
        actual_suffix.extend(page.rows);
        continued.after = page.next;
    }
    let mut all = params.clone();
    all.limit = Some(100);
    let canonical = host.find(&all).unwrap().answer.report.page.rows;
    assert_eq!(actual_suffix, canonical[found.rows.len()..]);
    assert_eq!(
        canonical.len(),
        8,
        "invalid UTF-8 is quarantined; malformed frontmatter retains its row"
    );
    let mut wrong_order = params.clone();
    wrong_order.sort = Some(norn_wire::Sort::new(
        norn_wire::SortKey::path(),
        norn_wire::Direction::Ascending,
    ));
    wrong_order.after = Some(cursor.clone());
    assert_eq!(
        host.find(&wrong_order).unwrap_err().code(),
        &norn_wire::ReasonCode::VaultCursorOrderChanged
    );
    std::fs::write(
        root.join(".norn/schema.yaml"),
        "version: 1\nfields:\n  rank: { type: text }\n",
    )
    .unwrap();
    let reload_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match host.reload(vault.name()) {
            Ok(_) => break,
            Err(norn_host::ReloadRefusal::Unavailable(_)) => {
                assert!(Instant::now() < reload_deadline);
                std::thread::yield_now();
            }
            Err(error) => panic!("schema reload failed: {error:?}"),
        }
    }
    wait_ready(&host, vault.name());
    let mut changed_schema = params.clone();
    changed_schema.after = Some(cursor);
    assert_eq!(
        host.find(&changed_schema).unwrap_err().code(),
        &norn_wire::ReasonCode::VaultCursorOrderChanged
    );
    let reader = probe.retained.lock().unwrap().take().unwrap();
    assert!(matches!(
        reader.find(&params, Instant::now() + Duration::from_secs(5)),
        Err(LiveFindRefusal::Unavailable)
    ));
    drop(demand);
    drop(host);
    assert_no_projection_files(scratch.root());
    vault.store().verify_integrity().unwrap();
}

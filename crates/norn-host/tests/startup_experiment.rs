//! Early query checkpoints must carry real answers before rich derivation.
#![cfg(feature = "induced-failure")]
#![allow(clippy::disallowed_methods)] // Isolated fixture arrangement.

mod attach;
#[path = "performance/staged.rs"]
mod staged;

static REPORTED: Mutex<Vec<serde_json::Value>> = Mutex::new(Vec::new());

fn emit(value: serde_json::Value) {
    REPORTED.lock().unwrap().push(value);
}

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use norn_host::startup_experiment::{Experiment, Observer, Stage};
use norn_store::{ContentModel, ReadNeed, SnapshotReader, startup_experiment::Catalogue};
use norn_wire::{Column, FindParams, Predicate, VaultAddress, VaultName};

#[derive(Default)]
struct Probe {
    stages: Mutex<Vec<Stage>>,
}

struct Interrupt;

struct Quiet;

impl Observer for Quiet {
    fn catalogue(&self, _: &Catalogue, _: &ContentModel) -> Result<(), String> {
        Ok(())
    }
    fn indexed(
        &self,
        _: Stage,
        _: Arc<SnapshotReader>,
        _: Arc<ContentModel>,
    ) -> Result<(), String> {
        Ok(())
    }
}

impl Observer for Interrupt {
    fn catalogue(&self, _catalogue: &Catalogue, _model: &ContentModel) -> Result<(), String> {
        Ok(())
    }

    fn indexed(
        &self,
        _stage: Stage,
        _reader: Arc<SnapshotReader>,
        _model: Arc<ContentModel>,
    ) -> Result<(), String> {
        Err("stop the experiment after the first document".into())
    }
}

#[test]
fn an_interrupted_partial_store_is_not_adopted_as_a_warm_index() {
    let scratch = norn_testkit::scratch::Scratch::new("norn-startup-interrupted");
    let root = scratch.join("vault");
    std::fs::create_dir_all(root.join(".norn")).unwrap();
    std::fs::write(root.join(".norn/schema.yaml"), "version: 1\n").unwrap();
    std::fs::write(root.join("first.md"), "[[later]]\n").unwrap();
    std::fs::write(root.join("later.md"), "# Later\n").unwrap();
    let vault = attach::Vault::adopt(scratch.root());
    let host = vault.host_with_startup_experiment(
        norn_host::ProductionPolicy::new(128, 128).unwrap(),
        Experiment::new("first", Arc::new(Interrupt)),
    );
    let demand = host.demand(vault.name()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while host.state(vault.name()).is_ok() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    drop(demand);
    drop(host);
    let host = vault.host();
    let _demand = host.demand(vault.name()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match host.state(vault.name()) {
            Err(_) => break,
            Ok(norn_wire::TrustState::Ready) => {
                panic!("a partial store was adopted as a complete index")
            }
            Ok(_) => {}
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
}

impl Observer for Probe {
    fn catalogue(&self, catalogue: &Catalogue, model: &ContentModel) -> Result<(), String> {
        let candidates = catalogue
            .candidates("first", model.ambiguity_ignore())
            .unwrap()
            .unwrap();
        assert_eq!(candidates.paths, ["first.md"]);
        Ok(())
    }

    fn indexed(
        &self,
        stage: Stage,
        reader: Arc<SnapshotReader>,
        model: Arc<ContentModel>,
    ) -> Result<(), String> {
        let turn = reader
            .acquire(ReadNeed::Source, Instant::now() + Duration::from_secs(5))
            .unwrap()
            .unwrap();
        let snapshot = turn.establish().unwrap();
        let get = norn_wire::GetParams::new(
            VaultAddress::name(VaultName::new("notes").unwrap()),
            norn_wire::ResolutionTarget::new("first").unwrap(),
        )
        .with_columns([
            Column::path(),
            Column::body(),
            Column::fields(),
            Column::links(),
        ]);
        let (unsatisfied, report) = norn_host::startup_experiment::get(&snapshot, &get, &model)
            .unwrap()
            .into_report();
        assert!(unsatisfied.is_empty());
        let norn_wire::GetReport::Record { document, .. } = report else {
            panic!("record expected")
        };
        assert_eq!(document.path.as_str(), "first.md");
        assert_eq!(document.body.unwrap().text(), "[[later]] #first\n");
        let expected_links = u64::from(matches!(
            stage,
            Stage::RichFacts | Stage::AllFacts | Stage::LinkHealth
        ));
        assert_eq!(document.links.unwrap().total, expected_links);
        if matches!(stage, Stage::Frontmatter | Stage::AllFacts) {
            let params = FindParams::new(VaultAddress::name(VaultName::new("notes").unwrap()))
                .with_predicates([Predicate::equal_to("status", "active")])
                .with_columns([Column::path()]);
            let result = snapshot.find(&params, &model).unwrap();
            let (unsatisfied, _, report) = result.into_report();
            assert!(unsatisfied.is_empty());
            let encoded = serde_json::to_value(report).unwrap();
            assert!(encoded.to_string().contains("later.md"));
            assert!(!encoded.to_string().contains("first.md"));
        }
        self.stages.lock().unwrap().push(stage);
        Ok(())
    }
}

#[test]
fn staged_queries_precede_the_same_final_healthy_index() {
    check_staged_queries(false);
}

#[test]
fn one_facts_pass_exposes_complete_queries_before_health() {
    check_staged_queries(true);
}

fn check_staged_queries(once: bool) {
    let scratch = norn_testkit::scratch::Scratch::new("norn-startup-stages");
    let root = scratch.join("vault");
    std::fs::create_dir_all(root.join(".norn")).unwrap();
    std::fs::write(root.join(".norn/schema.yaml"), "version: 1\n").unwrap();
    std::fs::write(
        root.join("first.md"),
        "---\nstatus: draft\n---\n[[later]] #first\n",
    )
    .unwrap();
    std::fs::write(
        root.join("later.md"),
        "---\nstatus: active\n---\n# Target\n",
    )
    .unwrap();
    let vault = attach::Vault::adopt(scratch.root());
    let probe = Arc::new(Probe::default());
    let host = vault.host_with_startup_experiment(
        norn_host::ProductionPolicy::new(128, 128).unwrap(),
        if once {
            Experiment::facts_once("first", probe.clone())
        } else {
            Experiment::new("first", probe.clone())
        },
    );
    let _demand = host.demand(vault.name()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while host.state(vault.name()).unwrap() != norn_wire::TrustState::Ready {
        assert!(Instant::now() < deadline, "experiment did not become ready");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        *probe.stages.lock().unwrap(),
        if once {
            vec![Stage::RequestedDocument, Stage::AllFacts, Stage::LinkHealth]
        } else {
            vec![
                Stage::RequestedDocument,
                Stage::Frontmatter,
                Stage::RichFacts,
                Stage::LinkHealth,
            ]
        }
    );
    drop(_demand);
    drop(host);
    let mut store = vault.store();
    assert!(
        store
            .begin_request()
            .stored_findings(&norn_store::DocumentPath::new("first.md").unwrap())
            .unwrap()
            .is_empty()
    );
    store.verify_integrity().unwrap();
}

#[cfg(unix)]
#[test]
fn staged_final_index_and_advisories_match_on_malformed_and_linked_documents() {
    use std::os::unix::ffi::OsStringExt;
    let scratch = norn_testkit::scratch::Scratch::new("norn-startup-edge-corpus");
    let root = scratch.join("vault");
    for folder in [".norn", "one", "two"] {
        std::fs::create_dir_all(root.join(folder)).unwrap();
    }
    std::fs::write(root.join(".norn/schema.yaml"), "version: 1\nfields:\n  kind: { type: text }\n  rating: { type: number }\n  related: { type: link }\ntags:\n  declared: [known]\n  undeclared: report\nrules:\n  task:\n    match: { frontmatter: { kind: task } }\n    required: { owner: }\n").unwrap();
    for (path, body) in [
        (
            "first.md",
            "---\nkind: task\nrating: nope\ntags: [known, other]\nrelated: '[[target]]'\n---\n[[target#Present]] [[missing]] [[dupe]] [[target#Absent]] [[vault://target]]\n#known #other\n`[[not-a-link]] #not-a-tag`\n",
        ),
        ("target.md", "# Present\nTarget body.\n"),
        ("one/dupe.md", "One\n"),
        ("two/dupe.md", "Two\n"),
        ("malformed.md", "---\nvalue: [\n---\nBody\n"),
        ("unclosed.md", "---\nvalue: unfinished\n"),
        ("bad\nname.md", "Refused path\n"),
    ] {
        std::fs::write(root.join(path), body).unwrap();
    }
    std::fs::write(root.join("bad-bytes.md"), [0xff, 0xfe]).unwrap();
    std::fs::write(
        root.join(std::ffi::OsString::from_vec(b"bad-\xff.md".to_vec())),
        "Refused name\n",
    )
    .unwrap();
    std::os::unix::fs::symlink("target.md", root.join("alias.md")).unwrap();
    let vault = attach::Vault::adopt(scratch.root());
    let mut results = Vec::new();
    for mode in ["ordinary", "staged", "facts-once"] {
        let machine = vault.beside(&scratch.join(mode));
        let host = if mode != "ordinary" {
            machine.host_with_startup_experiment(
                norn_host::ProductionPolicy::new(2, 2).unwrap(),
                if mode == "facts-once" {
                    Experiment::facts_once("first", Arc::new(Quiet))
                } else {
                    Experiment::new("first", Arc::new(Quiet))
                },
            )
        } else {
            machine.host_with_policy(norn_host::ProductionPolicy::new(2, 2).unwrap())
        };
        let demand = host.demand(machine.name()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while host.state(machine.name()).unwrap() != norn_wire::TrustState::Ready
            || !host.work_in_flight(machine.name()).unwrap().is_quiet()
        {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        let advisories = host.inspect(machine.name()).unwrap().advisories;
        drop(demand);
        drop(host);
        machine.store().verify_integrity().unwrap();
        let audit =
            norn_store::startup_audit::audit(&machine.database(), |_, _, _| Ok(())).unwrap();
        results.push((audit, advisories));
    }
    for result in &results[1..] {
        assert_eq!(results[0].0["semantic_sha256"], result.0["semantic_sha256"]);
        assert_eq!(results[0].0["counts"], result.0["counts"]);
        assert_eq!(results[0].1, result.1);
    }
    assert_eq!(
        results[0].1.len(),
        1,
        "one symlink advisory, not one per pass"
    );
}

#[test]
fn measurement_reporter_runs_every_query_including_the_section_shape() {
    let scratch = norn_testkit::scratch::Scratch::new("norn-startup-reporter");
    let root = scratch.join("vault");
    std::fs::create_dir_all(root.join(".norn")).unwrap();
    std::fs::create_dir_all(root.join("bench")).unwrap();
    std::fs::write(root.join(".norn/schema.yaml"), "version: 1\n").unwrap();
    std::fs::write(
        root.join("bench/small.md"),
        "---\nstatus: draft\nperf_bucket: probe\n---\nprofileraresignal profilecommonword\n",
    )
    .unwrap();
    std::fs::write(root.join("bench/large.md"), "---\nstatus: draft\nperf_bucket: active\n---\nprofilecommonword\n## Details\nSection body.\n").unwrap();
    let vault = attach::Vault::adopt(scratch.root());
    REPORTED.lock().unwrap().clear();
    let reporter = staged::Reporter::new(Instant::now(), vault.name().clone(), false);
    let host = vault.host_with_startup_experiment(
        norn_host::ProductionPolicy::new(2, 2).unwrap(),
        Experiment::new("small", Arc::new(reporter)),
    );
    let _demand = host.demand(vault.name()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while host.state(vault.name()).unwrap() != norn_wire::TrustState::Ready {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let reports = REPORTED.lock().unwrap();
    assert_eq!(reports.len(), 12);
    for report in reports.iter().filter(|row| row.get("answer").is_some()) {
        assert_eq!(report["answer"]["unsatisfied"], serde_json::json!([]));
    }
    let section = reports
        .iter()
        .find(|row| row["probe"] == "section")
        .unwrap();
    assert_eq!(
        section["answer"]["report"]["body"]["text"],
        "Section body.\n"
    );
    let base_boundaries: Vec<_> = reports
        .iter()
        .filter(|row| {
            matches!(
                row["startup_stage"].as_str(),
                Some("frontmatter" | "lexical")
            )
        })
        .map(|row| row["stage_boundary_ns"].as_u64().unwrap())
        .collect();
    assert!(
        base_boundaries
            .iter()
            .all(|value| *value == base_boundaries[0])
    );
}

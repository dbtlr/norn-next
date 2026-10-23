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

struct Interrupt {
    unwind: bool,
}

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
        assert!(
            !self.unwind,
            "unwind the experiment after its first document"
        );
        Err("stop the experiment after the first document".into())
    }
}

#[test]
fn an_interrupted_partial_store_is_not_adopted_as_a_warm_index() {
    check_interruption(false, false);
}

#[test]
fn failed_cache_import_restores_the_writer_and_refuses_partial_restart() {
    check_interruption(true, false);
}

#[test]
fn unwound_cache_import_restores_or_closes_before_any_warm_attachment() {
    check_interruption(true, true);
}

fn check_interruption(cache_enabled: bool, unwind: bool) {
    let scratch = norn_testkit::scratch::Scratch::under(
        std::path::Path::new(env!("CARGO_TARGET_TMPDIR")),
        "norn-startup-interrupted",
    )
    .unwrap();
    let root = scratch.join("vault");
    std::fs::create_dir_all(root.join(".norn")).unwrap();
    std::fs::write(root.join(".norn/schema.yaml"), "version: 1\n").unwrap();
    std::fs::write(root.join("first.md"), "[[later]]\n").unwrap();
    std::fs::write(root.join("later.md"), "# Later\n").unwrap();
    let vault = attach::Vault::adopt(scratch.root());
    let cache = cache_enabled
        .then(|| norn_host::startup_experiment::InitialImportCache::new(8192, true).unwrap());
    let host = vault.host_with_loading_options(
        norn_host::ProductionPolicy::new(128, 128).unwrap(),
        Some(Experiment::new("first", Arc::new(Interrupt { unwind }))),
        cache.clone(),
    );
    let demand = host.demand(vault.name()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while host.state(vault.name()).is_ok() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    if let Some(cache) = cache {
        let receipt = cache
            .receipt()
            .expect("the failed or unwound writer was restored");
        assert!(!receipt.import_succeeded);
        assert_eq!(receipt.writer.during, Some(-8192));
        assert_eq!(receipt.writer.after, Some(receipt.writer.before));
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
        ("collision\\name.md", "First refused spelling\n"),
        ("collision\u{7}name.md", "Second refused spelling\n"),
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
    for (mode, cached) in [
        ("ordinary", false),
        ("staged", false),
        ("facts-once", false),
        ("ordinary", true),
        ("staged", true),
        ("facts-once", true),
    ] {
        let machine = vault.beside(&scratch.join(format!("{mode}-{cached}")));
        let experiment = match mode {
            "ordinary" => None,
            "facts-once" => Some(Experiment::facts_once("first", Arc::new(Quiet))),
            _ => Some(Experiment::new("first", Arc::new(Quiet))),
        };
        let cache = cached
            .then(|| norn_host::startup_experiment::InitialImportCache::new(8192, true).unwrap());
        let host = machine.host_with_loading_options(
            norn_host::ProductionPolicy::new(128, 128).unwrap(),
            experiment,
            cache.clone(),
        );
        let demand = host.demand(machine.name()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while host.state(machine.name()).unwrap() != norn_wire::TrustState::Ready
            || !host.work_in_flight(machine.name()).unwrap().is_quiet()
        {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        let advisories = host.inspect(machine.name()).unwrap().advisories;
        if let Some(cache) = cache {
            let receipt = cache.receipt().unwrap();
            assert!(receipt.import_succeeded);
            assert_eq!(receipt.writer.during, Some(-8192));
            assert_eq!(receipt.writer.after, Some(receipt.writer.before));
        }
        let address = VaultAddress::name(machine.name().clone());
        let get = norn_wire::GetParams::new(
            address.clone(),
            norn_wire::ResolutionTarget::new("first").unwrap(),
        )
        .with_columns([
            Column::path(),
            Column::body(),
            Column::fields(),
            Column::links(),
        ]);
        let find = FindParams::new(address).with_predicates([Predicate::equal_to("kind", "task")]);
        let mut answers = serde_json::json!({"get":host.get(&get).unwrap().answer,"find":host.find(&find).unwrap().answer});
        // Independent stores have independent epochs; loader passes also spend generations.
        for answer in answers.as_object_mut().unwrap().values_mut() {
            assert_eq!(answer["reading"]["trust"]["state"], "ready");
            answer.as_object_mut().unwrap().remove("reading");
        }
        drop(demand);
        drop(host);
        machine.store().verify_integrity().unwrap();
        let audit =
            norn_store::startup_audit::audit(&machine.database(), |_, _, _| Ok(())).unwrap();
        results.push((audit, advisories, answers));
    }
    for result in &results[1..] {
        assert_eq!(results[0].0["semantic_sha256"], result.0["semantic_sha256"]);
        assert_eq!(results[0].0["counts"], result.0["counts"]);
        assert_eq!(results[0].1, result.1);
        assert_eq!(results[0].2, result.2);
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
        norn_host::ProductionPolicy::new(1, 1).unwrap(),
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

#[test]
fn ordinary_initial_cache_is_restored_before_ready_and_skipped_on_nonempty_restart() {
    use norn_host::startup_experiment::InitialImportCache;
    let scratch = norn_testkit::scratch::Scratch::under(
        std::path::Path::new(env!("CARGO_TARGET_TMPDIR")),
        "cache-ordinary",
    )
    .unwrap();
    let root = scratch.join("vault");
    std::fs::create_dir_all(root.join(".norn")).unwrap();
    std::fs::write(root.join(".norn/schema.yaml"), "version: 1\n").unwrap();
    for i in 0..260 {
        std::fs::write(
            root.join(format!("note-{i:03}.md")),
            "---\nstatus: active\n---\n[[missing]]\n",
        )
        .unwrap();
    }
    let vault = attach::Vault::adopt(scratch.root());
    let policy = norn_host::ProductionPolicy::new(128, 128).unwrap();
    let cache = InitialImportCache::new(8192, true).unwrap();
    let host = vault.host_with_loading_options(policy, None, Some(cache.clone()));
    let demand = host.demand(vault.name()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while host.state(vault.name()).unwrap() != norn_wire::TrustState::Ready {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let receipt = cache.receipt().expect("restoration precedes Ready");
    assert!(receipt.import_succeeded);
    assert_eq!(receipt.writer.during, Some(-8192));
    assert_eq!(receipt.writer.after, Some(receipt.writer.before));
    assert!(receipt.writer.retained_cache_budget_kib.unwrap() >= 8192);
    assert!(!receipt.writer.reclamation_verified);
    let params = FindParams::new(VaultAddress::name(vault.name().clone()))
        .with_predicates([Predicate::equal_to("status", "active")]);
    let first_answer = serde_json::to_value(host.find(&params).unwrap().answer).unwrap();
    drop(demand);
    drop(host);
    let mut store = vault.store();
    store.verify_integrity().unwrap();
    let before = store.begin_request().write_generation().unwrap();
    let epoch = store.epoch().to_string();
    drop(store);
    let cache = InitialImportCache::new(8192, true).unwrap();
    let host = vault.host_with_loading_options(policy, None, Some(cache.clone()));
    let demand = host.demand(vault.name()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while host.state(vault.name()).unwrap() != norn_wire::TrustState::Ready {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        cache.receipt().is_none(),
        "a reused store must not enter the cache window"
    );
    assert_eq!(
        serde_json::to_value(host.find(&params).unwrap().answer).unwrap(),
        first_answer
    );
    drop(demand);
    drop(host);
    let mut store = vault.store();
    assert_eq!(store.epoch(), epoch);
    assert_eq!(store.begin_request().write_generation().unwrap(), before);
    store.verify_integrity().unwrap();
}

#[path = "performance/cache.rs"]
mod cache_options;

#[test]
fn worker_cache_parameter_is_explicit_and_rejects_invalid_values() {
    use std::ffi::OsStr;
    let options = cache_options::CacheOptions::parse(None, None).unwrap();
    assert!(options.cache.is_none());
    assert!(!options.diagnostics);
    for value in ["", "0", "-1", "8193", "4294967296", "eight", " 8192"] {
        assert!(
            cache_options::CacheOptions::parse(Some(OsStr::new(value)), None).is_err(),
            "{value:?}"
        );
    }
    assert!(cache_options::CacheOptions::parse(None, Some(OsStr::new("1"))).is_err());
    assert!(
        cache_options::CacheOptions::parse(Some(OsStr::new("8192")), Some(OsStr::new("yes")))
            .is_err()
    );
    let options =
        cache_options::CacheOptions::parse(Some(OsStr::new("8192")), Some(OsStr::new("1")))
            .unwrap();
    assert!(options.cache.is_some());
    assert!(options.diagnostics);
    assert_eq!(options.kib, Some(8192));
    assert!(options.receipt().is_null());
}

#[test]
fn diagnostic_receipts_cannot_carry_accepted_worker_clocks() {
    let mut row = serde_json::json!({"ready":true,"attach_ns":1,"startup_complete_ns":2,
        "query_host_ns":3,"timings":{"entries":[{"elapsed_ns":4}]},"writer_cache":{"after":37}});
    cache_options::suppress_clocks(&mut row);
    assert_eq!(
        row,
        serde_json::json!({"ready":true,"diagnostic_only":true,"writer_cache":{"after":37}})
    );
}

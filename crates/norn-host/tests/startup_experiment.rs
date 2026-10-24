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
    for mode in ["layered", "canonical-demand", "canonical-intake"] {
        check_measurement_reporter(mode);
    }
    #[cfg(target_os = "linux")]
    {
        for cell in ["N", "G", "E", "Q", "M"] {
            check_residency_reporter(cell);
        }
        check_operator_prefix();
        check_operator_prefix_refused_ack();
        check_residency_ack_failures();
        check_residency_observer_error_cleanup();
    }
}

#[cfg(target_os = "linux")]
struct HandleWitness {
    observer: Arc<staged::residency::Diagnostic>,
    reader: Mutex<std::sync::Weak<SnapshotReader>>,
    intake: Mutex<std::sync::Weak<norn_host::startup_experiment::IntakeReader>>,
}

#[cfg(target_os = "linux")]
impl Observer for HandleWitness {
    fn catalogue(&self, catalogue: &Catalogue, model: &ContentModel) -> Result<(), String> {
        self.observer.catalogue(catalogue, model)
    }

    fn canonical_work(
        &self,
        work: &std::collections::BTreeMap<&'static str, u64>,
    ) -> Result<(), String> {
        self.observer.canonical_work(work)
    }

    fn indexed(
        &self,
        stage: Stage,
        reader: Arc<SnapshotReader>,
        model: Arc<ContentModel>,
    ) -> Result<(), String> {
        *self.reader.lock().unwrap() = Arc::downgrade(&reader);
        self.observer.indexed(stage, reader, model)
    }

    fn intake(
        &self,
        reader: Arc<norn_host::startup_experiment::IntakeReader>,
    ) -> Result<(), String> {
        *self.intake.lock().unwrap() = Arc::downgrade(&reader);
        self.observer.intake(reader)
    }
}

#[cfg(target_os = "linux")]
fn check_residency_reporter(cell: &str) {
    use staged::residency::{Cell, Diagnostic, thread_tid};
    let scratch = norn_testkit::scratch::Scratch::new("norn-residency-reporter");
    let root = scratch.join("vault");
    std::fs::create_dir_all(root.join(".norn")).unwrap();
    std::fs::create_dir_all(root.join("bench")).unwrap();
    std::fs::write(root.join(".norn/schema.yaml"), "version: 1\n").unwrap();
    std::fs::write(
        root.join("bench/small.md"),
        "---\nperf_bucket: probe\n---\nprofileraresignal profilecommonword [[missing]]\n",
    )
    .unwrap();
    std::fs::write(
        root.join("bench/large.md"),
        "---\nperf_bucket: active\n---\nprofilecommonword\n## Details\nSection body.\n",
    )
    .unwrap();
    let vault = attach::Vault::adopt(scratch.root());
    REPORTED.lock().unwrap().clear();
    let main = thread_tid();
    let (observer, mut dispatcher) = Diagnostic::new(
        Instant::now(),
        vault.name().clone(),
        Cell::parse(cell).unwrap(),
        Duration::from_secs(5),
    );
    let witness = Arc::new(HandleWitness {
        observer,
        reader: Mutex::new(std::sync::Weak::new()),
        intake: Mutex::new(std::sync::Weak::new()),
    });
    let host = vault.host_with_startup_experiment(
        norn_host::ProductionPolicy::new(128, 128).unwrap(),
        Experiment::canonical_intake("small", witness.clone()),
    );
    let demand = host.demand(vault.name()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while host.state(vault.name()).unwrap() != norn_wire::TrustState::Ready {
        dispatcher
            .pump(|event| {
                // Observe the real outer handles before acknowledgement permits
                // construction to continue. Weak counts add no reader owner.
                let reader = witness.reader.lock().unwrap().strong_count();
                let intake = witness.intake.lock().unwrap().strong_count();
                match event["residency_checkpoint"].as_str().unwrap() {
                    "before-early-get" | "intake-retired-before-final" => assert_eq!(reader, 3),
                    "after-early-get" | "after-final" => assert_eq!(
                        reader, 2,
                        "observer retained SnapshotReader at acknowledgement"
                    ),
                    "before-metadata" => assert_eq!(intake, 2),
                    "after-metadata" => assert_eq!(
                        intake, 1,
                        "observer retained IntakeReader at acknowledgement"
                    ),
                    other => panic!("unexpected handle checkpoint {other}"),
                }
                if event["residency_checkpoint"] == "intake-retired-before-final" {
                    assert_eq!(intake, 0, "intake handle survived retirement");
                }
                REPORTED.lock().unwrap().push(event.clone());
                Ok(())
            })
            .unwrap();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let reports = REPORTED.lock().unwrap();
    let checkpoints: Vec<_> = reports
        .iter()
        .filter(|r| r.get("residency_checkpoint").is_some())
        .collect();
    assert_eq!(
        checkpoints
            .iter()
            .map(|r| r["residency_checkpoint"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "before-early-get",
            "after-early-get",
            "before-metadata",
            "after-metadata",
            "intake-retired-before-final",
            "after-final"
        ]
    );
    let attachment = checkpoints[0]["attachment_tid"].as_u64().unwrap();
    assert_ne!(attachment, u64::from(main));
    for (i, checkpoint) in checkpoints.iter().enumerate() {
        assert_eq!(
            checkpoint["construction_eligibility_gets"], 1,
            "N still performs the constant eligibility Get"
        );
        assert_eq!(checkpoint["sequence"], i + 1);
        assert_eq!(checkpoint["worker_tid"], main);
        let position = reports
            .iter()
            .position(|r| std::ptr::eq(r, *checkpoint))
            .unwrap();
        assert_eq!(
            reports[position + 1]["residency_acknowledged"],
            i + 1,
            "no query or continuation precedes sampling acknowledgement"
        );
    }
    let queries: Vec<_> = reports
        .iter()
        .filter(|r| r.get("startup_stage").is_some())
        .collect();
    let mut expected = vec![("catalogue", "names")];
    if cell != "N" {
        expected.push(("canonical-demand", "whole_slug"));
    }
    let metadata = [
        "active_page",
        "document_count",
        "active_count",
        "sparse_page",
        "zero_page",
        "unknown_field",
        "field_page",
    ];
    if matches!(cell, "E" | "Q" | "M") {
        expected.extend(metadata.map(|p| ("canonical-intake", p)));
    }
    if cell == "Q" {
        expected.push(("canonical-health", "whole_slug"));
        expected.extend(metadata.map(|p| ("canonical-final-intake", p)));
        expected.extend([
            ("frontmatter", "active_page"),
            ("frontmatter", "document_count"),
            ("frontmatter", "active_count"),
            ("lexical", "common"),
            ("lexical", "rare"),
            ("lexical", "missing"),
            ("lexical", "filtered"),
            ("lexical", "coverage"),
            ("rich", "section"),
            ("health", "summary"),
        ]);
    }
    assert_eq!(
        queries
            .iter()
            .map(|r| (
                r["startup_stage"].as_str().unwrap(),
                r["probe"].as_str().unwrap()
            ))
            .collect::<Vec<_>>(),
        expected
    );
    for query in queries {
        assert_eq!(
            query["thread_tid"],
            if cell == "M" && query["startup_stage"] != "catalogue" {
                u64::from(main)
            } else {
                attachment
            }
        );
        if query["startup_stage"] == "canonical-demand" {
            assert_eq!(
                query["answer"]["report"]["document"]["findings"]["total"],
                1
            );
            assert_eq!(query["answer"]["report"]["document"]["links"]["total"], 1);
        }
    }
    drop(reports);
    drop(demand);
    drop(host);
    no_projection_files(scratch.root());
}

#[cfg(target_os = "linux")]
fn check_residency_ack_failures() {
    use staged::residency::sample_ack;
    let scratch = norn_testkit::scratch::Scratch::new("norn-residency-ack");
    std::fs::create_dir(scratch.join("residency-acks")).unwrap();
    let event = serde_json::json!({"sequence":1,"residency_checkpoint":"before-early-get"});
    assert!(
        sample_ack(scratch.root(), &event, Duration::ZERO)
            .unwrap_err()
            .contains("timed out")
    );
    for ack in [
        "{",
        r#"{"sequence":true,"checkpoint":"before-early-get"}"#,
        r#"{"sequence":2,"checkpoint":"before-early-get"}"#,
        r#"{"sequence":1,"checkpoint":"after-early-get"}"#,
    ] {
        std::fs::write(scratch.join("residency-acks/1.json"), ack).unwrap();
        assert!(
            sample_ack(scratch.root(), &event, Duration::ZERO)
                .unwrap_err()
                .contains("invalid residency")
        );
    }
    std::fs::write(
        scratch.join("residency-acks/1.json"),
        r#"{"sequence":1,"checkpoint":"before-early-get"}"#,
    )
    .unwrap();
    sample_ack(scratch.root(), &event, Duration::ZERO).unwrap();
    assert!(!scratch.join("residency-acks/1.json").exists());
}

#[cfg(target_os = "linux")]
fn check_residency_observer_error_cleanup() {
    use staged::residency::{Cell, Diagnostic};
    let scratch = norn_testkit::scratch::Scratch::new("norn-residency-error-cleanup");
    let root = scratch.join("vault");
    std::fs::create_dir_all(root.join(".norn")).unwrap();
    std::fs::create_dir(root.join("bench")).unwrap();
    std::fs::write(root.join(".norn/schema.yaml"), "version: 1\n").unwrap();
    std::fs::write(
        root.join("bench/small.md"),
        "---\nperf_bucket: probe\n---\nBody\n",
    )
    .unwrap();
    let vault = attach::Vault::adopt(scratch.root());
    let (observer, mut dispatcher) = Diagnostic::new(
        Instant::now(),
        vault.name().clone(),
        Cell::M,
        Duration::from_secs(2),
    );
    let host = vault.host_with_startup_experiment(
        norn_host::ProductionPolicy::new(128, 128).unwrap(),
        Experiment::canonical_intake("small", observer),
    );
    let demand = host.demand(vault.name()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let result = dispatcher.pump(|event| {
            if event["residency_checkpoint"] == "after-metadata" {
                Err("invalid external ack".into())
            } else {
                Ok(())
            }
        });
        if let Err(error) = result {
            assert_eq!(error, "invalid external ack");
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    drop(dispatcher);
    drop(demand);
    drop(host);
    fn check_files(path: &std::path::Path) {
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                check_files(&path);
            } else {
                assert!(
                    !path
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with("intake-")
                );
            }
        }
    }
    check_files(scratch.root());
}

#[test]
#[cfg(target_os = "linux")]
fn residency_rendezvous_drops_work_before_ack_and_propagates_errors() {
    use staged::residency::{Cell, Diagnostic};
    for cell in [Cell::N, Cell::G, Cell::E, Cell::Q, Cell::M] {
        let (observer, mut dispatcher) = Diagnostic::new(
            Instant::now(),
            VaultName::new("notes").unwrap(),
            cell,
            Duration::from_secs(2),
        );
        let captured = Arc::new(());
        let weak = Arc::downgrade(&captured);
        let main = std::thread::current().id();
        let (thread_tx, thread_rx) = std::sync::mpsc::channel();
        let join = std::thread::spawn(move || {
            observer.execute(Box::new(move || {
                thread_tx.send(std::thread::current().id()).unwrap();
                drop(captured);
                Err("query refused".into())
            }))
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while !join.is_finished() {
            let _ = dispatcher.pump(|_| Ok(()));
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert_eq!(join.join().unwrap(), Err("query refused".into()));
        assert!(
            weak.upgrade().is_none(),
            "dispatcher retained a work handle"
        );
        assert_eq!(thread_rx.recv().unwrap() == main, cell == Cell::M);
    }
}

#[test]
#[cfg(target_os = "linux")]
fn residency_disconnect_and_timeout_release_queued_handles_without_running_queries() {
    use staged::residency::{Cell, Diagnostic};
    for disconnected in [true, false] {
        let (observer, dispatcher) = Diagnostic::new(
            Instant::now(),
            VaultName::new("notes").unwrap(),
            Cell::M,
            Duration::from_millis(10),
        );
        let mut dispatcher = Some(dispatcher);
        if disconnected {
            dispatcher.take();
        }
        let captured = Arc::new(());
        let weak = Arc::downgrade(&captured);
        let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let call = called.clone();
        let worker = std::thread::spawn(move || {
            observer.execute(Box::new(move || {
                call.store(true, std::sync::atomic::Ordering::Relaxed);
                drop(captured);
                Ok(())
            }))
        });
        assert!(worker.join().unwrap().is_err());
        if let Some(dispatcher) = &mut dispatcher {
            assert!(
                dispatcher
                    .pump(|_| Ok(()))
                    .unwrap_err()
                    .contains("cancelled")
            );
        }
        drop(dispatcher);
        assert!(weak.upgrade().is_none());
        assert!(!called.load(std::sync::atomic::Ordering::Relaxed));
    }
}

fn check_measurement_reporter(mode: &str) {
    let canonical = mode.starts_with("canonical-");
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
        norn_host::ProductionPolicy::new(128, 128).unwrap(),
        if mode == "canonical-intake" {
            Experiment::canonical_intake("small", Arc::new(reporter))
        } else if canonical {
            Experiment::canonical_demand("small", Arc::new(reporter))
        } else {
            Experiment::new("small", Arc::new(reporter))
        },
    );
    let _demand = host.demand(vault.name()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while host.state(vault.name()).unwrap() != norn_wire::TrustState::Ready {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let reports = REPORTED.lock().unwrap();
    assert_eq!(
        reports.len(),
        if mode == "canonical-intake" {
            27
        } else if canonical {
            13
        } else {
            12
        }
    );
    if mode == "canonical-intake" {
        let early: Vec<_> = reports
            .iter()
            .filter(|r| r["startup_stage"] == "canonical-intake")
            .collect();
        let final_rows: Vec<_> = reports
            .iter()
            .filter(|r| r["startup_stage"] == "canonical-final-intake")
            .collect();
        assert_eq!(early.len(), 7);
        for (early, final_row) in early.iter().zip(final_rows) {
            assert_eq!(early["probe"], final_row["probe"]);
            let mut a = early["answer"].clone();
            let mut b = final_row["answer"].clone();
            if a["report"].get("counted_at").is_some() {
                assert_eq!(
                    a["report"]["counted_at"]["epoch"],
                    early["projection_epoch"]
                );
                assert_eq!(
                    b["report"]["counted_at"]["epoch"],
                    final_row["projection_epoch"]
                );
                a["report"].as_object_mut().unwrap().remove("counted_at");
                b["report"].as_object_mut().unwrap().remove("counted_at");
            }
            assert_eq!(a, b);
        }
    }
    if canonical {
        let early = reports
            .iter()
            .find(|row| row["startup_stage"] == "canonical-demand")
            .unwrap();
        let final_answer = reports
            .iter()
            .find(|row| row["startup_stage"] == "canonical-health")
            .unwrap();
        assert_eq!(early["answer"], final_answer["answer"]);
        assert_eq!(
            final_answer["canonical_work"].as_object().unwrap().len(),
            11
        );
        assert!(
            final_answer["canonical_work"]
                .as_object()
                .unwrap()
                .values()
                .any(|value| value.as_u64().unwrap() > 0)
        );
        assert_eq!(early["answer"]["report"]["document"]["links"]["total"], 0);
        assert_eq!(
            early["answer"]["report"]["document"]["findings"]["total"],
            0
        );
        assert_ne!(early["projection_epoch"], final_answer["projection_epoch"]);
        assert_eq!(early["simulation"], "paused");
    }
    for report in reports
        .iter()
        .filter(|row| row.get("answer").is_some() && row["probe"] != "unknown_field")
    {
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

#[cfg(target_os = "linux")]
fn check_operator_prefix() {
    let scratch = norn_testkit::scratch::Scratch::new("norn-operator-prefix");
    let root = scratch.join("vault");
    std::fs::create_dir_all(root.join(".norn")).unwrap();
    std::fs::create_dir(root.join("bench")).unwrap();
    std::fs::create_dir(scratch.join("operator-prefix-acks")).unwrap();
    std::fs::write(root.join(".norn/schema.yaml"), "version: 1\n").unwrap();
    let small = "---\nstatus: draft\nperf_bucket: probe\n---\n# Small\nBody #known ^block\n[[large#Details]] [[missing]] [[large#Absent]]\n";
    std::fs::write(root.join("bench/small.md"), small).unwrap();
    std::fs::write(
        root.join("bench/large.md"),
        "---\nperf_bucket: active\n---\n## Details\nBody\n",
    )
    .unwrap();
    let vault = attach::Vault::adopt(scratch.root());
    REPORTED.lock().unwrap().clear();
    let observer = Arc::new(staged::OperatorPrefix::new(
        Instant::now(),
        vault.name().clone(),
        scratch.root().to_path_buf(),
    ));
    let witness = Arc::new(PrefixWitness {
        observer: observer.clone(),
        reader: Mutex::new(std::sync::Weak::new()),
        intake: Mutex::new(std::sync::Weak::new()),
    });
    let host = vault.host_with_startup_experiment(
        norn_host::ProductionPolicy::new(128, 128).unwrap(),
        Experiment::canonical_intake("small", witness.clone()),
    );
    let demand = host.demand(vault.name()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut acknowledged = 0;
    while host.state(vault.name()).unwrap() != norn_wire::TrustState::Ready {
        let reports = REPORTED.lock().unwrap().clone();
        for (index, row) in reports.iter().enumerate().skip(acknowledged) {
            assert_ne!(
                host.state(vault.name()).unwrap(),
                norn_wire::TrustState::Ready
            );
            assert_eq!(row["operator_prefix_sequence"], index + 1);
            assert_eq!(row["global_ready"], false);
            if index == 0 {
                // production reader + retire guard; the observer's Arc must be gone.
                settle(
                    || witness.reader.lock().unwrap().strong_count(),
                    2,
                    "SnapshotReader",
                );
            } else {
                // production Projection only; the observer's Arc must be gone.
                settle(
                    || witness.intake.lock().unwrap().strong_count(),
                    1,
                    "IntakeReader",
                );
            }
            let ack = serde_json::json!({"sequence":index+1,"probe":row["probe"]});
            std::fs::write(
                scratch.join(format!("operator-prefix-acks/{}.json", index + 1)),
                ack.to_string(),
            )
            .unwrap();
            acknowledged += 1;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let reports = REPORTED.lock().unwrap().clone();
    assert_eq!(reports.len(), 2, "only the operator Get and Find may run");
    assert_eq!(
        reports[0]["command"],
        serde_json::json!({"op":"get","target":"small","whole":true})
    );
    assert_eq!(reports[1]["command"], serde_json::json!({"op":"find"}));
    assert_eq!(reports[0]["startup_stage"], "canonical-demand");
    assert_eq!(reports[1]["startup_stage"], "canonical-intake");
    let doc = &reports[0]["answer"]["report"]["document"];
    for name in ["headings", "blocks", "tags", "links", "findings"] {
        assert!(doc[name]["total"].as_u64().unwrap() > 0, "{name}");
    }
    let address = VaultAddress::name(vault.name().clone());
    let get = norn_wire::GetParams::new(
        address.clone(),
        norn_wire::ResolutionTarget::new("small").unwrap(),
    );
    let find = FindParams::new(address)
        .with_limit(25)
        .with_predicates([Predicate::equal_to("perf_bucket", "active")]);
    for (early, mut ordinary) in reports.iter().zip([
        serde_json::to_value(host.get(&get).unwrap().answer).unwrap(),
        serde_json::to_value(host.find(&find).unwrap().answer).unwrap(),
    ]) {
        ordinary.as_object_mut().unwrap().remove("reading");
        let mut early = early["answer"].clone();
        for answer in [&mut early, &mut ordinary] {
            if let Some(items) = answer["report"]["document"]["findings"]["items"].as_array_mut() {
                for finding in items {
                    finding.as_object_mut().unwrap().remove("id");
                    finding.as_object_mut().unwrap().remove("generation");
                }
            }
        }
        assert_eq!(early, ordinary, "full prefix answer parity");
    }
    assert!(
        observer.receipt()["canonical_work"]
            .as_object()
            .unwrap()
            .contains_key("tombstones_recorded")
    );
    assert_eq!(
        witness.intake.lock().unwrap().strong_count(),
        0,
        "intake survived Ready"
    );
    assert_eq!(
        witness.reader.lock().unwrap().strong_count(),
        0,
        "demand reader survived Ready"
    );
    drop(demand);
    drop(host);
    no_projection_files(scratch.root());
    vault.store().verify_integrity().unwrap();
    let audit = norn_store::startup_audit::audit(&vault.database(), |_, _, _| Ok(())).unwrap();
    let ordinary = vault.beside(&scratch.join("ordinary"));
    let host = ordinary.host();
    let demand = host.demand(ordinary.name()).unwrap();
    while host.state(ordinary.name()).unwrap() != norn_wire::TrustState::Ready {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    drop(demand);
    drop(host);
    ordinary.store().verify_integrity().unwrap();
    let other = norn_store::startup_audit::audit(&ordinary.database(), |_, _, _| Ok(())).unwrap();
    assert_eq!(audit["semantic_sha256"], other["semantic_sha256"]);
    for key in ["canonical_identity_sha256", "page_map_sha256"] {
        assert_eq!(audit[key].as_str().unwrap().len(), 64);
        assert_eq!(audit[key], other[key]);
    }
    assert_eq!(
        std::fs::read_to_string(root.join("bench/small.md")).unwrap(),
        small
    );
    assert!(
        std::fs::read_dir(scratch.join("operator-prefix-acks"))
            .unwrap()
            .next()
            .is_none()
    );
}

#[cfg(target_os = "linux")]
fn check_operator_prefix_refused_ack() {
    let scratch = norn_testkit::scratch::Scratch::new("norn-operator-prefix-refused");
    std::fs::create_dir(scratch.join("operator-prefix-acks")).unwrap();
    assert!(
        staged::operator_ack(scratch.root(), 1, "whole_slug", Duration::ZERO)
            .unwrap_err()
            .contains("timed out")
    );
    for ack in [
        "{",
        r#"{"sequence":true,"probe":"whole_slug"}"#,
        r#"{"sequence":2,"probe":"whole_slug"}"#,
        r#"{"sequence":1,"probe":"active_page"}"#,
    ] {
        std::fs::write(scratch.join("operator-prefix-acks/1.json"), ack).unwrap();
        assert!(
            staged::operator_ack(scratch.root(), 1, "whole_slug", Duration::ZERO)
                .unwrap_err()
                .contains("invalid operator prefix")
        );
    }
    let root = scratch.join("vault");
    std::fs::create_dir_all(root.join(".norn")).unwrap();
    std::fs::write(root.join(".norn/schema.yaml"), "version: 1\n").unwrap();
    std::fs::write(root.join("small.md"), "# Small\nBody\n").unwrap();
    let vault = attach::Vault::adopt(scratch.root());
    let observer = Arc::new(staged::OperatorPrefix::new(
        Instant::now(),
        vault.name().clone(),
        scratch.root().to_path_buf(),
    ));
    let host = vault.host_with_startup_experiment(
        norn_host::ProductionPolicy::new(128, 128).unwrap(),
        Experiment::canonical_intake("small", observer),
    );
    let demand = host.demand(vault.name()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while host.state(vault.name()).is_ok() {
        assert!(
            Instant::now() < deadline,
            "invalid prefix acknowledgement did not fail closed"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    drop(demand);
    drop(host);
    no_projection_files(scratch.root());
}

#[cfg(target_os = "linux")]
struct PrefixWitness {
    observer: Arc<staged::OperatorPrefix>,
    reader: Mutex<std::sync::Weak<SnapshotReader>>,
    intake: Mutex<std::sync::Weak<norn_host::startup_experiment::IntakeReader>>,
}

#[cfg(target_os = "linux")]
impl Observer for PrefixWitness {
    fn catalogue(&self, catalogue: &Catalogue, model: &ContentModel) -> Result<(), String> {
        self.observer.catalogue(catalogue, model)
    }
    fn declined(&self, reason: &str) -> Result<(), String> {
        self.observer.declined(reason)
    }
    fn canonical_work(
        &self,
        work: &std::collections::BTreeMap<&'static str, u64>,
    ) -> Result<(), String> {
        self.observer.canonical_work(work)
    }
    fn indexed(
        &self,
        stage: Stage,
        reader: Arc<SnapshotReader>,
        model: Arc<ContentModel>,
    ) -> Result<(), String> {
        if stage == Stage::CanonicalDemand {
            *self.reader.lock().unwrap() = Arc::downgrade(&reader);
        }
        self.observer.indexed(stage, reader, model)
    }
    fn intake(
        &self,
        reader: Arc<norn_host::startup_experiment::IntakeReader>,
    ) -> Result<(), String> {
        *self.intake.lock().unwrap() = Arc::downgrade(&reader);
        self.observer.intake(reader)
    }
}

#[cfg(target_os = "linux")]
fn settle(count: impl Fn() -> usize, expected: usize, what: &str) {
    // The row is emitted before the observer drops; it must settle before its ack is written.
    let until = Instant::now() + Duration::from_secs(2);
    while count() != expected {
        assert!(
            Instant::now() < until,
            "{what} still {} before ack",
            count()
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[cfg(target_os = "linux")]
fn no_projection_files(path: &std::path::Path) {
    for entry in std::fs::read_dir(path).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            no_projection_files(&path);
        } else {
            let name = path.file_name().unwrap().to_string_lossy();
            assert!(
                !name.starts_with("demand-") && !name.starts_with("intake-"),
                "projection residue: {name}"
            );
        }
    }
}

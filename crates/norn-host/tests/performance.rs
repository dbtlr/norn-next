//! An opt-in, persistent host measurement worker for the external baseline runner.
//! Fixture generation and response serialization sit outside each operation's clock.
#![cfg(unix)]
#![allow(clippy::disallowed_methods)] // Measurement harness: isolated trees and JSONL readings.

mod attach;

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::time::Instant;

use norn_wire::{
    ApplyMode, AuthoredValue, DocumentPath, FieldChange, FindParams, GetParams, MoveParams,
    MoveSubject, NewParams, Predicate, ResolutionTarget, Rung, RungSelection, RungSet,
    SearchParams, SetParams, VaultAddress, WriteTarget,
};
use serde_json::{Value, json};

#[allow(clippy::disallowed_macros)] // Machine-consumed measurement protocol, not product rendering.
fn emit(value: Value) {
    println!("NORN_PERFORMANCE {value}");
    std::io::stdout().flush().expect("flush a measurement");
}

fn text<'a>(command: &'a Value, field: &str) -> &'a str {
    command[field].as_str().expect("a string command argument")
}

fn root() -> PathBuf {
    let root = PathBuf::from(std::env::var_os("NORN_PERFORMANCE_ROOT").expect("an isolated root"));
    assert!(root.is_absolute(), "the root must be absolute");
    let token = std::env::var("NORN_PERFORMANCE_TOKEN").expect("a runner-issued token");
    assert!(!token.is_empty(), "the token must not be empty");
    assert_eq!(
        std::fs::read_to_string(root.join("performance-token")).expect("an issued root"),
        token,
        "the root must belong to this runner"
    );
    root
}

fn performance_generate() {
    let root = root();
    let mut profile = norn_fixtures::Profile::by_name("realistic").expect("the realistic profile");
    profile.docs = std::env::var("NORN_PERFORMANCE_DOCUMENTS")
        .expect("the document count")
        .parse()
        .expect("an integer document count");
    // Link-following is not a workload axis in this baseline. All fixtures stay self-contained.
    profile.symlinks = norn_fixtures::Symlinks::NONE;
    norn_fixtures::generate(&profile, 7, &root.join("vault")).expect("generate the fixture");
    emit(json!({"generated": profile.docs}));
}

#[cfg(feature = "induced-failure")]
fn performance_audit() {
    let root = root();
    let vault = attach::Vault::adopt(&root);
    let summary = norn_store::startup_audit::audit(&vault.database(), |path, hash, byte_length| {
        emit(json!({"document": {"path": path, "content_hash": hash, "byte_length": byte_length}}));
        Ok(())
    })
    .expect("audit the closed startup store");
    emit(json!({"summary": summary}));
}

fn performance_worker() {
    let root = root();
    let changeset_size = std::env::var("NORN_PERFORMANCE_CHANGESET_SIZE").map_or(128, |value| {
        value.parse().expect("an integer changeset size")
    });
    let policy =
        norn_host::ProductionPolicy::new(128, changeset_size).expect("bounded measurement policy");
    let profiling = std::env::var("NORN_PERFORMANCE_PROFILE").is_ok_and(|value| value == "1");
    #[cfg(not(feature = "induced-failure"))]
    assert!(
        !profiling,
        "startup profiling requires the induced-failure build feature"
    );
    #[cfg(feature = "induced-failure")]
    let recording = profiling.then(|| {
        norn_store::profiling::start(&[
            "host.attach",
            "host.watch_start",
            "host.store_open",
            "host.watch_sync_wait",
            "host.schema_pin",
            "host.heal",
            "host.reconcile",
            "host.document_read",
            "host.document_plan",
            "host.parse_map",
            "host.frontmatter_parse",
            "host.body_scan",
            "host.close_job",
            "store.increment",
            "store.upsert",
            "store.document_row",
            "store.link_health",
            "store.commit",
            "store.write_finding",
            "health.discard",
            "health.prepare_discard",
            "store.prepare_read",
            "health.judge",
            "health.read_links",
            "health.resolve_keys",
            "health.anchors",
            "health.candidate_names",
            "db.read_meta",
            "db.next_generation",
        ])
        .expect("start the isolated timing record")
    });
    let began = Instant::now();
    let vault = attach::Vault::adopt(&root);
    let host = vault.host_with_policy(policy);
    let host_setup = began.elapsed();
    let _demand = host.demand(vault.name()).expect("request attachment");
    let until = Instant::now() + std::time::Duration::from_secs(240);
    while host.state(vault.name()).expect("registered vault state") != norn_wire::TrustState::Ready
    {
        assert!(Instant::now() < until, "startup did not become ready");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let ready_ns = began.elapsed().as_nanos();
    #[cfg(feature = "induced-failure")]
    let (timings, evidence) = {
        // Ready is published before the job folds its account; wait for that tail.
        let until = Instant::now() + std::time::Duration::from_secs(30);
        while !host
            .work_in_flight(vault.name())
            .expect("a registered vault")
            .is_quiet()
        {
            assert!(Instant::now() < until, "startup work did not finish");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let timings = recording.map(|recording| {
            let report = recording.finish();
            json!({"entries": report.entries.iter().map(|entry| json!({
                "label": entry.label, "calls": entry.calls, "elapsed_ns": entry.elapsed_ns,
            })).collect::<Vec<_>>(), "unknown_labels": report.unknown_labels,
                "unfinished_spans": report.unfinished_spans})
        });
        let e = host.evidence();
        (
            timings,
            json!({"document_opens": e.document_opens, "stats": e.stats,
            "walk_dirents": e.walk_dirents, "documents_derived": e.documents_derived,
            "changesets_applied": e.changesets_applied, "documents_upserted": e.documents_upserted,
            "documents_deleted": e.documents_deleted, "findings_written": e.findings_written,
            "findings_discarded": e.findings_discarded, "links_redecided": e.links_redecided,
            "link_health_keys_resolved": e.link_health_keys_resolved,
            "link_health_candidates_read": e.link_health_candidates_read,
            "changeset_read_steps": e.changeset_read_steps, "recoveries_run": e.recoveries_run,
            "rebuilds_run": e.rebuilds_run, "watcher_polls": e.watcher_polls}),
        )
    };
    #[cfg(not(feature = "induced-failure"))]
    let (timings, evidence): (Option<Value>, Value) = (None, Value::Null);
    emit(
        json!({"ready": true, "attach_ns": ready_ns, "startup_complete_ns": began.elapsed().as_nanos(),
                "observed_ready_ns": ready_ns, "host_setup_ns": host_setup.as_nanos(),
                "store_page_size": policy.store_page_size, "changeset_size": policy.changeset_size,
                "timings": timings, "job_evidence": evidence}),
    );
    let address = VaultAddress::name(vault.name().clone());

    for line in std::io::stdin().lock().lines() {
        let line = line.expect("read a command");
        let command: Value = serde_json::from_str(&line).expect("a JSON command");
        let op = text(&command, "op");
        if op == "stop" {
            break;
        }
        let mut counters = BTreeMap::new();
        let (answer, elapsed) = match op {
            "search" => {
                let mut params = SearchParams::new(address.clone(), text(&command, "query"))
                    .with_rungs(RungSelection::exactly(
                        RungSet::of([Rung::Lexical]).expect("the lexical rung"),
                    ))
                    .with_limit(25);
                if let Some(path) = command["path"].as_str() {
                    params = params.with_predicates([Predicate::path(path)]);
                }
                let began = Instant::now();
                let answer = host.search(&params).expect("search answered");
                let elapsed = began.elapsed();
                counters.extend(answer.snapshot.readings());
                (
                    serde_json::to_value(answer.answer).expect("serialize search"),
                    elapsed,
                )
            }
            "find" => {
                let params = FindParams::new(address.clone())
                    .with_predicates([Predicate::equal_to("perf_bucket", "active")])
                    .with_limit(25);
                let began = Instant::now();
                let answer = host.find(&params).expect("find answered");
                let elapsed = began.elapsed();
                counters.extend(answer.snapshot.readings());
                (
                    serde_json::to_value(answer.answer).expect("serialize find"),
                    elapsed,
                )
            }
            "get" => {
                let params = GetParams::new(
                    address.clone(),
                    ResolutionTarget::new(text(&command, "target")).expect("a target"),
                );
                let began = Instant::now();
                let answer = host.get(&params).expect("get answered");
                let elapsed = began.elapsed();
                counters.extend(answer.snapshot.readings());
                (
                    serde_json::to_value(answer.answer).expect("serialize get"),
                    elapsed,
                )
            }
            "set" => {
                let params = SetParams::new(
                    address.clone(),
                    ApplyMode::Apply,
                    WriteTarget::path(DocumentPath::new("bench/subject.md").expect("a path")),
                    vec![FieldChange::set(
                        "status",
                        AuthoredValue::string(text(&command, "value")),
                    )],
                );
                let began = Instant::now();
                let answer = host
                    .set(params)
                    .expect("set admitted")
                    .wait()
                    .expect("set applied");
                let elapsed = began.elapsed();
                (
                    serde_json::to_value(answer).expect("serialize set"),
                    elapsed,
                )
            }
            "new" => {
                let params = NewParams::new(
                    address.clone(),
                    ApplyMode::Apply,
                    DocumentPath::new(text(&command, "path")).expect("a path"),
                    text(&command, "content"),
                );
                let began = Instant::now();
                let answer = host
                    .new_document(params)
                    .expect("new admitted")
                    .wait()
                    .expect("new applied");
                let elapsed = began.elapsed();
                (
                    serde_json::to_value(answer).expect("serialize new"),
                    elapsed,
                )
            }
            "move" => {
                let params = MoveParams::new(
                    address.clone(),
                    ApplyMode::Apply,
                    MoveSubject::document(
                        DocumentPath::new(text(&command, "from")).expect("a source"),
                        DocumentPath::new(text(&command, "to")).expect("a destination"),
                    ),
                );
                let began = Instant::now();
                let answer = host
                    .move_path(params)
                    .expect("move admitted")
                    .wait()
                    .expect("move applied");
                let elapsed = began.elapsed();
                (
                    serde_json::to_value(answer).expect("serialize move"),
                    elapsed,
                )
            }
            _ => panic!("unknown performance operation {op}"),
        };
        emit(json!({"answer": answer, "host_ns": elapsed.as_nanos(), "counters": counters}));
    }
}

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("generate") => performance_generate(),
        Some("serve") => performance_worker(),
        #[cfg(feature = "induced-failure")]
        Some("audit") => performance_audit(),
        _ => panic!("the performance worker requires generate or serve from its external runner"),
    }
}

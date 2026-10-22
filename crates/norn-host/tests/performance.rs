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

#[test]
#[ignore = "external performance runner only"]
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

#[test]
#[ignore = "external performance runner only"]
fn performance_worker() {
    let root = root();
    let began = Instant::now();
    let vault = attach::Vault::adopt(&root);
    let host = vault.host();
    let host_setup = began.elapsed();
    let _demand = attach::attach_and_wait(&host, vault.name());
    emit(
        json!({"ready": true, "attach_ns": began.elapsed().as_nanos(),
                "host_setup_ns": host_setup.as_nanos()}),
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

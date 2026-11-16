//! A `norn serve` killed outright during a write: the real binary takes an
//! apply through the wire route, is killed with `kill -9` partway through
//! publication, and is started again. The restart reaches `Ready` through
//! the heal path with no lost or torn document, and a resend of the same plan
//! finishes it.
//!
//! This is the kill-recovery row's real-process companion; the in-process
//! proof, `norn-host`'s `kill_recovery.rs`, is unchanged.
#![allow(clippy::disallowed_methods)] // Harness scaffolding: the vault a case serves and reads back.

mod support;

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use support::{Fixture, Reply, exchange, exchange_within, free_port};

/// Documents the killed plan creates: enough that publication takes far
/// longer than the poll that sees its first target land.
const CREATES: usize = 2000;

/// The documents the vault holds before the apply.
const BEFORE: [(&str, &str); 3] = [
    (
        "alpha.md",
        "---\nstatus: draft\n---\n# Alpha\n\nalpha body, see [[beta]]\n",
    ),
    ("beta.md", "# Beta\n\nbeta body\n"),
    ("delta.md", "# Delta\n\ndelta body, see [[beta]]\n"),
];

fn created(n: usize) -> (String, String) {
    (
        format!("made/doc-{n:05}.md"),
        format!("# Document {n}\n\nbody of document {n}, see [[beta]]\n"),
    )
}

fn a_vault(fixture: &Fixture) -> PathBuf {
    let root = fixture.sandbox.work_dir().join("notes");
    std::fs::create_dir_all(&root).unwrap();
    for (path, content) in BEFORE {
        std::fs::write(root.join(path), content).unwrap();
    }
    root
}

fn wire(port: u16, bearer: &str, verb: &str, body: &Value) -> Reply {
    exchange(
        port,
        "POST",
        &format!("/v1/wire/{verb}"),
        Some(bearer),
        &serde_json::to_vec(body).unwrap(),
    )
}

fn json_of(reply: &Reply) -> Value {
    serde_json::from_slice(&reply.body)
        .unwrap_or_else(|_| panic!("not JSON: {}", reply.body_text()))
}

/// The first resolved plan anywhere in `value`.
fn resolved_plan(value: &Value) -> Option<Value> {
    match value {
        Value::Object(map) => {
            if map.get("plan").and_then(Value::as_str) == Some("resolved") {
                return Some(value.clone());
            }
            map.values().find_map(resolved_plan)
        }
        Value::Array(items) => items.iter().find_map(resolved_plan),
        _ => None,
    }
}

fn state_of(port: u16, bearer: &str) -> String {
    let reply = wire(
        port,
        bearer,
        "vault_status",
        &json!({ "vault": { "by": "name", "name": "notes" } }),
    );
    json_of(&reply)["status"]["published"]["state"]["state"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Every `.md` file under `root`, by its path relative to `root`, with its
/// bytes.
fn documents(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, found: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if relative.starts_with('.') {
                continue;
            }
            if path.is_dir() {
                walk(root, &path, found);
            } else if relative.ends_with(".md") {
                found.insert(relative, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut found = BTreeMap::new();
    walk(root, root, &mut found);
    found
}

/// What the vault may hold: each document the vault held before, unchanged,
/// and each created document whole or absent, never torn. Answers how many
/// of the creates had landed.
fn assert_whole(root: &Path) -> usize {
    let found = documents(root);
    for (path, content) in BEFORE {
        assert_eq!(
            found.get(path).map(Vec::as_slice),
            Some(content.as_bytes()),
            "`{path}` was lost or changed"
        );
    }
    let mut landed = 0;
    for n in 0..CREATES {
        let (path, content) = created(n);
        if let Some(bytes) = found.get(&path) {
            assert_eq!(bytes, content.as_bytes(), "`{path}` is torn");
            landed += 1;
        }
    }
    assert_eq!(
        found.len(),
        BEFORE.len() + landed,
        "the vault holds a document no plan wrote"
    );
    landed
}

/// One attempt at killing the host partway through an apply's publication:
/// a fresh vault and host, the apply on a connection of its own whose answer
/// never comes, and `kill -9` once its first target has landed. Answers the
/// fixture, its vault, the port, the apply's body and how many creates had
/// landed, whole, at the kill; publication may still finish between the
/// first target landing and the kill, and then every create has landed.
fn kill_during_publication(label: &str) -> (Fixture, PathBuf, u16, Vec<u8>, usize) {
    let fixture = Fixture::new(label);
    let port = free_port();
    let root = a_vault(&fixture);
    let (served, bearer) = fixture.serve_on(port);
    let registered = wire(
        port,
        &bearer,
        "vault_register",
        &json!({ "registration": { "name": "notes", "root": root } }),
    );
    assert_eq!(registered.status, 200, "{}", registered.body_text());

    let operations: Vec<Value> = (0..CREATES)
        .map(|n| {
            let (path, content) = created(n);
            json!({ "kind": "create_document", "fields": { "path": path, "content": content } })
        })
        .collect();
    let preview = wire(
        port,
        &bearer,
        "apply",
        &json!({
            "mode": "preview",
            "plan": { "plan": "operations", "vault": { "by": "name", "name": "notes" }, "operations": operations },
        }),
    );
    assert_eq!(preview.status, 200, "{}", preview.body_text());
    let plan = resolved_plan(&json_of(&preview)).expect("a preview carries its resolved plan");
    let apply = serde_json::to_vec(&json!({ "mode": "apply", "plan": plan })).unwrap();

    // The apply, on a connection of its own whose answer never comes.
    let sending = {
        let apply = apply.clone();
        let bearer = bearer.clone();
        std::thread::spawn(move || {
            let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            let head = format!(
                "POST /v1/wire/apply HTTP/1.1\r\nHost: x\r\nConnection: close\r\nAuthorization: \
                 Bearer {bearer}\r\nContent-Length: {}\r\n\r\n",
                apply.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&apply);
            let mut answer = Vec::new();
            let _ = std::io::Read::read_to_end(&mut stream, &mut answer);
        })
    };
    let first = root.join(created(0).0);
    let deadline = Instant::now() + Duration::from_secs(120);
    while !first.exists() {
        assert!(Instant::now() < deadline, "the apply published nothing");
        std::thread::yield_now();
    }
    let mut served = served;
    served.child.kill().unwrap();
    served.child.wait().unwrap();
    let _ = sending.join();

    let landed = assert_whole(&root);
    assert!(landed >= 1, "the first target is gone");
    (fixture, root, port, apply, landed)
}

/// How many fresh attempts the kill gets to land mid-publication. One almost
/// always does: publication of every create takes far longer than the poll
/// that sees the first land.
const KILL_ATTEMPTS: usize = 5;

/// **Exit bar 9's kill row.** An apply killed with `kill -9` partway through
/// publication, then a restart: the vault holds every document it held and
/// each created one whole or not at all, the restart reaches `Ready`
/// through the heal path, and a resend of the plan finishes it.
#[test]
fn a_host_killed_during_an_apply_restarts_to_ready_and_a_resend_finishes_the_plan() {
    let started = Instant::now();
    // An attempt whose publication finished before the kill cut nothing off
    // and proves nothing here, so it is made again on a fresh vault, within
    // a bound on attempts.
    let mut attempt = 0;
    let (fixture, root, port, apply, landed) = loop {
        attempt += 1;
        let killed = kill_during_publication(&format!("serve-kill-{attempt}"));
        if killed.4 < CREATES {
            break killed;
        }
        assert!(
            attempt < KILL_ATTEMPTS,
            "in {attempt} attempts the apply finished before the kill, so no write was cut off"
        );
    };

    // The restart reaches `Ready` through the heal path, the vault as it
    // was: a read demands the attach, and is answered once it has.
    let (served, bearer) = fixture.serve_on(port);
    let read = wire(
        port,
        &bearer,
        "get",
        &serde_json::to_value(norn_wire::GetParams::new(
            norn_wire::VaultAddress::name(norn_wire::VaultName::new("notes").unwrap()),
            norn_wire::ResolutionTarget::new("alpha").unwrap(),
        ))
        .unwrap(),
    );
    assert_eq!(read.status, 200, "{}", read.body_text());
    let ready_by = Instant::now() + Duration::from_secs(120);
    loop {
        let state = state_of(port, &bearer);
        if state == "ready" {
            break;
        }
        assert!(
            Instant::now() < ready_by,
            "the restarted host never reached Ready: {state:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(assert_whole(&root), landed, "the heal changed a document");

    // A resend of the same plan finishes it.
    let resent = exchange_within(
        port,
        "POST",
        "/v1/wire/apply",
        Some(&bearer),
        &apply,
        Duration::from_secs(300),
    );
    assert_eq!(resent.status, 200, "{}", resent.body_text());
    assert_eq!(assert_whole(&root), CREATES);
    assert_eq!(served.terminate().0.code(), Some(0));
    eprintln!(
        "serve_kill: {CREATES} creates, killed after {landed} landed on attempt {attempt}, in {:?}",
        started.elapsed()
    );
}

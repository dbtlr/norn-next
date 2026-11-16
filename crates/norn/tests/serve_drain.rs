//! The bounded drain's answers over a real `norn serve`: an apply running when
//! SIGTERM lands is answered truthfully at the drain's bound, and what a
//! resend of each synchronous mutation answers after each point the drain
//! could have stopped it at.
//!
//! **Truthful at the bound.** The drain answers an apply still running at
//! `drain_bound` from what its record holds: `host/apply-not-run` before
//! publication began, `host/apply-outcome-unknown` with the plan after, and
//! the apply's own answer where it finished first. A process cannot choose
//! the moment its apply is at, so each case checks the answer it got against
//! the vault it left: not run means no document was written; unknown or
//! applied means a resend finishes the plan.
//!
//! **A resend of a synchronous mutation.** A registration, an unregistration,
//! a registration edit, `init`, `vault migrate` and an activating `vault
//! reload` still running at the bound are answered `serve/outcome-unknown`.
//! The drain can have stopped one before it took effect, so a resend is the
//! first request that does; or after, so a resend meets its effect. Each
//! case sends the verb twice and pins what the second answer is, which is
//! what the `serve/outcome-unknown` message for that verb tells a caller.
#![allow(clippy::disallowed_methods)] // Harness scaffolding: the vault a case serves and reads back.

mod support;

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use support::{Fixture, Reply, Served, exchange, exchange_within, free_port};

/// Documents an apply creates: enough that staging and publication each run
/// past a one-second drain bound.
const CREATES: usize = 2000;

fn created(n: usize) -> (String, String) {
    (
        format!("made/doc-{n:05}.md"),
        format!("# Document {n}\n\nbody of document {n}\n"),
    )
}

fn a_vault(fixture: &Fixture, name: &str) -> PathBuf {
    let root = fixture.sandbox.work_dir().join(name);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("alpha.md"), "# Alpha\n\nalpha body\n").unwrap();
    root
}

fn notes() -> Value {
    json!({ "by": "name", "name": "notes" })
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

fn register(port: u16, bearer: &str, root: &Path) -> Reply {
    wire(
        port,
        bearer,
        "vault_register",
        &json!({ "registration": { "name": "notes", "root": root } }),
    )
}

/// A host on a fresh port with a one-second drain bound, and `notes`
/// registered over a fresh vault.
fn serving(label: &str) -> (Fixture, Served, u16, String, PathBuf) {
    let fixture = Fixture::new(label);
    let port = free_port();
    fixture.write_settings(&format!(
        "[endpoint]\nport = {port}\n[serve]\ndrain_bound = \"1s\"\n"
    ));
    let mut served = fixture.start(&["serve"]);
    assert_eq!(served.listening_port(), port);
    let bearer = fixture.default_bearer();
    let root = a_vault(&fixture, "notes");
    let registered = register(port, &bearer, &root);
    assert_eq!(registered.status, 200, "{}", registered.body_text());
    (fixture, served, port, bearer, root)
}

/// The resolved plan of `CREATES` creates, by a preview.
fn the_plan(port: u16, bearer: &str) -> Value {
    let operations: Vec<Value> = (0..CREATES)
        .map(|n| {
            let (path, content) = created(n);
            json!({ "kind": "create_document", "fields": { "path": path, "content": content } })
        })
        .collect();
    let preview = wire(
        port,
        bearer,
        "apply",
        &json!({
            "mode": "preview",
            "plan": { "plan": "operations", "vault": notes(), "operations": operations },
        }),
    );
    assert_eq!(preview.status, 200, "{}", preview.body_text());
    resolved_plan(&json_of(&preview)).expect("a preview carries its resolved plan")
}

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

/// An apply sent on a connection of its own, answered on the returned
/// thread.
fn send_apply(port: u16, bearer: &str, plan: &Value) -> std::thread::JoinHandle<Option<Reply>> {
    let body = serde_json::to_vec(&json!({ "mode": "apply", "plan": plan })).unwrap();
    let bearer = bearer.to_string();
    std::thread::spawn(move || {
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let head = format!(
            "POST /v1/wire/apply HTTP/1.1\r\nHost: x\r\nConnection: close\r\nAuthorization: \
             Bearer {bearer}\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).ok()?;
        stream.write_all(&body).ok()?;
        let mut answer = Vec::new();
        std::io::Read::read_to_end(&mut stream, &mut answer).ok()?;
        support::parse(&answer)
    })
}

/// How many of the plan's creates landed, each whole; no other document
/// beside `alpha.md`.
fn landed(root: &Path) -> usize {
    let mut found = BTreeMap::new();
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
            } else {
                found.insert(relative, std::fs::read(&path).unwrap());
            }
        }
    }
    walk(root, root, &mut found);
    assert_eq!(
        found.remove("alpha.md").as_deref(),
        Some(&b"# Alpha\n\nalpha body\n"[..])
    );
    let mut count = 0;
    for n in 0..CREATES {
        let (path, content) = created(n);
        if let Some(bytes) = found.remove(&path) {
            assert_eq!(bytes, content.as_bytes(), "`{path}` is torn");
            count += 1;
        }
    }
    assert!(found.is_empty(), "files no plan wrote: {:?}", found.keys());
    count
}

/// What SIGTERM at `when` answers a running apply, checked against what the
/// vault holds; then the host's exit, within the bound and the margin.
fn sigterm_during_an_apply(label: &str, after_first_landed: bool) -> (Value, usize) {
    let (fixture, mut served, port, bearer, root) = serving(label);
    let plan = the_plan(port, &bearer);
    let answering = send_apply(port, &bearer, &plan);
    if after_first_landed {
        let first = root.join(created(0).0);
        let deadline = Instant::now() + Duration::from_secs(120);
        while !first.exists() {
            assert!(Instant::now() < deadline, "the apply published nothing");
            std::thread::yield_now();
        }
    } else {
        // The apply is staging: a shadow stands in the derived directory's
        // own temporary directory, and nothing is published yet.
        let data = fixture.dirs().data_dir().to_path_buf();
        let deadline = Instant::now() + Duration::from_secs(120);
        while !holds_a_shadow(&data) {
            assert!(Instant::now() < deadline, "the apply staged nothing");
            std::thread::yield_now();
        }
        // No check that the apply is still staging: it may finish staging
        // and begin publishing before the signal lands. The reply is judged
        // against the files the vault holds once the host has exited, which
        // holds whichever point the drain stopped it at.
    }
    let signalled = Instant::now();
    served.send(libc::SIGTERM);
    let reply = answering
        .join()
        .unwrap()
        .expect("the apply was answered before the host exited");
    let (status, lines) = served.wait_within(Duration::from_secs(30));
    assert_eq!(status.code(), Some(0), "{lines:?}");
    // The bound and the margin, and a second for the process to be reaped
    // and seen: an apply still publishing keeps the host's drop going, and
    // the margin's end is the exit.
    assert!(
        signalled.elapsed() < Duration::from_secs(1 + 5 + 1),
        "the drain outlasted its bound and margin: {:?}",
        signalled.elapsed()
    );
    let landed_at_the_bound = landed(&root);
    let answer = json_of(&reply);
    match (reply.status, reply.code().as_str()) {
        (200, _) => assert_eq!(
            landed_at_the_bound, CREATES,
            "applied, and not every target landed"
        ),
        (503, "host/apply-not-run") => {
            assert_eq!(
                landed_at_the_bound, 0,
                "answered not run, and a document was written"
            );
            assert_eq!(
                answer["detail"]["cause"]["code"], "serve/draining",
                "{answer}"
            );
        }
        (503, "host/apply-outcome-unknown") => {
            assert!(
                resolved_plan(&answer["detail"]).is_some(),
                "an unknown outcome carries its plan: {answer}"
            );
        }
        (status, code) => panic!("a running apply at the drain bound answered {status} {code}"),
    }
    // Whatever the answer, a resend on a restarted host finishes the plan.
    let (served, bearer) = fixture.serve_on(port);
    let resent = exchange_within(
        port,
        "POST",
        "/v1/wire/apply",
        Some(&bearer),
        &serde_json::to_vec(&json!({ "mode": "apply", "plan": plan })).unwrap(),
        Duration::from_secs(300),
    );
    assert_eq!(resent.status, 200, "{}", resent.body_text());
    assert_eq!(landed(&root), CREATES);
    assert_eq!(served.terminate().0.code(), Some(0));
    (answer, landed_at_the_bound)
}

/// Whether a staged shadow stands anywhere under `dir`.
fn holds_a_shadow(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let path = entry.path();
        entry
            .file_name()
            .to_string_lossy()
            .starts_with("norn-shadow-")
            || (path.is_dir() && holds_a_shadow(&path))
    })
}

/// **SIGTERM once publication began**: the apply still publishing at the
/// bound is answered `host/apply-outcome-unknown` with its plan — or applied,
/// where it finished first — and the host exits 0 within the bound and the
/// margin.
#[test]
fn sigterm_during_publication_answers_the_apply_unknown_with_its_plan() {
    let (answer, landed) = sigterm_during_an_apply("serve-drain-publishing", true);
    eprintln!("publishing: {} after {landed} landed", answer["code"]);
}

/// **SIGTERM before publication began**: the apply still planning or
/// staging at the bound is answered `host/apply-not-run`, its cause
/// `serve/draining`, and no document was written.
#[test]
fn sigterm_before_publication_answers_the_apply_not_run() {
    let (answer, landed) = sigterm_during_an_apply("serve-drain-staging", false);
    eprintln!("staging: {} after {landed} landed", answer["code"]);
}

/// The same synchronous mutation twice, and both answers.
fn twice(port: u16, bearer: &str, verb: &str, body: &Value) -> (Reply, Reply) {
    (
        wire(port, bearer, verb, body),
        wire(port, bearer, verb, body),
    )
}

/// **What a resend of each synchronous mutation answers once the first took
/// effect** — the stopping point after it; before it, the resend is the first
/// request that takes effect, and answers as the first did here. Each is
/// safe to resend, and the answer says which point the drain stopped at.
#[test]
fn a_resend_of_each_synchronous_mutation_answers_what_its_message_says() {
    let fixture = Fixture::new("serve-drain-resend");
    let port = free_port();
    let (served, bearer) = fixture.serve_on(port);
    let root = a_vault(&fixture, "notes");
    let registration = json!({ "registration": { "name": "notes", "root": root } });
    // The registration verbs act on a registration whose entry holds no
    // attachment; `init` attaches it, so it and the verbs after it come once
    // the vault is registered again.
    let mut set = norn_wire::VaultSetParams::new(norn_wire::VaultName::new("notes").unwrap());
    set.poll_backend = norn_wire::VaultChange::set(norn_wire::PollBackend::Poll);
    let set = serde_json::to_value(set).unwrap();
    let unregister = serde_json::to_value(norn_wire::UnregisterParams::new(
        norn_wire::VaultName::new("notes").unwrap(),
    ))
    .unwrap();
    for (verb, body) in [
        ("vault_register", registration.clone()),
        ("vault_set", set),
        ("vault_unregister", unregister),
        ("register again", registration.clone()),
        ("init", json!({ "vault": notes(), "mode": "apply" })),
        (
            "vault_migrate",
            json!({ "vault": notes(), "mode": "apply" }),
        ),
        (
            "vault_reload",
            json!({ "vault": notes(), "dry_run": false }),
        ),
    ] {
        if verb == "register again" {
            let again = wire(port, &bearer, "vault_register", &body);
            assert_eq!(again.status, 200, "{}", again.body_text());
            continue;
        }
        let (first, second) = twice(port, &bearer, verb, &body);
        assert_eq!(first.status, 200, "{verb}: {}", first.body_text());
        let resent = (second.status, second.code(), json_of(&second));
        match verb {
            // A registration already made: the resend is refused as
            // already served, which says the first took effect.
            "vault_register" => {
                assert_eq!((resent.0, resent.1.as_str()), (409, "host/already-served"));
            }
            // An unregistration already made: the vault is unknown, which
            // says the first took effect.
            "vault_unregister" => {
                assert_eq!((resent.0, resent.1.as_str()), (404, "host/unknown-vault"));
            }
            // A setup already made: answered as set up, nothing written.
            "init" => {
                assert_eq!(resent.0, 200);
                assert_eq!(
                    resent.2["outcome"],
                    "already_set_up",
                    "{}",
                    second.body_text()
                );
            }
            // A migration already made, or none owed: answered current.
            "vault_migrate" => {
                assert_eq!(resent.0, 200);
                assert_eq!(
                    resent.2["outcome"],
                    "already_current",
                    "{}",
                    second.body_text()
                );
            }
            // An edit or a reload answers as the first did: setting the same
            // values again, or reloading the same files, changes nothing more.
            _ => {
                assert_eq!(resent.0, 200, "{verb}: {}", second.body_text());
                assert_eq!(resent.2, json_of(&first), "{verb}");
            }
        }
    }
    assert_eq!(served.terminate().0.code(), Some(0));
}

/// One hello on a kept-alive connection, and its answer read by its declared
/// length, so the connection stays usable for the next request.
fn hello_on(stream: &mut std::net::TcpStream) -> Option<Reply> {
    use std::io::Read;
    let body = json!({ "label": "default", "nonce": "ab".repeat(32) }).to_string();
    write!(
        stream,
        "POST /v1/hello HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .ok()?;
    let mut raw = Vec::new();
    let mut byte = [0u8; 1];
    while !raw.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).ok()? == 0 {
            return None;
        }
        raw.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&raw).into_owned();
    let status = head.split(' ').nth(1)?.parse().ok()?;
    let mut reply = Reply {
        status,
        head,
        body: Vec::new(),
    };
    let length: usize = reply.header("content-length")?.parse().ok()?;
    reply.body = vec![0; length];
    stream.read_exact(&mut reply.body).ok()?;
    Some(reply)
}

/// **An answer made during the drain tells its client to close.** The
/// close is decided when the route answers, and only an answer that carries
/// it is half-closed after its flush, so a client never reuses a connection
/// whose write half is shut and reads end of stream where the contract owes
/// `serve/draining`. A hello whose body never completes holds the drain open
/// while a kept-alive connection is answered during it.
#[test]
fn an_answer_flushed_during_the_drain_carries_connection_close() {
    use std::io::{Read, Write};
    let fixture = Fixture::new("serve-drain-close");
    let port = free_port();
    fixture.write_settings(&format!(
        "[endpoint]\nport = {port}\n[serve]\ndrain_bound = \"20s\"\n"
    ));
    let mut served = fixture.start(&["serve"]);
    served.listening_port();
    // The host asks for the body only once the hello's route reads it, so
    // the `100 Continue` says the holder is in flight before the drain.
    let mut holder = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    holder
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    holder
        .write_all(
            b"POST /v1/hello HTTP/1.1\r\nHost: x\r\nExpect: 100-continue\r\nContent-Length: \
              100\r\n\r\n",
        )
        .unwrap();
    let mut continued = Vec::new();
    let mut byte = [0u8; 1];
    while !continued.ends_with(b"\r\n\r\n") {
        assert_eq!(holder.read(&mut byte).unwrap(), 1, "no 100 Continue");
        continued.push(byte[0]);
    }
    assert!(
        continued.starts_with(b"HTTP/1.1 100 "),
        "{}",
        String::from_utf8_lossy(&continued)
    );
    holder.write_all(b"{").unwrap();
    let mut open = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    open.set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let before = hello_on(&mut open).expect("a hello before the drain");
    assert_eq!(before.status, 200);
    assert_eq!(before.header("connection"), None);
    served.send(libc::SIGTERM);
    served.line_starting("draining: SIGTERM");
    let during = hello_on(&mut open).expect("an answer to the hello sent during the drain");
    assert_eq!(during.status, 200, "{}", during.head);
    assert_eq!(
        during.header("connection").as_deref(),
        Some("close"),
        "{}",
        during.head
    );
    drop(holder);
    assert_eq!(
        served.wait_within(Duration::from_secs(15)).0.code(),
        Some(0)
    );
}

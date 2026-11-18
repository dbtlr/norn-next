//! The bounded drain's answers over a real `norn serve`: an apply running when
//! SIGTERM lands is answered truthfully at the drain's bound, and what a
//! resend of each synchronous mutation answers after each point the drain
//! could have stopped it at.
//!
//! **Truthful at the bound.** An external fsync gate holds an apply before
//! publication or after its first rename while the host's other threads run.
//! Each case pins the exact drain answer and checks the vault it left.
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
use support::{Fixture, Reply, Served, exchange_within, free_port};

/// Two creates distinguish the first publication from a completed plan;
/// the external fsync gate holds the selected boundary for the whole drain.
const CREATES: usize = 2;

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
    exchange_within(
        port,
        "POST",
        &format!("/v1/wire/{verb}"),
        Some(bearer),
        &serde_json::to_vec(body).unwrap(),
        Duration::from_secs(30),
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

/// What SIGTERM at the witnessed fsync boundary answers, checked against what the
/// vault holds; then the host's exit, within the bound and the margin.
#[cfg(target_os = "linux")]
fn sigterm_during_an_apply(label: &str, after_first_landed: bool) -> (Value, usize) {
    let (fixture, mut served, port, bearer, root) = serving(label);
    // Restart with an external filesystem gate. No production test hook:
    // the preload library holds only the fsync-ing worker on a harness lock.
    assert_eq!(served.terminate().0.code(), Some(0));
    let lock_path = fixture.config_dir().join("apply-gate.lock");
    let gate = support::hold_file_lock(&lock_path);
    served = fixture.start_with(&["serve"], |command| {
        command
            .env("LD_PRELOAD", support::apply_gate())
            .env("NORN_TEST_FSYNC_LOCK", &lock_path)
            .env(
                "NORN_TEST_FSYNC_MATCH",
                if after_first_landed {
                    root.join("made").to_string_lossy().into_owned()
                } else {
                    "norn-shadow-".to_string()
                },
            );
    });
    served.listening_port();
    let plan = the_plan(port, &bearer);
    let answering = send_apply(port, &bearer, &plan);
    support::wait_for_file_lock(&served);
    if after_first_landed {
        assert!(root.join(created(0).0).exists(), "publication has begun");
    } else {
        assert_eq!(landed(&root), 0, "publication has not begun");
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
    drop(gate);
    assert_eq!(reply.status, 503, "{answer}");
    if after_first_landed {
        assert_eq!(reply.code(), "host/apply-outcome-unknown", "{answer}");
        assert_eq!(
            resolved_plan(&answer["detail"]),
            Some(plan.clone()),
            "{answer}"
        );
    } else {
        assert_eq!(reply.code(), "host/apply-not-run", "{answer}");
        assert_eq!(landed_at_the_bound, 0, "{answer}");
        assert_eq!(
            answer["detail"]["cause"]["code"], "serve/draining",
            "{answer}"
        );
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
    eprintln!("{label}: drain and resend {:?}", signalled.elapsed());
    (answer, landed_at_the_bound)
}

/// Bar 9b: SIGTERM once publication began: the apply still publishing at the
/// bound is answered `host/apply-outcome-unknown` with its plan,
/// and the host exits 0 within the bound and the
/// margin.
#[test]
#[cfg(target_os = "linux")]
fn sigterm_during_publication_answers_the_apply_unknown_with_its_plan() {
    sigterm_during_an_apply("serve-drain-publishing", true);
}

/// Bar 9b: SIGTERM before publication began: the apply still planning or
/// staging at the bound is answered `host/apply-not-run`, its cause
/// `serve/draining`, and no document was written.
#[test]
#[cfg(target_os = "linux")]
fn sigterm_before_publication_answers_the_apply_not_run() {
    sigterm_during_an_apply("serve-drain-staging", false);
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

/// Bar 9c: SIGTERM answers a running synchronous registration unknown.
/// The registry's own file lock holds the mutation inside its real host call;
/// /proc/locks witnesses that it started, rather than a queued request or
/// an incompletely delivered body. The lock stays held through the answer.
#[test]
#[cfg(target_os = "linux")]
fn sigterm_during_a_synchronous_registration_answers_outcome_unknown() {
    let (fixture, mut served, port, bearer, _) = serving("serve-drain-register");
    let root = a_vault(&fixture, "other");
    let held = support::hold_file_lock(&fixture.config_dir().join(".registry.toml.lock"));
    let answering = std::thread::spawn(move || {
        wire(
            port,
            &bearer,
            "vault_register",
            &json!({ "registration": { "name": "other", "root": root } }),
        )
    });
    support::wait_for_file_lock(&served);
    served.send(libc::SIGTERM);
    let reply = answering.join().unwrap();
    assert_eq!(reply.status, 503, "{}", reply.body_text());
    assert_eq!(
        reply.code(),
        "serve/outcome-unknown",
        "{}",
        reply.body_text()
    );
    let (status, lines) = served.wait_within(Duration::from_secs(7));
    assert_eq!(status.code(), Some(0), "{lines:?}");
    drop(held);
}

/// Bar 6h: a stopping host's 20 s drain frees the channel lock within 40 s
/// of listener close even when its waiter reads a one-second drain setting.
/// A blocked synchronous mutation holds the old host for its whole drain.
#[test]
#[cfg(target_os = "linux")]
fn a_longer_draining_host_releases_the_lock_for_a_shorter_waiter() {
    let (fixture, mut served, port, bearer, _) = serving("serve-drain-longer");
    assert_eq!(served.terminate().0.code(), Some(0));
    fixture.write_settings(&format!(
        "[endpoint]\nport = {port}\n[serve]\ndrain_bound = \"20s\"\n"
    ));
    served = fixture.start(&["serve"]);
    served.listening_port();
    let root = a_vault(&fixture, "other");
    let held = support::hold_file_lock(&fixture.config_dir().join(".registry.toml.lock"));
    let answering = std::thread::spawn(move || {
        wire(
            port,
            &bearer,
            "vault_register",
            &json!({ "registration": { "name": "other", "root": root } }),
        )
    });
    support::wait_for_file_lock(&served);
    let signalled = Instant::now();
    served.send(libc::SIGTERM);
    support::eventually("the listener closing", || {
        std::net::TcpStream::connect(("127.0.0.1", port))
            .is_err_and(|error| error.kind() == std::io::ErrorKind::ConnectionRefused)
    });
    let closed = Instant::now();
    assert!(
        served.running(),
        "the held mutation must keep the old host alive"
    );
    fixture.write_settings(&format!(
        "[endpoint]\nport = {port}\n[serve]\ndrain_bound = \"1s\"\n"
    ));
    let mut waiter = fixture.start(&["serve"]);
    waiter.line_starting("the channel lock in");
    let reply = answering.join().unwrap();
    assert!(signalled.elapsed() >= Duration::from_secs(20));
    assert_eq!(
        reply.code(),
        "serve/outcome-unknown",
        "{}",
        reply.body_text()
    );
    assert_eq!(served.wait_within(Duration::from_secs(7)).0.code(), Some(0));
    drop(held);
    // Listening proves the waiter actually acquired the released lock.
    assert_eq!(waiter.listening_port(), port);
    assert!(
        closed.elapsed() < Duration::from_secs(40),
        "{:?}",
        closed.elapsed()
    );
    eprintln!(
        "Bar 6h listener close to lock acquired: {:?}",
        closed.elapsed()
    );
    assert_eq!(waiter.terminate().0.code(), Some(0));
}

/// Bar 5d: an invalid drain knob falls back to the effective default 10 s,
/// rather than merely printing a warning. A witnessed running registration
/// stays blocked until the default drain answers it.
#[test]
#[cfg(target_os = "linux")]
fn an_invalid_drain_knob_uses_the_default_in_the_running_process() {
    let (fixture, served, port, bearer, _) = serving("serve-drain-default");
    assert_eq!(served.terminate().0.code(), Some(0));
    fixture.write_settings(&format!(
        "[endpoint]\nport = {port}\n[serve]\ndrain_bound = \"0s\"\n"
    ));
    let mut served = fixture.start(&["serve"]);
    served.listening_port();
    assert!(
        served
            .seen
            .iter()
            .any(|line| line.contains("warning: serve.drain_bound:") && line.contains("default")),
        "{:?}",
        served.seen
    );
    let root = a_vault(&fixture, "other");
    let held = support::hold_file_lock(&fixture.config_dir().join(".registry.toml.lock"));
    let answering = std::thread::spawn(move || {
        wire(
            port,
            &bearer,
            "vault_register",
            &json!({ "registration": { "name": "other", "root": root } }),
        )
    });
    support::wait_for_file_lock(&served);
    let signalled = Instant::now();
    served.send(libc::SIGTERM);
    let reply = answering.join().unwrap();
    assert_eq!(
        reply.code(),
        "serve/outcome-unknown",
        "{}",
        reply.body_text()
    );
    assert!(signalled.elapsed() >= Duration::from_secs(10));
    // Two seconds cover scheduling and socket delivery beyond the default.
    assert!(signalled.elapsed() < Duration::from_secs(12));
    eprintln!("Bar 5d drain answer: {:?}", signalled.elapsed());
    assert_eq!(served.wait_within(Duration::from_secs(7)).0.code(), Some(0));
    drop(held);
}

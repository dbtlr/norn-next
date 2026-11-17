//! The token counter bar (R1), read through the counters route of a binary
//! built with the `counters` feature.
//!
//! An unchanged token file costs one fingerprint per request and no read, and
//! authentication is one lookup by label or by digest, at one token and at
//! many. Each reading is a delta between two reads of the route, and the
//! second read is itself a bearer request: it costs one fingerprint and one
//! lookup of its own, which every expectation below names as `+ 1`.
//!
//! Every reading is taken on a settled token file: a fingerprint taken
//! within 3 s of the file's last write is not trusted, and each check reads
//! the file again until it is that old. So each case waits once, after its
//! last write to the file, before the reading it asserts the steady state
//! on; the first check after the wait may still read once, and it falls
//! before the reading.
//!
//! The host counts a lookup inside the held token set's two lookup methods,
//! by digest and by label, so `serve.token_lookups` is the lookups made
//! through the index. A walk over the tokens added beside the index makes no
//! counted lookup, and this bar cannot see it: `norn-serve`'s poisoned-index
//! unit test catches one, and review catches the rest. That test is also the
//! only guard against rebuilding the digest index on every check, an
//! O(tokens) cost per request this bar cannot see: a rebuild would wipe the
//! poison, so it must keep poisoning the index after the set is held.
#![cfg(feature = "counters")]

mod support;

use std::collections::BTreeMap;

use norn_config::machine::tokens::{self, TokenLabel};
use support::{Fixture, free_port, get, hello, settle_token_file};

type Counters = BTreeMap<String, u64>;

fn counters(port: u16, bearer: &str) -> Counters {
    let reply = get(port, "/v1/debug/counters", Some(bearer));
    assert_eq!(reply.status, 200, "{reply:?}");
    serde_json::from_slice(&reply.body).expect("one object of name to integer")
}

fn delta(before: &Counters, after: &Counters) -> [u64; 3] {
    [
        "serve.token_fingerprints",
        "serve.token_file_reads",
        "serve.token_lookups",
    ]
    .map(|name| after[name] - before[name])
}

/// A host over `count` tokens, `default` among them.
fn host_over(label: &str, count: usize) -> (Fixture, support::Served, u16, String) {
    let fixture = Fixture::new(label);
    fixture.config_dir();
    tokens::mutate(&fixture.dirs(), |tokens| {
        tokens.add(&TokenLabel::default_label(), &[1; 32])?;
        for n in 1..count {
            tokens.add(
                &TokenLabel::new(format!("t{n}")).unwrap(),
                &[(n + 1) as u8; 32],
            )?;
        }
        Ok(())
    })
    .unwrap();
    let port = free_port();
    let (served, bearer) = fixture.serve_on(port);
    (fixture, served, port, bearer)
}

const REQUESTS: u64 = 20;

#[test]
fn an_unchanged_file_costs_one_fingerprint_no_read_and_one_lookup_per_request() {
    for count in [1, 64] {
        let (fixture, served, port, bearer) = host_over("counters-unchanged", count);
        settle_token_file(&fixture.tokens_file());
        let before = counters(port, &bearer);
        for _ in 0..REQUESTS {
            assert_eq!(get(port, "/", Some(&bearer)).status, 404);
        }
        let measured = delta(&before, &counters(port, &bearer));
        eprintln!(
            "{count} tokens, {REQUESTS} bearer requests: [fingerprints, reads, lookups] = {measured:?}"
        );
        assert_eq!(measured, [REQUESTS + 1, 0, REQUESTS + 1], "{count} tokens");
        assert_eq!(served.terminate().0.code(), Some(0));
    }
}

#[test]
fn one_hello_costs_one_fingerprint_and_one_lookup() {
    for count in [1, 64] {
        let (fixture, served, port, bearer) = host_over("counters-hello", count);
        settle_token_file(&fixture.tokens_file());
        let before = counters(port, &bearer);
        assert_eq!(hello(port, "default").status, 200);
        let measured = delta(&before, &counters(port, &bearer));
        eprintln!("{count} tokens, one hello: [fingerprints, reads, lookups] = {measured:?}");
        assert_eq!(measured, [1 + 1, 0, 1 + 1], "{count} tokens");
        assert_eq!(served.terminate().0.code(), Some(0));
    }
}

/// An edit costs one re-read, on the next request, and the request after
/// reads nothing again. A changed check takes two fingerprints. The one wait
/// falls between the edit and that request, so the fingerprint the re-read
/// holds is settled.
#[test]
fn an_edit_costs_exactly_one_re_read_on_the_next_request() {
    let (fixture, served, port, bearer) = host_over("counters-edit", 1);
    let before = counters(port, &bearer);
    tokens::mutate(&fixture.dirs(), |tokens| {
        tokens.add(&TokenLabel::new("laptop").unwrap(), &[9; 32])
    })
    .unwrap();
    settle_token_file(&fixture.tokens_file());
    assert_eq!(get(port, "/", Some(&bearer)).status, 404);
    let measured = delta(&before, &counters(port, &bearer));
    eprintln!("one edit, one bearer request: [fingerprints, reads, lookups] = {measured:?}");
    assert_eq!(measured, [2 + 1, 1, 1 + 1]);

    let before = counters(port, &bearer);
    assert_eq!(get(port, "/", Some(&bearer)).status, 404);
    assert_eq!(delta(&before, &counters(port, &bearer)), [1 + 1, 0, 1 + 1]);
    assert_eq!(served.terminate().0.code(), Some(0));
}

/// One wire call whose report must succeed.
fn wire(port: u16, bearer: &str, verb: &str, body: serde_json::Value) -> support::Reply {
    let reply = support::exchange(
        port,
        "POST",
        &format!("/v1/wire/{verb}"),
        Some(bearer),
        &serde_json::to_vec(&body).unwrap(),
    );
    assert_eq!(reply.status, 200, "{verb}: {}", reply.body_text());
    reply
}

fn wait_for_ready(port: u16, bearer: &str) {
    support::eventually("served fixture ready", || {
        let status = wire(
            port,
            bearer,
            "vault_status",
            serde_json::json!({"vault":notes()}),
        );
        let status: serde_json::Value = serde_json::from_slice(&status.body).unwrap();
        status["status"]["published"]["state"]["state"] == "ready"
    });
}

fn notes() -> serde_json::Value {
    serde_json::json!({"by":"name", "name":"notes"})
}

#[allow(clippy::disallowed_methods)] // Generated vault fixture, never a user tree.
fn register_tree(fixture: &Fixture, port: u16, bearer: &str, name: &str, count: usize) {
    let root = fixture.sandbox.work_dir().join(name);
    std::fs::create_dir_all(&root).unwrap();
    for n in 0..count {
        std::fs::write(
            root.join(format!("d{n:04}.md")),
            "---\nstatus: draft\n---\n# Note\nbody\n",
        )
        .unwrap();
    }
    wire(
        port,
        bearer,
        "vault_register",
        serde_json::json!({"registration":{"name":name,"root":root}}),
    );
}

fn read_one(port: u16, bearer: &str) -> support::Reply {
    let reply = wire(
        port,
        bearer,
        "get",
        serde_json::to_value(norn_wire::GetParams::new(
            norn_wire::VaultAddress::name(norn_wire::VaultName::new("notes").unwrap()),
            norn_wire::ResolutionTarget::new("d0000.md").unwrap(),
        ))
        .unwrap(),
    );
    let answer: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
    assert_eq!(answer["report"]["document"]["path"], "d0000.md", "{answer}");
    assert!(
        answer["unsatisfied"].as_array().is_none_or(Vec::is_empty),
        "{answer}"
    );
    reply
}

fn differences(before: &Counters, after: &Counters) -> Counters {
    after
        .iter()
        .map(|(name, value)| (name.clone(), value - before[name]))
        .collect()
}

/// Drain every page, then apply to every document: a deliberately unbounded
/// control whose dispatches, statements, VM steps, encoded bytes and filesystem
/// work must grow with the vault. Authentication grows with its page count.
fn growing_control(port: u16, bearer: &str, documents: usize) -> Counters {
    let before = counters(port, bearer);
    let mut after = None;
    let mut paths = std::collections::BTreeSet::new();
    loop {
        let mut params = norn_wire::FindParams::new(norn_wire::VaultAddress::name(
            norn_wire::VaultName::new("notes").unwrap(),
        ))
        .with_limit(8);
        if let Some(cursor) = after.take() {
            params = params.with_after(cursor);
        }
        let reply = wire(port, bearer, "find", serde_json::to_value(params).unwrap());
        let answer: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
        for row in answer["report"]["page"]["rows"].as_array().unwrap() {
            assert!(paths.insert(row["path"].as_str().unwrap().to_owned()));
        }
        after = serde_json::from_value(answer["report"]["page"]["next"].clone()).unwrap();
        if after.is_none() {
            break;
        }
    }
    assert_eq!(paths.len(), documents);
    let reply = wire(
        port,
        bearer,
        "set",
        serde_json::json!({"vault":notes(), "mode":"apply", "where":[{"op":"path","glob":"d*.md"}], "changes":[{"change":"set", "field":"status", "value":"controlled"}]}),
    );
    let applied: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
    assert_eq!(applied["report"]["outcome"], "applied", "{applied}");
    differences(&before, &counters(port, bearer))
}

/// Fixed output and addressed work stay flat when each unrelated dimension
/// grows independently. Repeated requests and an unbounded read/write control
/// prove that nonzero request work moves and that vault growth is observable. T3's wire suite owns transport allowance,
/// no-walk, retry and unchanged re-attach bars; the token tests above own the
/// changed-file positive control for token reads. The route keeps no
/// connection list, so no request can walk one: `serve.connections_visited`
/// is a structural zero and has no growing control.
#[test]
fn served_counters_do_not_grow_with_the_vault() {
    let mut baseline = None;
    let mut apply_baseline = None;
    let mut small_control: Option<Counters> = None;
    for (documents, registrations, tokens, connections) in [
        (16, 1, 1, 0),
        (256, 1, 1, 0),
        (16, 4, 1, 0),
        (16, 1, 64, 0),
        (16, 1, 1, 16),
        (256, 4, 64, 16),
    ] {
        let (fixture, served, port, bearer) = host_over("serve-growth", tokens);
        register_tree(&fixture, port, &bearer, "notes", documents);
        for n in 1..registrations {
            register_tree(&fixture, port, &bearer, &format!("other{n}"), 1);
        }
        read_one(port, &bearer);
        settle_token_file(&fixture.tokens_file());
        let idle: Vec<_> = (0..connections)
            .map(|_| std::net::TcpStream::connect(("127.0.0.1", port)).unwrap())
            .collect();
        let before = counters(port, &bearer);
        read_one(port, &bearer);
        let after = counters(port, &bearer);
        let measured = differences(&before, &after);
        eprintln!(
            "documents={documents} registrations={registrations} tokens={tokens} connections={connections}: {measured:?}"
        );
        let names = [
            "serve.dispatches",
            "serve.ready_retries",
            "serve.token_fingerprints",
            "serve.token_file_reads",
            "serve.token_lookups",
            "host.request.registrations_visited",
            "serve.connections_visited",
            "serve.body_bytes_encoded",
            "serve.report_bytes",
            "host.request.statements",
            "host.request.vm_steps",
            "host.request.fs_reads",
        ];
        let costs: Vec<_> = names.iter().map(|name| measured[*name]).collect();
        if let Some(expected) = &baseline {
            assert_eq!(&costs, expected);
        } else {
            baseline = Some(costs);
        }
        let before = after;
        for _ in 0..3 {
            read_one(port, &bearer);
        }
        let control = differences(&before, &counters(port, &bearer));
        for name in names {
            // Both snapshots authenticate themselves, adding one token check.
            let snapshot = u64::from(matches!(
                name,
                "serve.token_fingerprints" | "serve.token_lookups"
            ));
            assert_eq!(
                control[name] - snapshot,
                3 * (measured[name] - snapshot),
                "{name}"
            );
        }
        assert!(control["host.request.statements"] > measured["host.request.statements"]);
        assert!(control["host.request.vm_steps"] > measured["host.request.vm_steps"]);
        assert!(control["serve.report_bytes"] > measured["serve.report_bytes"]);
        let before = counters(port, &bearer);
        wire(
            port,
            &bearer,
            "set",
            serde_json::json!({"vault":notes(), "mode":"apply", "where":[{"op":"path","glob":"d0000.md"}], "changes":[{"change":"set", "field":"status", "value":"done"}]}),
        );
        let applied = differences(&before, &counters(port, &bearer));
        let apply_names = [
            "host.request.statements",
            "host.request.vm_steps",
            "host.request.fs_reads",
        ];
        let costs: Vec<_> = apply_names.iter().map(|name| applied[*name]).collect();
        eprintln!("apply documents={documents}: {costs:?} [statements, vm steps, fs reads]");
        assert!(applied["host.request.statements"] > 0);
        assert!(applied["host.request.vm_steps"] > 0);
        assert!(
            applied["host.request.fs_reads"] > 0,
            "the apply is the fs-read positive control"
        );
        if let Some(expected) = &apply_baseline {
            assert_eq!(&costs, expected);
        } else {
            apply_baseline = Some(costs);
        }
        let before = counters(port, &bearer);
        wire(port, &bearer, "vault_list", serde_json::json!({}));
        let listed = differences(&before, &counters(port, &bearer));
        assert_eq!(
            listed["host.request.registrations_visited"],
            registrations as u64
        );
        let control = growing_control(port, &bearer, documents);
        if documents == 256 {
            let small = small_control.as_ref().unwrap();
            for name in [
                "serve.dispatches",
                "serve.token_fingerprints",
                "serve.token_lookups",
                "serve.body_bytes_encoded",
                "serve.report_bytes",
                "host.request.statements",
                "host.request.vm_steps",
                "host.request.fs_reads",
            ] {
                assert!(
                    control[name] > small[name],
                    "growing control {name}: {} -> {}",
                    small[name],
                    control[name]
                );
            }
        } else if small_control.is_none() {
            small_control = Some(control);
        }
        drop(idle);
        assert_eq!(served.terminate().0.code(), Some(0));
    }
}

/// Fixed pools spawn eagerly and never retire: there is no keep-alive to
/// wait out. Only an observed detach and the release of closed sockets are
/// synchronization points. Token edits retain the default bearer.
///
/// `idle_after` is one second, the setting's floor. No step relies on the
/// vault staying attached across a pause: the detach is waited for, a read
/// that finds the vault detached is answered by attaching it, and the one
/// wait for the re-attached state also accepts the vault having already gone
/// idle again, which only a read that was answered can have preceded.
#[test]
#[allow(clippy::disallowed_methods)] // /proc reads of this test's own child.
fn idle_connection_and_token_cycles_retain_no_descriptors_or_threads() {
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!(
            "skipped on {}: process fd/task counts currently require Linux /proc",
            std::env::consts::OS
        );
    }
    #[cfg(target_os = "linux")]
    {
        let fixture = Fixture::new("serve-cycles");
        fixture.write_settings("[serve]\nidle_after = \"1s\"\n");
        let port = free_port();
        let (served, bearer) = fixture.serve_on(port);
        register_tree(&fixture, port, &bearer, "notes", 16);
        read_one(port, &bearer);
        let counts = || {
            ["fd", "task"].map(|kind| {
                std::fs::read_dir(format!("/proc/{}/{kind}", served.child.id()))
                    .unwrap()
                    .count()
            })
        };
        let mut first = None;
        for cycle in 0..3 {
            let idle = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            drop(idle);
            tokens::mutate(&fixture.dirs(), |tokens| {
                let label = TokenLabel::new(format!("cycling{cycle}")).unwrap();
                tokens.add(&label, &[cycle + 2; 32])
            })
            .unwrap();
            assert_eq!(hello(port, &format!("cycling{cycle}")).status, 200);

            // The idle time, plus the 90 s that bounded detach scheduling
            // and a stalled runner when the idle time was a minute.
            let until = std::time::Instant::now() + std::time::Duration::from_secs(1 + 90);
            loop {
                let reply = wire(
                    port,
                    &bearer,
                    "vault_status",
                    serde_json::json!({"vault":notes()}),
                );
                let value: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
                if value["status"]["published"]["state"]["state"] == "unattached" {
                    break;
                }
                assert!(
                    std::time::Instant::now() < until,
                    "idle detach did not occur"
                );
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            // Sample at the same unattached phase of every complete cycle.
            // Ready can still hold readers within their idle grace; comparing
            // it with an already-trimmed Ready entry would count that grace.
            support::eventually("accepted sockets closed", || {
                let socket_inodes: std::collections::BTreeSet<_> =
                    std::fs::read_dir(format!("/proc/{}/fd", served.child.id()))
                        .unwrap()
                        .filter_map(|entry| std::fs::read_link(entry.unwrap().path()).ok())
                        .filter_map(|path| {
                            path.to_str()
                                .and_then(|text| text.strip_prefix("socket:["))
                                .and_then(|text| text.strip_suffix(']'))
                                .map(str::to_owned)
                        })
                        .collect();
                ["tcp", "tcp6"].iter().all(|table| {
                    std::fs::read_to_string(format!("/proc/{}/net/{table}", served.child.id()))
                        .unwrap()
                        .lines()
                        .skip(1)
                        .all(|line| {
                            let fields: Vec<_> = line.split_whitespace().collect();
                            !socket_inodes.contains(fields[9]) || fields[3] == "0A"
                        })
                })
            });
            if let Some(expected) = first {
                support::eventually("cycle resources returned", || {
                    let actual = counts();
                    actual == expected
                });
                assert_eq!(counts(), expected);
            } else {
                first = Some(counts());
            }
            eprintln!("cycle {}: {:?} [descriptors, threads]", cycle + 1, counts());
            let before = counters(port, &bearer);
            read_one(port, &bearer);
            let after = counters(port, &bearer);
            let work = differences(&before, &after);
            assert_eq!(work["host.jobs.documents_derived"], 0);
            assert_eq!(work["host.jobs.changesets_applied"], 0);
            assert_eq!(work["host.jobs.document_opens"], 16);
            support::eventually("re-attach ready", || {
                let reply = wire(
                    port,
                    &bearer,
                    "vault_status",
                    serde_json::json!({"vault":notes()}),
                );
                let value: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
                // A stall past `idle_after` here lets the vault go idle again
                // before this poll sees it ready; the answered read above
                // is what proves it attached.
                matches!(
                    value["status"]["published"]["state"]["state"].as_str(),
                    Some("ready" | "unattached")
                )
            });
            read_one(port, &bearer);
            read_one(port, &bearer);
        }
        assert_eq!(served.terminate().0.code(), Some(0));
    }
}

/// The in-process side uses the same settings defaults and named production
/// constants as the server. Both sides attach the same fixture and read the
/// same request tally, including the apply worker's attributed work.
#[test]
#[allow(clippy::disallowed_methods)] // Restore this test's fixture between the two hosts.
fn served_reads_and_applies_cost_the_same_as_in_process() {
    use norn_host::{Host, LifecyclePolicy, ProductionEntryOps, ProductionPolicy, RegistryRead};
    use norn_wire::{GetParams, ResolutionTarget, SetParams, TrustState, VaultAddress, VaultName};
    let (fixture, served, port, bearer) = host_over("serve-parity", 1);
    register_tree(&fixture, port, &bearer, "notes", 16);
    read_one(port, &bearer);
    wait_for_ready(port, &bearer);
    read_one(port, &bearer);

    let before = counters(port, &bearer);
    read_one(port, &bearer);
    let served_read = differences(&before, &counters(port, &bearer));
    let params = serde_json::json!({"vault":notes(), "mode":"apply", "where":[{"op":"path","glob":"d0000.md"}], "changes":[{"change":"set", "field":"status", "value":"done"}]});
    assert_eq!(served.terminate().0.code(), Some(0));
    let settings = norn_config::machine::settings::ServingSettings::default();
    let dirs = fixture.dirs();
    let host = Host::new(
        RegistryRead::load(&dirs).unwrap(),
        ProductionEntryOps::new(
            dirs,
            ProductionPolicy::serving(
                settings.read_connections,
                settings.read_idle_grace,
                settings.count_result_max_bytes,
                settings.count_results_total_bytes,
            )
            .unwrap(),
        ),
        LifecyclePolicy {
            idle_after: settings.idle_after,
            worker_slots: norn_host::WORKER_SLOTS,
            watch_poll_interval: norn_host::WATCH_POLL_INTERVAL,
            read_settle_bound: norn_host::READ_SETTLE_BOUND,
            background_threads: settings.background_threads,
            background_turn_budget: settings.background_turn_budget,
        },
    )
    .unwrap();
    let name = VaultName::new("notes").unwrap();
    let _lease = host.demand(&name).unwrap();
    support::eventually("in-process ready", || {
        host.state(&name) == Ok(TrustState::Ready)
    });
    let get = GetParams::new(
        VaultAddress::name(name),
        ResolutionTarget::new("d0000.md").unwrap(),
    );
    host.get(&get).unwrap();
    let (answer, read) = host.tallied(|| host.get(&get));
    answer.unwrap();
    let set: SetParams = serde_json::from_value(params.clone()).unwrap();
    let (answer, apply) = host.tallied(|| host.set(set).unwrap().wait());
    answer.unwrap();
    let mut restore = params.clone();
    restore["changes"][0]["value"] = serde_json::json!("draft");
    host.set(serde_json::from_value(restore).unwrap())
        .unwrap()
        .wait()
        .unwrap();
    drop(_lease);
    drop(host);
    let port = free_port();
    let (served, bearer) = fixture.serve_on(port);
    read_one(port, &bearer);
    wait_for_ready(port, &bearer);
    let before = counters(port, &bearer);
    wire(port, &bearer, "set", params);
    let served_apply = differences(&before, &counters(port, &bearer));
    assert_eq!(served.terminate().0.code(), Some(0));
    for (served, process) in [(served_read, read), (served_apply, apply)] {
        eprintln!("served/in-process parity: {process:?}");
        assert_eq!(served["host.request.statements"], process.statements);
        assert_eq!(served["host.request.vm_steps"], process.vm_steps);
        assert_eq!(served["host.request.fs_reads"], process.fs_reads);
    }
}

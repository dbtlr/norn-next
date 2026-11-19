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

/// One reading of the counters route.
///
/// Every reading also holds the tool list to one derivation for the life of
/// the process. A delta cannot: a derivation made on every call reads the same
/// delta at every scale and repeats three times over three calls, so only the
/// absolute count tells derive-once from derive-per-call. Holding it here holds
/// it after start and after every measured window of every case.
fn counters(port: u16, bearer: &str) -> Counters {
    let reply = get(port, "/v1/debug/counters", Some(bearer));
    assert_eq!(reply.status, 200, "{reply:?}");
    let counters: Counters =
        serde_json::from_slice(&reply.body).expect("one object of name to integer");
    assert_eq!(
        counters["mcp.tool_list_derivations"], 1,
        "the tool list is derived once per process, at start"
    );
    counters
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
    wire_within(port, bearer, verb, body, std::time::Duration::from_secs(10))
}

/// [`wire`], waiting at most `within` for the answer.
fn wire_within(
    port: u16,
    bearer: &str,
    verb: &str,
    body: serde_json::Value,
    within: std::time::Duration,
) -> support::Reply {
    let reply = support::exchange_within(
        port,
        "POST",
        &format!("/v1/wire/{verb}"),
        Some(bearer),
        &serde_json::to_vec(&body).unwrap(),
        within,
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

fn register_tree(fixture: &Fixture, port: u16, bearer: &str, name: &str, count: usize) {
    register_tree_of(fixture, port, bearer, name, count, "body\n");
}

/// The page every document of a linked tree names, so the tree's link rows
/// grow with it while every link resolves.
const LINKED_HUB: &str = "index";

/// Register a tree of `count` documents that each link to [`LINKED_HUB`],
/// which the tree also holds: the link tables grow with the vault, one row per
/// document, so a lookup that scans them rather than seeking grows with it.
#[allow(clippy::disallowed_methods)] // Generated vault fixture, never a user tree.
fn register_linked_tree(fixture: &Fixture, port: u16, bearer: &str, count: usize) {
    let root = fixture.sandbox.work_dir().join("notes");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join(format!("{LINKED_HUB}.md")), "# Index\n").unwrap();
    register_tree_of(
        fixture,
        port,
        bearer,
        "notes",
        count,
        &format!("body [[{LINKED_HUB}]]\n"),
    );
}

/// How many documents of the `notes` vault link to [`LINKED_HUB`]: the
/// growing control a linked tree's flat bars stand beside.
fn linked_to_hub(port: u16, bearer: &str) -> u64 {
    let reply = route(
        true,
        port,
        bearer,
        "count",
        serde_json::json!({"vault":notes(), "predicates":[{"op":"links_to","target":LINKED_HUB}], "by":[], "limit":null, "after":null}),
    );
    let answer: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
    answer["report"]["rows"][0]["count"]
        .as_u64()
        .unwrap_or_else(|| panic!("a count of the hub's referrers: {answer}"))
}

#[allow(clippy::disallowed_methods)] // Generated vault fixture, never a user tree.
fn register_tree_of(
    fixture: &Fixture,
    port: u16,
    bearer: &str,
    name: &str,
    count: usize,
    body: &str,
) {
    let root = fixture.sandbox.work_dir().join(name);
    std::fs::create_dir_all(&root).unwrap();
    for n in 0..count {
        std::fs::write(
            root.join(format!("d{n:04}.md")),
            format!("---\nstatus: draft\n---\n# Note\n{body}"),
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

/// A read of the settled vault: its answer must carry Ready trust, so its
/// bytes and work are those of a settled read and not of an early answer.
fn read_settled(port: u16, bearer: &str) {
    let reply = read_one(port, bearer);
    let answer: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
    assert_eq!(
        answer["reading"]["trust"],
        serde_json::json!({"state":"ready"}),
        "a measured read must not be an early answer"
    );
}

/// The route's one gauge: a reading at an instant, not a cumulative count, so
/// it may fall between two readings and has no delta.
const GAUGES: [&str; 1] = ["serve.alive_tasks"];

/// What each cumulative counter moved by between two readings.
fn differences(before: &Counters, after: &Counters) -> Counters {
    after
        .iter()
        .filter(|(name, _)| !GAUGES.contains(&name.as_str()))
        .map(|(name, value)| (name.clone(), value - before[name]))
        .collect()
}

/// Drain every page, then apply to every document: a deliberately unbounded
/// control whose dispatches, statements, VM steps, encoded bytes and filesystem
/// work must grow with the vault. Authentication grows with its page count.
fn growing_control(port: u16, bearer: &str, documents: usize, mcp: bool) -> Counters {
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
        let reply = route(
            mcp,
            port,
            bearer,
            "find",
            serde_json::to_value(params).unwrap(),
        );
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
    // The apply's work grows with the vault by design, so its answer may
    // outlast the default window on a loaded debug build. The window bounds
    // only how long a hung host can stall the test; it measures nothing.
    let reply = route_within(
        mcp,
        port,
        bearer,
        "set",
        serde_json::json!({"vault":notes(), "mode":"apply", "where":[{"op":"path","glob":"d*.md"}], "changes":[{"change":"set", "field":"status", "value":"controlled"}]}),
        std::time::Duration::from_secs(60),
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
    counter_bars(false);
}

#[test]
fn mcp_counters_hold_the_native_bars() {
    counter_bars(true);
}

fn counter_bars(mcp: bool) {
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
        // The first read starts the attach. An answer before Ready is an early
        // answer: it carries the warming trust with its phase and progress, so
        // its bytes differ from a settled read's. The bar measures the settled
        // vault, so it waits for Ready, and every measured read checks it.
        read_one(port, &bearer);
        wait_for_ready(port, &bearer);
        settle_token_file(&fixture.tokens_file());
        let idle: Vec<_> = (0..connections)
            .map(|_| std::net::TcpStream::connect(("127.0.0.1", port)).unwrap())
            .collect();
        let before = counters(port, &bearer);
        read_route(mcp, port, &bearer);
        let after = counters(port, &bearer);
        let measured = differences(&before, &after);
        eprintln!(
            "documents={documents} registrations={registrations} tokens={tokens} connections={connections}: {measured:?}"
        );
        if mcp {
            let before = counters(port, &bearer);
            read_settled(port, &bearer);
            let native = differences(&before, &counters(port, &bearer));
            for name in [
                "host.request.statements",
                "host.request.vm_steps",
                "host.request.fs_reads",
                "serve.token_fingerprints",
                "serve.token_file_reads",
                "serve.token_lookups",
            ] {
                assert_eq!(measured[name], native[name], "route parity {name}");
            }
            assert_eq!(measured["mcp.messages"], 1);
            assert_eq!(measured["mcp.calls"], 1);
        }
        let mut names = vec![
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
        if mcp {
            names.extend(["mcp.messages", "mcp.calls", "mcp.tool_list_derivations"]);
        }
        let costs: Vec<_> = names.iter().map(|name| measured[*name]).collect();
        if let Some(expected) = &baseline {
            assert_eq!(&costs, expected);
        } else {
            baseline = Some(costs);
        }
        let before = counters(port, &bearer);
        for _ in 0..3 {
            read_route(mcp, port, &bearer);
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
        route(
            mcp,
            port,
            &bearer,
            "set",
            serde_json::json!({"vault":notes(), "mode":"apply", "where":[{"op":"path","glob":"d0000.md"}], "changes":[{"change":"set", "field":"status", "value":"done"}]}),
        );
        let applied = differences(&before, &counters(port, &bearer));
        if mcp {
            wire(
                port,
                &bearer,
                "set",
                serde_json::json!({"vault":notes(),"mode":"apply","path":"d0000.md","changes":[{"change":"set","field":"status","value":"draft"}]}),
            );
            let before = counters(port, &bearer);
            wire(
                port,
                &bearer,
                "set",
                serde_json::json!({"vault":notes(),"mode":"apply","where":[{"op":"path","glob":"d0000.md"}],"changes":[{"change":"set","field":"status","value":"done"}]}),
            );
            let native = differences(&before, &counters(port, &bearer));
            for name in [
                "host.request.statements",
                "host.request.vm_steps",
                "host.request.fs_reads",
            ] {
                assert_eq!(applied[name], native[name], "apply route parity {name}");
            }
        }
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
        route(mcp, port, &bearer, "vault_list", serde_json::json!({}));
        let listed = differences(&before, &counters(port, &bearer));
        assert_eq!(
            listed["host.request.registrations_visited"],
            registrations as u64
        );
        let control = growing_control(port, &bearer, documents, mcp);
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
    read_settled(port, &bearer);

    let before = counters(port, &bearer);
    read_settled(port, &bearer);
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

/// Route-independent controls compare the same report and request counters.
// Empty vault-list reports measured 20 bytes: envelopes were 136 bytes on
// 2025-11-25 and 256 on 2026-07-28, residuals 76 and 196 after 3x report.
// Round the larger residual up to 256, leaving 60 bytes for build identity.
const MCP_BODY_ALLOWANCE: usize = 256;

fn route(
    mcp: bool,
    port: u16,
    bearer: &str,
    verb: &str,
    body: serde_json::Value,
) -> support::Reply {
    route_within(
        mcp,
        port,
        bearer,
        verb,
        body,
        std::time::Duration::from_secs(60),
    )
}

fn route_within(
    mcp: bool,
    port: u16,
    bearer: &str,
    verb: &str,
    mut body: serde_json::Value,
    within: std::time::Duration,
) -> support::Reply {
    if !mcp {
        return wire_within(port, bearer, verb, body, within);
    }
    let preview = body.get("mode").and_then(serde_json::Value::as_str) == Some("preview");
    body.as_object_mut().unwrap().remove("mode");
    let name = if preview {
        format!("{verb}_preview")
    } else {
        verb.to_string()
    };
    let mut reply = support::exchange_with(port, "POST", "/mcp", Some(bearer),
        &[("MCP-Protocol-Version", "2025-11-25")],
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":body}}).to_string().as_bytes(), within);
    assert_eq!(reply.status, 200, "{reply:?}");
    assert!(reply.header("Mcp-Session-Id").is_none());
    let result: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
    assert_ne!(result["result"]["isError"], true, "{result}");
    assert!(
        result["result"]["structuredContent"].is_object(),
        "{result}"
    );
    let report = serde_json::to_vec(&result["result"]["structuredContent"]).unwrap();
    assert!(
        reply.body.len() <= 3 * report.len() + MCP_BODY_ALLOWANCE,
        "encoded {} report {}",
        reply.body.len(),
        report.len()
    );
    reply.body = report;
    reply
}

fn read_route(mcp: bool, port: u16, bearer: &str) {
    let reply = route(
        mcp,
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
    assert_eq!(answer["report"]["document"]["path"], "d0000.md");
    assert_eq!(
        answer["reading"]["trust"],
        serde_json::json!({"state":"ready"})
    );
}

/// The complete finding stream at fixed defect density, including an ambiguity
/// class whose membership grows with the fixture. Paging must not hide payload.
#[test]
#[allow(clippy::disallowed_methods)] // Generated fixed-density defect fixture.
fn finding_payload_is_bounded_over_mcp() {
    let mut small = None;
    for size in [16, 64] {
        let (fixture, served, port, bearer) = host_over("mcp-findings", 1);
        register_tree(&fixture, port, &bearer, "notes", size);
        let root = fixture.sandbox.work_dir().join("notes");
        for n in 0..size {
            let dir = root.join(format!("class{n:04}"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("ambiguous.md"), "# Same name\n").unwrap();
            std::fs::write(
                root.join(format!("d{n:04}.md")),
                "# Referrer\n[[ambiguous]]\n",
            )
            .unwrap();
        }
        read_one(port, &bearer);
        wait_for_ready(port, &bearer);
        let mut after = None;
        let mut bytes = 0;
        let mut findings = 0;
        let mut seen = std::collections::BTreeSet::new();
        loop {
            let mut params = norn_wire::ValidateParams::new(norn_wire::VaultAddress::name(
                norn_wire::VaultName::new("notes").unwrap(),
            ))
            .with_limit(8);
            if let Some(cursor) = after.take() {
                params = params.with_after(cursor);
            }
            let before = counters(port, &bearer);
            let reply = route(
                true,
                port,
                &bearer,
                "validate",
                serde_json::to_value(params).unwrap(),
            );
            let work = differences(&before, &counters(port, &bearer));
            assert_eq!(work["mcp.calls"], 1);
            bytes += work["serve.body_bytes_encoded"];
            let answer: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
            findings += answer["report"]["page"]["rows"].as_array().unwrap().len();
            after = serde_json::from_value(answer["report"]["page"]["next"].clone()).unwrap();
            if let Some(cursor) = &after {
                assert!(
                    seen.insert(serde_json::to_string(cursor).unwrap()),
                    "validation repeated a cursor"
                );
            } else {
                break;
            }
        }
        assert!(
            findings >= size,
            "the planted ambiguous links yield findings"
        );
        eprintln!("finding payload size={size} findings={findings} bytes={bytes}");
        if let Some((old_findings, old_bytes)) = small {
            assert!(findings > old_findings);
            assert!(bytes > old_bytes);
            assert!(
                bytes <= old_bytes * 4 + 4096,
                "linear payload: {old_bytes} -> {bytes}"
            );
        } else {
            small = Some((findings, bytes));
        }
        assert_eq!(served.terminate().0.code(), Some(0));
    }
}

/// After K stateless MCP cycles in each revision, the host's descriptors, OS
/// threads and alive runtime tasks equal their values after one cycle, and no
/// answer carries a session id.
#[test]
#[cfg(target_os = "linux")]
#[allow(clippy::disallowed_methods)] // /proc of this test's owned child.
fn mcp_cycles_retain_no_descriptors_or_tasks() {
    use std::io::{Read, Write};
    let fixture = Fixture::new("mcp-cycles");
    fixture.write_settings("[serve]\nidle_after = \"1s\"\n");
    let port = free_port();
    let (server, bearer) = fixture.serve_on(port);
    // Descriptors and OS threads from /proc, and the serving runtime's alive
    // tasks from the counters route: a task a call leaves pending holds no
    // thread and may hold no descriptor, so only the runtime's own count sees
    // it. The read is itself a request, so each reading counts its own
    // connection task, the same one in every reading.
    let counts = || {
        let [fds, threads] = ["fd", "task"].map(|kind| {
            std::fs::read_dir(format!("/proc/{}/{kind}", server.child.id()))
                .unwrap()
                .count()
        });
        let tasks = counters(port, &bearer)["serve.alive_tasks"] as usize;
        [fds, threads, tasks]
    };
    // Startup uses a Tokio blocking worker with a 10 s idle lifetime. It
    // retires independently of requests; start the cycle baseline after that
    // lifetime, rather than counting its retirement as a change in MCP state.
    assert_eq!(hello(port, "default").status, 200);
    std::thread::sleep(std::time::Duration::from_secs(11));
    let idle = counts();
    register_tree(&fixture, port, &bearer, "notes", 16);
    read_one(port, &bearer);
    wait_for_ready(port, &bearer);
    for revision in ["2025-11-25", "2026-07-28"] {
        let mut baseline = None;
        for cycle in 0..16 {
            let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(60)))
                .unwrap();
            let methods = if revision == "2025-11-25" {
                vec![
                    "initialize",
                    "notifications/initialized",
                    "tools/list",
                    "tools/call",
                ]
            } else {
                vec!["server/discover", "tools/list", "tools/call"]
            };
            for method in methods {
                let mut params = match method {
                    "initialize" => {
                        serde_json::json!({"protocolVersion":revision,"capabilities":{},"clientInfo":{"name":"cycles","version":"1"}})
                    }
                    "tools/call" => {
                        serde_json::json!({"name":"get","arguments":norn_wire::GetParams::new(
                        norn_wire::VaultAddress::name(norn_wire::VaultName::new("notes").unwrap()),
                        norn_wire::ResolutionTarget::new("d0000.md").unwrap())})
                    }
                    _ => serde_json::json!({}),
                };
                let mut headers = format!("MCP-Protocol-Version: {revision}\r\n");
                if revision == "2026-07-28" {
                    params["_meta"] = serde_json::json!({"io.modelcontextprotocol/protocolVersion":revision,"io.modelcontextprotocol/clientCapabilities":{}});
                    headers.push_str(&format!("Mcp-Method: {method}\r\n"));
                    if method == "tools/call" {
                        headers.push_str("Mcp-Name: get\r\n");
                    }
                }
                let mut body = serde_json::json!({"jsonrpc":"2.0","method":method,"params":params});
                if method != "notifications/initialized" {
                    body["id"] = serde_json::json!(cycle);
                }
                let body = body.to_string();
                write!(stream, "POST /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {bearer}\r\n{headers}Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
                let mut raw = Vec::new();
                let mut byte = [0];
                while !raw.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    raw.push(byte[0]);
                }
                let mut reply = support::parse(&raw).unwrap();
                assert!(reply.header("Mcp-Session-Id").is_none());
                assert_eq!(
                    reply.status,
                    if method == "notifications/initialized" {
                        202
                    } else {
                        200
                    },
                    "{reply:?}"
                );
                let length = reply.header("content-length").unwrap().parse().unwrap();
                reply.body = vec![0; length];
                stream.read_exact(&mut reply.body).unwrap();
                if reply.status == 200 {
                    let value: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
                    assert!(value.get("result").is_some(), "{value}");
                    assert_ne!(value["result"]["isError"], true, "{value}");
                    if method == "tools/call" {
                        let report =
                            serde_json::to_vec(&value["result"]["structuredContent"]).unwrap();
                        assert!(reply.body.len() <= 3 * report.len() + MCP_BODY_ALLOWANCE);
                    }
                }
            }
            drop(stream);
            // Compare the same unattached phase as the native cycle bar: an
            // attached reader may retain descriptors inside its idle grace.
            support::eventually("MCP cycle detached", || {
                let reply = wire(
                    port,
                    &bearer,
                    "vault_status",
                    serde_json::json!({"vault":notes()}),
                );
                let status: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
                status["status"]["published"]["state"]["state"] == "unattached"
            });
            support::eventually("MCP cycle cleanup joined", || counts() == idle);
            eprintln!("MCP cycle {revision} {cycle}: {:?}", counts());
            if let Some(expected) = baseline {
                support::eventually("MCP cycle resources returned", || counts() == expected);
            } else {
                std::thread::sleep(std::time::Duration::from_millis(100));
                baseline = Some(counts());
            }
        }
        eprintln!(
            "MCP cycles revision={revision} count=16 resources={:?}",
            counts()
        );
    }
    assert_eq!(server.terminate().0.code(), Some(0));
}

/// Empty reports expose the envelope allowance; growing escaped reports in the
/// growth and cycle cases exercise the multiplicative term.
#[test]
fn mcp_report_encoding_is_bounded() {
    let (_fixture, server, port, bearer) = host_over("mcp-bytes", 1);
    for revision in ["2025-11-25", "2026-07-28"] {
        let mut params = serde_json::json!({"name":"vault_list","arguments":{}});
        let mut headers = vec![("MCP-Protocol-Version", revision)];
        if revision == "2026-07-28" {
            params["_meta"] = serde_json::json!({"io.modelcontextprotocol/protocolVersion":revision,"io.modelcontextprotocol/clientCapabilities":{}});
            headers.extend([("Mcp-Method", "tools/call"), ("Mcp-Name", "vault_list")]);
        }
        let before = counters(port, &bearer);
        let reply = support::exchange_with(
            port,
            "POST",
            "/mcp",
            Some(&bearer),
            &headers,
            serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":params})
                .to_string()
                .as_bytes(),
            std::time::Duration::from_secs(60),
        );
        assert_eq!(reply.status, 200);
        let answer: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
        assert_ne!(answer["result"]["isError"], true, "{answer}");
        assert!(answer["result"]["structuredContent"].is_object());
        let measured = differences(&before, &counters(port, &bearer));
        let body = measured["serve.body_bytes_encoded"];
        let report = measured["serve.report_bytes"];
        assert!(report > 0);
        assert_eq!(body, reply.body.len() as u64);
        assert!(body <= 3 * report + MCP_BODY_ALLOWANCE as u64);
        eprintln!(
            "MCP bytes revision={revision} encoded={body} report={report} residual={}",
            body.saturating_sub(3 * report)
        );
    }
    assert_eq!(server.terminate().0.code(), Some(0));
}

fn address() -> norn_wire::VaultAddress {
    norn_wire::VaultAddress::name(norn_wire::VaultName::new("notes").unwrap())
}

/// The `host.jobs.*` members of a reading: the work no one request owns.
fn jobs(counters: &Counters) -> Counters {
    counters
        .iter()
        .filter(|(name, _)| name.starts_with("host.jobs."))
        .map(|(name, value)| (name.clone(), *value))
        .collect()
}

/// Wait until the job account stops moving: the attach and the background
/// work Ready leaves owed have finished, so a window after this measures only
/// what the requests inside it cost.
fn settle_jobs(port: u16, bearer: &str) {
    let mut last = jobs(&counters(port, bearer));
    support::eventually("the job account settled", || {
        std::thread::sleep(std::time::Duration::from_millis(250));
        let now = jobs(&counters(port, bearer));
        std::mem::replace(&mut last, now) == last
    });
}

/// One served MCP call of `tool` with `arguments`, and what it cost.
fn mcp_cost(port: u16, bearer: &str, tool: &str, arguments: serde_json::Value) -> Counters {
    let before = counters(port, bearer);
    route(true, port, bearer, tool, arguments);
    differences(&before, &counters(port, bearer))
}

fn find(
    predicates: serde_json::Value,
    columns: serde_json::Value,
    limit: u32,
) -> serde_json::Value {
    serde_json::json!({"vault":notes(), "predicates":predicates, "columns":columns, "limit":limit})
}

#[allow(clippy::disallowed_methods)] // Generated vault fixture, never a user tree.
fn write_into(fixture: &Fixture, files: &[(String, String)]) {
    let root = fixture.sandbox.work_dir().join("notes");
    for (path, text) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
}

/// **A warm request derives nothing.** On a Ready vault whose job account has
/// settled, warm MCP reads leave every `host.jobs.*` member unchanged and open
/// and stat nothing on their threads. A tool that reads no vault state costs no
/// statement, no VM step, no filesystem read and no serving-set entry, and an
/// answer from constants costs no host request and no dispatch: the round-trip
/// floor and nothing more, in counts. The floor's clock is the nightly's MCP
/// overhead report, not this suite's (ADR 0004).
///
/// **Freshness is off the request path, and costs nothing per unowned entry.**
/// The same vault is served beside a dot-led subtree of one entry and of 256,
/// which the vault model does not own. The settled job account — the attach's
/// walk and stats among it — and every warm read's request work are the same
/// at both. Files that are not documents are not clutter for this purpose: the
/// model keeps them as vault files a link can name, so the walk sees each one.
#[test]
fn mcp_warm_requests_derive_nothing() {
    let mut tallies: Option<(Counters, Counters)> = None;
    for unowned in [1, 256] {
        let (fixture, served, port, bearer) = host_over("mcp-warm", 1);
        register_tree(&fixture, port, &bearer, "notes", 16);
        let clutter: Vec<_> = (0..unowned)
            .map(|n| (format!(".git/objects/o{n:04}"), "object".to_string()))
            .collect();
        write_into(&fixture, &clutter);
        read_one(port, &bearer);
        wait_for_ready(port, &bearer);
        settle_jobs(port, &bearer);
        let settled = jobs(&counters(port, &bearer));
        assert_eq!(settled["host.jobs.from_zero_builds"], 1);

        let before = counters(port, &bearer);
        let get = serde_json::to_value(norn_wire::GetParams::new(
            address(),
            norn_wire::ResolutionTarget::new("d0000.md").unwrap(),
        ))
        .unwrap();
        let mut reads = Counters::new();
        for (tool, arguments) in [
            ("get", get.clone()),
            (
                "find",
                find(serde_json::json!([]), serde_json::json!([]), 8),
            ),
            (
                "count",
                serde_json::to_value(norn_wire::CountParams::new(address())).unwrap(),
            ),
            (
                "describe",
                serde_json::to_value(norn_wire::DescribeParams::new(address())).unwrap(),
            ),
        ] {
            let cost = mcp_cost(port, &bearer, tool, arguments);
            eprintln!("warm unowned={unowned} {tool}: {cost:?}");
            assert_eq!(cost["host.request.fs_reads"], 0, "{tool} read the vault");
            for name in [
                "host.request.statements",
                "host.request.vm_steps",
                "host.request.fs_reads",
            ] {
                reads.insert(format!("{tool} {name}"), cost[name]);
            }
        }
        let window = differences(&before, &counters(port, &bearer));
        let derived: Counters = jobs(&window)
            .into_iter()
            .filter(|(_, value)| *value != 0)
            .collect();
        assert!(derived.is_empty(), "warm requests derived: {derived:?}");
        assert!(
            !jobs(&window).is_empty(),
            "the route reports the job account"
        );
        if let Some((expected_settled, expected_reads)) = &tallies {
            assert_eq!(
                &settled, expected_settled,
                "unowned entries moved the job account"
            );
            assert_eq!(
                &reads, expected_reads,
                "unowned entries moved a warm read's work"
            );
        } else {
            tallies = Some((settled, reads));
        }

        // A tool that reads no vault state.
        let listed = mcp_cost(port, &bearer, "vault_list", serde_json::json!({}));
        eprintln!("warm unowned={unowned} vault_list: {listed:?}");
        for name in [
            "host.request.statements",
            "host.request.vm_steps",
            "host.request.fs_reads",
            "host.request.entries_visited",
        ] {
            assert_eq!(listed[name], 0, "vault_list {name}");
        }
        assert_eq!(listed["serve.dispatches"], 1);
        // Answers from constants.
        let before = counters(port, &bearer);
        for method in ["initialize", "tools/list"] {
            let params = if method == "initialize" {
                serde_json::json!({"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"warm","version":"1"}})
            } else {
                serde_json::json!({})
            };
            let reply = support::exchange_with(
                port,
                "POST",
                "/mcp",
                Some(&bearer),
                &[("MCP-Protocol-Version", "2025-11-25")],
                serde_json::json!({"jsonrpc":"2.0","id":1,"method":method,"params":params})
                    .to_string()
                    .as_bytes(),
                std::time::Duration::from_secs(60),
            );
            assert_eq!(reply.status, 200, "{reply:?}");
        }
        let constants = differences(&before, &counters(port, &bearer));
        assert_eq!(constants["host.request.requests"], 0);
        assert_eq!(constants["serve.dispatches"], 0);
        let derived: Counters = jobs(&constants)
            .into_iter()
            .filter(|(_, value)| *value != 0)
            .collect();
        assert!(derived.is_empty(), "constant answers derived: {derived:?}");
        assert_eq!(served.terminate().0.code(), Some(0));
    }
}

/// **A targeted lookup is indexed.** Every served re-entry point that asks
/// about one identifier runs the same statements, and the same VM steps, at 16
/// documents and at 256: resolving one target (`get`), asking which documents
/// link to one (`find` with `links_to`), membership of a key in k values
/// (`find` with `in`), and the collision checks `new` and `move` make, at a
/// fresh path and at a stem another document already holds, the move carrying
/// in-links. Every generated document links to one hub, so the link tables
/// grow with the vault as the documents table does, and a lookup that scans
/// either grows. The answers are the same size at both scales, so any growth
/// is the lookup.
#[test]
fn mcp_targeted_lookups_stay_flat_with_the_vault() {
    let mut baseline: Option<Counters> = None;
    for documents in [16, 256] {
        let (fixture, served, port, bearer) = host_over("mcp-targeted", 1);
        register_linked_tree(&fixture, port, &bearer, documents);
        write_into(
            &fixture,
            &["hub", "spoke"].map(|name| {
                (
                    format!("{name}.md"),
                    "---\nrole: referrer\n---\n# Referrer\n[[d0000]]\n".to_string(),
                )
            }),
        );
        read_one(port, &bearer);
        wait_for_ready(port, &bearer);
        settle_jobs(port, &bearer);
        // The growing control: every generated document is a link row, so a
        // scan of the link tables has rows to grow with.
        assert_eq!(linked_to_hub(port, &bearer), documents as u64);
        let mut costs = Counters::new();
        for (label, tool, arguments) in [
            (
                "resolve",
                "get",
                serde_json::to_value(norn_wire::GetParams::new(
                    address(),
                    norn_wire::ResolutionTarget::new("d0000").unwrap(),
                ))
                .unwrap(),
            ),
            (
                "backlinks",
                "find",
                find(
                    serde_json::json!([{"op":"links_to","target":"d0000"}]),
                    serde_json::json!([]),
                    8,
                ),
            ),
            (
                "membership",
                "find",
                find(
                    serde_json::json!([{"op":"in","key":"role","values":["referrer","absent","other"]}]),
                    serde_json::json!([]),
                    8,
                ),
            ),
            // The applied forms: a preview's planning reaches no request
            // tally today (its statements read zero), so a preview would hide
            // the collision lookups this bar exists to hold. Each scale is a
            // fresh host, so every apply meets the same state.
            (
                "new at a fresh path",
                "new",
                serde_json::json!({"vault":notes(), "path":"fresh.md", "content":"# Fresh\n"}),
            ),
            (
                "new at a held stem",
                "new",
                serde_json::json!({"vault":notes(), "path":"sub/d0001.md", "content":"# Held\n"}),
            ),
            (
                "move with in-links onto a held stem",
                "move",
                serde_json::json!({"vault":notes(), "from":"d0000.md", "to":"archive/d0003.md"}),
            ),
        ] {
            let cost = mcp_cost(port, &bearer, tool, arguments);
            eprintln!(
                "targeted documents={documents} {label}: statements={} vm_steps={} fs_reads={} report_bytes={}",
                cost["host.request.statements"],
                cost["host.request.vm_steps"],
                cost["host.request.fs_reads"],
                cost["serve.report_bytes"],
            );
            assert!(
                cost["host.request.statements"] + cost["host.request.fs_reads"] > 0,
                "{label} looked nothing up"
            );
            for name in [
                "host.request.statements",
                "host.request.vm_steps",
                "host.request.fs_reads",
                "serve.report_bytes",
            ] {
                costs.insert(format!("{label}: {name}"), cost[name]);
            }
        }
        if let Some(expected) = &baseline {
            let grew: Vec<_> = costs
                .iter()
                .filter(|(name, value)| expected[*name] != **value)
                .map(|(name, value)| format!("{name} {} -> {value}", expected[name]))
                .collect();
            assert!(
                grew.is_empty(),
                "targeted lookups grew with the vault: {grew:#?}"
            );
        } else {
            baseline = Some(costs);
        }
        assert_eq!(served.terminate().0.code(), Some(0));
    }
}

/// **Wire bytes for a body-less answer are independent of body bytes.** The
/// same sixteen documents, once with a one-line body and once with 32 KiB of
/// plain text under the same heading, answer a body-less `find` and a `count`
/// in the same report and the same encoded bytes; asking for the `body`
/// column is the growing control that proves the bodies differ.
#[test]
#[allow(clippy::disallowed_methods)] // Generated vault fixture, never a user tree.
fn mcp_bodyless_answers_ignore_body_bytes() {
    let filler = "plain words without markup ".repeat(32 * 1024 / 27);
    let mut small: Option<(Counters, u64)> = None;
    for body in ["body\n".to_string(), format!("{filler}\n")] {
        let (fixture, served, port, bearer) = host_over("mcp-projection", 1);
        let root = fixture.sandbox.work_dir().join("notes");
        std::fs::create_dir_all(&root).unwrap();
        for n in 0..16 {
            std::fs::write(
                root.join(format!("d{n:04}.md")),
                format!("---\nstatus: draft\n---\n# Note\n{body}"),
            )
            .unwrap();
        }
        wire(
            port,
            &bearer,
            "vault_register",
            serde_json::json!({"registration":{"name":"notes","root":root}}),
        );
        read_one(port, &bearer);
        wait_for_ready(port, &bearer);
        let mut bytes = Counters::new();
        for (label, tool, arguments) in [
            (
                "find",
                "find",
                find(
                    serde_json::json!([]),
                    serde_json::json!([{"col":"path"},{"col":"field","key":"status"},{"col":"headings"},{"col":"links"},{"col":"tags"}]),
                    16,
                ),
            ),
            (
                "count",
                "count",
                serde_json::to_value(norn_wire::CountParams::new(address())).unwrap(),
            ),
        ] {
            let cost = mcp_cost(port, &bearer, tool, arguments);
            eprintln!(
                "projection body={} {label}: report={} encoded={}",
                body.len(),
                cost["serve.report_bytes"],
                cost["serve.body_bytes_encoded"]
            );
            for name in ["serve.report_bytes", "serve.body_bytes_encoded"] {
                bytes.insert(format!("{label} {name}"), cost[name]);
            }
        }
        let control = mcp_cost(
            port,
            &bearer,
            "find",
            find(
                serde_json::json!([]),
                serde_json::json!([{"col":"body"}]),
                16,
            ),
        )["serve.report_bytes"];
        if let Some((expected, small_control)) = &small {
            assert_eq!(&bytes, expected, "body-less bytes moved with the bodies");
            assert!(
                control > small_control + 16 * 32 * 1000,
                "the body column grows: {small_control} -> {control}"
            );
        } else {
            small = Some((bytes, control));
        }
        assert_eq!(served.terminate().0.code(), Some(0));
    }
}

/// **No cost term multiplies two request-varying quantities.** A page's
/// statements are the same whatever its row count and whatever the vault's
/// size, its links column included, so no row costs a statement of its own;
/// its VM steps are the same at either vault size, so no row's lookup scans
/// the vault. Moving a folder of k documents, each named by one link, follows
/// the moved documents' own neighborhood: its statements and VM steps are the
/// same at 16 and at 256 unrelated documents, and its statements grow by the
/// same number for every document k adds, at k = 2, 4 and 6. The unrelated
/// documents each link to one hub, so the link tables grow with the vault and
/// a scan of them shows in the VM steps.
#[test]
fn mcp_request_cost_follows_one_quantity_at_a_time() {
    const FOLDERS: [(&str, usize); 3] = [("two", 2), ("four", 4), ("six", 6)];
    let mut pages: Option<Counters> = None;
    let mut moves: Option<Counters> = None;
    for documents in [16, 256] {
        let (fixture, served, port, bearer) = host_over("mcp-pairwise", 1);
        register_linked_tree(&fixture, port, &bearer, documents);
        // Folders of linked documents, k = 2, 4 and 6, each document named
        // by one referrer outside the folder.
        let mut files = Vec::new();
        for (folder, k) in FOLDERS {
            for n in 0..k {
                files.push((
                    format!("{folder}/{folder}{n}.md"),
                    format!("# {folder} {n}\n[[d0001]]\n"),
                ));
                files.push((
                    format!("refs/{folder}-ref{n}.md"),
                    format!("# Ref\n[[{folder}{n}]]\n"),
                ));
            }
        }
        write_into(&fixture, &files);
        read_one(port, &bearer);
        wait_for_ready(port, &bearer);
        settle_jobs(port, &bearer);
        assert_eq!(linked_to_hub(port, &bearer), documents as u64);
        let mut page_costs = Counters::new();
        for limit in [2, 8] {
            let before = counters(port, &bearer);
            let reply = route(
                true,
                port,
                &bearer,
                "find",
                find(
                    serde_json::json!([]),
                    serde_json::json!([{"col":"path"},{"col":"links"},{"col":"field","key":"status"}]),
                    limit,
                ),
            );
            let answer: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
            assert_eq!(
                answer["report"]["page"]["rows"].as_array().unwrap().len(),
                limit as usize,
                "{answer}"
            );
            let cost = differences(&before, &counters(port, &bearer));
            eprintln!(
                "pairwise documents={documents} limit={limit}: statements={} vm_steps={}",
                cost["host.request.statements"], cost["host.request.vm_steps"]
            );
            for name in ["statements", "vm_steps"] {
                page_costs.insert(
                    format!("limit {limit} {name}"),
                    cost[&format!("host.request.{name}")],
                );
            }
        }
        assert_eq!(
            page_costs["limit 2 statements"], page_costs["limit 8 statements"],
            "a page's statements grew with its rows"
        );
        let mut move_costs = Counters::new();
        for (folder, k) in FOLDERS {
            let cost = mcp_cost(
                port,
                &bearer,
                "move",
                serde_json::json!({"vault":notes(), "from":format!("{folder}/"), "to":format!("moved-{folder}/")}),
            );
            eprintln!(
                "pairwise documents={documents} move k={k}: statements={} vm_steps={} fs_reads={}",
                cost["host.request.statements"],
                cost["host.request.vm_steps"],
                cost["host.request.fs_reads"]
            );
            for name in ["statements", "vm_steps"] {
                move_costs.insert(
                    format!("k {k} {name}"),
                    cost[&format!("host.request.{name}")],
                );
            }
        }
        let statements = |k: usize| move_costs[&format!("k {k} statements")];
        assert!(statements(2) > 0);
        assert_eq!(
            statements(4) - statements(2),
            statements(6) - statements(4),
            "a folder move's statements are not linear in its documents: {move_costs:?}"
        );
        if let (Some(expected_pages), Some(expected_moves)) = (&pages, &moves) {
            assert_eq!(
                &page_costs, expected_pages,
                "a page's statements or VM steps grew with the vault"
            );
            assert_eq!(
                &move_costs, expected_moves,
                "a move's statements or VM steps grew with the vault"
            );
        } else {
            pages = Some(page_costs);
            moves = Some(move_costs);
        }
        assert_eq!(served.terminate().0.code(), Some(0));
    }
}

//! Real serving-process peaks: the measuring launcher reaps the server,
//! while the parent generates the fixture and drives the HTTP workload.
#![allow(clippy::disallowed_methods)] // Generated trees and isolated child state.

mod support;

use norn_testkit::process::Run;
use serde_json::json;
use std::time::Duration;
use support::{Fixture, exchange, free_port};

fn call(port: u16, bearer: &str, verb: &str, body: serde_json::Value) -> serde_json::Value {
    let reply = exchange(
        port,
        "POST",
        &format!("/v1/wire/{verb}"),
        Some(bearer),
        &serde_json::to_vec(&body).unwrap(),
    );
    assert_eq!(reply.status, 200, "{verb}: {}", reply.body_text());
    serde_json::from_slice(&reply.body).unwrap()
}

/// Generation, installation and HTTP-client allocations belong to the parent,
/// never to the measured child. A null child uses exactly the same executable
/// and launcher and reaches listening without registering any vault.
fn peak(profile: Option<&str>, workload: &str) -> u64 {
    let fixture = Fixture::new("serve-memory");
    let root = fixture.sandbox.work_dir().join("vault");
    if let Some(profile) = profile {
        norn_fixtures::generate(&norn_fixtures::Profile::by_name(profile).unwrap(), 7, &root)
            .unwrap();
        std::fs::create_dir_all(root.join(".norn")).unwrap();
        std::fs::write(root.join(".norn/schema.yaml"), "version: 1\ntags:\n  declared: [research, writing, infra, reading, personal, planning, review, reference]\n  undeclared: report\n").unwrap();
        std::fs::write(
            root.join("memory-gate-probe.md"),
            "---\nstatus: draft\n---\n# Probe\nbody\n",
        )
        .unwrap();
    }
    if profile.is_some() {
        std::fs::write(
            root.join("memory-gate-referrer.md"),
            "# Referrer\n[[memory-gate-probe]]\n",
        )
        .unwrap();
    }
    let port = free_port();
    let result = Run::new(&fixture.sandbox, &fixture.binary)
        .args(["serve", "--port", &port.to_string()])
        .deadline(Duration::from_secs(240))
        .wait_controlled(|group| {
            support::eventually("serve listening", || std::net::TcpStream::connect(("127.0.0.1", port)).is_ok());
            let bearer = fixture.default_bearer();
            if profile.is_some() {
                call(port, &bearer, "vault_register", json!({"registration":{"name":"notes", "root":root}}));
                let address = norn_wire::VaultAddress::name(norn_wire::VaultName::new("notes").unwrap());
                let get = norn_wire::GetParams::new(address.clone(), norn_wire::ResolutionTarget::new("memory-gate-probe.md").unwrap());
                call(port, &bearer, "get", serde_json::to_value(&get).unwrap());
                let until = std::time::Instant::now() + Duration::from_secs(180);
                loop {
                    let status = call(port, &bearer, "vault_status", json!({"vault":address}));
                    if status["status"]["published"]["state"]["state"] == "ready" { break; }
                    assert!(std::time::Instant::now() < until, "attachment did not reach Ready: {status}");
                    std::thread::sleep(Duration::from_millis(20));
                }
                if workload == "read" {
                    use norn_wire::{Column, Direction, FindParams, Predicate, ResolutionTarget, Sort, SortKey};
                    let find = FindParams::new(address.clone())
                        .with_predicates([Predicate::not_equal_to("type", "meeting")])
                        .with_columns([Column::fields(), Column::tags(), Column::links()])
                        .with_sort(Sort::new(SortKey::field("created"), Direction::Descending))
                        .with_limit(25);
                    let mut after = None;
                    let mut found = 0;
                    for _ in 0..8 {
                        let mut page = find.clone();
                        if let Some(cursor) = after.take() { page = page.with_after(cursor); }
                        let answer = call(port, &bearer, "find", serde_json::to_value(page).unwrap());
                        assert!(answer["unsatisfied"].as_array().is_none_or(Vec::is_empty), "{answer}");
                        found += answer["report"]["page"]["rows"].as_array().unwrap().len();
                        after = serde_json::from_value(answer["report"]["page"]["next"].clone()).unwrap();
                        if after.is_none() { break; }
                    }
                    assert_eq!(found, 200, "eight full find pages ran");
                    let target = || ResolutionTarget::new("memory-gate-probe.md").unwrap();
                    for (shape, verb, params) in [
                        ("count", "count", serde_json::to_value(norn_wire::CountParams::new(address.clone()).with_by([norn_wire::GroupKey::field("type")]).with_limit(25)).unwrap()),
                        ("get", "get", serde_json::to_value(&get).unwrap()),
                        ("links-to", "count", serde_json::to_value(norn_wire::CountParams::new(address.clone()).with_predicates([Predicate::links_to(target())])).unwrap()),
                        ("backlinks", "find", serde_json::to_value(FindParams::new(address.clone()).with_predicates([Predicate::links_to(target())]).with_limit(25)).unwrap()),
                        ("validate", "validate", serde_json::to_value(norn_wire::ValidateParams::new(address.clone()).with_limit(25)).unwrap()),
                        ("search", "search", serde_json::to_value(norn_wire::SearchParams::new(address.clone(), "baseline").with_limit(25)).unwrap()),
                        ("describe", "describe", serde_json::to_value(norn_wire::DescribeParams::new(address.clone()).with_limit(25)).unwrap()),
                    ] {
                        let answer = call(port, &bearer, verb, params);
                        assert!(answer["unsatisfied"].as_array().is_none_or(Vec::is_empty), "{shape}: {answer}");
                        if shape == "links-to" {
                            let counted: u64 = answer["report"]["rows"].as_array().unwrap().iter().map(|row| row["count"].as_u64().unwrap()).sum();
                            assert_eq!(counted, 1, "the planted link is counted");
                        } else if shape == "backlinks" {
                            let rows = answer["report"]["page"]["rows"].as_array().unwrap();
                            assert_eq!(rows.len(), 1);
                            assert_eq!(rows[0]["path"], "memory-gate-referrer.md");
                        } else if shape == "get" {
                            assert_eq!(answer["report"]["document"]["path"], "memory-gate-probe.md");
                        } else if shape == "count" {
                            assert!(answer["report"]["rows"].as_array().unwrap().len() >= 6, "all six generated document types are grouped");
                        } else if shape == "search" || shape == "validate" {
                            assert_eq!(answer["report"]["page"]["rows"].as_array().unwrap().len(), 25, "{shape} filled its page");
                        } else if shape == "describe" {
                            assert_eq!(answer["report"]["rows"].as_array().unwrap().len(), 15, "all fixture facets answered");
                        }
                    }
                } else if workload == "plan" {
                    let answer = call(port, &bearer, "set", json!({"vault":address, "mode":"preview", "path":"memory-gate-probe.md", "changes":[{"change":"set", "field":"status", "value":"done"}]}));
                    assert!(answer.to_string().contains("resolved"), "a plan was actually built: {answer}");
                }
            }
            // SAFETY: this is the owned, still-running measuring group. Its
            // launcher shields SIGTERM and accounts for the server it reaps.
            assert_eq!(unsafe { libc::kill(-group, libc::SIGTERM) }, 0);
        }).unwrap();
    result.assert_success();
    eprintln!(
        "serve {profile:?} {workload}: {} peak RSS bytes",
        result.peak_rss_bytes
    );
    result.peak_rss_bytes
}

/// Measured over the eight-shape read mix, including eight rich find pages:
/// null 25,743,360 bytes; above-null attach/read/plan at the 300-document
/// profile plus two probes [18,690,048, 30,072,832, 19,836,928], and at the
/// 2,000-document profile plus two probes
/// [20,070,400, 33,009,664, 21,483,520]. Read includes report encoding.
/// The ceilings leave 5–9 MiB for process layout and allocator variation.
/// The attach pair also limits absolute growth, so a high small-profile
/// peak cannot hide large-profile retention.
const CEILINGS: [u64; 3] = [24 * 1024 * 1024, 40 * 1024 * 1024, 28 * 1024 * 1024];
const ATTACH_RATIO_PER_MILLE: u64 = 1250; // measured 1074
const ATTACH_GROWTH: u64 = 4 * 1024 * 1024; // measured 1,380,352 bytes
const READ_OVER_ATTACH_PER_MILLE: u64 = 1800; // measured 1645

fn readings() -> &'static [[u64; 3]; 2] {
    static READINGS: std::sync::OnceLock<[[u64; 3]; 2]> = std::sync::OnceLock::new();
    READINGS.get_or_init(|| {
        let null = (0..3).map(|_| peak(None, "null")).min().unwrap();
        let peaks = ["ambiguous", "realistic"].map(|profile| {
            ["attach", "read", "plan"].map(|workload| {
                peak(Some(profile), workload)
                    .checked_sub(null)
                    .expect("subject exceeds null")
            })
        });
        eprintln!("serve peaks above null {null}: {peaks:?}");
        peaks
    })
}

#[test]
#[ignore = "memory-lane case: runs alone in the memory job"]
fn real_serving_attach_read_and_plan_stay_under_their_peak_rss_ceilings() {
    for peaks in readings() {
        for (bytes, ceiling) in peaks.iter().zip(CEILINGS) {
            assert!(
                *bytes > 0 && *bytes <= ceiling,
                "{bytes} peak above null exceeds {ceiling}"
            );
        }
    }
}

#[test]
#[ignore = "memory-lane case: runs alone in the memory job"]
fn peak_memory_holds_flat_from_the_ambiguity_profile_to_the_gate_profile() {
    let [small, large] = readings().map(|peaks| peaks[0]);
    assert!(
        large * 1000 <= small * ATTACH_RATIO_PER_MILLE,
        "attach pair {small} -> {large}"
    );
    assert!(
        large <= small + ATTACH_GROWTH,
        "attach growth {small} -> {large}"
    );
}

#[test]
#[ignore = "memory-lane case: runs alone in the memory job"]
fn reading_the_gate_profile_holds_the_process_near_its_attach_peak() {
    let [attach, read, _] = readings()[1];
    assert!(
        read * 1000 <= attach * READ_OVER_ATTACH_PER_MILLE,
        "read over attach {attach} -> {read}"
    );
}

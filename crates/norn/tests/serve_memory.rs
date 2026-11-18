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
    let mut resident = None;
    let result = Run::new(&fixture.sandbox, &fixture.binary)
        .args(["serve", "--port", &port.to_string()])
        .deadline(Duration::from_secs(240))
        .wait_controlled(|group| {
            // The kernel completes a connection once the socket listens, which
            // is before the host leaves its start, and a SIGTERM in that window
            // ends the start with exit 1. Only an answered request proves the
            // host is serving; an unauthenticated one costs it the least.
            support::eventually("serve listening", || std::net::TcpStream::connect(("127.0.0.1", port)).is_ok());
            assert_eq!(exchange(port, "GET", "/", None, b"").status, 401, "the host answers before it is measured");
            let prefaulted = prefault_the_server(group);
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
            resident = prefaulted.map(|(server, after_prefault)| {
                let at_end = norn_testkit::image::residency_of(server).unwrap().unwrap();
                norn_testkit::image::ResidencyReport { after_prefault, at_end }
                    .wholly_resident()
                    .unwrap_or_else(|problem| panic!("serve {profile:?} {workload}: {problem}"))
            });
            // SAFETY: this is the owned, still-running measuring group. Its
            // launcher shields SIGTERM and accounts for the server it reaps.
            assert_eq!(unsafe { libc::kill(-group, libc::SIGTERM) }, 0);
        }).unwrap();
    result.assert_success();
    eprintln!(
        "serve {profile:?} {workload}: {} peak RSS bytes, executable {resident:?} bytes resident",
        result.peak_rss_bytes
    );
    result.peak_rss_bytes
}

/// Make the server's executable wholly resident, as every `norn-host` memory
/// child makes its own, and hand back the server's pid and its residency.
///
/// **Without it the code term depends on the page cache, not on the server.**
/// The kernel maps a touched text page and, through fault-around, the cached
/// pages beside it, and how far that reaches depends on the size of the page
/// cache's folios for the freshly copied binary. On hosted `ubuntu-latest`,
/// about 6% of null servers held exactly 17,904 KiB of text where the rest held
/// 20.9–22.6 MiB, with the same anonymous memory, threads and stacks and more
/// minor faults (NORN-577 evidence run 44273242054). The suite subtracts the
/// least of three nulls, so one such server raised every above-null reading of
/// its job by about 3 MB. With the executable wholly resident in every server,
/// null and measured alike, the term is the file's mapped size by
/// construction, and the differences cancel it.
///
/// The server prefaults after its first answer rather than at its entry: it
/// runs no harness this suite could enter, and what it touched starting is
/// resident already. Linux only, where the bars gate; elsewhere `None`.
fn prefault_the_server(
    group: libc::pid_t,
) -> Option<(libc::pid_t, norn_testkit::image::Residency)> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let server = server_in(group);
    let residency = norn_testkit::image::prefault_process(server).unwrap()?;
    Some((server, residency))
}

/// The one process of the measuring `group` other than its launcher, which
/// leads it: the server the launcher started and will reap.
fn server_in(group: libc::pid_t) -> libc::pid_t {
    let members: Vec<libc::pid_t> = std::fs::read_dir("/proc")
        .unwrap()
        .filter_map(|entry| {
            entry
                .ok()?
                .file_name()
                .to_str()?
                .parse::<libc::pid_t>()
                .ok()
        })
        .filter(|&pid| pid != group)
        .filter(|&pid| {
            // `stat`'s fields after the parenthesised command are state,
            // parent and process group.
            std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
                stat.rsplit_once(')').and_then(|(_, rest)| {
                    rest.split_whitespace().nth(2)?.parse::<libc::pid_t>().ok()
                }) == Some(group)
            })
        })
        .collect();
    assert_eq!(
        members.len(),
        1,
        "the measuring group {group} holds one server: {members:?}"
    );
    members[0]
}

/// Measured over the eight-shape read mix, including eight rich find pages:
/// null 25,743,360 bytes; above-null attach/read/plan at the 300-document
/// profile plus two probes [18,690,048, 30,072,832, 19,836,928], and at the
/// 2,000-document profile plus two probes
/// [20,070,400, 33,009,664, 21,483,520]. Read includes report encoding.
/// Over the 73 hosted `ubuntu-latest` readings that author the ratios below,
/// the highest above-null reading leaves the attach ceiling 2.55 MB, the read
/// ceiling 5.40 MB and the plan ceiling 5.23 MB. The attach ceiling's tightest
/// readings come from jobs whose null dipped about 3 MB (NORN-577).
/// The attach pair also limits absolute growth, so a high small-profile
/// peak cannot hide large-profile retention.
const CEILINGS: [u64; 3] = [24 * 1024 * 1024, 40 * 1024 * 1024, 28 * 1024 * 1024];
const ATTACH_GROWTH: u64 = 4 * 1024 * 1024; // measured 1,380,352 bytes

/// How far the 2,000-document attach may raise the whole-process peak over the
/// 300-document attach, in per mille.
///
/// **The ratios divide whole-process peaks**, as the in-process twins in
/// `norn-host`'s `tests/baselines` do: the null's code and fixed cost dilutes
/// them rather than cancelling, and the byte bars over the null are the sharper
/// instrument. A ratio of two above-null differences instead inherits the
/// null's noise in both terms. About 4% of null servers peak about 3 MB lower
/// than the rest, and the suite subtracts the minimum of three (NORN-577), so
/// above-null readings split into two populations while whole-process peaks
/// stay in one. That ratio read 1,033–1,196 against its earlier 1,250 bar.
///
/// Measured on 73 hosted `ubuntu-latest` readings: 33 from `ci.yml` and
/// `regression.yml` runs on 16 and 17 November 2026, and 40 from evidence run
/// 44264521896 on 18 November. They read **1.011–1.064**, highest 1.064 in that
/// run's job `serve_memory 6`. The whole-process attach peaks span 45.83–48.38
/// MB at 300 documents and 47.97–50.09 MB at 2,000.
///
/// The bar is a quarter of headroom over the highest reading, rounded up to the
/// next twentieth: 1.35, 27% over it. Over those readings it lets the
/// 2,000-document attach grow 13.3–16.2 MB (median 15.1) before refusing it.
/// [`ATTACH_GROWTH`] bounds the same growth at 4 MiB.
const ATTACH_RATIO_PER_MILLE: u64 = 1350;

/// How far the read mix may raise the whole-process peak over the attach alone,
/// at the 2,000-document profile, in per mille. Whole-process for the reason
/// [`ATTACH_RATIO_PER_MILLE`] gives; over the null, this ratio read
/// 1,593–1,872 and failed its earlier 1,800 bar on an ordinary low attach peak
/// (NORN-576).
///
/// Measured on the same 73 hosted readings: **1.245–1.315**, highest 1.315 in
/// `ci.yml` run 44033180714, attempt 1, whose whole-process attach of 48.37 MB
/// sits in the 47.97–50.09 MB spread. Whole-process reads span 60.19–63.87 MB.
///
/// The bar is a quarter of headroom over the highest reading, rounded up to the
/// next twentieth: 1.65, 25% over it. Over those readings it lets the read peak
/// grow 16.2–20.2 MB (median 18.6) before refusing it. The read ceiling in
/// [`CEILINGS`] is the sharper instrument: it refuses 5.4 MB of growth at the
/// highest reading.
///
/// It does not refuse a find that hydrates 1,000 documents with their fields,
/// tags and bodies into one 5.1 MB answer. Alone, that find raised the peak
/// about as far as the whole read mix does, 1.288–1.336 in the evidence run,
/// and a ratio with a quarter of headroom over the read mix cannot tell the two
/// apart.
const READ_OVER_ATTACH_PER_MILLE: u64 = 1650;

/// The null server's peak, and each subject's peak above it: attach, read and
/// plan at the 300- and then the 2,000-document profile.
struct Readings {
    null: u64,
    above_null: [[u64; 3]; 2],
}

impl Readings {
    /// A subject's whole-process peak, which the ratios divide.
    fn whole(&self, profile: usize, workload: usize) -> u64 {
        self.null + self.above_null[profile][workload]
    }
}

fn readings() -> &'static Readings {
    static READINGS: std::sync::OnceLock<Readings> = std::sync::OnceLock::new();
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
        Readings {
            null,
            above_null: peaks,
        }
    })
}

#[test]
#[ignore = "memory-lane case: runs alone in the memory job"]
fn real_serving_attach_read_and_plan_stay_under_their_peak_rss_ceilings() {
    for peaks in &readings().above_null {
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
    let readings = readings();
    let (small, large) = (readings.whole(0, 0), readings.whole(1, 0));
    assert!(
        large * 1000 <= small * ATTACH_RATIO_PER_MILLE,
        "whole-process attach pair {small} -> {large}"
    );
    let [small, large] = readings.above_null.map(|peaks| peaks[0]);
    assert!(
        large <= small + ATTACH_GROWTH,
        "attach growth {small} -> {large}"
    );
}

#[test]
#[ignore = "memory-lane case: runs alone in the memory job"]
fn reading_the_gate_profile_holds_the_process_near_its_attach_peak() {
    let readings = readings();
    let (attach, read) = (readings.whole(1, 0), readings.whole(1, 1));
    assert!(
        read * 1000 <= attach * READ_OVER_ATTACH_PER_MILLE,
        "whole-process read over attach {attach} -> {read}"
    );
}

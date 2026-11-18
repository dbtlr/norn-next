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
            // Keep a prefault or final-reading failure as a result: a panic
            // here would unwind before SIGTERM, and the launcher's waiter
            // would then hold the still-listening server until its deadline.
            let measured = prefaulted.map(|prefaulted| {
                let (server, after_prefault) = prefaulted?;
                let at_end = norn_testkit::image::residency_of(server)
                    .map_err(|error| format!("reading the final residency: {error}"))?
                    .ok_or_else(|| "the final residency is not reported".to_owned())?;
                norn_testkit::image::ResidencyReport { after_prefault, at_end }
                    .wholly_resident()
            });
            // SAFETY: this is the owned, still-running measuring group. Its
            // launcher shields SIGTERM and accounts for the server it reaps.
            assert_eq!(unsafe { libc::kill(-group, libc::SIGTERM) }, 0);
            resident = measured;
        }).unwrap();
    result.assert_success();
    if let Some(Err(problem)) = &resident {
        panic!("serve {profile:?} {workload}: {problem}");
    }
    let resident = resident.and_then(Result::ok);
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
/// pages beside it. How far that reaches varies with the page cache's state for
/// the freshly copied binary, most likely the size of the folios it was written
/// into; the evidence below shows the effect, not that mechanism. In NORN-577
/// evidence run 44273242054, 72 of 1,200 null servers on 20 hosted
/// `ubuntu-latest` jobs, some on every job, held exactly 17,904 KiB of text
/// where the rest held 20,604–21,808 KiB. Their other resident pages, anonymous memory, threads
/// and stacks matched the rest, and they took more minor faults, not fewer:
/// each fault mapped less text. The suite subtracts the least of three nulls,
/// so one such server raised every above-null reading of its job by about
/// 3 MB. With the executable wholly resident in every server,
/// null and measured alike, the term is the file's mapped size at the two
/// sampled points, and the differences cancel it.
///
/// The server prefaults after its first answer rather than at its entry: it
/// runs no harness this suite could enter, and what it touched starting is
/// resident already. Linux only, where the bars gate; elsewhere `None`. A
/// failure to prefault is returned, not raised, so the caller can stop the
/// server before reporting it.
///
/// **The claim is two readings, not the whole run.** The suite checks that the
/// executable is wholly resident after the prefault and again just before
/// SIGTERM. Under memory pressure, executable pages reclaimed and faulted back
/// between those readings could lower the recorded peak, and neither reading
/// would show it.
fn prefault_the_server(
    group: libc::pid_t,
) -> Option<Result<(libc::pid_t, norn_testkit::image::Residency), String>> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let server = server_in(group);
    let prefaulted = norn_testkit::image::prefault_process(server)
        .map_err(|error| format!("prefaulting the server: {error}"))
        .transpose()?;
    Some(prefaulted.map(|residency| (server, residency)))
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

/// How far attach, read and plan may raise the server's peak above the null
/// server's, in bytes, at either profile. Read runs the eight-shape mix,
/// including eight rich find pages and report encoding.
///
/// **Every server holds its executable wholly resident** (see
/// [`prefault_the_server`]), so the code term cancels and these bound what
/// the work holds beyond an empty listening server. The ceilings are authored
/// by the rule of `norn-host`'s `tests/baselines`: 1.61x the highest hosted
/// reading, rounded up to a whole MiB, from at least three runs.
///
/// Measured on 45 hosted `ubuntu-latest` jobs, 15 in each of NORN-577 evidence
/// runs 44275100312, 44275119129 and 44275128386, tabled below. Their 135 null
/// peaks read 53,424,128–53,964,800 bytes in one population, each with the
/// executable's 51,081,216 bytes resident. Each job subtracts the least of its
/// three.
///
/// - **Attach: 12 MiB.** 4,923,392–5,611,520 bytes at 300 documents and
///   7,180,288–7,680,000 at 2,000; the highest is in run 44275100312, job 2,
///   over a null of 53,448,704. 1.61x is 11.79 MiB. It leaves 4.90 MB (64%) of
///   headroom over that reading.
/// - **Read: 31 MiB.** 16,470,016–17,072,128 and 19,001,344–19,714,048; the
///   highest is in run 44275119129, job 3, over 53,616,640. 1.61x is 30.27 MiB.
///   It leaves 12.79 MB (65%).
/// - **Plan: 12 MiB.** 5,304,320–5,955,584 and 7,122,944–7,639,040; the highest
///   is in run 44275100312, job 4, over 53,530,624. 1.61x is 11.73 MiB. It
///   leaves 4.94 MB (65%).
///
/// They were 24, 40 and 28 MiB over a null whose text residency was left to
/// fault-around. That definition also carried the code each work path touched
/// beyond the null's, and about 4–6% of null servers held about 3 MB less text
/// than the rest (NORN-577), which left the attach ceiling 2.55 MB of headroom
/// over the highest of 73 hosted readings.
///
/// Readings in KiB: the job's null, then each subject above it.
///
/// | run, job | null | 300: attach, read, plan | 2,000: attach, read, plan |
/// | --- | --- | --- | --- |
/// | 44275100312, 1 | 52,264 | 5,248, 16,524, 5,524 | 7,368, 19,184, 7,272 |
/// | 44275100312, 2 | 52,196 | 5,176, 16,276, 5,484 | 7,500, 19,132, 7,228 |
/// | 44275100312, 3 | 52,356 | 5,084, 16,320, 5,564 | 7,156, 18,904, 7,220 |
/// | 44275100312, 4 | 52,276 | 5,448, 16,636, 5,444 | 7,404, 19,028, 7,460 |
/// | 44275100312, 5 | 52,448 | 5,128, 16,200, 5,516 | 7,184, 18,908, 7,308 |
/// | 44275100312, 6 | 52,256 | 5,324, 16,400, 5,404 | 7,388, 18,976, 7,352 |
/// | 44275100312, 7 | 52,348 | 5,232, 16,292, 5,588 | 7,316, 18,940, 7,220 |
/// | 44275100312, 8 | 52,396 | 5,060, 16,256, 5,440 | 7,172, 18,992, 7,152 |
/// | 44275100312, 9 | 52,344 | 5,012, 16,400, 5,700 | 7,088, 19,080, 7,212 |
/// | 44275100312, 10 | 52,292 | 5,240, 16,364, 5,468 | 7,408, 19,052, 7,320 |
/// | 44275100312, 11 | 52,332 | 5,132, 16,224, 5,460 | 7,260, 18,992, 7,188 |
/// | 44275100312, 12 | 52,516 | 4,808, 16,092, 5,228 | 7,092, 18,784, 7,180 |
/// | 44275100312, 13 | 52,260 | 5,060, 16,332, 5,816 | 7,312, 18,904, 7,312 |
/// | 44275100312, 14 | 52,308 | 5,432, 16,336, 5,436 | 7,364, 18,988, 7,364 |
/// | 44275100312, 15 | 52,268 | 4,956, 16,556, 5,772 | 7,344, 18,824, 7,176 |
/// | 44275119129, 1 | 52,312 | 5,104, 16,628, 5,448 | 7,200, 19,180, 7,196 |
/// | 44275119129, 2 | 52,364 | 5,364, 16,392, 5,556 | 7,168, 18,928, 7,388 |
/// | 44275119129, 3 | 52,360 | 5,232, 16,468, 5,404 | 7,396, 19,252, 7,256 |
/// | 44275119129, 4 | 52,392 | 5,184, 16,660, 5,468 | 7,212, 19,084, 7,372 |
/// | 44275119129, 5 | 52,384 | 5,000, 16,348, 5,452 | 7,212, 18,768, 7,232 |
/// | 44275119129, 6 | 52,264 | 5,292, 16,348, 5,380 | 7,280, 18,960, 7,308 |
/// | 44275119129, 7 | 52,376 | 5,216, 16,408, 5,592 | 7,272, 18,820, 7,108 |
/// | 44275119129, 8 | 52,376 | 5,112, 16,304, 5,440 | 7,316, 19,184, 7,284 |
/// | 44275119129, 9 | 52,292 | 5,076, 16,408, 5,180 | 7,468, 19,172, 7,372 |
/// | 44275119129, 10 | 52,572 | 4,816, 16,084, 5,508 | 7,044, 18,632, 6,956 |
/// | 44275119129, 11 | 52,360 | 5,220, 16,324, 5,776 | 7,416, 18,972, 7,264 |
/// | 44275119129, 12 | 52,172 | 5,480, 16,428, 5,600 | 7,424, 18,948, 7,392 |
/// | 44275119129, 13 | 52,388 | 5,108, 16,364, 5,424 | 7,140, 18,828, 7,324 |
/// | 44275119129, 14 | 52,540 | 5,084, 16,276, 5,220 | 7,208, 18,892, 7,276 |
/// | 44275119129, 15 | 52,424 | 5,292, 16,500, 5,476 | 7,300, 18,556, 7,356 |
/// | 44275128386, 1 | 52,364 | 5,344, 16,568, 5,184 | 7,424, 19,116, 7,072 |
/// | 44275128386, 2 | 52,356 | 5,388, 16,500, 5,676 | 7,480, 18,976, 7,364 |
/// | 44275128386, 3 | 52,320 | 5,272, 16,508, 5,404 | 7,332, 19,128, 7,416 |
/// | 44275128386, 4 | 52,344 | 5,316, 16,420, 5,248 | 7,160, 18,892, 7,296 |
/// | 44275128386, 5 | 52,476 | 5,044, 16,216, 5,544 | 7,124, 18,828, 7,172 |
/// | 44275128386, 6 | 52,364 | 5,080, 16,364, 5,452 | 7,124, 18,760, 7,280 |
/// | 44275128386, 7 | 52,352 | 5,168, 16,248, 5,616 | 7,268, 18,884, 7,196 |
/// | 44275128386, 8 | 52,324 | 5,200, 16,672, 5,608 | 7,264, 19,068, 7,348 |
/// | 44275128386, 9 | 52,228 | 5,460, 16,476, 5,364 | 7,304, 19,084, 7,360 |
/// | 44275128386, 10 | 52,392 | 5,212, 16,332, 5,332 | 7,252, 18,832, 7,256 |
/// | 44275128386, 11 | 52,388 | 5,064, 16,464, 5,440 | 7,060, 19,140, 6,984 |
/// | 44275128386, 12 | 52,452 | 5,160, 16,316, 5,500 | 7,012, 18,744, 7,180 |
/// | 44275128386, 13 | 52,352 | 5,344, 16,404, 5,420 | 7,260, 19,012, 7,436 |
/// | 44275128386, 14 | 52,240 | 5,252, 16,364, 5,484 | 7,272, 18,896, 7,440 |
/// | 44275128386, 15 | 52,264 | 5,208, 16,324, 5,568 | 7,292, 19,080, 7,316 |
const CEILINGS: [u64; 3] = [12 * 1024 * 1024, 31 * 1024 * 1024, 12 * 1024 * 1024];

/// How far the 2,000-document attach may raise the peak over the 300-document
/// attach, in bytes, so a high small-profile peak cannot hide large-profile
/// retention. The null cancels in this difference.
///
/// Over the same 45 jobs it read 1,847,296–2,449,408 bytes, highest in run
/// 44275119129, job 9. 1.61x is 3.76 MiB, rounded up to **4 MiB**, which it
/// already was. It leaves 1.74 MB (71%) of headroom over that reading.
const ATTACH_GROWTH: u64 = 4 * 1024 * 1024;

/// How far the 2,000-document attach may raise the whole-process peak over the
/// 300-document attach, in per mille.
///
/// **The ratios divide whole-process peaks**, as the in-process twins in
/// `norn-host`'s `tests/baselines` do: the null's code and fixed cost dilutes
/// them rather than cancelling, and the byte bars over the null are the sharper
/// instrument. A ratio of two above-null differences instead inherits the
/// null's noise in both terms; over a null left to fault-around, that ratio
/// read 1,033–1,196 against its earlier 1,250 bar (NORN-576). Each peak now
/// carries the executable's 51,081,216 resident bytes, which dilutes the
/// ratios further.
///
/// Measured on the 45 jobs [`CEILINGS`] tables: **1.031–1.042**, highest in run
/// 44275100312, job 15. The whole-process attach peaks span 58.60–59.13 MB at
/// 300 documents and 60.86–61.27 MB at 2,000.
///
/// The bar is a quarter of headroom over the highest reading, rounded up to the
/// next twentieth: 1.35, 30% over it, as it was. Over those readings it lets
/// the 2,000-document attach grow 18.1–18.8 MB (median 18.5) before refusing
/// it. [`ATTACH_GROWTH`] bounds the same growth at 4 MiB.
const ATTACH_RATIO_PER_MILLE: u64 = 1350;

/// How far the read mix may raise the whole-process peak over the attach alone,
/// at the 2,000-document profile, in per mille. Whole-process for the reason
/// [`ATTACH_RATIO_PER_MILLE`] gives; over the null, this ratio read
/// 1,593–1,872 and failed its earlier 1,800 bar on an ordinary low attach peak
/// (NORN-576).
///
/// Measured on the same 45 jobs: **1.189–1.203**, highest in run 44275128386,
/// job 11. Whole-process reads span 72.68–73.33 MB.
///
/// The bar is a quarter of headroom over the highest reading, rounded up to the
/// next twentieth: 1.55, 29% over it. It was 1.65 over peaks without the
/// executable wholly resident. Over those readings it lets the read peak grow
/// 21.1–22.1 MB (median 21.6) before refusing it. The read ceiling in
/// [`CEILINGS`] is the sharper instrument: it refuses 12.8 MB of growth at the
/// highest reading.
///
/// It does not refuse a find that hydrates 1,000 documents with their fields,
/// tags and bodies into one 5.1 MB answer. Alone, that find raised the peak
/// about as far as the whole read mix does, 1.288–1.336 over peaks without the
/// executable wholly resident (NORN-576 evidence run 44264521896), and a ratio
/// with a quarter of headroom over the read mix cannot tell the two apart.
const READ_OVER_ATTACH_PER_MILLE: u64 = 1550;

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

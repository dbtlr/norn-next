//! A real serving process under bounded file churn and MCP reads.
// The dose is mandatory on this lane; its runner rejects an empty build if
// either feature is omitted, as the library soak's runner does.
#![cfg(all(feature = "counters", feature = "induced-failure", target_os = "linux"))]
#![allow(clippy::disallowed_methods)] // Isolated generated fixture and owned child /proc readings.

mod support;

#[path = "../../norn-host/tests/baselines/mod.rs"]
mod native_bars;

use serde_json::{Value, json};
use std::time::{Duration, Instant};
use support::{Fixture, free_port};

// Calibrated from one full 3,600 s run (default profile, 5k documents) that
// read anonymous memory, resident or swapped, above the least of three
// prefaulted nulls (6,737,920 bytes). The peak above null was 29,515,776 bytes;
// 25% headroom rounded up to whole MiB sets 36 MiB for the peak and the high
// water alike (high water read 18,165,760 bytes above the null's VmHWM).
// Anonymous memory rose from 9.1 MB to 19.5 MB over the first quarter hour as
// caches and the allocator filled, then by 1.46 MB between the second and last
// quarters' means (19,548,282 and 21,008,733 bytes: 1075 per mille). The slope
// therefore compares the last quarter with the second, leaving the warm-up
// quarter out, and its ceiling is the library soak's 1150 per mille, twice the
// growth this run showed. A run shorter than SOAK_SLOPE_MIN_SECS cannot
// separate warm-up from growth, so it reports its slope without judging it.
const SOAK_PEAK_RSS_CEILING_BYTES: u64 = 36 * 1024 * 1024;
const SOAK_HIGH_WATER_RSS_CEILING_BYTES: u64 = 36 * 1024 * 1024;
const SOAK_RSS_SLOPE_PER_MILLE: u64 = 1150;
const SOAK_SLOPE_MIN_SECS: u64 = 1800;
// The induced-failure build forwards the watcher environment seam. Budget
// stream=fails to the first watch only, so the re-attachment stays healthy.

fn attempted_call(port: u16, bearer: &str, name: &str, arguments: Value) -> Result<Value, Value> {
    let reply = support::exchange_with(port, "POST", "/mcp", Some(bearer),
        &[("MCP-Protocol-Version", "2025-11-25")],
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}}).to_string().as_bytes(), Duration::from_secs(60));
    assert_eq!(reply.status, 200, "{reply:?}");
    assert!(reply.header("Mcp-Session-Id").is_none());
    let value: Value = serde_json::from_slice(&reply.body).unwrap();
    assert!(value.get("error").is_none(), "{value}");
    let answer = value["result"]["structuredContent"].clone();
    if value["result"]["isError"] == true {
        Err(answer)
    } else {
        Ok(answer)
    }
}

fn call(port: u16, bearer: &str, name: &str, arguments: Value) -> Value {
    attempted_call(port, bearer, name, arguments).unwrap_or_else(|error| panic!("{name}: {error}"))
}

struct Recovery {
    began: Instant,
    tick: u64,
    reads: u64,
    period: Duration,
}

fn address() -> norn_wire::VaultAddress {
    norn_wire::VaultAddress::name(norn_wire::VaultName::new("notes").unwrap())
}
fn find() -> Value {
    serde_json::to_value(norn_wire::FindParams::new(address()).with_limit(1)).unwrap()
}
fn get(target: &str) -> Value {
    serde_json::to_value(norn_wire::GetParams::new(
        address(),
        norn_wire::ResolutionTarget::new(target).unwrap(),
    ))
    .unwrap()
}
fn validate() -> Value {
    serde_json::to_value(norn_wire::ValidateParams::new(address()).with_limit(1)).unwrap()
}

#[test]
fn anonymous_status_counts_resident_and_swapped_pages() {
    assert_eq!(
        anonymous_status(
            "Name: norn
RssAnon: 12 kB
VmSwap: 7 kB
VmRSS: 30 kB
"
        ),
        19 * 1024
    );
}

fn memory(pid: u32, key: &str) -> u64 {
    status_memory(
        &std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap(),
        key,
    )
}

fn status_memory(status: &str, key: &str) -> u64 {
    status
        .lines()
        .find(|line| line.starts_with(key))
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse::<u64>()
        .unwrap()
        * 1024
}

/// The process's anonymous memory, resident or swapped out: `RssAnon` plus
/// `VmSwap`. Leaked heap is anonymous, and this sum holds it wherever the
/// kernel keeps it. `VmRSS` alone falls when a loaded host swaps the server out
/// or drops its file-backed pages, which can hide a leak behind the null's
/// reading for the rest of a run.
fn anonymous(pid: u32) -> u64 {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    anonymous_status(&status)
}

fn anonymous_status(status: &str) -> u64 {
    status_memory(status, "RssAnon:") + status_memory(status, "VmSwap:")
}

fn fds(pid: u32) -> usize {
    std::fs::read_dir(format!("/proc/{pid}/fd"))
        .unwrap()
        .count()
}

fn counters(port: u16, bearer: &str) -> Value {
    let reply = support::get(port, "/v1/debug/counters", Some(bearer));
    serde_json::from_slice(&reply.body).unwrap()
}

#[test]
#[ignore = "soak-lane case: real MCP process under churn for one hour"]
fn mcp_process_soak() {
    let duration = std::env::var("NORN_SOAK_DURATION_SECS")
        .map_or(3600, |s| s.parse::<u64>().expect("duration in seconds"));
    assert!(duration >= 8, "at least eight samples for quartiles");
    let mut null_readings = Vec::new();
    for _ in 0..3 {
        let null_fixture = Fixture::new("serve-soak-null");
        let (null, _) = null_fixture.serve_on(free_port());
        norn_testkit::image::prefault_process(null.child.id() as i32).unwrap();
        null_readings.push((
            anonymous(null.child.id()),
            memory(null.child.id(), "VmHWM:"),
        ));
        assert_eq!(null.terminate().0.code(), Some(0));
    }
    let null_rss = null_readings.iter().map(|r| r.0).min().unwrap();
    let null_hwm = null_readings.iter().map(|r| r.1).min().unwrap();
    eprintln!("null readings: {null_readings:?}");
    let fixture = Fixture::new("serve-soak");
    fixture.write_settings("[serve]\nidle_after = \"24h\"\n");
    let root = fixture.sandbox.work_dir().join("attached/vault");
    norn_fixtures::generate(&norn_fixtures::Profile::by_name("soak").unwrap(), 7, &root).unwrap();
    std::fs::create_dir_all(root.join("soak-churn")).unwrap();
    let port = free_port();
    let hits = fixture.sandbox.work_dir().join("arm-records/hits");
    std::fs::create_dir_all(hits.parent().unwrap()).unwrap();
    let dose = cfg!(feature = "induced-failure");
    let mut server = fixture.start_with(&["serve", "--port", &port.to_string()], |command| {
        if dose {
            command
                .env("NORN_FS_WATCH_ARMED_STAGES", "stream=fails")
                .env("NORN_FS_WATCH_ARMED_WATCHES", "1")
                .env("NORN_FS_ARM_HITS", &hits);
        }
    });
    assert_eq!(server.listening_port(), port);
    let bearer = fixture.default_bearer();
    let pid = server.child.id();
    norn_testkit::image::prefault_process(pid as i32).unwrap();
    let vault = json!({"by":"name","name":"notes"});
    call(
        port,
        &bearer,
        "vault_register",
        json!({"registration":{"name":"notes","root":root}}),
    );
    call(port, &bearer, "find", find());
    // From-zero attachment is preparation, not a warm-operation clock. Use
    // the same loaded-runner allowance as the serving memory suite.
    let attach_deadline = Instant::now() + Duration::from_secs(180);
    loop {
        if call(port, &bearer, "vault_status", json!({"vault":vault}))["status"]["published"]["state"]
            ["state"]
            == "ready"
        {
            break;
        }
        assert!(
            Instant::now() < attach_deadline,
            "soak attachment did not become ready"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let subject = call(port, &bearer, "find", find())["report"]["page"]["rows"][0]["path"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut rss = Vec::new();
    let mut descriptors = Vec::new();
    let mut slope_samples = Vec::new();
    let mut recovering: Option<Recovery> = None;
    let mut recoveries = 0;
    let mut warm_calls = 0u64;
    let (completed, replies) = std::sync::mpsc::channel();
    let mut pending: Option<std::thread::JoinHandle<()>> = None;
    let began = Instant::now();
    let mut tick = 0u64;
    let mut warm_reads = 0;
    while began.elapsed() < Duration::from_secs(duration)
        || recovering.is_some()
        || pending.is_some()
    {
        let at = |n| root.join(format!("soak-churn/note-{:04}.md", n % 64));
        std::fs::write(at(tick), format!("# Churn {tick}\n\nA body.\n")).unwrap();
        if tick > 0 {
            std::fs::write(
                at(tick - 1),
                format!("# Rewritten {tick}\n\nLonger body.\n"),
            )
            .unwrap();
        }
        if tick > 1 {
            let _ = std::fs::remove_file(at(tick - 2));
        }
        if tick.is_multiple_of(60) {
            eprintln!("soak tick={tick} elapsed_s={}", began.elapsed().as_secs());
        }
        if pending
            .as_ref()
            .is_some_and(std::thread::JoinHandle::is_finished)
        {
            pending
                .take()
                .unwrap()
                .join()
                .expect("warm MCP client returned");
            let (before, after, outcomes) = replies
                .recv()
                .expect("completed warm batch sent its result");
            let before: Value = before;
            let after: Value = after;
            let outcomes: Vec<Result<Value, Value>> = outcomes;
            assert_eq!(
                after["host.request.fs_reads"],
                before["host.request.fs_reads"]
            );
            // Concurrent metadata probes also increment calls; the three
            // read outcomes prove this batch itself reached the route.
            assert!(
                after["mcp.calls"].as_u64().unwrap() - before["mcp.calls"].as_u64().unwrap() >= 3
            );
            for outcome in outcomes {
                match outcome {
                    Ok(_) => warm_calls += 1,
                    Err(error) => {
                        assert!(dose, "{error}");
                        assert!(
                            matches!(
                                error["error"]["code"].as_str(),
                                Some("host/entry-untrusted" | "host/entry-not-ready")
                            ),
                            "{error}"
                        );
                        if recovering.is_none() {
                            recoveries += 1;
                            recovering = Some(Recovery {
                                began: Instant::now(),
                                tick,
                                reads: warm_calls,
                                period: began.elapsed() / u32::try_from(tick.max(3)).unwrap(),
                            });
                        }
                    }
                }
            }
            warm_reads += 1;
        }
        let status = call(port, &bearer, "vault_status", json!({"vault":vault}));
        let state = &status["status"]["published"]["state"];
        let lost = state["state"] == "untrusted";
        // Three baseline ticks establish the achieved cadence before demanding
        // recovery. Metadata probes do not re-arm the untrusted attachment.
        if dose && lost && tick >= 3 && recovering.is_none() {
            recoveries += 1;
            recovering = Some(Recovery {
                began: Instant::now(),
                tick,
                reads: warm_calls,
                period: began.elapsed() / u32::try_from(tick).unwrap(),
            });
        }
        if state["state"] == "ready" && pending.is_none() {
            if let Some(window) = recovering.take() {
                let elapsed = window.began.elapsed();
                let charged = elapsed.saturating_sub(window.period * 2);
                let owed_churn = charged.as_nanos() / window.period.as_nanos().max(1);
                // The native cadence owes one warm read per five turns;
                // this harness sends a three-tool mix at each checkpoint.
                let owed_reads = charged.as_nanos() / (window.period * 5).as_nanos().max(1);
                assert!(u128::from(tick - window.tick) >= owed_churn);
                assert!(u128::from(warm_calls - window.reads) >= owed_reads);
                eprintln!(
                    "recovery window_ms={} baseline_tick_ms={} churn={} owed_churn={owed_churn} warm_calls={} owed_reads={owed_reads}",
                    elapsed.as_millis(),
                    window.period.as_millis(),
                    tick - window.tick,
                    warm_calls - window.reads
                );
            }
        } else {
            assert!(
                state["state"] == "ready" || state["state"] == "warming" || (dose && lost),
                "{status}"
            );
        }
        if let Some(window) = &recovering {
            // Native soak permits three 60 s attempts. Reads make a fresh
            // demand through the production route while churn keeps advancing.
            assert!(window.began.elapsed() <= Duration::from_secs(3 * 60));
        }
        // Keep HTTP waits off the sampler/churn thread: a recovering read can
        // spend the server's readiness window waiting for a report.
        if pending.is_none()
            && (began.elapsed() < Duration::from_secs(duration) || recovering.is_some())
            && ((tick.is_multiple_of(5) && (!dose || tick >= 3)) || recovering.is_some())
        {
            let bearer = bearer.clone();
            let subject = subject.clone();
            let completed = completed.clone();
            pending = Some(std::thread::spawn(move || {
                let before = counters(port, &bearer);
                let outcomes = [
                    ("find", find()),
                    ("get", get(&subject)),
                    ("validate", validate()),
                ]
                .into_iter()
                .map(|(name, params)| attempted_call(port, &bearer, name, params))
                .collect::<Vec<_>>();
                let after = counters(port, &bearer);
                completed
                    .send((before, after, outcomes))
                    .expect("soak receiver alive");
            }));
        }
        rss.push(anonymous(pid).saturating_sub(null_rss));
        if recovering.is_none() && !lost {
            slope_samples.push(*rss.last().unwrap());
        }
        descriptors.push(fds(pid));
        eprintln!(
            "soak_sample tick={tick} elapsed_ms={} rss_above_null={} descriptors={} recovering={} warm_calls={warm_calls}",
            began.elapsed().as_millis(),
            rss.last().unwrap(),
            descriptors.last().unwrap(),
            recovering.is_some()
        );
        tick += 1;
        std::thread::sleep(Duration::from_secs(1));
    }
    assert!(
        recovering.is_none(),
        "recovery remained in flight at the end"
    );
    if dose {
        let records = std::fs::read_to_string(&hits).expect("the stream dose fired");
        assert_eq!(
            records
                .lines()
                .filter(|line| line.contains("seam=norn-fs/watch stage=stream answer=fails"))
                .count(),
            1,
            "{records}"
        );
        assert!(recoveries >= native_bars::SOAK_RECOVERY_DOSE);
    }
    eprintln!("recoveries={recoveries} successful_warm_calls={warm_calls}");
    let stopped = Instant::now();
    // Prove reconciliation saw the last write, rather than measuring an idle watcher.
    let target = format!("soak-churn/note-{:04}.md", (tick - 1) % 64);
    support::eventually("last churn observed", || {
        let answer = call(port, &bearer, "get", get(&target));
        answer.to_string().contains(&format!("Churn {}", tick - 1))
    });
    assert!(
        stopped.elapsed()
            <= native_bars::SOAK_SETTLE_CEILING.expect("the native settle bar is authored"),
        "settle exceeded five seconds"
    );
    let before = counters(port, &bearer);
    call(port, &bearer, "find", find());
    let after = counters(port, &bearer);
    assert_eq!(
        after["host.jobs.documents_derived"],
        before["host.jobs.documents_derived"]
    );
    let quarter = slope_samples.len() / 4;
    assert!(quarter > 0);
    // The second quarter, not the first: the first is warm-up.
    let first = slope_samples[quarter..2 * quarter].iter().sum::<u64>() / quarter as u64;
    let last = slope_samples[slope_samples.len() - quarter..]
        .iter()
        .sum::<u64>()
        / quarter as u64;
    let slope = last * 1000 / first.max(1);
    let peak = *rss.iter().max().unwrap();
    let hwm = memory(pid, "VmHWM:").saturating_sub(null_hwm);
    let growth = descriptors.last().unwrap().saturating_sub(descriptors[0]);
    let initial_fds = descriptors[0];
    support::eventually("soak quiescent descriptors", || {
        fds(pid)
            <= initial_fds
                + native_bars::SOAK_QUIESCENT_FD_RETENTION
                    .expect("the native retention bar is authored")
    });
    eprintln!(
        "soak duration={duration} ticks={tick} warm_reads={warm_reads} null_rss={null_rss} null_hwm={null_hwm} peak={peak} hwm={hwm} first={first} last={last} slope_per_mille={slope} fd_growth={growth}"
    );
    assert!(growth <= native_bars::SOAK_FD_GROWTH_ALLOWANCE);
    assert!(peak <= SOAK_PEAK_RSS_CEILING_BYTES);
    assert!(hwm <= SOAK_HIGH_WATER_RSS_CEILING_BYTES);
    if duration >= SOAK_SLOPE_MIN_SECS {
        assert!(slope <= SOAK_RSS_SLOPE_PER_MILLE);
    }
    assert_eq!(server.terminate().0.code(), Some(0));
}

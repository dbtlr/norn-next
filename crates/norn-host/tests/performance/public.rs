//! Ordered real caller-thread requests while ordinary attachment continues independently.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use norn_host::{Host, ProductionEntryOps};
use norn_wire::{
    FindParams, GetParams, Predicate, ResolutionTarget, TrustState, VaultAddress, VaultName,
};
use serde_json::{Value, json};

fn prefix_commands(targets: Option<&str>) -> Vec<Value> {
    let targets: Vec<String> = targets.map_or_else(
        || vec!["small".into()],
        |value| serde_json::from_str(value).expect("an ordered JSON list of Get targets"),
    );
    assert!(!targets.is_empty(), "at least one early Get target");
    let mut commands: Vec<_> = targets
        .into_iter()
        .map(|target| json!({"op":"get","target":target,"whole":true}))
        .collect();
    commands.push(json!({"op":"find"}));
    commands
}

/// ACKs rendezvous with this caller only; attachment owns no observer or pause.
/// Ready here means a caller observation, never the instant of publication.
fn prefix_requests(
    host: &Host<ProductionEntryOps>,
    name: &VaultName,
    root: &Path,
    began: Instant,
) -> (Option<u128>, usize) {
    let address = VaultAddress::name(name.clone());
    let mut observed_ready = None;
    let targets = std::env::var("NORN_PERFORMANCE_PUBLIC_GET_TARGETS").ok();
    let commands = prefix_commands(targets.as_deref());
    let request_count = commands.len();
    for (index, command) in commands.into_iter().enumerate() {
        let is_get = command["op"] == "get";
        let started = began.elapsed().as_nanos();
        let finished;
        let (answer, error, counters, work) = if is_get {
            let params = GetParams::new(
                address.clone(),
                ResolutionTarget::new(super::text(&command, "target")).expect("a Get target"),
            );
            let result = host.get(&params);
            finished = began.elapsed().as_nanos();
            match result {
                Ok(answer) => (
                    json!(answer.answer),
                    Value::Null,
                    json!(answer.snapshot.readings().collect::<BTreeMap<_, _>>()),
                    json!(answer.work.readings().collect::<BTreeMap<_, _>>()),
                ),
                Err(error) => (Value::Null, json!(error), Value::Null, Value::Null),
            }
        } else {
            let params = FindParams::new(address.clone())
                .with_predicates([Predicate::equal_to("perf_bucket", "active")])
                .with_limit(25);
            let result = host.find(&params);
            finished = began.elapsed().as_nanos();
            match result {
                Ok(answer) => (
                    json!(answer.answer),
                    Value::Null,
                    json!(answer.snapshot.readings().collect::<BTreeMap<_, _>>()),
                    json!(answer.work.readings().collect::<BTreeMap<_, _>>()),
                ),
                Err(error) => (Value::Null, json!(error), Value::Null, Value::Null),
            }
        };
        let state = host.state(name).expect("registered vault state");
        let observed = began.elapsed().as_nanos();
        if state == TrustState::Ready {
            observed_ready.get_or_insert(observed);
        }
        let disposition = if !answer.is_null() {
            "served"
        } else if error["code"] == "host/entry-not-ready" {
            "unavailable"
        } else {
            "error"
        };
        // A Ready answer is ordinary service. Even a warming hold may finish
        // after promotion; a Ready post-call observation earns no early label.
        let early = !answer.is_null()
            && answer["reading"]["trust"]["state"] == "warming"
            && state != TrustState::Ready;
        let probe = if is_get { "whole_slug" } else { "active_page" };
        super::emit(
            json!({"startup_stage":(if is_get { "public-get" } else { "public-find" }),
            "probe":probe,"operator_prefix_sequence":index+1,"command":command,
            "mode":"public","availability_scope":"public Host first-open requests",
            "disposition":disposition,"answer":answer,"error":error,
            "counters":counters,"builder_work":work,
            "query_started_ns":started,"query_finished_ns":finished,"query_host_ns":finished-started,
            "state_after_call":state,"state_observed_ns":observed,"early_answer":early}),
        );
        assert_ne!(
            disposition, "error",
            "public operational refusal stops the cell"
        );
        operator_ack(
            root,
            (index + 1) as u64,
            probe,
            std::time::Duration::from_secs(5),
        )
        .expect("bounded caller-only parent acknowledgement");
    }
    (observed_ready, request_count)
}

fn operator_ack(
    root: &std::path::Path,
    sequence: u64,
    probe: &str,
    timeout: std::time::Duration,
) -> Result<(), String> {
    let path = root
        .join("operator-prefix-acks")
        .join(format!("{sequence}.json"));
    let deadline = Instant::now() + timeout.min(std::time::Duration::from_secs(5));
    loop {
        match std::fs::read(&path) {
            Ok(bytes) => {
                let ack: Value = serde_json::from_slice(&bytes)
                    .map_err(|e| format!("invalid operator prefix ack: {e}"))?;
                if ack["sequence"].as_u64() != Some(sequence) || ack["probe"] != probe {
                    return Err("invalid operator prefix acknowledgement identity".into());
                }
                std::fs::remove_file(path).map_err(|e| e.to_string())?;
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(format!("reading operator prefix ack: {e}")),
        }
        if Instant::now() >= deadline {
            return Err("operator prefix parent acknowledgement timed out".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// The timed comparison runs independently of prefix ACKs and ordinary Get admission.
pub fn prefix(
    host: &Host<ProductionEntryOps>,
    name: &VaultName,
    root: &Path,
    began: Instant,
) -> (Option<u128>, usize) {
    std::thread::scope(|scope| {
        let mid = std::env::var("NORN_PERFORMANCE_MID_REQUEST_PATH").ok().map(|path| {
            let offset_ns: u64 = std::env::var("NORN_PERFORMANCE_MID_REQUEST_OFFSET_NS")
                .expect("an explicit lab-observed wall offset").parse().expect("an integer wall offset");
            scope.spawn(move || {
                let deadline = began + std::time::Duration::from_nanos(offset_ns);
                if let Some(wait) = deadline.checked_duration_since(Instant::now()) {
                    std::thread::sleep(wait);
                }
                let params = GetParams::new(VaultAddress::name(name.clone()),
                    ResolutionTarget::new(format!("/{path}")).expect("a mid-request target"));
                let request_ns = began.elapsed().as_nanos();
                let result = host.get(&params);
                let delivery_ns = began.elapsed().as_nanos();
                let (answer, error, counters, work) = match result {
                    Ok(answer) => (json!(answer.answer), Value::Null,
                        json!(answer.snapshot.readings().collect::<BTreeMap<_, _>>()),
                        json!(answer.work.readings().collect::<BTreeMap<_, _>>())),
                    Err(error) => (Value::Null, json!(error), Value::Null, Value::Null),
                };
                let disposition = if !answer.is_null() { "served" }
                    else if error["code"] == "host/entry-not-ready" { "unavailable" } else { "error" };
                super::emit(json!({"startup_stage":"public-mid-get", "path":path,
                    "requested_offset_ns":offset_ns, "request_ns":request_ns, "delivery_ns":delivery_ns,
                    "query_started_ns":request_ns, "query_finished_ns":delivery_ns,
                    "query_host_ns":delivery_ns-request_ns, "request_to_delivery_ns":delivery_ns-request_ns,
                    "comparison":"not comparable (side by side, no ratios)",
                    "timing_scope":"public Host::get at prior lab content-25% wall offset",
                    "disposition":disposition, "answer":answer, "error":error,
                    "counters":counters, "builder_work":work}));
                assert_ne!(disposition, "error", "public mid-request operational refusal stops the cell");
            })
        });
        let result = prefix_requests(host, name, root, began);
        if let Some(mid) = mid {
            mid.join().expect("public mid-request caller");
        }
        result
    })
}

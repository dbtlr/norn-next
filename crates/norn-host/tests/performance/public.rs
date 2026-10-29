//! Two real caller-thread requests while ordinary attachment continues independently.
//!
//! Each answer is labelled with where it was read: the step of the from-zero
//! build that admitted it early, or none for a canonical read. The pull-ahead
//! evidence the build had spent by then is recorded beside it.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use norn_host::EarlyReadsServed;
use norn_wire::{
    FindParams, GetParams, Predicate, ResolutionTarget, TrustState, VaultAddress, VaultName,
};
use serde_json::{Value, json};

/// ACKs rendezvous with this caller only; attachment owns no observer or pause.
/// Ready here means a caller observation, never the instant of publication.
pub fn prefix(
    host: &super::attach::ServingHost,
    name: &VaultName,
    root: &Path,
    began: Instant,
) -> Option<u128> {
    let address = VaultAddress::name(name.clone());
    let mut observed_ready = None;
    for (index, command) in [
        json!({"op":"get","target":"small","whole":true}),
        json!({"op":"find"}),
    ]
    .into_iter()
    .enumerate()
    {
        let served = host.read_evidence().early_reads_served;
        let started = began.elapsed().as_nanos();
        let finished;
        let (answer, error, counters, work) = if index == 0 {
            let params = GetParams::new(address.clone(), ResolutionTarget::new("small").unwrap());
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
        // The caller issues nothing else meanwhile, so the step whose count
        // moved is the one that admitted this answer.
        let early_step = admitted_at(host.read_evidence().early_reads_served.since(served));
        let (reads, jobs) = (host.read_evidence(), host.evidence());
        let pull_ahead = json!({"pull_ahead_requests": reads.pull_ahead_requests,
            "requests_past_bound": reads.requests_past_bound,
            "pull_ahead_documents": jobs.pull_ahead_documents,
            "construction_commits_for_requests": jobs.construction_commits_for_requests,
            "construction_publications": jobs.construction_publications});
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
        let probe = ["whole_slug", "active_page"][index];
        super::emit(
            json!({"startup_stage":(["public-get","public-find"][index]),
            "probe":probe,"operator_prefix_sequence":index+1,"command":command,
            "mode":"public","availability_scope":"public Host first-open requests",
            "disposition":disposition,"answer":answer,"error":error,
            "counters":counters,"builder_work":work,
            "query_started_ns":started,"query_finished_ns":finished,"query_host_ns":finished-started,
            "state_after_call":state,"state_observed_ns":observed,"early_answer":early,
            "read_from":(if early_step.is_some() {"construction"} else {"canonical"}),
            "early_step":early_step,"pull_ahead":pull_ahead}),
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
    observed_ready
}

/// The one step whose count moved over `served`, a single answer's window.
fn admitted_at(served: EarlyReadsServed) -> Option<&'static str> {
    let steps = [
        ("names", served.names),
        ("content", served.content),
        ("structure", served.structure),
        ("relations", served.relations),
        ("full_text_complete", served.full_text_complete),
    ];
    assert!(
        served.total() <= 1,
        "one answer was served once: {served:?}"
    );
    steps
        .into_iter()
        .find_map(|(step, count)| (count == 1).then_some(step))
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

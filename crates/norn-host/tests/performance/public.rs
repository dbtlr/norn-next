//! Two real caller-thread requests while ordinary attachment continues independently.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use norn_host::{Host, ProductionEntryOps};
use norn_wire::{
    FindParams, GetParams, Predicate, ResolutionTarget, TrustState, VaultAddress, VaultName,
};
use serde_json::{Value, json};

/// ACKs rendezvous with this caller only; attachment owns no observer or pause.
/// Ready here means a caller observation, never the instant of publication.
pub fn prefix(
    host: &Host<ProductionEntryOps>,
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
            "state_after_call":state,"state_observed_ns":observed,"early_answer":early}),
        );
        assert_ne!(
            disposition, "error",
            "public operational refusal stops the cell"
        );
        super::staged::operator_ack(
            root,
            (index + 1) as u64,
            probe,
            super::staged::residency::PARENT_ACK_TIMEOUT,
        )
        .expect("bounded caller-only parent acknowledgement");
    }
    observed_ready
}

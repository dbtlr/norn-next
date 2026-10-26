//! Callback clocks include canonical pooled acquisition and the ordinary read builders.
use std::sync::Arc;
use std::time::Instant;

use norn_host::{FourStepEvent, FourStepLoading, FourStepObserver, FourStepRead};
use norn_wire::{FindParams, GetParams, Predicate, ResolutionTarget, VaultAddress, VaultName};
use serde_json::json;

struct Probe {
    began: Instant,
    requested: Vec<String>,
}

impl FourStepObserver for Probe {
    fn at(&self, event: FourStepEvent<'_>, read: &FourStepRead<'_>) -> Result<(), String> {
        let stage = match event {
            FourStepEvent::NamesComplete => "names",
            FourStepEvent::ContentComplete => "content",
            FourStepEvent::Structured(path) => {
                if self.requested.iter().any(|requested| requested == path) {
                    super::emit(
                        json!({"startup_stage":"four-step-requested-structure", "path":path,
                        "elapsed_ns":self.began.elapsed().as_nanos(),"admission":false}),
                    );
                }
                return Ok(());
            }
            FourStepEvent::GetReady(path) => {
                if self.requested.iter().any(|requested| requested == path) {
                    let params = GetParams::new(
                        VaultAddress::name(VaultName::new("notes").unwrap()),
                        ResolutionTarget::new(format!("/{path}")).unwrap(),
                    );
                    let started = self.began.elapsed().as_nanos();
                    let answer = read.get(&params);
                    let finished = self.began.elapsed().as_nanos();
                    match answer {
                        Ok(answer) => {
                            super::emit(json!({"startup_stage":"four-step-get", "path":path,
                            "availability_scope":"lab canonical read hook", "answer":answer.report,
                            "unsatisfied":answer.unsatisfied,"snapshot":answer.snapshot,
                            "query_started_ns":started,"query_finished_ns":finished,"query_host_ns":finished-started,
                            "builder_work":answer.work.readings().collect::<std::collections::BTreeMap<_,_>>(),
                            "current":true}))
                        }
                        Err(error) => super::emit(
                            json!({"startup_stage":"four-step-get", "path":path,"current":false,"error":error}),
                        ),
                    }
                }
                return Ok(());
            }
            FourStepEvent::StructureComplete => "structure",
            FourStepEvent::Complete(report) => {
                super::emit(
                    json!({"startup_stage":"four-step-relations", "elapsed_ns":self.began.elapsed().as_nanos(), "steps":report.steps, "reconciled":report.reconciled, "requested_structure_ns":report.requested_structure_ns, "content_complete_ns":report.content_complete_ns}),
                );
                return Ok(());
            }
        };
        super::emit(
            json!({"startup_stage":format!("four-step-{stage}"), "elapsed_ns":self.began.elapsed().as_nanos()}),
        );
        if matches!(event, FourStepEvent::ContentComplete) {
            let params = FindParams::new(VaultAddress::name(VaultName::new("notes").unwrap()))
                .with_predicates([Predicate::equal_to("perf_bucket", "active")])
                .with_limit(25);
            let started = self.began.elapsed().as_nanos();
            let answer = read.find(&params);
            let finished = self.began.elapsed().as_nanos();
            match answer {
                Ok(answer) => {
                    let work = answer
                        .work
                        .readings()
                        .collect::<std::collections::BTreeMap<_, _>>();
                    let snapshot = answer.snapshot.clone();
                    let (unsatisfied, advisories, report) = answer.into_report();
                    super::emit(
                        json!({"startup_stage":"four-step-find", "availability_scope":"lab canonical read hook",
                    "answer":report,"unsatisfied":unsatisfied,"advisories":advisories,"snapshot":snapshot,
                    "query_started_ns":started,"query_finished_ns":finished,"query_host_ns":finished-started,
                    "builder_work":work,"current":true}),
                    );
                }
                Err(error) => super::emit(
                    json!({"startup_stage":"four-step-find","current":false,"error":error}),
                ),
            }
        }
        Ok(())
    }
}

pub fn selection(mode: &str, began: Instant) -> FourStepLoading {
    let bound = std::env::var("NORN_PERFORMANCE_TRANSACTION_BOUND").map_or(128, |value| {
        value.parse().expect("an integer transaction bound")
    });
    let requested: Vec<String> = std::env::var("NORN_PERFORMANCE_REQUESTED_PATHS").map_or_else(
        |_| Vec::new(),
        |value| serde_json::from_str(&value).expect("a JSON list of requested document paths"),
    );
    FourStepLoading::new(
        bound,
        mode == "four-step-no-body",
        requested.clone(),
        Arc::new(Probe { began, requested }),
    )
    .expect("valid four-step loading options")
}

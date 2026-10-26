//! Lab callbacks retain typed answers; JSON and stdout work run after construction.
use std::sync::{Arc, Mutex};
use std::time::Instant;

use norn_host::{FourStepEvent, FourStepLoading, FourStepObserver, FourStepRead, FourStepReport};
use norn_store::{Found, Gotten, LexicalQuery, Searched};
use norn_wire::{FindParams, GetParams, Predicate, ResolutionTarget, VaultAddress, VaultName};
use serde_json::{Value, json};

const QUERY: &str = "profilecommonword";

struct Query<T> {
    available_ns: u128,
    started_ns: u128,
    finished_ns: u128,
    answer: Result<T, String>,
}

impl<T> Query<T> {
    fn run(began: Instant, available_ns: u128, read: impl FnOnce() -> Result<T, String>) -> Self {
        let started_ns = began.elapsed().as_nanos();
        let answer = read();
        let finished_ns = began.elapsed().as_nanos();
        Self {
            available_ns,
            started_ns,
            finished_ns,
            answer,
        }
    }

    fn record(&self, stage: &str) -> Value {
        json!({"startup_stage":stage, "availability_scope":"lab canonical read hook",
            "timing_scope":"earliest availability plus builder query time",
            "delivery_scope":"buffered in worker; serialized and delivered after build",
            "earliest_available_ns":self.available_ns,
            "query_started_ns":self.started_ns, "query_finished_ns":self.finished_ns,
            "query_host_ns":self.finished_ns-self.started_ns})
    }
}

enum Observation {
    Boundary(&'static str, u128),
    RequestedStructure(String, u128),
    Get(String, Query<Gotten>),
    Find(Query<Found>),
    Search(Query<Searched>),
    Complete(FourStepReport, u128),
}

pub(super) struct Probe {
    began: Instant,
    requested: Vec<String>,
    no_body: bool,
    observations: Mutex<Vec<Observation>>,
}

impl FourStepObserver for Probe {
    fn at(&self, event: FourStepEvent<'_>, read: &FourStepRead<'_>) -> Result<(), String> {
        let observed_ns = self.began.elapsed().as_nanos();
        let mut observations = self
            .observations
            .lock()
            .map_err(|error| error.to_string())?;
        let stage = match event {
            FourStepEvent::NamesComplete => "names",
            FourStepEvent::ContentComplete => "content",
            FourStepEvent::Structured(path) => {
                if self.requested.iter().any(|requested| requested == path) {
                    observations.push(Observation::RequestedStructure(
                        path.to_owned(),
                        observed_ns,
                    ));
                }
                return Ok(());
            }
            FourStepEvent::GetReady(path) => {
                if self.requested.iter().any(|requested| requested == path) {
                    let params = GetParams::new(
                        VaultAddress::name(VaultName::new("notes").unwrap()),
                        ResolutionTarget::new(format!("/{path}")).unwrap(),
                    );
                    observations.push(Observation::Get(
                        path.to_owned(),
                        Query::run(self.began, observed_ns, || read.get(&params)),
                    ));
                }
                return Ok(());
            }
            FourStepEvent::StructureComplete => "structure",
            FourStepEvent::Complete(report) => {
                observations.push(Observation::Complete(report.clone(), observed_ns));
                return Ok(());
            }
        };
        observations.push(Observation::Boundary(stage, observed_ns));
        if matches!(event, FourStepEvent::ContentComplete) {
            let params = FindParams::new(VaultAddress::name(VaultName::new("notes").unwrap()))
                .with_predicates([Predicate::equal_to("perf_bucket", "active")])
                .with_limit(25);
            observations.push(Observation::Find(Query::run(
                self.began,
                observed_ns,
                || read.find(&params),
            )));
        }
        if matches!(event, FourStepEvent::ContentComplete) && !self.no_body
            || matches!(event, FourStepEvent::StructureComplete) && self.no_body
        {
            if !read.search_complete() {
                return Err("lexical search is incomplete at its expected lab boundary".into());
            }
            let query = LexicalQuery::new(QUERY).with_limit(25);
            observations.push(Observation::Search(Query::run(
                self.began,
                observed_ns,
                || read.search(&query),
            )));
        }
        Ok(())
    }
}

impl Probe {
    pub(super) fn emit(&self) {
        let observations =
            std::mem::take(&mut *self.observations.lock().expect("buffered lab answers"));
        for observation in observations {
            let record = match observation {
                Observation::Boundary(stage, elapsed_ns) => json!({
                    "startup_stage":format!("four-step-{stage}"), "elapsed_ns":elapsed_ns}),
                Observation::RequestedStructure(path, elapsed_ns) => json!({
                    "startup_stage":"four-step-requested-structure", "path":path,
                    "elapsed_ns":elapsed_ns,"admission":false}),
                Observation::Get(path, query) => {
                    let mut record = query.record("four-step-get");
                    record["path"] = json!(path);
                    record["findings_scope"] =
                        json!("answered before link health (health loading)");
                    match query.answer {
                        Ok(answer) => {
                            record["answer"] = json!(answer.report);
                            record["unsatisfied"] = json!(answer.unsatisfied);
                            record["snapshot"] = json!(answer.snapshot);
                            record["builder_work"] = json!(
                                answer
                                    .work
                                    .readings()
                                    .collect::<std::collections::BTreeMap<_, _>>()
                            );
                            record["current"] = json!(true);
                        }
                        Err(error) => {
                            record["current"] = json!(false);
                            record["error"] = json!(error);
                        }
                    }
                    record
                }
                Observation::Find(query) => {
                    let mut record = query.record("four-step-find");
                    match query.answer {
                        Ok(answer) => {
                            record["builder_work"] = json!(
                                answer
                                    .work
                                    .readings()
                                    .collect::<std::collections::BTreeMap<_, _>>()
                            );
                            record["snapshot"] = json!(answer.snapshot);
                            let (unsatisfied, advisories, report) = answer.into_report();
                            record["answer"] = json!(report);
                            record["unsatisfied"] = json!(unsatisfied);
                            record["advisories"] = json!(advisories);
                            record["current"] = json!(true);
                        }
                        Err(error) => {
                            record["current"] = json!(false);
                            record["error"] = json!(error);
                        }
                    }
                    record
                }
                Observation::Search(query) => {
                    let mut record = query.record("four-step-search");
                    record["query"] = json!(QUERY);
                    match query.answer {
                        Ok(answer) => {
                            record["builder_work"] = json!(
                                answer
                                    .work
                                    .readings()
                                    .collect::<std::collections::BTreeMap<_, _>>()
                            );
                            record["snapshot"] = json!(answer.snapshot);
                            let (unsatisfied, advisories, report) = answer.into_report();
                            record["answer"] = json!(report);
                            record["unsatisfied"] = json!(unsatisfied);
                            record["advisories"] = json!(advisories);
                            record["current"] = json!(true);
                        }
                        Err(error) => {
                            record["current"] = json!(false);
                            record["error"] = json!(error);
                        }
                    }
                    record
                }
                Observation::Complete(report, elapsed_ns) => json!({
                    "startup_stage":"four-step-relations", "elapsed_ns":elapsed_ns,
                    "steps":report.steps, "reconciled":report.reconciled,
                    "requested_structure_ns":report.requested_structure_ns,
                    "content_complete_ns":report.content_complete_ns,
                    "boundary_ns":report.boundary_ns, "busy_ns":report.busy_ns,
                    "pull_ahead_ns":report.pull_ahead_ns}),
            };
            super::emit(record);
        }
    }
}

pub(super) fn selection(mode: &str, began: Instant) -> (FourStepLoading, Arc<Probe>) {
    let bound = std::env::var("NORN_PERFORMANCE_TRANSACTION_BOUND").map_or(128, |value| {
        value.parse().expect("an integer transaction bound")
    });
    let requested: Vec<String> = std::env::var("NORN_PERFORMANCE_REQUESTED_PATHS").map_or_else(
        |_| Vec::new(),
        |value| serde_json::from_str(&value).expect("a JSON list of requested document paths"),
    );
    let probe = Arc::new(Probe {
        began,
        requested: requested.clone(),
        no_body: mode == "four-step-no-body",
        observations: Mutex::new(Vec::new()),
    });
    let loading =
        FourStepLoading::new(bound, mode == "four-step-no-body", requested, probe.clone())
            .expect("valid four-step loading options");
    (loading, probe)
}

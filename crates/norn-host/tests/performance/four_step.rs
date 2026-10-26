//! Lab callbacks retain typed answers; JSON and stdout work run after construction.
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use norn_host::{
    FourStepEvent, FourStepLoading, FourStepObserver, FourStepRead, FourStepReport,
    FourStepRequests,
};
use norn_store::{Found, Gotten, LexicalQuery, Searched};
use norn_wire::{FindParams, GetParams, Predicate, ResolutionTarget, VaultAddress, VaultName};
use serde_json::{Value, json};

#[path = "timing.rs"]
mod timing;
use timing::GetPrerequisites;

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
            "timing_scope":"actual lab hook query and typed-answer delivery",
            "availability_clock_scope":"prerequisite completion observed on worker clock",
            "delivery_scope":"buffered in worker; serialized and delivered after build",
            "earliest_available_ns":self.available_ns,
            "delivery_ns":self.finished_ns,
            "query_started_ns":self.started_ns, "query_finished_ns":self.finished_ns,
            "query_host_ns":self.finished_ns-self.started_ns})
    }
}

enum Observation {
    Boundary(&'static str, u128),
    RequestedStructure(String, u128),
    Request(String, u128, Option<(u64, u64)>),
    Get(String, Query<Gotten>, bool, bool),
    Find(Query<Found>),
    Search(Query<Searched>),
    Complete(FourStepReport, u128),
}

#[derive(Default)]
struct Buffer {
    observations: Vec<Observation>,
    prerequisites: GetPrerequisites,
    requested: BTreeSet<String>,
    mid_requested: bool,
}

pub(super) struct Probe {
    began: Instant,
    mid_target: Option<String>,
    requests: Mutex<Option<FourStepRequests>>,
    fts_mode: &'static str,
    buffer: Mutex<Buffer>,
}

impl FourStepObserver for Probe {
    fn at(&self, event: FourStepEvent<'_>, read: &FourStepRead<'_>) -> Result<(), String> {
        let observed_ns = self.began.elapsed().as_nanos();
        let mut buffer = self.buffer.lock().map_err(|error| error.to_string())?;
        let stage = match event {
            FourStepEvent::NamesComplete => "names",
            FourStepEvent::ContentComplete => "content",
            FourStepEvent::RelationsComplete => "relations",
            FourStepEvent::FullTextComplete => "full-text",
            FourStepEvent::Batch {
                step,
                completed,
                total,
            } => {
                if step == "content" && total > 0 && completed * 4 >= total && !buffer.mid_requested
                {
                    if let Some(path) = &self.mid_target {
                        let request_ns = self.began.elapsed().as_nanos();
                        self.requests
                            .lock()
                            .map_err(|error| error.to_string())?
                            .as_ref()
                            .ok_or("missing dynamic request handle")?
                            .request(path.clone())?;
                        buffer.mid_requested = true;
                        buffer.requested.insert(path.clone());
                        buffer.observations.push(Observation::Request(
                            path.clone(),
                            request_ns,
                            Some((completed, total)),
                        ));
                    }
                }
                return Ok(());
            }
            FourStepEvent::Structured(path) => {
                if buffer.requested.contains(path) {
                    buffer.prerequisites.structured(path, observed_ns);
                    buffer.observations.push(Observation::RequestedStructure(
                        path.to_owned(),
                        observed_ns,
                    ));
                }
                return Ok(());
            }
            FourStepEvent::RequestRefused { reason, .. } => return Err(reason.to_owned()),
            FourStepEvent::GetReady(path) => {
                if buffer.requested.contains(path) {
                    let available_ns = buffer.prerequisites.available_ns(path).ok_or(
                        "GetReady arrived without its own observed structure prerequisite",
                    )?;
                    let params = GetParams::new(
                        VaultAddress::name(VaultName::new("notes").unwrap()),
                        ResolutionTarget::new(format!("/{path}")).unwrap(),
                    );
                    buffer.observations.push(Observation::Get(
                        path.to_owned(),
                        Query::run(self.began, available_ns, || read.get(&params)),
                        read.link_resolution_loading(),
                        read.link_health_loading(),
                    ));
                }
                return Ok(());
            }
            FourStepEvent::StructureComplete => "structure",
            FourStepEvent::Complete(report) => {
                buffer
                    .observations
                    .push(Observation::Complete(report.clone(), observed_ns));
                return Ok(());
            }
        };
        buffer
            .observations
            .push(Observation::Boundary(stage, observed_ns));
        if matches!(event, FourStepEvent::ContentComplete) {
            let params = FindParams::new(VaultAddress::name(VaultName::new("notes").unwrap()))
                .with_predicates([Predicate::equal_to("perf_bucket", "active")])
                .with_limit(25);
            buffer.observations.push(Observation::Find(Query::run(
                self.began,
                observed_ns,
                || read.find(&params),
            )));
        }
        if matches!(event, FourStepEvent::FullTextComplete) {
            if !read.search_complete() {
                return Err("lexical search is incomplete at its expected lab boundary".into());
            }
            let query = LexicalQuery::new(QUERY).with_limit(25);
            buffer.observations.push(Observation::Search(Query::run(
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
        let observations = std::mem::take(
            &mut self
                .buffer
                .lock()
                .expect("buffered lab answers")
                .observations,
        );
        for observation in observations {
            let record = match observation {
                Observation::Boundary(stage, elapsed_ns) => json!({
                    "startup_stage":format!("four-step-{stage}"), "elapsed_ns":elapsed_ns}),
                Observation::RequestedStructure(path, elapsed_ns) => json!({
                    "startup_stage":"four-step-requested-structure", "path":path,
                    "elapsed_ns":elapsed_ns,"admission":false}),
                Observation::Request(path, elapsed_ns, progress) => json!({
                    "startup_stage":"four-step-request", "path":path, "request_ns":elapsed_ns,
                    "request_kind":if progress.is_some() { "mid-content" } else { "start" },
                    "request_alignment":if progress.is_some() { "boundary-aligned" } else { "startup" },
                    "current_batch_wait_included":progress.map(|_| false),
                    "latency_scope":progress.map(|_| "boundary-aligned; excludes current-batch wait; best-case relative to asynchronous arrival"),
                    "content_progress":progress.map(|(completed,total)| json!({"completed":completed,"total":total})),
                }),
                Observation::Get(path, query, resolution_loading, health_loading) => {
                    let mut record = query.record("four-step-get");
                    record["path"] = json!(path);
                    record["link_resolution_loading"] = json!(resolution_loading);
                    record["link_health_loading"] = json!(health_loading);
                    record["findings_scope"] =
                        json!("link resolution loading; link health loading");
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
                    "startup_stage":"four-step-complete", "elapsed_ns":elapsed_ns,
                    "fts_mode":self.fts_mode, "request_ns":report.request_ns,
                    "delivery_ns":report.delivery_ns,
                    "refused_ns":report.refused_ns, "request_refusals":report.request_refusals,
                    "timing_commits":report.timing_commits, "timing_ns":report.timing_ns,
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
    let fts_mode = match std::env::var("NORN_PERFORMANCE_FTS_MODE").as_deref() {
        Ok("rebuild") => ("rebuild", norn_store::four_step::FtsBuildMode::Rebuild),
        Ok("batched") => ("batched", norn_store::four_step::FtsBuildMode::Batched),
        other => panic!("explicit lab FTS mode required: {other:?}"),
    };
    let mut buffer = Buffer::default();
    for path in &requested {
        let request_ns = began.elapsed().as_nanos();
        buffer.requested.insert(path.clone());
        buffer
            .observations
            .push(Observation::Request(path.clone(), request_ns, None));
    }
    let probe = Arc::new(Probe {
        began,
        mid_target: std::env::var("NORN_PERFORMANCE_MID_REQUEST_PATH").ok(),
        requests: Mutex::new(None),
        fts_mode: fts_mode.0,
        buffer: Mutex::new(buffer),
    });
    let loading =
        FourStepLoading::new(bound, mode == "four-step-no-body", requested, probe.clone())
            .expect("valid four-step loading options")
            .with_fts_mode(fts_mode.1);
    *probe.requests.lock().expect("dynamic request handle") = Some(loading.requests());
    (loading, probe)
}

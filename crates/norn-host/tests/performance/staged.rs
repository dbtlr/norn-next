//! Checkpoint requests use production SQL builders while ordinary readiness is closed.

use norn_host::startup_experiment::{Observer, Stage};
use norn_store::{ContentModel, ReadNeed, SnapshotReader, startup_experiment::Catalogue};
use norn_wire::{
    Column, CountParams, FindParams, GetParams, Predicate, ResolutionTarget, ValidateParams,
    VaultAddress, VaultName,
};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct Reporter {
    began: Instant,
    address: VaultAddress,
    checkpoint: AtomicU64,
    silent: bool,
    diagnostic_threads: bool,
    intake: AtomicBool,
    canonical_work: Mutex<Option<std::collections::BTreeMap<&'static str, u64>>>,
}

impl Reporter {
    pub fn new(began: Instant, name: VaultName, silent: bool) -> Self {
        Self {
            began,
            address: VaultAddress::name(name),
            checkpoint: AtomicU64::new(0),
            silent,
            diagnostic_threads: false,
            intake: AtomicBool::new(false),
            canonical_work: Mutex::new(None),
        }
    }

    fn emit(&self, mut value: Value) {
        if self.diagnostic_threads {
            value["thread_tid"] = json!(residency::thread_tid());
        }
        super::emit(value);
    }

    fn query(
        &self,
        stage: &str,
        probe: &str,
        reader: &Arc<SnapshotReader>,
        model: &ContentModel,
        command: Value,
    ) -> Result<(), String> {
        let boundary = self.checkpoint.load(Ordering::Relaxed);
        let requested = self.began.elapsed().as_nanos();
        let started = Instant::now();
        let turn = reader
            .acquire(ReadNeed::Source, Instant::now() + Duration::from_secs(5))
            .map_err(|error| error.to_string())?
            .ok_or("checkpoint reader unavailable")?;
        let snapshot = turn.establish().map_err(|error| error.to_string())?;
        let answer = match command["op"].as_str().expect("probe operation") {
            "get" => {
                let target = command["target"].as_str().unwrap();
                let mut params =
                    GetParams::new(self.address.clone(), ResolutionTarget::new(target).unwrap());
                if !target.contains('#') && command["whole"] != true {
                    params =
                        params.with_columns([Column::path(), Column::body(), Column::fields()]);
                }
                let (unsatisfied, report) =
                    norn_host::startup_experiment::get(&snapshot, &params, model)
                        .map_err(|error| format!("{error:?}"))?
                        .into_report();
                json!({"report":report,"unsatisfied":unsatisfied,"advisories":[]})
            }
            "find" => {
                let params = find_params(&self.address, &command);
                let (unsatisfied, advisories, report) = snapshot
                    .find(&params, model)
                    .map_err(|error| format!("{error:?}"))?
                    .into_report();
                json!({"report":report,"unsatisfied":unsatisfied,"advisories":advisories})
            }
            "search" => {
                let mut params = norn_store::LexicalQuery::new(command["query"].as_str().unwrap())
                    .with_limit(25);
                if let Some(path) = command["path"].as_str() {
                    params = params.with_predicates([Predicate::path(path)]);
                }
                let (unsatisfied, advisories, report) = snapshot
                    .search(&params, model)
                    .map_err(|error| format!("{error:?}"))?
                    .into_report();
                json!({"report":report,"unsatisfied":unsatisfied,"advisories":advisories})
            }
            "count" => {
                let params = count_params(&self.address, &command);
                let (unsatisfied, advisories, report) = snapshot
                    .count(&params, model)
                    .map_err(|error| format!("{error:?}"))?
                    .into_report();
                json!({"report":report,"unsatisfied":unsatisfied,"advisories":advisories})
            }
            "validate" => {
                let params = ValidateParams::new(self.address.clone()).summarized();
                let (unsatisfied, advisories, report) = snapshot
                    .validate(&params, model)
                    .map_err(|error| format!("{error:?}"))?
                    .into_report();
                json!({"report":report,"unsatisfied":unsatisfied,"advisories":advisories})
            }
            other => return Err(format!("unknown checkpoint operation {other}")),
        };
        let elapsed = started.elapsed().as_nanos();
        let mut record = json!({"startup_stage":stage,"probe":probe,"stage_boundary_ns":boundary,
            "query_host_ns":elapsed,"query_started_ns":requested,"query_finished_ns":self.began.elapsed().as_nanos(),
            "command":command,"answer":answer,"projection_epoch":snapshot.reading().epoch(),"projection_generation":snapshot.reading().write_generation(),"counters":snapshot.counters().readings().collect::<std::collections::BTreeMap<_,_>>(),"simulation":"paused","global_ready":false});
        if stage == "canonical-health" {
            record["canonical_work"] = json!(*self.canonical_work.lock().unwrap());
        }
        self.emit(record);
        Ok(())
    }
}

impl Observer for Reporter {
    fn canonical_work(
        &self,
        work: &std::collections::BTreeMap<&'static str, u64>,
    ) -> Result<(), String> {
        *self.canonical_work.lock().unwrap() = Some(work.clone());
        Ok(())
    }

    fn intake(
        &self,
        reader: Arc<norn_host::startup_experiment::IntakeReader>,
    ) -> Result<(), String> {
        self.intake.store(true, Ordering::Relaxed);
        let boundary = self.began.elapsed().as_nanos();
        if self.silent {
            self.emit(
                json!({"startup_stage":"canonical-intake", "probe":"boundary",
                "stage_boundary_ns":boundary,"query_host_ns":0,"queries_executed":false,
                "simulation":"paused","global_ready":false}),
            );
            return Ok(());
        }
        self.intake_queries(&reader, boundary)
    }

    fn declined(&self, reason: &str) -> Result<(), String> {
        self.emit(
            json!({"startup_stage":"canonical-demand", "probe":"declined", "reason":reason, "simulation":"paused","global_ready":false}),
        );
        Ok(())
    }
    fn catalogue(&self, catalogue: &Catalogue, model: &ContentModel) -> Result<(), String> {
        let boundary = self.began.elapsed().as_nanos();
        if self.silent {
            self.emit(
                json!({"startup_stage":"catalogue","probe":"boundary","stage_boundary_ns":boundary,
                "query_host_ns":0,"queries_executed":false,"simulation":"paused","global_ready":false}),
            );
            return Ok(());
        }
        let requested = self.began.elapsed().as_nanos();
        let began = Instant::now();
        let mut answers = Vec::new();
        for target in ["small", "roadmap", "profilenonexistentdocument"] {
            let found = catalogue
                .candidates(target, model.ambiguity_ignore())
                .map_err(|error| error.to_string())?
                .ok_or("catalogue incomplete")?;
            answers.push(json!({"target":target,"paths":found.paths,"total":found.total}));
        }
        let mut record = json!({"startup_stage":"catalogue","probe":"names","stage_boundary_ns":boundary,
            "query_host_ns":began.elapsed().as_nanos(),"query_finished_ns":self.began.elapsed().as_nanos(),
            "candidates":answers,"simulation":"paused","global_ready":false});
        if self.diagnostic_threads {
            record["query_started_ns"] = json!(requested);
        }
        self.emit(record);
        Ok(())
    }

    fn indexed(
        &self,
        stage: Stage,
        reader: Arc<SnapshotReader>,
        model: Arc<ContentModel>,
    ) -> Result<(), String> {
        let boundary = self.began.elapsed().as_nanos() as u64;
        self.checkpoint.store(boundary, Ordering::Relaxed);
        if self.silent {
            let names: &[&str] = match stage {
                Stage::CanonicalDemand => &["canonical-demand"],
                Stage::CanonicalHealth => &["canonical-health"],
                Stage::RequestedDocument => &["get"],
                Stage::Frontmatter => &["frontmatter", "lexical"],
                Stage::RichFacts => &["rich"],
                Stage::AllFacts => &["frontmatter", "lexical", "rich"],
                Stage::LinkHealth => &["health"],
            };
            for name in names {
                let mut record = json!({"startup_stage":name,"probe":"boundary","stage_boundary_ns":boundary,
                    "query_host_ns":0,"queries_executed":false,"simulation":"paused","global_ready":false});
                if stage == Stage::CanonicalHealth {
                    record["canonical_work"] = json!(*self.canonical_work.lock().unwrap());
                }
                self.emit(record);
            }
            return Ok(());
        }
        self.probes(stage, &reader, &model)
    }
}

impl Reporter {
    fn intake_queries(
        &self,
        reader: &Arc<norn_host::startup_experiment::IntakeReader>,
        boundary: u128,
    ) -> Result<(), String> {
        for (probe, command) in intake_probes() {
            let requested = self.began.elapsed().as_nanos();
            let started = Instant::now();
            let snapshot = reader
                .snapshot(Instant::now() + Duration::from_secs(5))
                .map_err(|e| e.to_string())?
                .ok_or("intake reader unavailable")?;
            let answer = if command["op"] == "find" {
                let (unsatisfied, advisories, report) = snapshot
                    .find(&find_params(&self.address, &command))
                    .map_err(|e| format!("{e:?}"))?
                    .into_report();
                json!({"report":report,"unsatisfied":unsatisfied,"advisories":advisories})
            } else {
                let (unsatisfied, advisories, report) = snapshot
                    .count(&count_params(&self.address, &command))
                    .map_err(|e| format!("{e:?}"))?
                    .into_report();
                json!({"report":report,"unsatisfied":unsatisfied,"advisories":advisories})
            };
            self.emit(json!({"startup_stage":"canonical-intake","probe":probe,
                "stage_boundary_ns":boundary,"query_host_ns":started.elapsed().as_nanos(),
                "query_started_ns":requested,"query_finished_ns":self.began.elapsed().as_nanos(),
                "command":command,"answer":answer,"projection_epoch":snapshot.reading().epoch(),
                "projection_generation":snapshot.reading().write_generation(),"simulation":"paused","global_ready":false}));
        }
        Ok(())
    }
    fn probes(
        &self,
        stage: Stage,
        reader: &Arc<SnapshotReader>,
        model: &ContentModel,
    ) -> Result<(), String> {
        match stage {
            Stage::CanonicalDemand => self.query(
                "canonical-demand",
                "whole_slug",
                reader,
                model,
                json!({"op":"get","target":"small","whole":true}),
            ),
            Stage::CanonicalHealth => {
                self.query(
                    "canonical-health",
                    "whole_slug",
                    reader,
                    model,
                    json!({"op":"get","target":"small","whole":true}),
                )?;
                if self.intake.load(Ordering::Relaxed) {
                    for (probe, command) in intake_probes() {
                        self.query("canonical-final-intake", probe, reader, model, command)?;
                    }
                }
                self.probes(Stage::Frontmatter, reader, model)?;
                self.probes(Stage::RichFacts, reader, model)?;
                self.probes(Stage::LinkHealth, reader, model)
            }
            Stage::AllFacts => {
                self.probes(Stage::Frontmatter, reader, model)?;
                self.probes(Stage::RichFacts, reader, model)
            }
            Stage::RequestedDocument => self.query(
                "get",
                "slug",
                reader,
                model,
                json!({"op":"get","target":"small"}),
            ),
            Stage::Frontmatter => {
                self.query(
                    "frontmatter",
                    "active_page",
                    reader,
                    model,
                    json!({"op":"find"}),
                )?;
                self.query(
                    "frontmatter",
                    "document_count",
                    reader,
                    model,
                    json!({"op":"count"}),
                )?;
                self.query(
                    "frontmatter",
                    "active_count",
                    reader,
                    model,
                    json!({"op":"count","active":true}),
                )?;
                self.query(
                    "lexical",
                    "common",
                    reader,
                    model,
                    json!({"op":"search","query":"profilecommonword"}),
                )?;
                self.query(
                    "lexical",
                    "rare",
                    reader,
                    model,
                    json!({"op":"search","query":"profileraresignal"}),
                )?;
                self.query(
                    "lexical",
                    "missing",
                    reader,
                    model,
                    json!({"op":"search","query":"profilenonexistenttoken"}),
                )?;
                self.query(
                    "lexical",
                    "filtered",
                    reader,
                    model,
                    json!({"op":"search","query":"profilecommonword","path":"bench/**"}),
                )?;
                self.query(
                    "lexical",
                    "coverage",
                    reader,
                    model,
                    json!({"op":"count","matches":"profilecommonword"}),
                )
            }
            Stage::RichFacts => self.query(
                "rich",
                "section",
                reader,
                model,
                json!({"op":"get","target":"bench/large.md#Details"}),
            ),
            Stage::LinkHealth => {
                self.query("health", "summary", reader, model, json!({"op":"validate"}))
            }
        }
    }
}

fn find_params(address: &VaultAddress, command: &Value) -> FindParams {
    let key = command["key"].as_str().unwrap_or("perf_bucket");
    let value = command["value"].as_str().unwrap_or("active");
    let mut params = FindParams::new(address.clone())
        .with_limit(25)
        .with_predicates([Predicate::equal_to(key, value)]);
    if command["field_sort"] == true {
        params = params
            .with_sort(norn_wire::Sort::new(
                norn_wire::SortKey::field("perf_bucket"),
                norn_wire::Direction::Descending,
            ))
            .with_columns([Column::path(), Column::field("perf_bucket")]);
    }
    params
}

fn count_params(address: &VaultAddress, command: &Value) -> CountParams {
    let mut params = CountParams::new(address.clone());
    if command["active"] == true {
        params = params.with_predicates([Predicate::equal_to("perf_bucket", "active")]);
    }
    if let Some(text) = command["matches"].as_str() {
        params = params.with_predicates([Predicate::matches(text)]);
    }
    params
}

fn intake_probes() -> [(&'static str, Value); 7] {
    [
        ("active_page", json!({"op":"find"})),
        ("document_count", json!({"op":"count"})),
        ("active_count", json!({"op":"count","active":true})),
        ("sparse_page", json!({"op":"find","value":"probe"})),
        (
            "zero_page",
            json!({"op":"find","value":"profilenonexistentvalue"}),
        ),
        (
            "unknown_field",
            json!({"op":"find","key":"profilenonexistentkey"}),
        ),
        (
            "field_page",
            json!({"op":"find","value":"probe","field_sort":true}),
        ),
    ]
}

#[path = "residency.rs"]
pub mod residency;

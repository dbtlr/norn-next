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
    operator_prefix: bool,
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
            operator_prefix: false,
            diagnostic_threads: false,
            intake: AtomicBool::new(false),
            canonical_work: Mutex::new(None),
        }
    }

    fn emit(&self, mut value: Value) {
        if self.operator_prefix {
            value["operator_prefix_sequence"] =
                json!(if value["startup_stage"] == "canonical-demand" {
                    1
                } else {
                    2
                });
            value["availability_scope"] = json!("private simulated capability availability");
            if value["startup_stage"] == "canonical-intake" {
                value["counters"] = Value::Null;
                value["counters_unavailable"] =
                    json!("IntakeSnapshot exposes guarded queries and provenance, not counters");
            }
        }
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
            self.intake_query(reader, boundary, probe, command)?;
        }
        Ok(())
    }
    fn intake_query(
        &self,
        reader: &Arc<norn_host::startup_experiment::IntakeReader>,
        boundary: u128,
        probe: &str,
        command: Value,
    ) -> Result<(), String> {
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

/// Exactly two private operator requests on the existing attachment thread.
/// Parent acknowledgement follows receipt and full validation, before heal resumes.
pub struct OperatorPrefix {
    reporter: Reporter,
    root: std::path::PathBuf,
    completed: AtomicU64,
}

impl OperatorPrefix {
    pub fn new(began: Instant, name: VaultName, root: std::path::PathBuf) -> Self {
        let mut reporter = Reporter::new(began, name, false);
        reporter.operator_prefix = true;
        Self {
            reporter,
            root,
            completed: AtomicU64::new(0),
        }
    }

    pub fn receipt(&self) -> Value {
        json!({"mode":"private-operator-prefix", "queries_executed":self.completed.load(Ordering::Relaxed),
            "canonical_work":*self.reporter.canonical_work.lock().unwrap(),
            "construction_eligibility_gets_declared":1,
            "dispatch":"private observer on attachment thread",
            "availability_scope":"private simulated capability availability"})
    }

    fn acknowledged(&self, sequence: u64, probe: &str) -> Result<(), String> {
        operator_ack(&self.root, sequence, probe, residency::PARENT_ACK_TIMEOUT)?;
        self.completed.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

pub fn operator_ack(
    root: &std::path::Path,
    sequence: u64,
    probe: &str,
    timeout: Duration,
) -> Result<(), String> {
    let path = root
        .join("operator-prefix-acks")
        .join(format!("{sequence}.json"));
    let deadline = Instant::now() + timeout.min(residency::PARENT_ACK_TIMEOUT);
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
        std::thread::sleep(Duration::from_millis(1));
    }
}

impl Observer for OperatorPrefix {
    fn catalogue(&self, _: &Catalogue, _: &ContentModel) -> Result<(), String> {
        Ok(())
    }

    fn declined(&self, reason: &str) -> Result<(), String> {
        Err(format!(
            "operator prefix requires the frozen eligible fixture: {reason}"
        ))
    }

    fn canonical_work(
        &self,
        work: &std::collections::BTreeMap<&'static str, u64>,
    ) -> Result<(), String> {
        self.reporter.canonical_work(work)
    }

    fn indexed(
        &self,
        stage: Stage,
        reader: Arc<SnapshotReader>,
        model: Arc<ContentModel>,
    ) -> Result<(), String> {
        match stage {
            Stage::CanonicalDemand => {
                self.reporter.checkpoint.store(
                    self.reporter.began.elapsed().as_nanos() as u64,
                    Ordering::Relaxed,
                );
                self.reporter.query(
                    "canonical-demand",
                    "whole_slug",
                    &reader,
                    &model,
                    json!({"op":"get","target":"small","whole":true}),
                )?;
                drop(reader);
                drop(model);
                self.acknowledged(1, "whole_slug")
            }
            Stage::CanonicalHealth => Ok(()),
            _ => Err("unexpected operator prefix stage".into()),
        }
    }

    fn intake(
        &self,
        reader: Arc<norn_host::startup_experiment::IntakeReader>,
    ) -> Result<(), String> {
        self.reporter.intake_query(
            &reader,
            self.reporter.began.elapsed().as_nanos(),
            "active_page",
            json!({"op":"find"}),
        )?;
        drop(reader);
        self.acknowledged(2, "active_page")
    }
}

/// One frozen capped attempt, followed by an explicitly required Host fallback
/// after Ready when the ordinary page cannot certify its terminal boundary.
#[allow(dead_code)] // The full-intake native harness compiles this shared module too.
pub struct CappedPrefix {
    operator: OperatorPrefix,
    attempt: Mutex<Option<Value>>,
}
#[allow(dead_code)]
impl CappedPrefix {
    pub fn new(began: Instant, name: VaultName, root: std::path::PathBuf) -> Self {
        Self {
            operator: OperatorPrefix::new(began, name, root),
            attempt: Mutex::new(None),
        }
    }
    pub fn receipt(&self) -> Value {
        let attempt = self.attempt.lock().unwrap();
        let attempted = attempt.is_some();
        let published = attempt
            .as_ref()
            .is_some_and(|a| a["early_published"] == true);
        let sql = attempt
            .as_ref()
            .is_some_and(|a| a["query_executed"] == true);
        json!({"mode":"private-capped-prefix", "cap_document_candidates":128,
            "logical_requests":2,"early_find_attempts":u64::from(attempted),
            "early_find_published":u64::from(published),"prefix_find_sql_dispatches":u64::from(sql),
            "queries_executed":1+u64::from(sql),"fallback_required":attempted && !published,
            "fallback_dispatch":"same logical Find through Host after Ready; controller owns this dispatch",
            "canonical_work":*self.operator.reporter.canonical_work.lock().unwrap(),
            "construction_eligibility_gets_declared":1,
            "frozen_input_authority":"explicit immutable fixture assumption; not live freshness",
            "dispatch":"private observer on attachment thread",
            "availability_scope":"private simulated capability availability"})
    }
}
impl Observer for CappedPrefix {
    fn catalogue(&self, _: &Catalogue, _: &ContentModel) -> Result<(), String> {
        Ok(())
    }
    fn indexed(
        &self,
        stage: Stage,
        reader: Arc<SnapshotReader>,
        model: Arc<ContentModel>,
    ) -> Result<(), String> {
        self.operator.indexed(stage, reader, model)
    }
    fn canonical_work(
        &self,
        work: &std::collections::BTreeMap<&'static str, u64>,
    ) -> Result<(), String> {
        self.operator.canonical_work(work)
    }
    fn declined(&self, reason: &str) -> Result<(), String> {
        self.operator.declined(reason)
    }
    fn prefix(&self, reader: &norn_host::startup_experiment::PrefixReader) -> Result<(), String> {
        let reporter = &self.operator.reporter;
        let boundary = reporter.began.elapsed().as_nanos();
        let started = Instant::now();
        let command = json!({"op":"find"});
        let attempt = reader.find(&find_params(&reporter.address, &command));
        let elapsed = started.elapsed().as_nanos();
        // This private phase has one sequential query. Its established grants
        // must return before the observer emits a response and waits for ACK2.
        if let Some([_, _, after]) = attempt.read_budget
            && after.live_connections != after.idle_connections
        {
            return Err("query grants retained before sequential ACK2".into());
        }
        let certificate = &attempt.certificate;
        let answer = attempt.answer.map(|answer| {
            let (unsatisfied, advisories, report) = answer.into_report();
            json!({"report":report,"unsatisfied":unsatisfied,"advisories":advisories})
        });
        let published = answer.is_some();
        let read_budget = attempt.read_budget.map(|readings| readings.map(|reading| json!({
            "connections_opened":reading.connections_opened,"live_connections":reading.live_connections,
            "idle_connections":reading.idle_connections,"waiting_reads":reading.waiting_reads,"peak_connections":reading.peak_connections
        })));
        let record = json!({"startup_stage":"canonical-prefix", "probe":"active_page",
            "command":command,"answer":answer,"early_published":published,
            "query_executed":attempt.query_executed,"decline_reason":attempt.decline,"read_budget_before_during_after":read_budget,
            "projection_epoch":attempt.reading.as_ref().map(|r| r.epoch()),
            "projection_generation":attempt.reading.as_ref().map(|r| r.write_generation()),
            "counters":attempt.counters,"stage_boundary_ns":boundary,
            "query_started_ns":boundary,"query_host_ns":elapsed,"query_finished_ns":reporter.began.elapsed().as_nanos(),
            "certificate":{"cap_document_candidates":128,"consumed":certificate.consumed,"catalogue_replayed":certificate.catalogue_replayed,
                "catalogue_document_candidates":certificate.catalogue_document_candidates,
                "frontier":certificate.frontier,"eof":certificate.eof,"invalid":certificate.invalid,
                "bytes_read_including_reobservation":certificate.bytes_read,"writer_work":certificate.writer_work,
                "authority":"FROZEN fixture names, bytes, schema and root; no live freshness proof",
                "catalogue_sql_counters":null,"seal_sql_counters":null},
            "simulation":"paused","global_ready":false});
        *self.attempt.lock().unwrap() = Some(record.clone());
        reporter.emit(record);
        // PrefixReader::find has already released its actual IntakeSnapshot.
        self.operator.acknowledged(2, "active_page")
    }
    fn prefix_retired(&self) -> Result<(), String> {
        fn check(directory: &std::path::Path) -> Result<(), String> {
            if !directory.exists() {
                return Ok(());
            }
            for entry in std::fs::read_dir(directory).map_err(|e| e.to_string())? {
                let path = entry.map_err(|e| e.to_string())?.path();
                if path.is_dir() {
                    check(&path)?;
                } else {
                    let name = path.file_name().unwrap().to_string_lossy();
                    if name.starts_with("intake-")
                        || name.starts_with("demand-")
                        || path.ancestors().any(|part| {
                            matches!(
                                part.file_name().and_then(|name| name.to_str()),
                                Some("canonical-demand" | "canonical-intake")
                            )
                        })
                    {
                        return Err(format!(
                            "projection retained before canonical heal: {}",
                            path.display()
                        ));
                    }
                }
            }
            Ok(())
        }
        check(&self.operator.root.join("data"))
    }
}

/// The live selection reuses the operator rendezvous with phase-specific receipts.
/// Catalogue and canonical-health observations deliberately perform no queries.
pub struct LivePrefix {
    operator: OperatorPrefix,
    attempts: Mutex<Vec<Value>>,
    eligibility: Mutex<Vec<Value>>,
}
impl LivePrefix {
    pub fn new(began: Instant, name: VaultName, root: std::path::PathBuf) -> Self {
        Self {
            operator: OperatorPrefix::new(began, name, root),
            attempts: Mutex::new(Vec::new()),
            eligibility: Mutex::new(Vec::new()),
        }
    }

    pub fn receipt(&self) -> Value {
        json!({"mode":"private-live-prefix","logical_requests":2,
            "attempts":*self.attempts.lock().unwrap(),
            "construction_eligibility_gets":*self.eligibility.lock().unwrap(),
            "canonical_work":*self.operator.reporter.canonical_work.lock().unwrap(),
            "dispatch":"private observer on attachment thread",
            "availability_scope":"private live capability availability"})
    }

    fn send<T>(
        &self,
        phase: norn_host::startup_experiment::LivePhase,
        boundary: u128,
        started: u128,
        attempt: norn_host::startup_experiment::LiveAttempt<T>,
        encode: impl FnOnce(T) -> Value,
    ) -> Result<(), String> {
        use norn_host::startup_experiment::{LivePhase, LiveRefusal};
        let reporter = &self.operator.reporter;
        let finished = reporter.began.elapsed().as_nanos();
        let (disposition, reason, answer, withdrawn) = match attempt.result {
            Ok(answer) => ("served", None, Some(encode(answer)), None),
            Err(error) => {
                let disposition = match error {
                    LiveRefusal::Unavailable if attempt.withdrawn.is_some() => "withdrawn",
                    LiveRefusal::Unavailable => "unavailable",
                    LiveRefusal::Unsupported => "unsupported",
                    _ => "error",
                };
                (
                    disposition,
                    Some(format!("{error:?}")),
                    None,
                    attempt.withdrawn.map(encode),
                )
            }
        };
        let typed = answer.as_ref().or(withdrawn.as_ref());
        let (sequence, probe, command, stage) = match phase {
            LivePhase::Get => (
                1,
                "whole_slug",
                json!({"op":"get","target":"small","whole":true}),
                "live-get",
            ),
            LivePhase::Find => (2, "active_page", json!({"op":"find"}), "live-find"),
        };
        let record = json!({"startup_stage":stage,"probe":probe,"command":command,
            "operator_prefix_sequence":sequence,"mode":"live","global_ready":false,
            "availability_scope":"private live capability availability",
            "disposition":disposition,"reason":reason,
            "builder_invoked":attempt.evidence.builder_invoked,
            "counters_before_builder":attempt.evidence.counters_before_builder.map(|c| c.readings().collect::<std::collections::BTreeMap<_,_>>()),
            "projection_epoch":attempt.evidence.reading.as_ref().map(|r| r.epoch()),
            "projection_generation":attempt.evidence.reading.as_ref().map(|r| r.write_generation()),
            "counters":attempt.evidence.counters.map(|c| c.readings().collect::<std::collections::BTreeMap<_,_>>()),
            "typed_snapshot":typed.map(|v| &v["snapshot"]),
            "builder_work":typed.map(|v| &v["work"]),
            "answer":answer.as_ref().map(|v| &v["answer"]),
            "withdrawn_answer":withdrawn.as_ref().map(|v| &v["answer"]),
            "stage_boundary_ns":boundary,"query_started_ns":started,"query_finished_ns":finished,
            "query_host_ns":finished-started});
        self.attempts.lock().unwrap().push(record.clone());
        super::emit(record);
        self.operator.acknowledged(sequence, probe)
    }
}
impl Observer for LivePrefix {
    fn catalogue(&self, _: &Catalogue, _: &ContentModel) -> Result<(), String> {
        Ok(())
    }
    fn indexed(
        &self,
        stage: Stage,
        _: Arc<SnapshotReader>,
        _: Arc<ContentModel>,
    ) -> Result<(), String> {
        if stage != Stage::CanonicalHealth {
            return Err("unexpected live stage".into());
        }
        Ok(())
    }
    fn canonical_work(
        &self,
        work: &std::collections::BTreeMap<&'static str, u64>,
    ) -> Result<(), String> {
        self.operator.reporter.canonical_work(work)
    }
    fn live_eligibility_get(
        &self,
        evidence: &norn_host::startup_experiment::LiveReadEvidence,
    ) -> Result<(), String> {
        self.eligibility.lock().unwrap().push(json!({"builder_invoked":evidence.builder_invoked,
            "projection_epoch":evidence.reading.as_ref().map(|r| r.epoch()),
            "projection_generation":evidence.reading.as_ref().map(|r| r.write_generation()),
            "counters":evidence.counters.map(|c| c.readings().collect::<std::collections::BTreeMap<_,_>>()),
            "counters_before_builder":evidence.counters_before_builder.map(|c| c.readings().collect::<std::collections::BTreeMap<_,_>>())}));
        Ok(())
    }
    fn live_declined(
        &self,
        phase: norn_host::startup_experiment::LivePhase,
        reason: &str,
    ) -> Result<(), String> {
        use norn_host::startup_experiment::LivePhase;
        let (sequence, probe, command, stage) = match phase {
            LivePhase::Get => (
                1,
                "whole_slug",
                json!({"op":"get","target":"small","whole":true}),
                "live-get",
            ),
            LivePhase::Find => (2, "active_page", json!({"op":"find"}), "live-find"),
        };
        let now = self.operator.reporter.began.elapsed().as_nanos();
        let record = json!({"startup_stage":stage,"probe":probe,"command":command,"operator_prefix_sequence":sequence,
            "mode":"live","global_ready":false,"availability_scope":"private live capability availability",
            "disposition":"construction-declined","reason":reason,"builder_invoked":false,
            "projection_epoch":null,"projection_generation":null,"counters":null,"counters_before_builder":null,"typed_snapshot":null,
            "builder_work":null,"answer":null,"withdrawn_answer":null,
            "stage_boundary_ns":now,"query_started_ns":now,"query_finished_ns":now,"query_host_ns":0});
        self.attempts.lock().unwrap().push(record.clone());
        super::emit(record);
        self.operator.acknowledged(sequence, probe)
    }
    fn live_get(
        &self,
        reader: Arc<norn_host::startup_experiment::LiveGetReader>,
    ) -> Result<(), String> {
        let reporter = &self.operator.reporter;
        let boundary = reporter.began.elapsed().as_nanos();
        let params = GetParams::new(
            reporter.address.clone(),
            ResolutionTarget::new("small").unwrap(),
        );
        let started = reporter.began.elapsed().as_nanos();
        let attempt = reader.get_measured(&params, Instant::now() + Duration::from_secs(5));
        self.send(norn_host::startup_experiment::LivePhase::Get,boundary,started,attempt, |answer| {
            let snapshot = answer.snapshot.clone();
            let work = answer.work.readings().collect::<std::collections::BTreeMap<_,_>>();
            let (unsatisfied,report) = answer.into_report();
            json!({"snapshot":snapshot,"work":work,"answer":{"report":report,"unsatisfied":unsatisfied,"advisories":[]}})
        })
    }
    fn live_find(
        &self,
        reader: Arc<norn_host::startup_experiment::LiveFindReader>,
    ) -> Result<(), String> {
        let reporter = &self.operator.reporter;
        let boundary = reporter.began.elapsed().as_nanos();
        let params = find_params(&reporter.address, &json!({"op":"find"}));
        let started = reporter.began.elapsed().as_nanos();
        let attempt = reader.find_measured(&params, Instant::now() + Duration::from_secs(5));
        self.send(norn_host::startup_experiment::LivePhase::Find,boundary,started,attempt, |answer| {
            let snapshot = answer.snapshot.clone();
            let work = answer.work.readings().collect::<std::collections::BTreeMap<_,_>>();
            let (unsatisfied,advisories,report) = answer.into_report();
            json!({"snapshot":snapshot,"work":work,"answer":{"report":report,"unsatisfied":unsatisfied,"advisories":advisories}})
        })
    }
}

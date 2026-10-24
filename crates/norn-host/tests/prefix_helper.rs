//! Private native lifecycle controls. Every clock in this target is nonqualifying.
#![cfg(feature = "induced-failure")]
#![allow(clippy::disallowed_methods)]
mod attach;
use norn_host::startup_experiment::{
    Experiment, FrozenInput, HelperCause, HelperControl, Observer, PrefixReader, Stage,
};
use norn_store::{ContentModel, SnapshotReader, startup_experiment::Catalogue};
use norn_wire::{FindParams, GetParams, Predicate, VaultAddress};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn query(address: &VaultAddress) -> FindParams {
    FindParams::new(address.clone())
        .with_limit(25)
        .with_predicates([Predicate::equal_to("perf_bucket", "active")])
}
fn get_query(address: &VaultAddress) -> GetParams {
    GetParams::new(
        address.clone(),
        norn_wire::ResolutionTarget::new("first").unwrap(),
    )
}
fn scratch() -> norn_testkit::scratch::Scratch {
    norn_testkit::scratch::Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "prefix-helper")
        .unwrap()
}
fn fixture(root: &Path, documents: usize, active: bool) -> attach::Vault {
    std::fs::create_dir_all(root.join("vault/.norn")).unwrap();
    std::fs::write(
        root.join("vault/.norn/schema.yaml"),
        "version: 1\nfields:\n  perf_bucket: { type: text }\n",
    )
    .unwrap();
    std::fs::write(root.join("vault/first.md"), "---\nperf_bucket: active\nlist: [one, two]\n---\n# First\n[[later#anchor]] [[missing]] #tag\nParagraph ^block\n").unwrap();
    std::fs::write(root.join("vault/later.md"), "# Anchor\n").unwrap();
    for i in 2..documents {
        std::fs::write(
            root.join(format!("vault/a-{i:03}.md")),
            format!(
                "---\nperf_bucket: {}\n---\nBody\n",
                if active || i > 150 {
                    "active"
                } else {
                    "inactive"
                }
            ),
        )
        .unwrap();
    }
    attach::Vault::adopt(root)
}
fn files(root: &Path) -> Vec<PathBuf> {
    let mut result = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                result.extend(files(&entry.path()));
            } else {
                result.push(entry.path());
            }
        }
    }
    result.sort();
    result
}
fn source(root: &Path) -> Vec<(PathBuf, Vec<u8>, u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    files(root)
        .into_iter()
        .map(|path| {
            let metadata = std::fs::metadata(&path).unwrap();
            let bytes = std::fs::read(&path).unwrap();
            (
                path.strip_prefix(root).unwrap().to_owned(),
                bytes,
                metadata.dev(),
                metadata.ino(),
            )
        })
        .collect()
}
fn assert_no_projection(root: &Path) {
    for path in files(root) {
        assert!(
            !path.components().any(|part| matches!(
                part.as_os_str().to_str(),
                Some("canonical-demand" | "canonical-intake")
            )),
            "residue: {}",
            path.display()
        );
    }
}

struct Transcript {
    address: VaultAddress,
    stages: Mutex<Vec<Stage>>,
    get: Mutex<Option<Value>>,
    find: Mutex<Option<Value>>,
    unsupported_find: bool,
    held: Mutex<Option<norn_store::Snapshot>>,
    retained_reader: Mutex<Option<Arc<SnapshotReader>>>,
    hold: bool,
    held_model: Mutex<Option<Arc<ContentModel>>>,
    ack: Option<Arc<HelperControl>>,
    sabotage: Option<PathBuf>,
    retired_path: Mutex<Option<PathBuf>>,
}
impl Transcript {
    fn new(vault: &attach::Vault) -> Self {
        Self {
            address: VaultAddress::name(vault.name().clone()),
            stages: Mutex::new(Vec::new()),
            get: Mutex::new(None),
            find: Mutex::new(None),
            unsupported_find: false,
            held: Mutex::new(None),
            retained_reader: Mutex::new(None),
            hold: false,
            held_model: Mutex::new(None),
            ack: None,
            sabotage: None,
            retired_path: Mutex::new(None),
        }
    }
    fn acknowledge(&self, sequence: u8, receipt: &Value) -> Result<(), String> {
        if let Some(control) = &self.ack {
            use std::io::Write;
            println!(
                "HELPER {}",
                json!({"event":"response","sequence":sequence,"receipt":receipt})
            );
            std::io::stdout().flush().map_err(|e| e.to_string())?;
            let mut ack = String::new();
            if std::io::stdin()
                .read_line(&mut ack)
                .map_err(|e| e.to_string())?
                == 0
            {
                return Err("ACK transport disconnected".into());
            }
            if ack.trim() == "cancel" {
                control.cancel();
                return Err("ACK cancelled".into());
            }
            if ack.trim() != format!("ack-{sequence}") {
                return Err("unexpected ACK".into());
            }
        }
        Ok(())
    }
}
impl Observer for Transcript {
    fn canonical_work(
        &self,
        _: &std::collections::BTreeMap<&'static str, u64>,
    ) -> Result<(), String> {
        panic!("helper entered canonical heal")
    }
    fn catalogue(&self, _: &Catalogue, _: &ContentModel) -> Result<(), String> {
        Ok(())
    }
    fn indexed(
        &self,
        stage: Stage,
        reader: Arc<SnapshotReader>,
        model: Arc<ContentModel>,
    ) -> Result<(), String> {
        self.stages.lock().unwrap().push(stage);
        assert_eq!(
            stage,
            Stage::CanonicalDemand,
            "no canonical health or Ready path"
        );
        let snapshot = reader
            .acquire(
                norn_store::ReadNeed::Source,
                Instant::now() + Duration::from_secs(5),
            )
            .unwrap()
            .unwrap()
            .establish()
            .unwrap();
        let gotten =
            norn_host::startup_experiment::get(&snapshot, &get_query(&self.address), &model)
                .unwrap();
        assert!(gotten.unsatisfied.is_empty());
        assert_eq!(gotten.snapshot.epoch, snapshot.reading().epoch());
        let receipt = json!({"report":gotten.report,"snapshot":gotten.snapshot,"unsatisfied":gotten.unsatisfied});
        *self.get.lock().unwrap() = Some(receipt.clone());
        if self.hold {
            *self.held.lock().unwrap() = Some(snapshot);
            *self.held_model.lock().unwrap() = Some(model);
            *self.retained_reader.lock().unwrap() = Some(reader);
        } else {
            drop(snapshot);
            drop(reader);
        }
        self.acknowledge(1, &receipt)
    }
    fn prefix(&self, reader: &PrefixReader) -> Result<(), String> {
        let params = if self.unsupported_find {
            query(&self.address).with_limit(24)
        } else {
            query(&self.address)
        };
        let mut attempt = reader.find(&params);
        let answer = attempt.answer.take().map(|found| {
            assert_eq!(found.snapshot.epoch, attempt.reading.as_ref().unwrap().epoch());
            if let Some(cursor) = &found.next { assert_eq!(cursor.snapshot(), &found.snapshot); }
            let snapshot = found.snapshot.clone();
            let (unsatisfied, advisories, report) = found.into_report();
            json!({"snapshot":snapshot,"report":report,"unsatisfied":unsatisfied,"advisories":advisories})
        });
        let receipt = json!({"answer":answer,"decline":attempt.decline,"query_executed":attempt.query_executed,
            "counters":attempt.counters,"consumed":attempt.certificate.consumed,"eof":attempt.certificate.eof});
        *self.find.lock().unwrap() = Some(receipt.clone());
        if let Some(root) = &self.sabotage {
            *self.retired_path.lock().unwrap() = files(root).into_iter().find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("intake-prefix-")
            });
        }
        self.acknowledge(2, &receipt)
    }
    fn prefix_retired(&self) -> Result<(), String> {
        if let Some(path) = self.retired_path.lock().unwrap().as_ref() {
            assert!(
                !path.exists(),
                "the actual intake retired before this callback"
            );
            std::fs::create_dir(path).unwrap();
        }
        Ok(())
    }
}
fn experiment(probe: Arc<Transcript>, target: &str) -> Arc<Experiment> {
    Experiment::capped_prefix_frozen(target, probe, FrozenInput::assume_immutable())
}

#[test]
fn successful_prefix_helper_stops_before_canonical_heal_or_ready() {
    let scratch = scratch();
    let vault = fixture(scratch.root(), 2, true);
    let before = source(vault.path());
    let probe = Arc::new(Transcript::new(&vault));
    let completed = vault
        .prefix_helper(&experiment(probe.clone(), "first"), &Default::default())
        .unwrap();
    assert_eq!(completed.budget().live_connections, 0);
    assert_eq!(completed.budget().waiting_reads, 0);
    assert!(!completed.retired_paths().is_empty());
    for path in completed.retired_paths() {
        assert!(!path.exists(), "{}", path.display());
    }
    assert_eq!(*probe.stages.lock().unwrap(), [Stage::CanonicalDemand]);
    assert_eq!(
        probe.find.lock().unwrap().as_ref().unwrap()["query_executed"],
        true
    );
    assert_eq!(source(vault.path()), before);
    assert_no_projection(scratch.root());
}

#[test]
fn sql_and_non_sql_find_declines_still_finish_but_required_get_refusal_fails() {
    for unsupported in [false, true] {
        let scratch = scratch();
        let vault = fixture(scratch.root(), 314, false);
        let mut probe = Transcript::new(&vault);
        probe.unsupported_find = unsupported;
        let probe = Arc::new(probe);
        vault
            .prefix_helper(&experiment(probe.clone(), "first"), &Default::default())
            .unwrap();
        let find = probe.find.lock().unwrap();
        let find = find.as_ref().unwrap();
        assert!(find["answer"].is_null());
        assert!(find["decline"].is_string());
        assert_eq!(find["query_executed"], !unsupported);
        assert_eq!(find["counters"].is_object(), !unsupported);
        assert_no_projection(scratch.root());
    }
    let scratch = scratch();
    let vault = fixture(scratch.root(), 2, true);
    let probe = Arc::new(Transcript::new(&vault));
    let failure = vault
        .prefix_helper(&experiment(probe.clone(), "absent"), &Default::default())
        .unwrap_err();
    assert!(
        matches!(failure.cause, HelperCause::RequiredGetUnavailable),
        "{failure:?}"
    );
    assert!(probe.get.lock().unwrap().is_none());
    assert_no_projection(scratch.root());
}

#[test]
fn held_demand_snapshot_refuses_completion_without_preemption_or_waiting() {
    let scratch = scratch();
    let vault = fixture(scratch.root(), 2, true);
    let mut probe = Transcript::new(&vault);
    probe.hold = true;
    let probe = Arc::new(probe);
    let failure = vault
        .prefix_helper(&experiment(probe.clone(), "first"), &Default::default())
        .unwrap_err();
    assert!(
        matches!(failure.cause, HelperCause::OutstandingReaders(census) if census.live_connections == 1),
        "{failure:?}"
    );
    let reader = probe.retained_reader.lock().unwrap().take().unwrap();
    assert!(
        reader
            .acquire(
                norn_store::ReadNeed::Source,
                Instant::now() + Duration::from_secs(1)
            )
            .unwrap()
            .is_none()
    );
    let held = probe.held.lock().unwrap().take().unwrap();
    let model = probe.held_model.lock().unwrap().take().unwrap();
    let answer = held.find(
        &FindParams::new(probe.address.clone()).with_limit(25),
        &model,
    );
    assert!(
        answer.is_ok(),
        "established snapshot survives teardown: {answer:?}"
    );
    drop(held);
    drop(reader);
    assert_no_projection(scratch.root());
}

#[test]
fn actual_retired_path_residue_prevents_success_and_initial_marker_stays() {
    let scratch = scratch();
    let vault = fixture(scratch.root(), 2, true);
    let mut probe = Transcript::new(&vault);
    probe.sabotage = Some(scratch.root().to_owned());
    let probe = Arc::new(probe);
    let attempt = experiment(probe.clone(), "first");
    let failure = vault
        .prefix_helper(&attempt, &Default::default())
        .unwrap_err();
    assert!(matches!(failure.cause, HelperCause::Cleanup), "{failure:?}");
    assert!(!failure.cleanup.is_empty());
    assert!(
        probe
            .retired_path
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .is_dir()
    );
    assert!(matches!(
        vault
            .prefix_helper(&attempt, &Default::default())
            .unwrap_err()
            .cause,
        HelperCause::Ineligible
    ));
    let error = norn_store::Store::open(
        vault.database(),
        norn_store::StoredPathOrder::Sensitive,
        norn_host::DERIVATION_VERSION,
    )
    .err()
    .expect("helper namespace remains incomplete");
    assert!(error.to_string().contains("incomplete"), "{error}");
}

#[test]
#[cfg(target_os = "linux")]
#[ignore = "subprocess fixture invoked only by native lifecycle controls"]
fn native_helper_worker() {
    let root = PathBuf::from(std::env::var_os("NORN_PREFIX_HELPER_FIXTURE").unwrap());
    let vault = attach::Vault::adopt(&root).beside(&root.join("helper-machine"));
    let control = Arc::new(HelperControl::default());
    let mut probe = Transcript::new(&vault);
    probe.ack = Some(control.clone());
    let result = vault.prefix_helper(&experiment(Arc::new(probe), "first"), &control);
    let fds: Vec<_> = std::fs::read_dir("/proc/self/fd")
        .unwrap()
        .filter_map(|entry| {
            let path = std::fs::read_link(entry.ok()?.path()).ok()?;
            path.starts_with(&root).then_some(path)
        })
        .collect();
    let children: Vec<_> = std::fs::read_dir("/proc/self/task")
        .unwrap()
        .flat_map(|entry| {
            std::fs::read_to_string(entry.unwrap().path().join("children"))
                .unwrap()
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(fds.is_empty(), "owned descriptors survive: {fds:?}");
    assert!(
        children.is_empty(),
        "owned descendant survives: {children:?}"
    );
    match result {
        Ok(completed) => println!(
            "HELPER {}",
            json!({"event":"complete","retired_paths":completed.retired_paths(),"live":completed.budget().live_connections,"waiting":completed.budget().waiting_reads,"owned_fds":fds,"children":children,"global_ready":false})
        ),
        Err(failure) => {
            println!(
                "HELPER {}",
                json!({"event":"failed","cause":format!("{:?}",failure.cause),"cleanup":failure.cleanup})
            );
            panic!("helper failed");
        }
    }
}

#[cfg(target_os = "linux")]
struct Child {
    process: std::process::Child,
    input: Option<std::process::ChildStdin>,
    events: std::sync::mpsc::Receiver<Value>,
    reader: Option<std::thread::JoinHandle<Vec<String>>>,
}
#[cfg(target_os = "linux")]
impl Child {
    fn spawn(root: &Path) -> Self {
        use std::io::BufRead;
        let mut process = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "native_helper_worker",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("NORN_PREFIX_HELPER_FIXTURE", root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let input = process.stdin.take();
        let output = process.stdout.take().unwrap();
        let (send, events) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut lines = Vec::new();
            for line in std::io::BufReader::new(output).lines() {
                let line = line.unwrap();
                if let Some((_, value)) = line.split_once("HELPER ") {
                    let _ = send.send(serde_json::from_str(value).unwrap());
                }
                lines.push(line);
            }
            lines
        });
        Self {
            process,
            input,
            events,
            reader: Some(reader),
        }
    }
    fn next(&self) -> Value {
        self.events
            .recv_timeout(Duration::from_secs(30))
            .expect("native event before runaway bound")
    }
    fn ack(&mut self, value: &str) {
        use std::io::Write;
        writeln!(self.input.as_mut().unwrap(), "{value}").unwrap();
    }
    fn finish(mut self, success: bool) {
        self.input.take();
        let deadline = Instant::now() + Duration::from_secs(30);
        let status = loop {
            if let Some(status) = self.process.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "native child did not exit normally"
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        let lines = self.reader.take().unwrap().join().unwrap();
        assert_eq!(status.success(), success, "{status}: {lines:?}");
        assert!(
            matches!(
                self.events.try_recv(),
                Err(std::sync::mpsc::TryRecvError::Disconnected)
            ),
            "EOF with no extra terminal"
        );
        eprintln!("native exit={status}; stdout EOF=true; transcript={lines:?}");
    }
}
#[cfg(target_os = "linux")]
impl Drop for Child {
    fn drop(&mut self) {
        // A runaway invalidates the test; only the failure guard may force cleanup.
        if self.process.try_wait().ok().flatten().is_none() {
            let _ = self.process.kill();
            let _ = self.process.wait();
        }
        self.input.take();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

#[test]
#[cfg(target_os = "linux")]
fn native_exit_then_fresh_ordinary_attach_preserves_whole_answers_and_source_identity() {
    let scratch = scratch();
    let vault = fixture(scratch.root(), 314, true);
    let before = source(vault.path());
    let mut child = Child::spawn(scratch.root());
    let get = child.next();
    assert_eq!(get["sequence"], 1);
    assert!(child.process.try_wait().unwrap().is_none());
    child.ack("ack-1");
    let find = child.next();
    assert_eq!(find["sequence"], 2);
    assert!(child.process.try_wait().unwrap().is_none());
    child.ack("ack-2");
    let terminal = child.next();
    assert_eq!(terminal["event"], "complete");
    assert_eq!(terminal["live"], 0);
    assert_eq!(terminal["waiting"], 0);
    assert_eq!(terminal["global_ready"], false);
    child.finish(true);
    assert_no_projection(scratch.root());
    let owner = vault.beside(&scratch.join("owner-machine"));
    assert!(!owner.database().exists());
    let host = owner.host();
    let demand = attach::attach_and_wait(&host, owner.name());
    let address = VaultAddress::name(owner.name().clone());
    let ordinary_get = host.get(&get_query(&address)).unwrap().answer;
    let mut early_get = get["receipt"]["report"].clone();
    let mut canonical_get = serde_json::to_value(ordinary_get.report).unwrap();
    assert_eq!(early_get["shape"], "record");
    assert_eq!(early_get["document"]["links"]["total"], 2);
    assert_eq!(early_get["document"]["findings"]["total"], 1);
    for finding in early_get["document"]["findings"]["items"]
        .as_array()
        .unwrap()
    {
        assert!(finding["id"].as_u64().unwrap() > 0);
        assert!(
            finding["generation"].as_u64().unwrap()
                <= get["receipt"]["snapshot"]["generation"].as_u64().unwrap()
        );
    }
    // Finding identifiers and publication generations belong to their own store.
    // Preserve raw receipts and compare every other rich field to ordinary Get.
    for report in [&mut early_get, &mut canonical_get] {
        for finding in report["document"]["findings"]["items"]
            .as_array_mut()
            .unwrap()
        {
            finding.as_object_mut().unwrap().remove("id");
            finding.as_object_mut().unwrap().remove("generation");
        }
    }
    assert_eq!(early_get, canonical_get);
    assert_eq!(
        get["receipt"]["unsatisfied"],
        json!(ordinary_get.unsatisfied)
    );
    let ordinary_find = host.find(&query(&address)).unwrap().answer;
    let mut early_find = find["receipt"]["answer"]["report"].clone();
    let mut canonical_find = serde_json::to_value(ordinary_find.report).unwrap();
    assert_ne!(
        get["receipt"]["snapshot"]["epoch"],
        find["receipt"]["answer"]["snapshot"]["epoch"]
    );
    let early_cursor: norn_wire::Cursor =
        serde_json::from_value(early_find["page"]["next"].clone()).unwrap();
    let canonical_cursor: norn_wire::Cursor =
        serde_json::from_value(canonical_find["page"]["next"].clone()).unwrap();
    assert_eq!(early_cursor.key(), canonical_cursor.key());
    assert_ne!(
        early_cursor.snapshot().epoch,
        canonical_cursor.snapshot().epoch
    );
    assert_eq!(
        early_cursor.snapshot().schema_fingerprint,
        canonical_cursor.snapshot().schema_fingerprint
    );
    // Provenance is asserted separately; only its independent epochs/generations differ.
    early_find["page"]["next"] = Value::Null;
    canonical_find["page"]["next"] = Value::Null;
    assert_eq!(early_find, canonical_find);
    drop(demand);
    drop(host);
    assert_eq!(source(vault.path()), before);
}

#[test]
#[cfg(target_os = "linux")]
fn cancellation_or_disconnect_at_either_ack_never_completes_successfully() {
    for sequence in [1, 2] {
        for cancel in [true, false] {
            let scratch = scratch();
            let _vault = fixture(scratch.root(), 2, true);
            let mut child = Child::spawn(scratch.root());
            assert_eq!(child.next()["sequence"], 1);
            if sequence == 2 {
                child.ack("ack-1");
                assert_eq!(child.next()["sequence"], 2);
            }
            assert!(child.process.try_wait().unwrap().is_none());
            if cancel {
                child.ack("cancel");
            } else {
                child.input.take();
            }
            let failed = child.next();
            assert_eq!(failed["event"], "failed");
            assert!(failed["cause"].as_str().unwrap().contains(if cancel {
                "Cancelled"
            } else {
                "disconnected"
            }));
            child.finish(false);
            assert_no_projection(scratch.root());
        }
    }
}

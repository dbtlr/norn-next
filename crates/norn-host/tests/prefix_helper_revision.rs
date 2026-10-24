//! Reviewer-owned adversarial probes over a COPY of the frozen helper source.
#![cfg(feature = "induced-failure")]
#![allow(clippy::disallowed_methods)]
mod attach;
use norn_host::startup_experiment::{
    Experiment, FrozenInput, HelperCause, HelperControl, Observer, PrefixReader, Stage,
};
use norn_store::{ContentModel, SnapshotReader, startup_experiment::Catalogue};
use norn_wire::{FindParams, Predicate, VaultAddress};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

fn scratch() -> norn_testkit::scratch::Scratch {
    norn_testkit::scratch::Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "review-probe")
        .unwrap()
}
fn fixture(root: &Path, documents: usize, schema: &str) -> attach::Vault {
    std::fs::create_dir_all(root.join("vault/.norn")).unwrap();
    std::fs::write(root.join("vault/.norn/schema.yaml"), schema).unwrap();
    std::fs::write(
        root.join("vault/first.md"),
        "---\nperf_bucket: active\n---\n# First\n[[later]]\n",
    )
    .unwrap();
    std::fs::write(root.join("vault/later.md"), "# Later\n").unwrap();
    for i in 2..documents {
        std::fs::write(
            root.join(format!("vault/a-{i:03}.md")),
            "---\nperf_bucket: active\n---\nBody\n",
        )
        .unwrap();
    }
    attach::Vault::adopt(root)
}
const SCHEMA: &str = "version: 1\nfields:\n  perf_bucket: { type: text }\n";
fn files(root: &Path) -> Vec<PathBuf> {
    let mut result = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                result.extend(files(&entry.path()));
            }
            result.push(entry.path());
        }
    }
    result
}

#[derive(Default)]
struct Probe {
    address: Option<VaultAddress>,
    /// Mutate this consumed name during its load/rehash window.
    mutate: Option<(PathBuf, String)>,
    /// Write through this alternate hard link instead of the vault path.
    via_link: Option<PathBuf>,
    fail_prefix_load: bool,
    fail_callback: bool,
    cancel_before_failure: Option<Arc<HelperControl>>,
    cancel_in_prefix: Option<Arc<HelperControl>>,
    hold_reader: Mutex<Option<Arc<SnapshotReader>>>,
    hold: bool,
    scan_root: Option<PathBuf>,
    builder_path: Mutex<Option<PathBuf>>,
    recreate_builder_as_file: bool,
    decline: Mutex<Option<String>>,
}
impl Observer for Probe {
    fn catalogue(&self, _: &Catalogue, _: &ContentModel) -> Result<(), String> {
        Ok(())
    }
    fn canonical_work(
        &self,
        _: &std::collections::BTreeMap<&'static str, u64>,
    ) -> Result<(), String> {
        panic!("helper entered canonical heal")
    }
    fn indexed(
        &self,
        stage: Stage,
        reader: Arc<SnapshotReader>,
        _: Arc<ContentModel>,
    ) -> Result<(), String> {
        assert_eq!(stage, Stage::CanonicalDemand);
        if self.hold {
            *self.hold_reader.lock().unwrap() = Some(reader);
        }
        Ok(())
    }
    fn prefix_loaded(&self, path: &str) -> Result<(), String> {
        if let Some(root) = &self.scan_root
            && self.builder_path.lock().unwrap().is_none()
        {
            *self.builder_path.lock().unwrap() = files(root).into_iter().find(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("intake-prefix-")
                    && p.extension().is_some_and(|e| e == "sqlite3")
            });
        }
        if let Some(control) = &self.cancel_before_failure { control.cancel(); }
        if self.fail_prefix_load {
            return Err("probe: injected prefix load failure".into());
        }
        if let Some((target, bytes)) = &self.mutate
            && target.file_name().unwrap().to_string_lossy() == path
        {
            let write_to = self.via_link.as_ref().unwrap_or(target);
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(write_to)
                .unwrap();
            file.write_all(bytes.as_bytes()).unwrap();
            file.sync_all().unwrap();
        }
        Ok(())
    }
    fn prefix(&self, reader: &PrefixReader) -> Result<(), String> {
        let attempt = reader.find(
            &FindParams::new(self.address.clone().unwrap())
                .with_limit(25)
                .with_predicates([Predicate::equal_to("perf_bucket", "active")]),
        );
        *self.decline.lock().unwrap() = attempt.decline.clone();
        if self.recreate_builder_as_file
            && let Some(path) = self.builder_path.lock().unwrap().as_ref()
        {
            assert!(!path.exists(), "builder dropped before the Find callback");
            std::fs::write(path, b"probe residue").unwrap();
        }
        if let Some(control) = &self.cancel_in_prefix {
            control.cancel();
        }
        if self.fail_callback { return Err("probe: callback failure".into()); }
        Ok(())
    }
}
fn run(vault: &attach::Vault, probe: Arc<Probe>, control: &HelperControl) -> Result<norn_host::startup_experiment::HelperComplete, norn_host::startup_experiment::HelperFailure> {
    let experiment = Experiment::capped_prefix_frozen("first", probe, FrozenInput::assume_immutable());
    vault.prefix_helper(&experiment, control)
}

#[test]
fn observed_rehash_movement_fails_even_without_a_tree_watch_event() {
    for outside in [true, false] {
        let scratch = scratch(); let vault = fixture(scratch.root(), 10, SCHEMA);
        let target = vault.path().join("a-005.md");
        let link = scratch.root().join("outside-link.md");
        std::fs::hard_link(&target, &link).unwrap();
        let probe = Arc::new(Probe {
            address: Some(VaultAddress::name(vault.name().clone())),
            mutate: Some((target.clone(), "---\nperf_bucket: inactive\n---\nChanged\n".into())),
            via_link: outside.then_some(link),
            ..Default::default()
        });
        let result = run(&vault, probe.clone(), &Default::default());
        eprintln!("rehash outside={outside}: {result:?}; decline={:?}", probe.decline.lock().unwrap());
        assert!(matches!(result, Err(norn_host::startup_experiment::HelperFailure {cause: HelperCause::SourceChanged, ..})), "actual rehash movement must fail independently of watcher delivery");
    }
}

#[test]
fn preseal_residue_is_preserved_beside_source_or_operation_failure() {
    for source_change in [true, false] {
        let scratch = scratch(); let vault = fixture(scratch.root(), 10, SCHEMA);
        let target = vault.path().join("a-005.md");
        let link = scratch.root().join("outside-link.md");
        std::fs::hard_link(&target, &link).unwrap();
        let probe = Arc::new(Probe {
            address: Some(VaultAddress::name(vault.name().clone())),
            mutate: source_change.then_some((target, "---\nperf_bucket: inactive\n---\nChanged\n".into())),
            via_link: Some(link), fail_prefix_load: !source_change,
            scan_root: Some(scratch.root().to_owned()), recreate_builder_as_file: true,
            ..Default::default()
        });
        let failure = run(&vault, probe.clone(), &Default::default()).unwrap_err();
        eprintln!("preseal source={source_change}: {failure:?}");
        assert!(if source_change { matches!(failure.cause, HelperCause::SourceChanged) } else { matches!(failure.cause, HelperCause::Operation(_)) });
        let path = probe.builder_path.lock().unwrap().clone().expect("actual builder path observed");
        assert_eq!(std::fs::read(&path).unwrap(), b"probe residue");
        assert!(!failure.cleanup.is_empty(), "pre-seal intake residue must survive beside primary failure");
    }
}

#[test]
fn callback_error_does_not_discard_preseal_retirement_evidence() {
    let scratch = scratch(); let vault = fixture(scratch.root(), 4, SCHEMA);
    let probe = Arc::new(Probe {
        address: Some(VaultAddress::name(vault.name().clone())),
        fail_prefix_load: true, fail_callback: true,
        scan_root: Some(scratch.root().to_owned()), recreate_builder_as_file: true,
        ..Default::default()
    });
    let failure = run(&vault, probe.clone(), &Default::default()).unwrap_err();
    eprintln!("callback + preseal: {failure:?}");
    assert!(matches!(failure.cause, HelperCause::Operation(_)));
    assert!(!failure.cleanup.is_empty());
    assert!(probe.builder_path.lock().unwrap().as_ref().unwrap().exists());
}

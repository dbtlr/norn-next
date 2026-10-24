//! Real inventory replay and SQLite failures must close both early query gates.
#![cfg(feature = "induced-failure")]
#![allow(clippy::disallowed_methods)]
mod attach;
use norn_host::startup_experiment::{
    Experiment, FrozenInput, Observer, PrefixAttempt, PrefixReader, Stage,
};
use norn_store::{ContentModel, DocumentPath, SnapshotReader, startup_experiment::Catalogue};
use norn_wire::{FindParams, Predicate, TrustState, VaultAddress};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct ReplayFailure {
    root: PathBuf,
    address: VaultAddress,
    duplicate: bool,
    fired: AtomicBool,
    gets: AtomicUsize,
    attempt: Mutex<Option<PrefixAttempt>>,
}
impl Observer for ReplayFailure {
    fn catalogue_replay(
        &self,
        path: &Path,
        catalogue: &mut Catalogue,
        model: &ContentModel,
    ) -> Result<(), String> {
        let selected = if self.duplicate {
            "small.md"
        } else {
            "gone.md"
        };
        if path == Path::new(selected) && !self.fired.swap(true, Ordering::SeqCst) {
            if self.duplicate {
                // Seed a real row; the entry runner's next record meets SQLite's
                // UNIQUE constraint. The callback itself succeeds.
                catalogue
                    .record_document(
                        &DocumentPath::new(selected).unwrap(),
                        model.ambiguity_ignore(),
                    )
                    .map_err(|error| error.to_string())?;
            } else {
                std::fs::remove_file(self.root.join(path)).unwrap();
            }
        }
        Ok(())
    }
    fn catalogue(&self, _: &Catalogue, _: &ContentModel) -> Result<(), String> {
        Ok(())
    }
    fn indexed(
        &self,
        stage: Stage,
        _: Arc<SnapshotReader>,
        _: Arc<ContentModel>,
    ) -> Result<(), String> {
        if stage == Stage::CanonicalDemand {
            self.gets.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }
    fn prefix(&self, reader: &PrefixReader) -> Result<(), String> {
        *self.attempt.lock().unwrap() = Some(reader.find(&query(&self.address)));
        Ok(())
    }
}
fn query(address: &VaultAddress) -> FindParams {
    FindParams::new(address.clone())
        .with_limit(25)
        .with_predicates([Predicate::equal_to("perf_bucket", "active")])
}
fn run(duplicate: bool, capped: bool) {
    let scratch = norn_testkit::scratch::Scratch::new("capped-catalogue-failure");
    let root = scratch.join("vault");
    std::fs::create_dir_all(root.join(".norn")).unwrap();
    std::fs::write(root.join(".norn/schema.yaml"), "version: 1\n").unwrap();
    std::fs::write(root.join("small.md"), "Small\n").unwrap();
    std::fs::write(root.join("gone.md"), "Gone\n").unwrap();
    for i in 0..150 {
        std::fs::write(
            root.join(format!("a-{i:03}.md")),
            "---\nperf_bucket: active\n---\nBody\n",
        )
        .unwrap();
    }
    let vault = attach::Vault::adopt(scratch.root());
    let address = VaultAddress::name(vault.name().clone());
    let observer = Arc::new(ReplayFailure {
        root,
        address: address.clone(),
        duplicate,
        fired: AtomicBool::new(false),
        gets: AtomicUsize::new(0),
        attempt: Mutex::new(None),
    });
    let experiment = if capped {
        Experiment::capped_prefix_frozen("small", observer.clone(), FrozenInput::assume_immutable())
    } else {
        Experiment::canonical_demand("small", observer.clone())
    };
    let host = vault.host_with_startup_experiment(
        norn_host::ProductionPolicy::new(32, 32).unwrap(),
        experiment,
    );
    let demand = host.demand(vault.name()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let outcome = loop {
        match host.state(vault.name()) {
            Ok(TrustState::Ready) => break Ok(()),
            Err(error) => break Err(format!("{error:?}")),
            _ => (),
        }
        assert!(Instant::now() < deadline, "entry runner did not finish");
        std::thread::sleep(Duration::from_millis(1));
    };
    eprintln!(
        "duplicate={duplicate} capped={capped} fired={} gets={} outcome={outcome:?}",
        observer.fired.load(Ordering::SeqCst),
        observer.gets.load(Ordering::SeqCst)
    );
    assert!(observer.fired.load(Ordering::SeqCst));
    if duplicate {
        assert!(outcome.unwrap_err().contains("constraint failed"));
        assert_eq!(observer.gets.load(Ordering::SeqCst), 0);
        assert!(observer.attempt.lock().unwrap().is_none());
    } else {
        outcome.unwrap();
        assert_eq!(
            observer.gets.load(Ordering::SeqCst),
            0,
            "known nonstanding replay emitted Get"
        );
        let attempt = observer.attempt.lock().unwrap().take().unwrap();
        assert!(attempt.answer.is_none());
        assert!(!attempt.query_executed);
        assert!(
            attempt
                .certificate
                .invalid
                .unwrap()
                .contains("unresolved catalogue observation")
        );
        assert_eq!(
            host.find(&query(&address))
                .unwrap()
                .answer
                .report
                .page
                .rows
                .len(),
            25
        );
    }
    drop(demand);
    drop(host);
}
#[test]
fn real_sqlite_record_error_refuses_every_mode_before_get() {
    for capped in [false, true] {
        run(true, capped);
    }
}
#[test]
fn vanished_inventory_replay_closes_get_and_find_then_reaches_ready() {
    run(false, true);
}

//! Private live metadata executes only while the original observation stands.
#![cfg(feature = "induced-failure")]
#![allow(clippy::disallowed_methods)]
mod attach;

use norn_host::startup_experiment::{
    Experiment, LiveFindReader, LiveFindRefusal, LiveGetReader, Observer, Stage,
};
use norn_store::{ContentModel, SnapshotReader, startup_experiment::Catalogue};
use norn_wire::{Column, FindParams, Predicate, VaultAddress, VaultName};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn query() -> FindParams {
    FindParams::new(VaultAddress::name(VaultName::new("notes").unwrap()))
        .with_columns([Column::path(), Column::fields()])
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Change {
    Construction,
    Delayed,
    Schema,
}
struct Movement {
    root: PathBuf,
    point: Change,
    observation: Mutex<Option<norn_fs::watch::Observation>>,
    changed: AtomicBool,
    offered: AtomicBool,
    declined: AtomicBool,
}
impl Movement {
    fn change(&self) {
        if self.changed.swap(true, Ordering::SeqCst) {
            return;
        }
        match self.point {
            Change::Construction | Change::Delayed => {
                std::fs::write(self.root.join("first.md"), "---\nrank: 99\n---\nChanged\n").unwrap()
            }
            Change::Schema => std::fs::write(
                self.root.join(".norn/schema.yaml"),
                "version: 1\nfields:\n  rank: { type: text }\n",
            )
            .unwrap(),
        }
        let observation = self.observation.lock().unwrap().clone().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while observation.is_current() {
            assert!(Instant::now() < deadline, "watcher did not accept the edit");
            std::thread::yield_now();
        }
    }
}
impl Observer for Movement {
    fn live_observation(&self, observation: &norn_fs::watch::Observation) -> Result<(), String> {
        *self.observation.lock().unwrap() = Some(observation.clone());
        Ok(())
    }
    fn catalogue(&self, _: &Catalogue, _: &ContentModel) -> Result<(), String> {
        Ok(())
    }
    fn metadata_loaded(&self, path: &Path) -> Result<(), String> {
        if self.point == Change::Construction && path == Path::new("first.md") {
            self.change();
        }
        Ok(())
    }
    fn live_find(&self, reader: Arc<LiveFindReader>) -> Result<(), String> {
        self.offered.store(true, Ordering::SeqCst);
        if self.point != Change::Construction {
            assert!(
                reader
                    .find(&query(), Instant::now() + Duration::from_secs(5))
                    .is_ok()
            );
            self.change();
            assert!(matches!(
                reader.find(&query(), Instant::now() + Duration::from_secs(5)),
                Err(LiveFindRefusal::Unavailable)
            ));
        }
        Ok(())
    }
    fn declined(&self, _: &str) -> Result<(), String> {
        self.declined.store(true, Ordering::SeqCst);
        Ok(())
    }
    fn indexed(
        &self,
        stage: Stage,
        _: Arc<SnapshotReader>,
        _: Arc<ContentModel>,
    ) -> Result<(), String> {
        assert_eq!(stage, Stage::CanonicalHealth);
        Ok(())
    }
}

fn fixture(label: &str) -> (norn_testkit::scratch::Scratch, PathBuf) {
    let scratch =
        norn_testkit::scratch::Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), label)
            .unwrap();
    let root = scratch.join("vault");
    std::fs::create_dir_all(root.join(".norn")).unwrap();
    std::fs::write(
        root.join(".norn/schema.yaml"),
        "version: 1\nfields:\n  rank: { type: number }\n",
    )
    .unwrap();
    std::fs::write(root.join("first.md"), "---\nrank: 1\n---\nBody\n").unwrap();
    (scratch, root)
}

#[test]
fn observed_source_change_during_construction_or_delayed_execution_declines() {
    for point in [Change::Construction, Change::Delayed, Change::Schema] {
        let (scratch, root) = fixture("live-metadata-movement");
        let vault = attach::Vault::adopt(scratch.root());
        let probe = Arc::new(Movement {
            root,
            point,
            observation: Mutex::new(None),
            changed: AtomicBool::new(false),
            offered: AtomicBool::new(false),
            declined: AtomicBool::new(false),
        });
        let host = vault.host_with_startup_experiment(
            norn_host::ProductionPolicy::new(2, 2).unwrap(),
            Experiment::live_metadata("first", probe.clone()),
        );
        let demand = host.demand(vault.name()).unwrap();
        attach::wait_for_ready(&host, vault.name(), Duration::from_secs(10));
        assert!(probe.changed.load(Ordering::SeqCst), "{point:?}");
        assert_eq!(
            probe.offered.load(Ordering::SeqCst),
            point != Change::Construction
        );
        if point == Change::Construction {
            assert!(probe.declined.load(Ordering::SeqCst));
        }
        if point != Change::Schema {
            let got = host
                .find(&query().with_predicates([Predicate::equal_to("rank", "99")]))
                .unwrap();
            assert_eq!(
                got.answer.report.page.rows.len(),
                1,
                "ordinary heal must reflect changed fields"
            );
        }
        drop(demand);
        drop(host);
        assert_no_projections(scratch.root());
        vault.store().verify_integrity().unwrap();
    }
}

// Resource ownership is observable on disk after all capabilities have retired.
fn assert_no_projections(root: &Path) {
    for entry in std::fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            assert_no_projections(&entry.path());
        } else {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            assert!(
                !name.starts_with("intake-") && !name.starts_with("live-demand-"),
                "leaked {}",
                entry.path().display()
            );
        }
    }
}

#[test]
fn legitimate_get_nonclaims_leave_complete_metadata_available_and_rich_shapes_decline() {
    struct Nonclaim {
        offered: AtomicBool,
    }
    impl Observer for Nonclaim {
        fn catalogue(&self, _: &Catalogue, _: &ContentModel) -> Result<(), String> {
            Ok(())
        }
        fn indexed(
            &self,
            _: Stage,
            _: Arc<SnapshotReader>,
            _: Arc<ContentModel>,
        ) -> Result<(), String> {
            Ok(())
        }
        fn live_get(&self, _: Arc<LiveGetReader>) -> Result<(), String> {
            panic!("unsupported or over-bound Get must decline")
        }
        fn live_find(&self, reader: Arc<LiveFindReader>) -> Result<(), String> {
            self.offered.store(true, Ordering::SeqCst);
            let deadline = || Instant::now() + Duration::from_secs(5);
            for predicate in [
                Predicate::matches("Body"),
                Predicate::tag("tag"),
                Predicate::links_to(norn_wire::ResolutionTarget::new("first").unwrap()),
                Predicate::resolves(norn_wire::ResolutionTarget::new("first").unwrap()),
                Predicate::has_finding(norn_wire::FindingKind::Broken),
            ] {
                assert!(matches!(
                    reader.find(&query().with_predicates([predicate]), deadline()),
                    Err(LiveFindRefusal::Unsupported)
                ));
            }
            for column in [
                Column::body(),
                Column::tags(),
                Column::links(),
                Column::headings(),
                Column::blocks(),
                Column::findings(),
            ] {
                assert!(matches!(
                    reader.find(&query().with_columns([column]), deadline()),
                    Err(LiveFindRefusal::Unsupported)
                ));
            }
            assert!(matches!(
                reader.find(&query(), Instant::now()),
                Err(LiveFindRefusal::Unavailable)
            ));
            assert!(!reader.find(&query(), deadline()).unwrap().rows.is_empty());
            Ok(())
        }
    }
    for target in ["first#Heading", "crowded"] {
        let (scratch, root) = fixture("live-metadata-nonclaim");
        if target == "crowded" {
            for i in 0..257 {
                let directory = root.join(format!("group-{i:03}"));
                std::fs::create_dir(&directory).unwrap();
                std::fs::write(directory.join("crowded.md"), "Crowded\n").unwrap();
            }
        }
        let vault = attach::Vault::adopt(scratch.root());
        let probe = Arc::new(Nonclaim {
            offered: AtomicBool::new(false),
        });
        let host = vault.host_with_startup_experiment(
            norn_host::ProductionPolicy::new(32, 32).unwrap(),
            Experiment::live_metadata(target, probe.clone()),
        );
        let demand = host.demand(vault.name()).unwrap();
        attach::wait_for_ready(&host, vault.name(), Duration::from_secs(10));
        assert!(probe.offered.load(Ordering::SeqCst));
        drop(demand);
        drop(host);
        assert_no_projections(scratch.root());
    }
}

#[test]
fn query_and_observer_failures_keep_their_causes_and_retire_metadata() {
    struct Failure {
        query_failure: bool,
        observed: Mutex<Option<norn_store::StoreError>>,
        retained: Mutex<Option<Arc<LiveFindReader>>>,
    }
    impl Observer for Failure {
        fn catalogue(&self, _: &Catalogue, _: &ContentModel) -> Result<(), String> {
            Ok(())
        }
        fn indexed(
            &self,
            _: Stage,
            _: Arc<SnapshotReader>,
            _: Arc<ContentModel>,
        ) -> Result<(), String> {
            Ok(())
        }
        fn live_find(&self, reader: Arc<LiveFindReader>) -> Result<(), String> {
            *self.retained.lock().unwrap() = Some(reader.clone());
            if self.query_failure {
                norn_store::induced_failure::fail_next_meta_read_as_busy();
                let result = reader.find(&query(), Instant::now() + Duration::from_secs(5));
                norn_store::induced_failure::clear_the_meta_read_arm();
                match result {
                    Err(
                        LiveFindRefusal::Store(error)
                        | LiveFindRefusal::Page(norn_store::PageRefusal::Store(error)),
                    ) => *self.observed.lock().unwrap() = Some(error),
                    other => return Err(format!("expected typed storage failure, got {other:?}")),
                }
                // Swallowing the read error must not erase the attach's failure.
                Ok(())
            } else {
                reader
                    .find(&query(), Instant::now() + Duration::from_secs(5))
                    .unwrap();
                Err("metadata observer stopped".into())
            }
        }
    }
    for query_failure in [true, false] {
        let (scratch, _) = fixture("live-metadata-failure");
        let vault = attach::Vault::adopt(scratch.root());
        let probe = Arc::new(Failure {
            query_failure,
            observed: Mutex::new(None),
            retained: Mutex::new(None),
        });
        let host = vault.host_with_startup_experiment(
            norn_host::ProductionPolicy::new(2, 2).unwrap(),
            Experiment::live_metadata("first", probe.clone()),
        );
        let demand = host.demand(vault.name()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let error = loop {
            match host.state(vault.name()) {
                Err(error) => break format!("{error:?}"),
                Ok(norn_wire::TrustState::Ready) => {
                    panic!("operation failure was swallowed as loading")
                }
                _ => (),
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        };
        if query_failure {
            let observed = probe
                .observed
                .lock()
                .unwrap()
                .clone()
                .expect("typed query error");
            let norn_store::StoreError::Sql {
                operation,
                condition: Some(_),
                ..
            } = observed
            else {
                panic!("{observed:?}");
            };
            assert!(error.contains(operation), "{error}");
        } else {
            assert!(error.contains("metadata observer stopped"), "{error}");
        }
        let retained = probe.retained.lock().unwrap().take().unwrap();
        assert!(matches!(
            retained.find(&query(), Instant::now() + Duration::from_secs(5)),
            Err(LiveFindRefusal::Unavailable)
        ));
        drop(retained);
        drop(demand);
        drop(host);
        assert_no_projections(scratch.root());
        let host = vault.host();
        let _demand = attach::attach_and_wait(&host, vault.name());
        assert_eq!(
            host.find(&query()).unwrap().answer.report.page.rows.len(),
            1
        );
    }
}

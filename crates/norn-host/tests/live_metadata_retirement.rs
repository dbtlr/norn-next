//! Independent bounded probe for an actual sealed-metadata unlink failure.
#![cfg(all(feature = "induced-failure", target_os = "linux"))]
#![allow(clippy::disallowed_methods)]
mod attach;
use norn_host::startup_experiment::{Experiment, LiveFindReader, LiveFindRefusal, Observer, Stage};
use norn_store::{ContentModel, SnapshotReader, startup_experiment::Catalogue};
use norn_wire::{FindParams, VaultAddress, VaultName};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn query() -> FindParams {
    FindParams::new(VaultAddress::name(VaultName::new("notes").unwrap()))
}
struct Probe {
    fixture: PathBuf,
    file: Mutex<Option<PathBuf>>,
    retained: Mutex<Option<Arc<LiveFindReader>>>,
}
impl Observer for Probe {
    fn catalogue(&self, _: &Catalogue, _: &ContentModel) -> Result<(), String> {
        Ok(())
    }
    fn live_find(&self, reader: Arc<LiveFindReader>) -> Result<(), String> {
        let answer = reader
            .find(&query(), Instant::now() + Duration::from_secs(5))
            .unwrap();
        assert_eq!(answer.rows.len(), 1);
        // Identify the actual SQLite connection by its open descriptor, then
        // match its inode to the named file. Filename absence is not our oracle.
        let (fd, file) = std::fs::read_dir("/proc/self/fd")
            .unwrap()
            .filter_map(|e| {
                let fd = e.unwrap().path();
                let path = std::fs::read_link(&fd).ok()?;
                (path.starts_with(&self.fixture)
                    && path
                        .components()
                        .any(|c| c.as_os_str() == "canonical-intake")
                    && path.extension().is_some_and(|e| e == "sqlite3"))
                .then_some((fd, path))
            })
            .next()
            .expect("live metadata connection is open");
        let actual = std::fs::metadata(&file).unwrap();
        let open = std::fs::metadata(&fd).unwrap();
        assert_eq!((actual.dev(), actual.ino()), (open.dev(), open.ino()));
        assert_eq!(&std::fs::read(&file).unwrap()[..16], b"SQLite format 3\0");
        let parent = file.parent().unwrap();
        let sentinel = parent.join("review-unlink-sentinel");
        std::fs::write(&sentinel, b"proof").unwrap();
        *self.file.lock().unwrap() = Some(file.clone());
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o500)).unwrap();
        let denied = std::fs::remove_file(&sentinel).unwrap_err();
        assert_eq!(denied.kind(), std::io::ErrorKind::PermissionDenied);
        eprintln!(
            "PROBE actual file={} inode={} dev={} unlink_errno={:?}",
            file.display(),
            actual.ino(),
            actual.dev(),
            denied.raw_os_error()
        );
        *self.retained.lock().unwrap() = Some(reader);
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
impl Drop for Probe {
    fn drop(&mut self) {
        if let Some(file) = self.file.get_mut().unwrap().as_ref() {
            let _ = std::fs::set_permissions(
                file.parent().unwrap(),
                std::fs::Permissions::from_mode(0o700),
            );
        }
    }
}
#[test]
fn actual_sealed_metadata_cleanup_failure_must_not_publish_ready() {
    let scratch = norn_testkit::scratch::Scratch::under(
        std::path::Path::new(env!("CARGO_TARGET_TMPDIR")),
        "review-live-cleanup",
    )
    .unwrap();
    let root = scratch.join("vault");
    std::fs::create_dir_all(root.join(".norn")).unwrap();
    std::fs::write(root.join(".norn/schema.yaml"), "version: 1\n").unwrap();
    std::fs::write(root.join("first.md"), "---\nrank: 1\n---\nBody\n").unwrap();
    let vault = attach::Vault::adopt(scratch.root());
    let probe = Arc::new(Probe {
        fixture: scratch.root().to_path_buf(),
        file: Mutex::new(None),
        retained: Mutex::new(None),
    });
    let host = vault.host_with_startup_experiment(
        norn_host::ProductionPolicy::new(2, 2).unwrap(),
        Experiment::live_metadata("first", probe.clone()),
    );
    let demand = host.demand(vault.name()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let outcome = loop {
        match host.state(vault.name()) {
            Ok(norn_wire::TrustState::Ready) => break "Ready".to_string(),
            Err(e) => break format!("{e:?}"),
            _ => assert!(Instant::now() < deadline, "attach did not finish"),
        }
        std::thread::yield_now();
    };
    let file = probe
        .file
        .lock()
        .unwrap()
        .clone()
        .expect("metadata offered");
    let residue = file.exists();
    let identity = std::fs::metadata(&file).unwrap();
    let open_owners = std::fs::read_dir("/proc/self/fd")
        .unwrap()
        .filter_map(|e| std::fs::metadata(e.unwrap().path()).ok())
        .filter(|m| (m.dev(), m.ino()) == (identity.dev(), identity.ino()))
        .count();
    assert_eq!(
        open_owners, 0,
        "no active source descriptor explains the residual file"
    );
    let retained = probe.retained.lock().unwrap().take().unwrap();
    assert!(matches!(
        retained.find(&query(), Instant::now() + Duration::from_secs(5)),
        Err(LiveFindRefusal::Unavailable)
    ));
    drop(retained);
    drop(demand);
    drop(host);
    std::fs::set_permissions(
        file.parent().unwrap(),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    eprintln!(
        "PROBE outcome={outcome}; actual_file_remains={residue}; retained_reader=Unavailable; permissions restored; fixture removed by Scratch"
    );
    assert_ne!(
        outcome, "Ready",
        "an actual metadata unlink failure was silently accepted; actual file remains={residue}"
    );
}

#[test]
fn cleanup_failure_keeps_the_observer_cause_before_and_after_sealing() {
    struct Failure {
        probe: Probe,
        during_build: bool,
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
            panic!("failed metadata must not reach canonical construction")
        }
        fn metadata_loaded(&self, _: &std::path::Path) -> Result<(), String> {
            if !self.during_build {
                return Ok(());
            }
            // The writer is still open here. Identify its real file, not a
            // fabricated filename, before making only its parent unwritable.
            let (fd, file) = std::fs::read_dir("/proc/self/fd")
                .unwrap()
                .filter_map(|entry| {
                    let fd = entry.unwrap().path();
                    let file = std::fs::read_link(&fd).ok()?;
                    (file.starts_with(&self.probe.fixture)
                        && file
                            .components()
                            .any(|c| c.as_os_str() == "canonical-intake")
                        && file.extension().is_some_and(|e| e == "sqlite3"))
                    .then_some((fd, file))
                })
                .next()
                .expect("actual intake writer");
            let named = std::fs::metadata(&file).unwrap();
            let opened = std::fs::metadata(fd).unwrap();
            assert_eq!((named.dev(), named.ino()), (opened.dev(), opened.ino()));
            assert_eq!(&std::fs::read(&file).unwrap()[..16], b"SQLite format 3\0");
            let parent = file.parent().unwrap();
            let sentinel = parent.join("observer-unlink-sentinel");
            std::fs::write(&sentinel, b"proof").unwrap();
            *self.probe.file.lock().unwrap() = Some(file.clone());
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o500)).unwrap();
            let error = std::fs::remove_file(sentinel).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
            assert_eq!(error.raw_os_error(), Some(13));
            Err("metadata observer stopped before sealing".into())
        }
        fn live_find(&self, reader: Arc<LiveFindReader>) -> Result<(), String> {
            assert!(!self.during_build);
            self.probe.live_find(reader)?;
            Err("metadata observer stopped after sealing".into())
        }
    }
    for during_build in [true, false] {
        let scratch = norn_testkit::scratch::Scratch::under(
            std::path::Path::new(env!("CARGO_TARGET_TMPDIR")),
            "live-cleanup-causes",
        )
        .unwrap();
        let root = scratch.join("vault");
        std::fs::create_dir_all(root.join(".norn")).unwrap();
        std::fs::write(root.join(".norn/schema.yaml"), "version: 1\n").unwrap();
        std::fs::write(root.join("first.md"), "---\nrank: 1\n---\nBody\n").unwrap();
        let vault = attach::Vault::adopt(scratch.root());
        let observer = Arc::new(Failure {
            probe: Probe {
                fixture: scratch.root().to_path_buf(),
                file: Mutex::new(None),
                retained: Mutex::new(None),
            },
            during_build,
        });
        let host = vault.host_with_startup_experiment(
            norn_host::ProductionPolicy::new(2, 2).unwrap(),
            Experiment::live_metadata("first", observer.clone()),
        );
        let demand = host.demand(vault.name()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let error = loop {
            match host.state(vault.name()) {
                Err(error) => break format!("{error:?}"),
                Ok(norn_wire::TrustState::Ready) => panic!("metadata failure published Ready"),
                _ => assert!(Instant::now() < deadline, "attach did not finish"),
            }
            std::thread::yield_now();
        };
        assert!(error.contains("metadata observer stopped"), "{error}");
        assert!(error.contains("verifying disposable retirement"), "{error}");
        let file = observer.probe.file.lock().unwrap().clone().unwrap();
        let identity = std::fs::metadata(&file).unwrap();
        assert!(
            std::fs::read_dir("/proc/self/fd").unwrap().all(|entry| {
                std::fs::metadata(entry.unwrap().path()).map_or(true, |open| {
                    (open.dev(), open.ino()) != (identity.dev(), identity.ino())
                })
            }),
            "an owner must not explain the remaining file"
        );
        eprintln!(
            "PROBE during_build={during_build}; both_causes={error}; actual_file_remains=true"
        );
        drop(demand);
        drop(host);
        // Restore only this test's fixture after its residue/cause assertions.
        drop(observer);
    }
}

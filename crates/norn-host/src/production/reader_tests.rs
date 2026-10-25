use super::*;
use norn_testkit::scratch::Scratch;

#[cfg(unix)]
#[test]
#[allow(clippy::disallowed_methods)] // Private derived-state access failure.
fn one_inaccessible_startup_store_does_not_skip_other_stores() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = Scratch::new("count-orphan-discovery-errors");
    let dirs = ConfigDirs::new(scratch.join("config"), scratch.join("data")).unwrap();
    for name in ["aaa-failed", "zzz-success"] {
        let derived = dirs.derived_dir(&VaultName::new(name).unwrap());
        let results = derived.join(STORE_FILE).with_extension("count-results");
        std::fs::create_dir_all(&results).unwrap();
        std::fs::write(
            results.join("result-0123456789abcdef0123456789abcdef.sqlite3"),
            b"orphan",
        )
        .unwrap();
    }
    let inaccessible = dirs.derived_dir(&VaultName::new("aaa-failed").unwrap());
    std::fs::set_permissions(&inaccessible, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_dir(&inaccessible).is_ok() {
        std::fs::set_permissions(&inaccessible, std::fs::Permissions::from_mode(0o700)).unwrap();
        eprintln!("skipped: this account reads a mode-000 directory");
        return;
    }
    let ops = ProductionEntryOps::new(dirs.clone(), ProductionPolicy::new(64, 64).unwrap());
    std::fs::set_permissions(&inaccessible, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(ops.last_reader_cleanup_error().is_some());
    let good = dirs
        .derived_dir(&VaultName::new("zzz-success").unwrap())
        .join(STORE_FILE)
        .with_extension("count-results");
    assert!(
        !good
            .join("result-0123456789abcdef0123456789abcdef.sqlite3")
            .exists(),
        "one inaccessible store cannot prevent cleaning another"
    );
}

#[test]
#[allow(clippy::disallowed_methods)] // Private derived-state fixtures.
fn one_failed_startup_sweep_does_not_skip_other_stores() {
    let scratch = Scratch::new("count-orphan-sweep-errors");
    let dirs = ConfigDirs::new(scratch.join("config"), scratch.join("data")).unwrap();
    for name in ["aaa-failed", "zzz-success"] {
        let derived = dirs.derived_dir(&VaultName::new(name).unwrap());
        let results = derived.join(STORE_FILE).with_extension("count-results");
        std::fs::create_dir_all(&results).unwrap();
        std::fs::write(
            results.join("result-0123456789abcdef0123456789abcdef.sqlite3"),
            b"orphan",
        )
        .unwrap();
        if name == "aaa-failed" {
            std::fs::create_dir(derived.join(MAINTAINER_LOCK_FILE)).unwrap();
        }
    }
    let ops = ProductionEntryOps::new(dirs.clone(), ProductionPolicy::new(64, 64).unwrap());
    assert!(ops.last_reader_cleanup_error().is_some());
    let good = dirs
        .derived_dir(&VaultName::new("zzz-success").unwrap())
        .join(STORE_FILE)
        .with_extension("count-results");
    assert!(
        !good
            .join("result-0123456789abcdef0123456789abcdef.sqlite3")
            .exists(),
        "one store failure does not skip later stores"
    );
}

#[test]
#[allow(clippy::disallowed_methods)] // A private derived-state fixture, not a vault tree.
fn startup_reclaims_count_orphans_only_after_acquiring_maintainership() {
    let scratch = Scratch::new("count-orphan-lock");
    let dirs = ConfigDirs::new(scratch.join("config"), scratch.join("data")).unwrap();
    let name = VaultName::new("counts").unwrap();
    let derived = dirs.derived_dir(&name);
    let results = derived.join(STORE_FILE).with_extension("count-results");
    std::fs::create_dir_all(&results).unwrap();
    let orphan = results.join("result-0123456789abcdef0123456789abcdef.sqlite3");
    let unrelated = results.join("keep-this-file");
    std::fs::write(&orphan, b"unfinished private result").unwrap();
    std::fs::write(&unrelated, b"not a result").unwrap();
    let Acquisition::Acquired(held) = try_acquire(&derived.join(MAINTAINER_LOCK_FILE)).unwrap()
    else {
        panic!("fixture lock");
    };
    let ops = ProductionEntryOps::new(dirs.clone(), ProductionPolicy::new(64, 64).unwrap());
    assert!(
        orphan.exists(),
        "startup must leave another maintainer's files alone"
    );
    assert!(ops.last_reader_cleanup_error().is_none());
    drop(held);
    let ops = ProductionEntryOps::new(dirs, ProductionPolicy::new(64, 64).unwrap());
    assert!(!orphan.exists(), "startup reclaims an abandoned result");
    assert!(
        unrelated.exists(),
        "the sweep only owns its recognized names"
    );
    assert!(ops.last_reader_cleanup_error().is_none());
}

#[test]
#[allow(clippy::disallowed_methods)] // Private derived-state fixtures.
fn startup_sweeps_retain_every_actual_failure() {
    let scratch = Scratch::new("startup-sweep-all-causes");
    let dirs = ConfigDirs::new(scratch.join("config"), scratch.join("data")).unwrap();
    for name in ["first-failure", "second-failure"] {
        let derived = dirs.derived_dir(&VaultName::new(name).unwrap());
        std::fs::create_dir_all(derived.join("store.count-results")).unwrap();
        std::fs::create_dir(derived.join(MAINTAINER_LOCK_FILE)).unwrap();
    }
    let ops = ProductionEntryOps::new(dirs, ProductionPolicy::new(2, 2).unwrap());
    let failures = ops
        .last_reader_cleanup_error()
        .expect("both failed lock acquisitions");
    assert!(failures.contains("first-failure"), "{failures}");
    assert!(failures.contains("second-failure"), "{failures}");
}

#[test]
fn cleanup_failure_history_keeps_recent_causes_and_counts_dropped_failures() {
    let scratch = Scratch::new("cleanup-failure-history");
    let dirs = ConfigDirs::new(scratch.join("config"), scratch.join("data")).unwrap();
    let ops = ProductionEntryOps::new(dirs, ProductionPolicy::new(2, 2).unwrap());
    for index in 0..1000 {
        ops.record_cleanup_failure(format!("failure-{index:04}"));
    }
    let failures = ops.last_reader_cleanup_error().expect("recorded failures");
    assert!(
        failures.len() < 200,
        "unbounded failure text: {} bytes",
        failures.len()
    );
    assert!(
        failures.contains("992 older failures dropped"),
        "{failures}"
    );
    assert!(!failures.contains("failure-0991"), "{failures}");
    for index in 992..1000 {
        assert!(
            failures.contains(&format!("failure-{index:04}")),
            "{failures}"
        );
    }
    assert!(failures.ends_with("failure-0999"), "{failures}");
}

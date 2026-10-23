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

#![cfg(unix)]
#![allow(clippy::disallowed_methods)] // Contract scaffolding arranges filesystem states.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use norn_fs::{ContentHash, Refusal, path_identity, read_and_hash};

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "norn-fs-observations-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("scratch directory");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn configured_file_bytes_and_fingerprint_are_one_observation() {
    let scratch = Scratch::new("schema");
    let schema = scratch.0.join("schema.toml");
    fs::write(&schema, b"version = 1\n").expect("schema bytes");

    let observed = read_and_hash(&schema).expect("schema observation");

    assert_eq!(observed.path(), schema);
    assert_eq!(observed.bytes(), b"version = 1\n");
    assert_eq!(observed.content_hash(), ContentHash::of(observed.bytes()));
    let (bytes, fingerprint) = observed.into_parts();
    assert_eq!(fingerprint, ContentHash::of(&bytes));
}

#[test]
fn missing_and_dangling_configured_files_are_distinct_refusals() {
    let scratch = Scratch::new("missing-schema");
    let missing = scratch.0.join("missing.toml");
    let dangling = scratch.0.join("dangling.toml");
    symlink(&missing, &dangling).expect("dangling schema name");

    let Refusal::Environment {
        operation: missing_operation,
        kind: missing_kind,
        path: missing_path,
        ..
    } = read_and_hash(&missing).expect_err("missing schema must refuse")
    else {
        panic!("missing schema was not an environmental refusal")
    };
    let Refusal::Environment {
        operation: dangling_operation,
        kind: dangling_kind,
        path: dangling_path,
        ..
    } = read_and_hash(&dangling).expect_err("dangling schema must refuse")
    else {
        panic!("dangling schema was not an environmental refusal")
    };

    assert_eq!(missing_kind, std::io::ErrorKind::NotFound);
    assert_eq!(dangling_kind, std::io::ErrorKind::NotFound);
    assert_eq!(missing_path, missing);
    assert_eq!(dangling_path, dangling);
    assert_eq!(missing_operation, "opening");
    assert_eq!(dangling_operation, "resolving dangling symbolic link at");
}

#[test]
fn a_directory_cannot_masquerade_as_configured_file_bytes() {
    let scratch = Scratch::new("directory-schema");
    let refusal = read_and_hash(&scratch.0).expect_err("a directory must refuse");
    assert!(
        matches!(refusal, Refusal::Environment { kind, .. }
            if kind == std::io::ErrorKind::InvalidData),
        "{refusal:?}"
    );
}

#[test]
fn unreadable_configured_file_is_an_environmental_refusal() {
    let scratch = Scratch::new("unreadable-schema");
    let schema = scratch.0.join("schema.toml");
    fs::write(&schema, b"secret").expect("schema bytes");
    fs::set_permissions(&schema, fs::Permissions::from_mode(0o000)).expect("remove access");

    let refusal = read_and_hash(&schema).expect_err("unreadable schema must refuse");

    assert!(
        matches!(refusal, Refusal::Environment { kind, ref path, .. }
            if kind == std::io::ErrorKind::PermissionDenied && *path == schema),
        "{refusal:?}"
    );
}

#[test]
fn root_identity_detects_dot_and_symbolic_link_aliases() {
    let scratch = Scratch::new("root-aliases");
    let vault = scratch.0.join("vault");
    let alias = scratch.0.join("alias");
    fs::create_dir(&vault).expect("vault root");
    symlink(&vault, &alias).expect("root alias");

    let identity = path_identity(&vault).expect("root observation");
    assert_eq!(path_identity(&vault.join(".")).unwrap(), identity);
    assert_eq!(path_identity(&alias).unwrap(), identity);
}

#[test]
fn absent_and_dangling_roots_have_no_identity() {
    let scratch = Scratch::new("missing-root");
    let missing = scratch.0.join("missing");
    let dangling = scratch.0.join("dangling");
    symlink(&missing, &dangling).expect("dangling root name");

    assert_eq!(path_identity(&missing).unwrap(), None);
    assert_eq!(path_identity(&dangling).unwrap(), None);
}

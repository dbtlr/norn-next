//! Scratch trees for the integration suites.
//!
//! Trees are generated under cargo's per-package temporary directory and
//! removed as soon as the value being asserted has been extracted from them.
//! Two properties matter and are easy to lose: the directory name is never
//! dot-prefixed, because a hidden root is exactly what a vault walk is
//! entitled to skip; and every generation gets its own name, because the
//! suites run in parallel.
//!
//! Each integration binary compiles the whole module and uses part of it, so
//! the unused remainder is expected rather than a defect.
#![allow(dead_code)]

use std::fs;
use std::path::PathBuf;

use norn_fixtures::probe::VaultStats;
use norn_fixtures::{Manifest, Profile, generate, probe};

/// A fresh, empty, non-hidden directory named `label`.
pub fn fresh(label: &str) -> PathBuf {
    assert!(
        !label.starts_with('.'),
        "a scratch root must not be hidden: {label}"
    );
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(label);
    if dir.exists() {
        fs::remove_dir_all(&dir).expect("clearing a stale scratch tree");
    }
    dir
}

/// Generate, take the contract digest, then delete the tree.
///
/// The digest comes off the manifest rather than off a walk of the result:
/// the contract is stated over what the generator emits, and a walk would key
/// on whatever spelling the filesystem hands back.
pub fn generate_and_digest(label: &str, profile: &Profile, seed: u64) -> ([u8; 32], Manifest) {
    let dir = fresh(label);
    let manifest = generate(profile, seed, &dir).expect("generating a scratch tree");
    fs::remove_dir_all(&dir).expect("removing a scratch tree");
    (manifest.tree_digest, manifest)
}

/// Generate, measure the tree's shape, then delete it.
pub fn generate_and_measure(label: &str, profile: &Profile, seed: u64) -> (VaultStats, Manifest) {
    let dir = fresh(label);
    let manifest = generate(profile, seed, &dir).expect("generating a scratch tree");
    let stats = probe::measure(&dir).expect("measuring a scratch tree");
    fs::remove_dir_all(&dir).expect("removing a scratch tree");
    (stats, manifest)
}

/// Generate, hand the tree to `inspect`, then delete it.
pub fn with_tree<T>(
    label: &str,
    profile: &Profile,
    seed: u64,
    inspect: impl FnOnce(&PathBuf, &Manifest) -> T,
) -> T {
    let dir = fresh(label);
    let manifest = generate(profile, seed, &dir).expect("generating a scratch tree");
    let out = inspect(&dir, &manifest);
    fs::remove_dir_all(&dir).expect("removing a scratch tree");
    out
}

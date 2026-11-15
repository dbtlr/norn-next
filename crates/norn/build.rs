//! Captures the build identity: `<package version>+<short commit>`, with
//! `-dirty` when the work tree has changes, or `+unknown` where git is not
//! available. The bin hands it to the serving and client crates as
//! `NORN_BUILD_IDENTITY`.
//!
//! The script reruns when the checked-out commit, the index or any source the
//! workspace builds from changes, so a binary built from an edited tree says
//! so. A rebuild at the same commit and dirty state carries the same identity,
//! which is accepted.

use std::path::{Path, PathBuf};
use std::process::Command;

#[allow(clippy::disallowed_macros)] // Cargo build-script protocol.
fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets it"));
    let workspace = manifest
        .ancestors()
        .nth(2)
        .expect("the bin sits two directories under the workspace root")
        .to_path_buf();
    let version = std::env::var("CARGO_PKG_VERSION").expect("cargo sets it");
    let source = commit(&workspace).unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=NORN_BUILD_IDENTITY={version}+{source}");

    for path in ["crates", "vendor", "Cargo.toml", "Cargo.lock"] {
        println!("cargo:rerun-if-changed={}", workspace.join(path).display());
    }
    for path in git_paths(&workspace) {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

/// The short commit and the dirty mark, or `None` where git cannot answer.
fn commit(workspace: &Path) -> Option<String> {
    let short = git(workspace, &["rev-parse", "--short=8", "HEAD"])?;
    // `--no-optional-locks` keeps the status read-only: a status that refreshed
    // the index would touch a file this script reruns on, and rerun it on
    // every build after.
    let status = git(workspace, &["--no-optional-locks", "status", "--porcelain"])?;
    Some(if status.is_empty() {
        short
    } else {
        format!("{short}-dirty")
    })
}

/// The files whose change means the commit or the index moved: `HEAD` and the
/// index in this work tree's git directory, and the refs in the common one.
fn git_paths(workspace: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(git_dir) = git(workspace, &["rev-parse", "--absolute-git-dir"]) {
        let git_dir = PathBuf::from(git_dir);
        paths.push(git_dir.join("HEAD"));
        paths.push(git_dir.join("index"));
    }
    if let Some(common) = git(workspace, &["rev-parse", "--git-common-dir"]) {
        let common = workspace.join(common);
        paths.push(common.join("refs"));
        paths.push(common.join("packed-refs"));
    }
    paths
}

fn git(workspace: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

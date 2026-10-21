//! Build each package once and list stable copies of the executables Cargo
//! reported. Package boundaries preserve the feature graph of each audit.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use super::{Listing, Target};
use crate::process::{Run, RunStatus, Sandbox};

pub(super) fn collect(
    workspace_root: &Path,
    wanted: BTreeSet<(String, Target)>,
    features: &[&str],
) -> BTreeMap<(String, Target), Result<Listing, String>> {
    let mut packages: BTreeMap<String, BTreeSet<Target>> = BTreeMap::new();
    for (package, target) in wanted {
        packages.entry(package).or_default().insert(target);
    }
    let mut listings = BTreeMap::new();
    for (package, targets) in packages {
        let artifacts = build(workspace_root, &package, &targets, features);
        for target in targets {
            let listing = artifacts
                .as_ref()
                .map_err(Clone::clone)
                .and_then(|artifacts| {
                    let executable = artifacts.get(&target).ok_or_else(|| {
                        format!("Cargo reported no test executable for `{target}`")
                    })?;
                    list(executable)
                })
                .map_err(|problem| {
                    format!("listing `{package} {target}`'s tests failed: {problem}")
                });
            listings.insert((package.clone(), target), listing);
        }
    }
    listings
}

/// A failed batch refuses every selected target. Partial artifacts never turn
/// a failed build into successful evidence, and no automatic retry hides it.
fn build(
    workspace_root: &Path,
    package: &str,
    targets: &BTreeSet<Target>,
    features: &[&str],
) -> Result<BTreeMap<Target, PathBuf>, String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsStr::new("cargo").to_os_string());
    let mut command = Command::new(cargo);
    command.current_dir(workspace_root).args([
        "test",
        "--locked",
        "-p",
        package,
        "--no-run",
        "--message-format=json-render-diagnostics",
    ]);
    if !features.is_empty() {
        command.arg("--features").arg(features.join(","));
    }
    for target in targets {
        command.args(target.selector());
    }
    let output = command
        .output()
        .map_err(|e| format!("running Cargo: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "Cargo build ended {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let output = std::str::from_utf8(&output.stdout)
        .map_err(|e| format!("Cargo artifact output is not UTF-8: {e}"))?;
    let mut artifacts = BTreeMap::new();
    let mut finished = false;
    for line in output.lines() {
        let message: serde_json::Value = serde_json::from_str(line)
            .map_err(|e| format!("reading Cargo artifact output: {e}"))?;
        match message["reason"].as_str() {
            Some("compiler-artifact") => {
                let artifact: Artifact = serde_json::from_value(message)
                    .map_err(|e| format!("reading a Cargo artifact: {e}"))?;
                if !artifact.profile.test {
                    continue;
                }
                let Some(target) = artifact.target.test_target() else {
                    continue;
                };
                if !targets.contains(&target) {
                    continue;
                }
                let executable = artifact.executable.ok_or_else(|| {
                    format!("Cargo's test artifact for `{target}` has no executable")
                })?;
                if artifacts.insert(target.clone(), executable).is_some() {
                    return Err(format!(
                        "Cargo reported multiple test executables for `{target}`"
                    ));
                }
            }
            Some("build-finished") => finished = message["success"].as_bool() == Some(true),
            _ => {}
        }
    }
    if !finished {
        return Err("Cargo reported no successful build-finished record".to_string());
    }
    Ok(artifacts)
}

#[derive(Deserialize)]
struct Artifact {
    target: CargoTarget,
    profile: Profile,
    executable: Option<PathBuf>,
}

#[derive(Deserialize)]
struct Profile {
    test: bool,
}

#[derive(Deserialize)]
struct CargoTarget {
    name: String,
    kind: Vec<String>,
}

impl CargoTarget {
    fn test_target(&self) -> Option<Target> {
        if self.kind.iter().any(|kind| kind == "test") {
            Some(Target::Integration(self.name.clone()))
        } else if self.kind.iter().any(|kind| {
            matches!(
                kind.as_str(),
                "lib" | "rlib" | "dylib" | "cdylib" | "staticlib" | "proc-macro"
            )
        }) {
            // The library target's name need not equal its package's name.
            Some(Target::Lib)
        } else {
            None
        }
    }
}

fn list(executable: &Path) -> Result<Listing, String> {
    let sandbox = Sandbox::new(&std::env::temp_dir(), "norn-compiled-test-list")
        .map_err(|e| format!("creating a listing sandbox: {e}"))?;
    let executable = sandbox
        .install_binary(executable)
        .map_err(|e| format!("installing the test executable: {e}"))?;
    let all = names(&sandbox, &executable, false)?;
    let ignored = names(&sandbox, &executable, true)?;
    if !ignored.is_subset(&all) {
        return Err("the ignored listing names tests absent from the complete listing".to_string());
    }
    Ok(Listing { all, ignored })
}

fn names(sandbox: &Sandbox, executable: &Path, ignored: bool) -> Result<BTreeSet<String>, String> {
    let mut run = Run::new(sandbox, executable).arg("--list");
    if ignored {
        run = run.arg("--ignored");
    }
    let output = run
        .wait()
        .map_err(|e| format!("running the test listing: {e}"))?;
    if output.status != RunStatus::Exited(0) {
        return Err(format!(
            "test listing ended {:?}: {}",
            output.status,
            output.stderr_text()
        ));
    }
    if output.stdout_truncated || output.stderr_truncated {
        return Err("test listing output was truncated".to_string());
    }
    let text = std::str::from_utf8(&output.stdout)
        .map_err(|e| format!("test listing output is not UTF-8: {e}"))?;
    let mut lines = text.lines().filter(|line| !line.is_empty());
    let summary = lines.next_back().ok_or("test listing has no summary")?;
    let (tests, benchmarks) = summary
        .split_once(", ")
        .ok_or_else(|| format!("test listing has an invalid summary: {summary}"))?;
    let test_count = count(tests, "test")?;
    let benchmark_count = count(benchmarks, "benchmark")?;
    let mut names = BTreeSet::new();
    let mut benches = BTreeSet::new();
    for line in lines {
        if let Some(name) = line.strip_suffix(": test") {
            names.insert(name.to_string());
        } else if let Some(name) = line.strip_suffix(": benchmark") {
            benches.insert(name.to_string());
        } else {
            return Err(format!("test listing has an unexpected line: {line}"));
        }
    }
    if names.len() != test_count || benches.len() != benchmark_count {
        return Err("test listing does not match its summary".to_string());
    }
    Ok(names)
}

fn count(text: &str, kind: &str) -> Result<usize, String> {
    text.strip_suffix(&format!(" {kind}s"))
        .or_else(|| text.strip_suffix(&format!(" {kind}")))
        .and_then(|number| number.parse().ok())
        .ok_or_else(|| format!("test listing has an invalid count: {text}"))
}

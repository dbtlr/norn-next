//! Actual checkout identity for rolling regression and release evidence.

use std::path::{Path, PathBuf};

use norn_testkit::process::{Outcome, Run, RunStatus, Sandbox};
use serde_json::Value;

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(".github/scripts/regression-identity.py")
}

fn git(sandbox: &Sandbox, args: &[&str]) -> String {
    let result = Run::new(sandbox, "git")
        .args(args)
        .wait()
        .expect("running fixture Git");
    assert_eq!(
        result.status,
        RunStatus::Exited(0),
        "{}{}",
        result.stdout_text(),
        result.stderr_text()
    );
    result.stdout_text().trim().to_string()
}

fn checkout() -> (Sandbox, String) {
    let sandbox = Sandbox::new(
        Path::new(env!("CARGO_TARGET_TMPDIR")),
        "regression-identity",
    )
    .expect("an isolated regression checkout");
    git(&sandbox, &["init", "--quiet"]);
    git(&sandbox, &["config", "user.name", "Fixture"]);
    git(
        &sandbox,
        &["config", "user.email", "fixture@example.invalid"],
    );
    git(
        &sandbox,
        &["commit", "--quiet", "--allow-empty", "-m", "candidate"],
    );
    let sha = git(&sandbox, &["rev-parse", "HEAD"]);
    (sandbox, sha)
}

#[allow(clippy::disallowed_methods)] // Harness scaffolding: read this run's private receipt.
fn identity(sandbox: &Sandbox, candidate: &str, event: &str, workflow: &str) -> (Outcome, Value) {
    let receipt = sandbox.work_dir().join("identity.json");
    let result = Run::new(sandbox, "python3")
        .arg(script())
        .arg(&receipt)
        .env("NORN_REGRESSION_SHA", candidate)
        .env("NORN_REGRESSION_EVENT_SHA", event)
        .env("NORN_REGRESSION_WORKFLOW_SHA", workflow)
        .env("GITHUB_JOB", "build")
        .env("GITHUB_RUN_ID", "123")
        .env("GITHUB_RUN_ATTEMPT", "2")
        .env("GIT_CEILING_DIRECTORIES", sandbox.root())
        .wait()
        .expect("running the regression identity check");
    let read = std::fs::read(&receipt).unwrap_or_else(|error| {
        panic!(
            "missing receipt: {error}; {}{}",
            result.stdout_text(),
            result.stderr_text()
        )
    });
    (
        result,
        serde_json::from_slice(&read).expect("a JSON receipt"),
    )
}

#[test]
fn regression_identity_checks_the_actual_checkout() {
    let (sandbox, sha) = checkout();
    let (result, receipt) = identity(&sandbox, &sha, &sha, &sha);
    assert_eq!(
        result.status,
        RunStatus::Exited(0),
        "{}{}",
        result.stdout_text(),
        result.stderr_text()
    );
    assert_eq!(receipt["checkout_sha"], sha);
    assert_eq!(receipt["candidate_sha"], sha);
    assert_eq!(receipt["workflow_sha"], sha);
    assert_eq!(receipt["event_sha"], sha);
    assert_eq!(receipt["identity_matches"], true);
    assert_eq!(receipt["job"], "build");
    assert_eq!(receipt["run_id"], "123");
    assert_eq!(receipt["run_attempt"], "2");
}

#[test]
fn mismatches_and_malformed_assertions_leave_failed_identity_evidence() {
    let (sandbox, sha) = checkout();
    let other = "0".repeat(40);
    for (candidate, event, workflow) in [
        (other.as_str(), sha.as_str(), sha.as_str()),
        (sha.as_str(), other.as_str(), sha.as_str()),
        (sha.as_str(), sha.as_str(), other.as_str()),
        (other.as_str(), other.as_str(), other.as_str()),
        ("", sha.as_str(), sha.as_str()),
        ("main", sha.as_str(), sha.as_str()),
        ("abc\n", sha.as_str(), sha.as_str()),
    ] {
        let (result, receipt) = identity(&sandbox, candidate, event, workflow);
        assert_eq!(result.status, RunStatus::Exited(1));
        assert_eq!(receipt["identity_matches"], false);
        assert_eq!(receipt["candidate_sha"], candidate);
        assert_eq!(receipt["checkout_sha"], sha);
    }
    git(
        &sandbox,
        &["commit", "--quiet", "--allow-empty", "-m", "new tip"],
    );
    let (result, receipt) = identity(&sandbox, &sha, &sha, &sha);
    assert_eq!(result.status, RunStatus::Exited(1));
    assert_eq!(receipt["identity_matches"], false);
    assert_ne!(receipt["checkout_sha"], sha);
}

#[test]
fn missing_checkout_never_establishes_candidate_identity() {
    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), "no-checkout")
        .expect("a sandbox without Git");
    let sha = "0".repeat(40);
    let (result, receipt) = identity(&sandbox, &sha, &sha, &sha);
    assert_eq!(result.status, RunStatus::Exited(1));
    assert_eq!(receipt["identity_matches"], false);
    assert_eq!(receipt["checkout_sha"], "");
}

//! The applier's cases that need a failure the environment will not produce:
//! a process that dies at a publication, and another writer landing between
//! a target's staging and its publication.
//!
//! **Each runs the apply in a child.** `norn-fs`'s fault seam arms itself from
//! the process's environment, once, and a process that dies cannot report on
//! itself, so the parent builds the vault and the plan, a child of this same
//! test binary applies it under the arm, and the parent reads what the child
//! left — the tree, the arm's record, and the outcome where the child lived —
//! and then sends the plan again itself.
#![allow(clippy::disallowed_methods)] // Harness scaffolding: the child's inputs and outputs.

use std::path::PathBuf;
use std::process::Command;

use norn_wire::{
    ApplyReport, ErrorDetail, ErrorEnvelope, InterruptionCause, Operation, ResolvedPlan,
};

use super::tests::{Fixture, applied, creating, deleting, editing, moving, path};

const CHILD: &str = "NORN_APPLIER_CHILD";
const VAULT: &str = "NORN_APPLIER_VAULT";
const DATA: &str = "NORN_APPLIER_DATA";
const PLAN: &str = "NORN_APPLIER_PLAN";
const OUTCOME: &str = "NORN_APPLIER_OUTCOME";
const ARMED: &str = "NORN_FS_ARMED_STAGES";
const HITS: &str = "NORN_FS_ARM_HITS";

/// The child: apply the plan it is handed to the vault it is handed, under
/// whatever the environment arms, and write the outcome as the wire's JSON.
/// Run as an ordinary case with no child variable, it does nothing.
#[test]
fn applier_child() {
    if std::env::var_os(CHILD).is_none() {
        return;
    }
    let var = |name: &str| PathBuf::from(std::env::var_os(name).expect("the child's input"));
    let plan: ResolvedPlan =
        serde_json::from_slice(&std::fs::read(var(PLAN)).expect("the plan")).expect("a plan");
    let mut fixture = Fixture::over(None, var(VAULT), var(DATA), "child.sqlite3");
    let outcome = fixture.apply(plan).into_wire();
    std::fs::write(
        var(OUTCOME),
        serde_json::to_vec(&outcome).expect("an outcome"),
    )
    .expect("the outcome is written");
}

/// What a child applying `plan` over `fixture` under `armed` came to.
struct Child {
    /// Whether it lived to answer.
    lived: bool,
    /// The arm's record: one line per firing.
    hits: String,
    /// Its outcome, where it lived.
    outcome: Option<Result<ApplyReport, ErrorEnvelope>>,
}

fn run_child(fixture: &Fixture, plan: &ResolvedPlan, armed: &str) -> Child {
    let dir = fixture
        .data
        .join(norn_testkit::scratch::unique_name("child"));
    std::fs::create_dir_all(&dir).expect("the child's directory");
    let (plan_file, outcome_file, hits_file) = (
        dir.join("plan.json"),
        dir.join("outcome.json"),
        dir.join("hits"),
    );
    std::fs::write(&plan_file, serde_json::to_vec(plan).expect("a plan")).expect("the plan");
    std::fs::write(&hits_file, "").expect("the record file");
    let output = Command::new(std::env::current_exe().expect("this test binary"))
        .args(["--exact", "applier::induced::applier_child", "--nocapture"])
        .env(CHILD, "1")
        .env(VAULT, &fixture.vault)
        .env(DATA, &fixture.data)
        .env(PLAN, &plan_file)
        .env(OUTCOME, &outcome_file)
        .env(ARMED, armed)
        .env(HITS, &hits_file)
        .output()
        .expect("the child runs");
    let outcome = std::fs::read(&outcome_file)
        .ok()
        .map(|bytes| serde_json::from_slice(&bytes).expect("an outcome"));
    Child {
        lived: output.status.success(),
        hits: std::fs::read_to_string(&hits_file).expect("the record"),
        outcome,
    }
}

/// A vault and a plan that touches every publication position: a move into
/// a folder it makes, a chain of moves, an edit, a create two folders deep,
/// and a removal, publishing eight times.
fn every_position() -> (Fixture, Vec<Operation>) {
    let fixture = Fixture::new(&[
        ("a.md", "# A\n[[b]]\n"),
        ("b.md", "# B\n"),
        ("e.md", "status draft\n"),
        ("gone.md", "# Gone\n"),
        ("inbox/m.md", "# M\n[[e]]\n"),
    ]);
    let operations = vec![
        moving("inbox/m.md", "archive/m.md"),
        moving("b.md", "c.md"),
        moving("a.md", "b.md"),
        editing("e.md", "draft", "final"),
        creating("deep/new/n.md", "# N\n"),
        deleting("gone.md"),
    ];
    (fixture, operations)
}

const FINAL_TREE: [&str; 8] = [
    "archive",
    "archive/m.md",
    "b.md",
    "c.md",
    "deep",
    "deep/new",
    "deep/new/n.md",
    "e.md",
];

/// **A crash at every publication position, then a re-send.** For each
/// publication the plan makes and each step inside it — the exclusive rename
/// of a create or the rename of a replace, a removal's unlink, a folder a
/// create makes, a folder sync — the process ends there; sending the resolved
/// plan again then lands it whole, and the store equals a build from zero
/// over the tree it leaves. Each move leg and each link of the chain is one of
/// those positions.
#[test]
fn a_plan_a_crash_cut_short_at_any_publication_is_finished_by_sending_it_again() {
    let mut fired = 0;
    for stage in ["swap", "unlink", "mkdir", "parent-sync"] {
        for ordinal in 1..=8 {
            let (mut fixture, operations) = every_position();
            let plan = fixture.plan(operations);
            let child = run_child(&fixture, &plan, &format!("{stage}@{ordinal}=ends"));
            if child.hits.is_empty() {
                // This position has no such step: the child ran to the end.
                assert!(child.lived, "{stage}@{ordinal}: the child died unarmed");
            } else {
                fired += 1;
                assert!(
                    !child.lived,
                    "{stage}@{ordinal}: the child outlived its end"
                );
                assert!(
                    child.hits.contains(&format!("stage={stage}")),
                    "{}",
                    child.hits
                );
            }
            let finished = applied(fixture.apply(plan));
            assert_eq!(
                finished.targets.len(),
                8,
                "{stage}@{ordinal}: every target lands"
            );
            assert_eq!(fixture.tree(), FINAL_TREE, "{stage}@{ordinal}");
            assert_eq!(fixture.read("b.md").as_deref(), Some("# A\n[[b]]\n"));
            assert_eq!(fixture.read("c.md").as_deref(), Some("# B\n"));
            assert!(
                fixture
                    .shadows_left()
                    .iter()
                    .all(|name| !name.contains(&format!("-{}-", std::process::id()))),
                "{stage}@{ordinal}: the re-send leaves no shadow of its own; the dead child's are the sweep's: {:?}",
                fixture.shadows_left()
            );
            fixture.assert_store_is_a_build_from_zero();
        }
    }
    // Five renames, three unlinks, two creates that make folders, and a sync
    // after each of the eight.
    assert_eq!(fired, 5 + 3 + 2 + 8, "every position was reached");
}

fn envelope(child: &Child) -> &ErrorEnvelope {
    match child.outcome.as_ref().expect("the child answered") {
        Err(envelope) => envelope,
        Ok(report) => panic!("the apply is not applied: {report:?}"),
    }
}

/// **A foreign edit after staging, before anything landed, refuses** with a
/// fresh plan, and writes nothing; after something landed, it interrupts,
/// naming what landed.
#[test]
fn a_foreign_edit_after_staging_refuses_before_anything_landed_and_interrupts_after() {
    let (fixture, _) = every_position();
    let plan = fixture.plan(vec![
        editing("e.md", "draft", "final"),
        creating("n.md", "n\n"),
    ]);
    // The create publishes first: the edit's target changes under its second
    // publication.
    let child = run_child(&fixture, &plan, "foreign@2=edit");
    assert!(child.lived);
    let ErrorDetail::PlanInterrupted { landed, cause, .. } = envelope(&child).detail() else {
        panic!("the apply is interrupted: {:?}", child.outcome);
    };
    assert_eq!(*landed, vec![path("n.md")]);
    assert_eq!(*cause, InterruptionCause::foreign_edit(path("e.md")));

    let (fixture, _) = every_position();
    let plan = fixture.plan(vec![
        editing("e.md", "draft", "final"),
        creating("n.md", "n\n"),
    ]);
    let before = fixture.tree();
    let child = run_child(&fixture, &plan, "foreign@1=take");
    let ErrorDetail::PlanRefused {
        checks,
        plan: fresh,
        ..
    } = envelope(&child).detail()
    else {
        panic!("the apply is refused: {:?}", child.outcome);
    };
    assert_eq!(
        *checks,
        vec![norn_wire::RefusedCheck::name_taken(path("n.md"))]
    );
    assert!(
        fresh
            .operations
            .contains(&editing("e.md", "draft", "final"))
    );
    let mut expected = before;
    expected.push("n.md".to_string());
    expected.sort();
    assert_eq!(fixture.tree(), expected, "only the foreign file is new");
    assert_eq!(fixture.read("e.md").as_deref(), Some("status draft\n"));
}

/// **A foreign edit on an interrupted move's source**, or on a chain's middle,
/// leaves the move unresolved in the fresh plan a re-send answers with, and
/// nothing foreign is removed.
#[test]
fn a_foreign_edit_on_an_interrupted_moves_source_leaves_the_move_unresolved() {
    for (operations, foreign_at, edited) in [
        // The move's destination publishes first, then its source's removal.
        (vec![moving("inbox/m.md", "archive/m.md")], 2, "inbox/m.md"),
        // The chain publishes `c.md`, then replaces `b.md`, then removes `a.md`.
        (
            vec![moving("b.md", "c.md"), moving("a.md", "b.md")],
            2,
            "b.md",
        ),
    ] {
        let (mut fixture, _) = every_position();
        let plan = fixture.plan(operations.clone());
        let child = run_child(&fixture, &plan, &format!("foreign@{foreign_at}=edit"));
        let ErrorDetail::PlanInterrupted { cause, .. } = envelope(&child).detail() else {
            panic!("the apply is interrupted: {:?}", child.outcome);
        };
        assert_eq!(*cause, InterruptionCause::foreign_edit(path(edited)));
        let foreign = fixture.read(edited).expect("the foreign edit stands");
        let refused = super::tests::refused(fixture.apply(plan));
        assert!(refused.plan.operations.is_empty(), "{:?}", refused.plan);
        assert_eq!(
            refused
                .unresolved
                .iter()
                .map(|left| left.operation.clone())
                .collect::<Vec<_>>(),
            operations
        );
        assert_eq!(
            fixture.read(edited).as_deref(),
            Some(foreign.as_str()),
            "nothing foreign is removed"
        );
        assert!(refused.forecast.drifted.contains(&path(edited)));
    }
}

/// A staging failure is an I/O failure before anything landed: the plan is
/// not applied, the removal staged before it publishes nothing, and no shadow
/// is left.
#[test]
fn a_staging_failure_writes_nothing_and_answers_write_failed() {
    let (fixture, _) = every_position();
    let plan = fixture.plan(vec![deleting("a.md"), creating("z.md", "z\n")]);
    let before = fixture.tree();
    let child = run_child(&fixture, &plan, "stage-write=fails");
    assert!(child.lived);
    assert!(
        matches!(envelope(&child).detail(), ErrorDetail::WriteFailed { .. }),
        "{:?}",
        child.outcome
    );
    assert_eq!(fixture.tree(), before);
    assert!(
        fixture.shadows_left().is_empty(),
        "{:?}",
        fixture.shadows_left()
    );
}

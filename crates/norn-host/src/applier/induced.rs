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

use super::tests::{
    Fixture, applied, creating, deleting, editing, moving, path, quarantined_fixture,
};

const CHILD: &str = "NORN_APPLIER_CHILD";
const VAULT: &str = "NORN_APPLIER_VAULT";
const DATA: &str = "NORN_APPLIER_DATA";
const PLAN: &str = "NORN_APPLIER_PLAN";
const OUTCOME: &str = "NORN_APPLIER_OUTCOME";
const ARMED: &str = "NORN_FS_ARMED_STAGES";
const HITS: &str = "NORN_FS_ARM_HITS";
const SCHEMA: &str = "NORN_APPLIER_SCHEMA";

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
    if let Some(schema) = std::env::var_os(SCHEMA) {
        fixture.pin(schema.to_str().expect("a UTF-8 schema"));
    }
    let outcome = fixture
        .apply(plan)
        .into_wire()
        .expect("a child that publishes does not stand down");
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
    run_child_pinned(fixture, plan, armed, None)
}

/// What a child applying `plan` over `fixture` under `armed` came to, with
/// `schema` pinned in its store where one is given.
fn run_child_pinned(
    fixture: &Fixture,
    plan: &ResolvedPlan,
    armed: &str,
    schema: Option<&str>,
) -> Child {
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
    let mut command = Command::new(std::env::current_exe().expect("this test binary"));
    if let Some(schema) = schema {
        command.env(SCHEMA, schema);
    }
    let output = command
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
            // `a.md`'s `[[b]]` named the document the chain carries to
            // `c.md`, and follows it there off the path `a.md` refills.
            assert_eq!(fixture.read("b.md").as_deref(), Some("# A\n[[c]]\n"));
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

/// **An interrupted forced apply lists the violations its force let through
/// in the targets that landed**: the create that published carries its
/// undeclared tag on the interruption, and the edit a foreign writer stopped
/// carries none, since it did not land.
#[test]
fn an_interrupted_forced_apply_lists_what_its_force_let_through_where_it_landed() {
    let fixture = Fixture::with_schema(super::tests::TAG_SCHEMA, &[("e.md", "status draft\n")]);
    let mut plan = fixture.plan(vec![
        editing("e.md", "draft", "draft #other"),
        creating("n.md", "# N\n#stray\n"),
    ]);
    plan.force = true;
    // The create publishes first: the edit's target changes under its second
    // publication.
    let child = run_child_pinned(
        &fixture,
        &plan,
        "foreign@2=edit",
        Some(super::tests::TAG_SCHEMA),
    );
    assert!(child.lived);
    let ErrorDetail::PlanInterrupted { landed, forced, .. } = envelope(&child).detail() else {
        panic!("the apply is interrupted: {:?}", child.outcome);
    };
    assert_eq!(*landed, vec![path("n.md")]);
    let forced: Vec<(&str, Option<&str>)> = forced
        .iter()
        .map(|violation| (violation.path.as_str(), violation.target.as_deref()))
        .collect();
    assert_eq!(forced, vec![("n.md", Some("stray"))]);
}

/// **A force does not bypass a create's exclusive publication**: a forced
/// create whose name another writer takes after staging refuses on the taken
/// name and leaves the other writer's document.
#[test]
fn a_forced_create_whose_name_is_taken_after_staging_refuses() {
    let fixture = Fixture::with_schema(super::tests::TAG_SCHEMA, &[("e.md", "status draft\n")]);
    let mut plan = fixture.plan(vec![creating("n.md", "# N\n#stray\n")]);
    plan.force = true;
    let child = run_child_pinned(
        &fixture,
        &plan,
        "foreign@1=take",
        Some(super::tests::TAG_SCHEMA),
    );
    assert!(child.lived);
    let ErrorDetail::PlanRefused { checks, .. } = envelope(&child).detail() else {
        panic!("the apply is refused: {:?}", child.outcome);
    };
    assert_eq!(
        *checks,
        vec![norn_wire::RefusedCheck::name_taken(path("n.md"))]
    );
    assert_ne!(fixture.read("n.md").as_deref(), Some("# N\n#stray\n"));
}

/// **A foreign edit on an interrupted move's source**, or on a chain's middle,
/// leaves the move unresolved in the fresh plan a re-send answers with, listed
/// as the refused plan carried it, cascade and all, and nothing foreign is
/// removed.
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
        let plan = fixture.plan(operations);
        let carried = plan.operations.clone();
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
            carried
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

/// On a root that folds case, a respell that dies at either of its steps is
/// finished by a re-send; one whose rename fails after its content landed is
/// interrupted, and the changeset carries that first step at the old
/// spelling, which is where the own-write ledger recorded it.
#[test]
fn a_respell_cut_short_at_either_step_is_finished_and_its_first_step_is_committed() {
    if !super::tests::volume_folds(
        "a_respell_cut_short_at_either_step_is_finished_and_its_first_step_is_committed",
    ) {
        return;
    }
    let respelled = || {
        let fixture = Fixture::new(&[("Note.md", "# Note\ndraft\n")]);
        let plan = fixture.plan(vec![
            editing("Note.md", "draft", "final"),
            moving("Note.md", "note.md"),
        ]);
        (fixture, plan)
    };
    for armed in ["swap@1=ends", "respell@1=ends", "parent-sync@1=ends"] {
        let (mut fixture, plan) = respelled();
        let child = run_child(&fixture, &plan, armed);
        assert!(!child.hits.is_empty(), "{armed}: the arm fired");
        assert!(!child.lived, "{armed}");
        applied(fixture.apply(plan));
        assert_eq!(fixture.tree(), vec!["note.md"], "{armed}");
        assert_eq!(fixture.read("note.md").as_deref(), Some("# Note\nfinal\n"));
        fixture.assert_store_is_a_build_from_zero();
    }

    let (fixture, plan) = respelled();
    let child = run_child(&fixture, &plan, "respell@1=fails");
    assert!(child.lived);
    let ErrorDetail::PlanInterrupted { landed, .. } = envelope(&child).detail() else {
        panic!("the respell is interrupted: {:?}", child.outcome);
    };
    assert!(landed.is_empty(), "neither spelling is at its after-state");
    let mut store = norn_store::Store::open(
        fixture.data.join("child.sqlite3"),
        norn_store::StoredPathOrder::AsciiCaseInsensitive,
        crate::DERIVATION_VERSION,
    )
    .expect("the child's store");
    let row = store
        .begin_request()
        .stored_document(&norn_store::DocumentPath::new("Note.md").expect("a path"))
        .expect("a read")
        .expect("the old spelling's row");
    assert_eq!(
        row.content_hash,
        norn_fs::ContentHash::of(b"# Note\nfinal\n").to_string(),
        "the changeset carries the first step"
    );
}

/// **A landing not synced stops publication**, whether this apply wrote the
/// target or found it already landed: a later target may draw on it, so the
/// apply is interrupted naming what landed, nothing after it publishes, and
/// the shadows staged for what did not publish are discarded.
#[test]
fn a_landing_whose_folder_is_not_synced_interrupts_the_apply() {
    for (written_first, armed) in [
        // The create publishes first and its folder sync fails.
        (true, "parent-sync@1=fails"),
        // The create is found already landed, and confirming it syncs its
        // folders, which fail; confirming counts no publication, so the arm
        // is bare.
        (false, "parent-sync=fails"),
    ] {
        let (fixture, _) = every_position();
        let plan = fixture.plan(vec![
            editing("e.md", "draft", "final"),
            creating("n.md", "n\n"),
        ]);
        if !written_first {
            fixture.write("n.md", "n\n");
        }
        let child = run_child(&fixture, &plan, armed);
        assert!(child.lived, "{armed}");
        assert!(!child.hits.is_empty(), "{armed}: the arm fired");
        let ErrorDetail::PlanInterrupted { landed, cause, .. } = envelope(&child).detail() else {
            panic!("{armed}: the apply is interrupted: {:?}", child.outcome);
        };
        assert_eq!(*landed, vec![path("n.md")], "{armed}");
        assert!(
            matches!(cause, InterruptionCause::IoFailure { .. }),
            "{armed}: {cause:?}"
        );
        assert_eq!(
            fixture.read("e.md").as_deref(),
            Some("status draft\n"),
            "{armed}: nothing after it published"
        );
        assert!(
            fixture.shadows_left().is_empty(),
            "{armed}: {:?}",
            fixture.shadows_left()
        );
    }
}

/// **A target another writer lands inside its own publication, whose folder
/// is then not synced, stops publication** as one this apply wrote would: the
/// removal the foreign writer made is found, its folder sync fails, and the
/// apply is interrupted naming it, with the later removal left unpublished.
#[test]
fn a_target_found_inside_its_publication_whose_folder_is_not_synced_interrupts_the_apply() {
    let (fixture, _) = every_position();
    let plan = fixture.plan(vec![deleting("e.md"), deleting("gone.md")]);
    let child = run_child(&fixture, &plan, "foreign@1=remove,parent-sync@1=fails");
    assert!(child.lived);
    assert!(child.hits.contains("stage=foreign"), "{}", child.hits);
    assert!(child.hits.contains("stage=parent-sync"), "{}", child.hits);
    let ErrorDetail::PlanInterrupted { landed, cause, .. } = envelope(&child).detail() else {
        panic!("the apply is interrupted: {:?}", child.outcome);
    };
    assert_eq!(*landed, vec![path("e.md")]);
    assert!(
        matches!(cause, InterruptionCause::IoFailure { .. }),
        "{cause:?}"
    );
    assert_eq!(
        fixture.read("gone.md").as_deref(),
        Some("# Gone\n"),
        "nothing after it published"
    );
}

/// **A quarantined file's delete or move cut short at any publication is
/// finished by sending it again, computing the set it recorded.** The plan
/// records no link change, since no document stands at the file on either
/// side; once part of it landed, the applier reads a target whose
/// before-bytes are gone as the plan records it — quarantined — and one
/// whose bytes it holds by those bytes, so the re-send previews as the plan
/// it is and lands.
#[test]
fn a_quarantined_files_delete_or_move_cut_short_is_finished_by_sending_it_again() {
    let mut fired = 0;
    for operations in [
        vec![deleting("q.md"), editing("d.md", "d", "dd")],
        vec![moving("q.md", "elsewhere/q.md")],
    ] {
        for armed in [
            "swap@1=ends",
            "unlink@2=ends",
            "parent-sync@1=ends",
            "parent-sync@2=ends",
        ] {
            let mut fixture = quarantined_fixture();
            let plan = fixture.plan(operations.clone());
            assert_eq!(plan.conditions, vec![], "{armed}");
            let child = run_child(&fixture, &plan, armed);
            if child.hits.is_empty() {
                assert!(child.lived, "{armed}: the child died unarmed");
            } else {
                fired += 1;
                assert!(!child.lived, "{armed}: the child outlived its end");
            }
            assert_eq!(
                fixture
                    .preview(plan.clone())
                    .map(|(previewed, _)| previewed),
                Ok(plan.clone()),
                "{armed}"
            );
            applied(fixture.apply(plan));
            assert!(!fixture.vault.join("q.md").exists(), "{armed}");
        }
    }
    // Each plan publishes twice — a rename, then the removal's unlink — and
    // syncs after each.
    assert_eq!(fired, 2 * 4, "every position was reached");
}

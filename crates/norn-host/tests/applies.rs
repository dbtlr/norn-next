//! **The production apply path, end to end**: a real vault, a real
//! attachment, the one planner and the one applier behind `Host::apply`.
//!
//! What the lifecycle suite pins against fakes is the choreography — when an
//! apply takes the claim, what it takes in, how its caller is answered. What
//! is pinned here is the join: a preview plans against the files the entry's
//! coverage stands on and writes nothing, the resolved plan it answers with
//! is applied by sending it back, the applier's publication lands on disk,
//! its changeset lands in the store the entry's reads answer from, and a read
//! after the answer sees it.
#![cfg(unix)]
#![allow(clippy::disallowed_methods)] // Harness scaffolding: this suite's own generated tree.

mod attach;

use std::path::Path;

use norn_testkit::process::Sandbox;
use norn_wire::{
    AppliedTarget, ApplyMode, ApplyParams, ApplyReport, AuthoredPlan, ChangesetOutcome,
    DocumentPath, ErrorDetail, GetParams, GetReport, Operation, OperationId, OperationKind,
    PlanDocument, ReasonCode, ResolutionTarget, TargetResult, VaultAddress,
};

/// The generated profile every case here attaches.
const PROFILE: &str = "tiny";

/// The document every case here edits, and what it holds before.
const SUBJECT: &str = "apply-subject.md";
const BEFORE: &str = "# Subject\n\nstatus draft\n";

/// A sandbox and a vault generated inside it, holding the subject document.
fn a_vault(label: &str) -> (Sandbox, attach::Vault) {
    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), label).expect("a sandbox");
    let vault = attach::Vault::generate(&sandbox.work_dir().join("attached"), PROFILE);
    std::fs::write(vault.path().join(SUBJECT), BEFORE).expect("write the subject");
    (sandbox, vault)
}

/// The plan that edits the subject from draft to final.
fn finalizing(vault: &attach::Vault) -> PlanDocument {
    PlanDocument::operations(AuthoredPlan::new(
        VaultAddress::name(vault.name().clone()),
        vec![Operation::new(OperationKind::str_replace(
            DocumentPath::new(SUBJECT).expect("a document path"),
            "draft",
            "final",
        ))],
    ))
}

/// The subject's section body as a read answers it now.
fn read_back(host: &attach::ServingHost, vault: &attach::Vault) -> String {
    let answered = host
        .get(&GetParams::new(
            VaultAddress::name(vault.name().clone()),
            ResolutionTarget::new("apply-subject#subject").expect("a target"),
        ))
        .expect("an attached vault answers a get");
    let GetReport::Section { body, .. } = answered.answer.report else {
        panic!("a heading anchor answered no section");
    };
    body.text().to_string()
}

/// **A previewed plan, sent back, applies and reads back.** The preview
/// answers the resolved plan and writes nothing; applying that plan answers
/// applied, carrying the same plan, with its one target written and its
/// changeset committed; the file holds the new text, and a read after the
/// answer reads it from the store.
#[test]
fn a_previewed_plan_sent_back_applies_and_a_read_after_it_answers_the_change() {
    let (_sandbox, vault) = a_vault("host-applies-round-trip");
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());

    let previewed = host
        .apply(ApplyParams::new(ApplyMode::Preview, finalizing(&vault)))
        .expect("a preview is answered")
        .wait()
        .expect("the plan previews");
    let ApplyReport::Previewed { plan, .. } = previewed.report else {
        panic!("a preview answered {:?}", previewed.report);
    };
    assert_eq!(plan.transitions.len(), 1);
    assert_eq!(
        std::fs::read_to_string(vault.path().join(SUBJECT)).unwrap(),
        BEFORE,
        "a preview wrote to the vault"
    );
    assert!(read_back(&host, &vault).contains("status draft"));

    let applied = host
        .apply(ApplyParams::new(
            ApplyMode::Apply,
            PlanDocument::resolved(plan.clone()),
        ))
        .expect("an apply over a ready vault is admitted")
        .wait()
        .expect("the previewed plan applies");
    let ApplyReport::Applied {
        plan: applied_plan,
        changeset,
        targets,
        ..
    } = applied.report
    else {
        panic!("an apply answered {:?}", applied.report);
    };
    assert_eq!(applied_plan, plan, "the answer carries another plan");
    assert_eq!(changeset, ChangesetOutcome::Committed);
    assert_eq!(
        targets,
        vec![AppliedTarget::new(
            DocumentPath::new(SUBJECT).unwrap(),
            TargetResult::Wrote
        )]
    );
    assert_eq!(
        std::fs::read_to_string(vault.path().join(SUBJECT)).unwrap(),
        "# Subject\n\nstatus final\n"
    );
    let body = read_back(&host, &vault);
    assert!(
        body.contains("status final"),
        "a read after the apply answered the state before it: {body:?}"
    );
}

/// **A preview whose operation does not resolve is refused, never
/// reported.** An edit whose text the subject does not hold answers
/// `vault/plan-refused` with the plan the rest resolved to — here none of
/// it — and the operation left for the caller, and nothing is written.
#[test]
fn a_preview_whose_operation_does_not_resolve_is_refused_with_the_operation_left_out() {
    let (_sandbox, vault) = a_vault("host-applies-preview-unresolved");
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let absent_text = Operation::new(OperationKind::str_replace(
        DocumentPath::new(SUBJECT).expect("a document path"),
        "no such text",
        "final",
    ));
    let plan = PlanDocument::operations(AuthoredPlan::new(
        VaultAddress::name(vault.name().clone()),
        vec![absent_text.clone()],
    ));

    let refused = host
        .apply(ApplyParams::new(ApplyMode::Preview, plan))
        .expect("a preview is answered")
        .wait()
        .expect_err("a preview of an unresolved operation was reported");
    assert_eq!(refused.code(), &ReasonCode::VaultPlanRefused);
    let ErrorDetail::PlanRefused {
        plan, unresolved, ..
    } = refused.detail()
    else {
        panic!("the refusal carries {:?}", refused.detail());
    };
    assert!(plan.transitions.is_empty());
    assert_eq!(
        unresolved
            .iter()
            .map(|left| &left.operation)
            .collect::<Vec<_>>(),
        vec![&absent_text]
    );
    assert_eq!(
        std::fs::read_to_string(vault.path().join(SUBJECT)).unwrap(),
        BEFORE
    );
}

/// **A preview of operations whose own shape is wrong is refused as
/// `request/plan-invalid`.** Two operations each requiring the other are no
/// plan at all.
#[test]
fn a_preview_of_operations_requiring_each_other_is_refused_as_an_invalid_plan() {
    let (_sandbox, vault) = a_vault("host-applies-preview-invalid");
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let first = OperationId::new("first").unwrap();
    let second = OperationId::new("second").unwrap();
    let edit = |id: &OperationId, requires: &OperationId| {
        Operation::new(OperationKind::str_replace(
            DocumentPath::new(SUBJECT).expect("a document path"),
            "draft",
            "final",
        ))
        .with_id(id.clone())
        .with_requires(vec![requires.clone()])
    };
    let plan = PlanDocument::operations(AuthoredPlan::new(
        VaultAddress::name(vault.name().clone()),
        vec![edit(&first, &second), edit(&second, &first)],
    ));

    let refused = host
        .apply(ApplyParams::new(ApplyMode::Preview, plan))
        .expect("a preview is answered")
        .wait()
        .expect_err("a preview of a cycle was reported");
    assert_eq!(refused.code(), &ReasonCode::RequestPlanInvalid);
}

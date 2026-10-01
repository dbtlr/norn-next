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
    DocumentPath, ErrorDetail, FindingKind, FolderPath, Forecast, GetParams, GetReport, Operation,
    OperationId, OperationKind, PlanDocument, PlanFault, ReasonCode, RefusedCheck,
    ResolutionTarget, ResolvedPlan, TargetResult, Transition, VaultAddress,
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

/// The plan that moves the subject to `moved.md`.
fn moving_the_subject(vault: &attach::Vault) -> PlanDocument {
    PlanDocument::operations(AuthoredPlan::new(
        VaultAddress::name(vault.name().clone()),
        vec![Operation::new(OperationKind::move_document(
            DocumentPath::new(SUBJECT).expect("a document path"),
            DocumentPath::new(MOVED).expect("a document path"),
        ))],
    ))
}

/// Where [`moving_the_subject`] moves it.
const MOVED: &str = "moved.md";

/// Preview `plan` over `vault`, answering the plan and the forecast.
fn previewed(host: &attach::ServingHost, plan: PlanDocument) -> (ResolvedPlan, Forecast) {
    let answered = host
        .apply(ApplyParams::new(ApplyMode::Preview, plan))
        .expect("a preview is answered")
        .wait()
        .expect("the plan previews");
    let ApplyReport::Previewed { plan, forecast, .. } = answered.report else {
        panic!("a preview answered {:?}", answered.report);
    };
    (plan, forecast)
}

/// **A resolved plan previews as itself: what the caller previewed is what
/// applies.** A move interrupted after its destination landed and its source
/// was removed is previewed by sending its resolved plan: the preview judges
/// it as an apply would — every target at its before- or after-state — and
/// answers the same plan, where resolving its operations afresh would find
/// no source to move. Sending it back applies it, writing nothing.
#[test]
fn an_interrupted_plan_previews_as_the_same_plan_and_then_applies() {
    let (_sandbox, vault) = a_vault("host-applies-preview-interrupted");
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let (plan, _) = previewed(&host, moving_the_subject(&vault));
    std::fs::rename(vault.path().join(SUBJECT), vault.path().join(MOVED))
        .expect("the move lands by hand");

    let (again, forecast) = previewed(&host, PlanDocument::resolved(plan.clone()));
    assert_eq!(again, plan, "the preview answered another plan");
    assert!(forecast.drifted.is_empty());

    let applied = host
        .apply(ApplyParams::new(
            ApplyMode::Apply,
            PlanDocument::resolved(plan.clone()),
        ))
        .expect("an apply over a ready vault is admitted")
        .wait()
        .expect("the previewed plan applies");
    let ApplyReport::Applied { plan: carried, .. } = applied.report else {
        panic!("an apply answered {:?}", applied.report);
    };
    assert_eq!(carried, plan);
}

/// **A resolved plan's preview refuses as its apply would.** A target that
/// holds neither of its states is drift: the preview answers
/// `vault/plan-refused` with the target marked drifted, as an apply of the
/// same plan does, and writes nothing.
#[test]
fn a_resolved_plan_whose_target_drifted_previews_as_its_apply_refuses() {
    let (_sandbox, vault) = a_vault("host-applies-preview-drifted");
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let (plan, _) = previewed(&host, finalizing(&vault));
    std::fs::write(vault.path().join(SUBJECT), "# Subject\n\nstatus other\n")
        .expect("another writer edits the subject");

    let refused = host
        .apply(ApplyParams::new(
            ApplyMode::Preview,
            PlanDocument::resolved(plan),
        ))
        .expect("a preview is answered")
        .wait()
        .expect_err("a drifted plan previewed");
    assert_eq!(refused.code(), &ReasonCode::VaultPlanRefused);
    let ErrorDetail::PlanRefused { forecast, .. } = refused.detail() else {
        panic!("the refusal carries {:?}", refused.detail());
    };
    assert_eq!(forecast.drifted, vec![DocumentPath::new(SUBJECT).unwrap()]);
    assert_eq!(
        std::fs::read_to_string(vault.path().join(SUBJECT)).unwrap(),
        "# Subject\n\nstatus other\n"
    );
}

/// **A resolved plan whose transitions are not what its operations do
/// previews as its apply answers it.** The subject's transition is carried
/// twice: the preview answers `request/plan-invalid` naming the subject, with
/// no plan to send back, and an apply of the same plan answers the same
/// fault. Neither writes.
#[test]
fn a_resolved_plan_whose_transitions_disagree_previews_as_its_apply_answers() {
    let (_sandbox, vault) = a_vault("host-applies-preview-disagreeing");
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let (mut plan, _) = previewed(&host, finalizing(&vault));
    let repeated = plan.transitions[0].clone();
    plan.transitions.push(repeated);

    let answer = |mode| {
        host.apply(ApplyParams::new(mode, PlanDocument::resolved(plan.clone())))
            .expect("the request is answered")
            .wait()
            .expect_err("a disagreeing plan is answered as invalid")
    };
    let previewed = answer(ApplyMode::Preview);
    let applied = answer(ApplyMode::Apply);
    for refused in [&previewed, &applied] {
        assert_eq!(refused.code(), &ReasonCode::RequestPlanInvalid);
        let ErrorDetail::PlanInvalid {
            fault: PlanFault::TransitionsDisagree { paths, .. },
            ..
        } = refused.detail()
        else {
            panic!("the answer carries {:?}", refused.detail());
        };
        assert_eq!(paths, &vec![DocumentPath::new(SUBJECT).unwrap()]);
    }
    assert_eq!(previewed.detail(), applied.detail());
    assert_eq!(
        std::fs::read_to_string(vault.path().join(SUBJECT)).unwrap(),
        BEFORE
    );
}

/// **A resolved plan carrying a transition for a file none of its operations
/// touches previews as its apply answers it**: `request/plan-invalid` naming
/// that file alone, judged from the plan's shape before the vault is read,
/// and neither writes.
#[test]
fn a_resolved_plan_with_a_transition_for_an_untouched_file_previews_as_plan_invalid() {
    let (_sandbox, vault) = a_vault("host-applies-preview-extra-transition");
    std::fs::write(vault.path().join(UNTOUCHED), "# Untouched\n").expect("write a bystander");
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let (mut plan, _) = previewed(&host, finalizing(&vault));
    let edit = plan.transitions[0].clone();
    plan.transitions.push(Transition::new(
        DocumentPath::new(UNTOUCHED).unwrap(),
        edit.before,
        edit.after,
    ));

    let answer = |mode| {
        host.apply(ApplyParams::new(mode, PlanDocument::resolved(plan.clone())))
            .expect("the request is answered")
            .wait()
            .expect_err("a plan with an extra transition is answered as invalid")
    };
    let previewed = answer(ApplyMode::Preview);
    let applied = answer(ApplyMode::Apply);
    let ErrorDetail::PlanInvalid {
        fault: PlanFault::TransitionsDisagree { paths, .. },
        ..
    } = previewed.detail()
    else {
        panic!("the preview answered {:?}", previewed.detail());
    };
    assert_eq!(paths, &vec![DocumentPath::new(UNTOUCHED).unwrap()]);
    assert_eq!(previewed.code(), &ReasonCode::RequestPlanInvalid);
    assert_eq!(previewed.detail(), applied.detail());
    assert_eq!(
        std::fs::read_to_string(vault.path().join(UNTOUCHED)).unwrap(),
        "# Untouched\n"
    );
    assert_eq!(
        std::fs::read_to_string(vault.path().join(SUBJECT)).unwrap(),
        BEFORE
    );
}

/// A document no plan here touches.
const UNTOUCHED: &str = "apply-untouched.md";

/// Every entry under `root`, with its inode and modification time.
fn tree_state(root: &Path) -> Vec<(std::path::PathBuf, u64, std::time::SystemTime)> {
    use std::os::unix::fs::MetadataExt;
    let mut found = Vec::new();
    let mut pending = vec![root.to_owned()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).expect("a listing") {
            let path = entry.expect("an entry").path();
            let metadata = std::fs::symlink_metadata(&path).expect("metadata");
            if metadata.is_dir() {
                pending.push(path.clone());
            }
            found.push((path, metadata.ino(), metadata.modified().expect("an mtime")));
        }
    }
    found.sort();
    found
}

/// **A preview writes nothing.** Previewing operations and previewing the
/// resolved plan they give leave every file and folder of the vault at the
/// inode and modification time it had.
#[test]
fn a_preview_leaves_every_entry_of_the_vault_as_it_was() {
    let (_sandbox, vault) = a_vault("host-applies-preview-writes-nothing");
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let before = tree_state(vault.path());

    let (plan, _) = previewed(&host, moving_the_subject(&vault));
    previewed(&host, PlanDocument::resolved(plan));

    assert_eq!(
        tree_state(vault.path()),
        before,
        "a preview wrote to the vault"
    );
}

/// A vault schema declaring the one tag `project` and reporting any other.
const TAG_SCHEMA: &str = "version: 1\ntags:\n  declared: [project]\n  undeclared: report\n";

/// **An operations preview refuses as its apply would: a composed result
/// that violates the vault schema answers `vault/plan-refused`.** Tagging the
/// subject with a tag the schema does not declare resolves, but the result
/// breaks the schema: the preview answers the same refusal, carrying the
/// same `SchemaViolation` check, as an apply of the same operations, and
/// neither writes.
#[test]
fn an_operations_preview_introducing_a_schema_violation_refuses_as_its_apply_does() {
    let (_sandbox, vault) = a_vault("host-applies-preview-schema");
    std::fs::write(vault.path().join(".norn/schema.yaml"), TAG_SCHEMA).expect("write the schema");
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let before = tree_state(vault.path());
    let tagging = PlanDocument::operations(AuthoredPlan::new(
        VaultAddress::name(vault.name().clone()),
        vec![Operation::new(OperationKind::str_replace(
            DocumentPath::new(SUBJECT).expect("a document path"),
            "draft",
            "draft #stray",
        ))],
    ));

    let answer = |mode| {
        host.apply(ApplyParams::new(mode, tagging.clone()))
            .expect("the request is answered")
            .wait()
            .expect_err("a plan violating the schema is refused")
    };
    let previewed = answer(ApplyMode::Preview);
    assert_eq!(
        tree_state(vault.path()),
        before,
        "a preview wrote to the vault"
    );
    let applied = answer(ApplyMode::Apply);
    assert_eq!(previewed.code(), &ReasonCode::VaultPlanRefused);
    let ErrorDetail::PlanRefused { checks, .. } = previewed.detail() else {
        panic!("the preview answered {:?}", previewed.detail());
    };
    assert!(
        matches!(
            checks.as_slice(),
            [RefusedCheck::SchemaViolation { violation, .. }]
                if violation.path == DocumentPath::new(SUBJECT).unwrap()
                    && violation.kind == FindingKind::UndeclaredTag
                    && violation.target.as_deref() == Some("stray")
        ),
        "the preview refused for {checks:?}"
    );
    assert_eq!(previewed.detail(), applied.detail());
    assert_eq!(
        std::fs::read_to_string(vault.path().join(SUBJECT)).unwrap(),
        BEFORE
    );
}

/// **A valid operations preview answers the plan its operations resolve to,
/// with that plan's forecast**: moving the one document out of `inbox/` into
/// `archive/` forecasts the folder it makes and the one it empties, and
/// previewing the resolved plan it answers agrees on both.
#[test]
fn a_valid_operations_preview_answers_its_resolved_plan_and_forecast() {
    let (_sandbox, vault) = a_vault("host-applies-preview-valid");
    std::fs::create_dir(vault.path().join("inbox")).expect("make the inbox");
    std::fs::write(vault.path().join("inbox/filed.md"), "# Filed\n").expect("write a document");
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let filing = PlanDocument::operations(AuthoredPlan::new(
        VaultAddress::name(vault.name().clone()),
        vec![Operation::new(OperationKind::move_document(
            DocumentPath::new("inbox/filed.md").expect("a document path"),
            DocumentPath::new("archive/filed.md").expect("a document path"),
        ))],
    ));

    let (plan, forecast) = previewed(&host, filing);
    assert_eq!(plan.operations.len(), 1);
    assert_eq!(
        forecast,
        Forecast::new(
            Vec::new(),
            vec![FolderPath::new("archive").unwrap()],
            vec![FolderPath::new("inbox").unwrap()],
        )
    );
    assert_eq!(
        previewed(&host, PlanDocument::resolved(plan.clone())),
        (plan, forecast)
    );
}

/// **A forced operations plan previews the violation it lets through and
/// applies the same, end to end.** Pushing an undeclared tag into the
/// subject's frontmatter with `force` previews with the violation in the
/// forecast's `forced`, and the previewed plan, sent back, applies with the
/// same violation in the applied report and the tag on disk.
#[test]
fn a_forced_operations_plan_previews_its_violation_and_applies_the_same() {
    let (_sandbox, vault) = a_vault("host-applies-forced");
    std::fs::write(vault.path().join(".norn/schema.yaml"), TAG_SCHEMA).expect("write the schema");
    std::fs::write(
        vault.path().join(SUBJECT),
        "---\ntags: [project]\n---\n# Subject\n",
    )
    .expect("write the subject");
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let pushing = PlanDocument::operations(
        AuthoredPlan::new(
            VaultAddress::name(vault.name().clone()),
            vec![Operation::new(OperationKind::push_frontmatter(
                norn_wire::WriteTarget::path(DocumentPath::new(SUBJECT).expect("a document path")),
                "tags",
                norn_wire::AuthoredValue::string("stray"),
            ))],
        )
        .with_force(true),
    );
    let (plan, forecast) = previewed(&host, pushing);
    assert!(plan.force);
    let [violation] = forecast.forced.as_slice() else {
        panic!("the preview lists one forced violation: {forecast:?}");
    };
    assert_eq!(violation.kind, FindingKind::UndeclaredTag);
    assert_eq!(violation.target.as_deref(), Some("stray"));
    let answered = host
        .apply(ApplyParams::new(
            ApplyMode::Apply,
            PlanDocument::resolved(plan.clone()),
        ))
        .expect("the apply is admitted")
        .wait()
        .expect("the forced plan applies");
    let ApplyReport::Applied {
        plan: applied,
        forced,
        ..
    } = answered.report
    else {
        panic!("an apply answered {:?}", answered.report);
    };
    assert_eq!(applied, plan);
    assert_eq!(forced, forecast.forced);
    assert_eq!(
        std::fs::read_to_string(vault.path().join(SUBJECT)).unwrap(),
        "---\ntags: [project, stray]\n---\n# Subject\n"
    );
}

/// The document holding a link to the subject by its stem, written beside it.
const LINKER: &str = "apply-linker.md";

/// A sandbox and a vault holding the subject and a document linking it.
fn a_linked_vault(label: &str) -> (Sandbox, attach::Vault) {
    let (sandbox, vault) = a_vault(label);
    std::fs::write(vault.path().join(LINKER), "See [[apply-subject]].\n")
        .expect("write the linker");
    (sandbox, vault)
}

/// The key of the linker's one link.
fn linkers_link() -> norn_wire::LinkKey {
    norn_wire::LinkKey::new(
        DocumentPath::new(LINKER).expect("a document path"),
        norn_wire::LinkFamily::Wikilink,
        "apply-subject",
    )
}

/// **A previewed move records the link it retargets, and the plan sent back
/// applies as it previewed.** The preview's plan records `[[apply-subject]]`
/// going from the subject's old path to its new one; the apply computes the
/// same set on its own snapshot after its intake, lands the plan, and answers
/// with the plan it previewed.
#[test]
fn a_previewed_move_records_the_link_it_retargets_and_applies_as_previewed() {
    let (_sandbox, vault) = a_linked_vault("host-applies-move-links");
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let moved = DocumentPath::new("archive/apply-subject.md").expect("a document path");
    let moving = PlanDocument::operations(AuthoredPlan::new(
        VaultAddress::name(vault.name().clone()),
        vec![Operation::new(OperationKind::move_document(
            DocumentPath::new(SUBJECT).expect("a document path"),
            moved.clone(),
        ))],
    ));

    let previewed = host
        .apply(ApplyParams::new(ApplyMode::Preview, moving))
        .expect("a preview is answered")
        .wait()
        .expect("the plan previews");
    let ApplyReport::Previewed { plan, forecast, .. } = previewed.report else {
        panic!("a preview answered {:?}", previewed.report);
    };
    assert_eq!(
        plan.conditions,
        vec![norn_wire::PlanCondition::link_resolution(
            linkers_link(),
            norn_wire::Resolves::one(DocumentPath::new(SUBJECT).unwrap()),
            norn_wire::Resolves::one(moved.clone()),
        )]
    );
    assert!(forecast.links.is_empty(), "{:?}", forecast.links);

    let applied = host
        .apply(ApplyParams::new(
            ApplyMode::Apply,
            PlanDocument::resolved(plan.clone()),
        ))
        .expect("an apply over a ready vault is admitted")
        .wait()
        .expect("the previewed plan applies");
    let ApplyReport::Applied {
        plan: applied_plan, ..
    } = applied.report
    else {
        panic!("an apply answered {:?}", applied.report);
    };
    assert_eq!(applied_plan, plan, "the apply answered another plan");
    assert!(vault.path().join(moved.as_str()).exists());
}

/// **A previewed delete of a linked document advises that it leaves the link
/// broken**, records the link going from the document to none, and the same
/// operations applied plan and land the same set.
#[test]
fn a_delete_of_a_linked_document_previews_the_link_it_leaves_broken() {
    let (_sandbox, vault) = a_linked_vault("host-applies-delete-links");
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let deleting = || {
        PlanDocument::operations(AuthoredPlan::new(
            VaultAddress::name(vault.name().clone()),
            vec![Operation::new(OperationKind::delete_document(
                DocumentPath::new(SUBJECT).expect("a document path"),
            ))],
        ))
    };

    let previewed = host
        .apply(ApplyParams::new(ApplyMode::Preview, deleting()))
        .expect("a preview is answered")
        .wait()
        .expect("the plan previews");
    let ApplyReport::Previewed { plan, forecast, .. } = previewed.report else {
        panic!("a preview answered {:?}", previewed.report);
    };
    assert_eq!(
        forecast.links,
        vec![norn_wire::LinkAdvisory::left_broken(linkers_link())]
    );
    assert_eq!(
        plan.conditions,
        vec![norn_wire::PlanCondition::link_resolution(
            linkers_link(),
            norn_wire::Resolves::one(DocumentPath::new(SUBJECT).unwrap()),
            norn_wire::Resolves::none(),
        )]
    );

    let applied = host
        .apply(ApplyParams::new(ApplyMode::Apply, deleting()))
        .expect("an apply over a ready vault is admitted")
        .wait()
        .expect("the operations apply");
    let ApplyReport::Applied {
        plan: applied_plan, ..
    } = applied.report
    else {
        panic!("an apply answered {:?}", applied.report);
    };
    assert_eq!(applied_plan, plan, "the apply planned another set");
    assert!(!vault.path().join(SUBJECT).exists());
}

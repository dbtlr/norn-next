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

use std::collections::BTreeSet;
use std::path::Path;

use norn_testkit::process::Sandbox;
use norn_wire::{
    AppliedTarget, ApplyMode, ApplyParams, ApplyReport, AuthoredPlan, ChangesetOutcome,
    DocumentPath, ErrorDetail, ErrorEnvelope, FindingKind, FolderPath, Forecast, GetParams,
    GetReport, Operation, OperationId, OperationKind, PlanDocument, PlanFault, ReasonCode,
    RefusedCheck, ResolutionTarget, ResolvedPlan, TargetResult, Transition, VaultAddress,
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

/// **A resolved plan aimed through a folder since linked out of the vault
/// previews as its apply refuses.** Four plans are previewed while `linked`
/// is a folder in the vault: a create inside it, an edit and a delete of the
/// document it holds, and a move of the subject into it. Then the folder is
/// moved out and a symbolic link to it stands in its place. Each plan's
/// preview answers exactly the refusal its apply does — the same checks,
/// each the target drifted to absent, and the same drifted forecast — never
/// the plan with a forecast of folders that stand already, as a link. The
/// folder outside holds what it held, and the subject stays where it was.
#[test]
fn a_resolved_plan_aimed_through_a_folder_since_linked_out_previews_as_its_apply_refuses() {
    let (sandbox, vault) = a_vault("host-applies-preview-linked-folder");
    let held = "# Held\n\nstatus draft\n";
    std::fs::create_dir(vault.path().join("linked")).expect("make the folder");
    std::fs::write(vault.path().join("linked/held.md"), held).expect("write the held document");
    let outside = sandbox.work_dir().join("outside");
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let document = |text: &str| DocumentPath::new(text).expect("a document path");
    let shapes = [
        (
            OperationKind::create_document(document("linked/fresh.md"), "# Fresh\n"),
            "linked/fresh.md",
        ),
        (
            OperationKind::str_replace(document("linked/held.md"), "draft", "final"),
            "linked/held.md",
        ),
        (
            OperationKind::move_document(document(SUBJECT), document("linked/moved.md")),
            "linked/moved.md",
        ),
        (
            OperationKind::delete_document(document("linked/held.md")),
            "linked/held.md",
        ),
    ];
    let plans: Vec<(ResolvedPlan, &str)> = shapes
        .into_iter()
        .map(|(kind, at)| {
            let operations = PlanDocument::operations(AuthoredPlan::new(
                VaultAddress::name(vault.name().clone()),
                vec![Operation::new(kind)],
            ));
            (previewed(&host, operations).0, at)
        })
        .collect();
    std::fs::rename(vault.path().join("linked"), &outside).expect("the folder moves out");
    std::os::unix::fs::symlink(&outside, vault.path().join("linked"))
        .expect("a link in the folder's place");

    for (plan, at) in plans {
        let answer = |mode| {
            host.apply(ApplyParams::new(mode, PlanDocument::resolved(plan.clone())))
                .expect("the request is answered")
                .wait()
                .expect_err("a plan aimed through a link is refused")
        };
        let previewed = answer(ApplyMode::Preview);
        let applied = answer(ApplyMode::Apply);
        let refusal = |answered: &ErrorEnvelope| {
            assert_eq!(answered.code(), &ReasonCode::VaultPlanRefused, "{at}");
            let ErrorDetail::PlanRefused {
                checks, forecast, ..
            } = answered.detail()
            else {
                panic!("{at}: the refusal carries {:?}", answered.detail());
            };
            (checks.clone(), forecast.drifted.clone())
        };
        let previewed = refusal(&previewed);
        assert_eq!(previewed, refusal(&applied), "{at}");
        assert_eq!(
            previewed,
            (
                vec![RefusedCheck::drifted(
                    document(at),
                    norn_wire::FileState::absent()
                )],
                vec![document(at)],
            ),
            "{at}"
        );
    }
    let mut outside_entries: Vec<String> = std::fs::read_dir(&outside)
        .expect("the folder outside lists")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    outside_entries.sort();
    assert_eq!(outside_entries, vec!["held.md".to_string()]);
    assert_eq!(
        std::fs::read_to_string(outside.join("held.md")).unwrap(),
        held
    );
    assert_eq!(
        std::fs::read_to_string(vault.path().join(SUBJECT)).unwrap(),
        BEFORE
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

/// `root` and every entry under it, with its inode and modification time.
///
/// The root is an entry too: a file made and removed again directly under it
/// leaves no entry behind, only the root's own modification time moved.
fn tree_state(root: &Path) -> Vec<(std::path::PathBuf, u64, std::time::SystemTime)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::symlink_metadata(root).expect("the root's metadata");
    let mut found = vec![(
        root.to_owned(),
        metadata.ino(),
        metadata.modified().expect("an mtime"),
    )];
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

/// A registered vault holding exactly `files`, with no schema unless one of
/// them is it.
fn an_adopted_vault(label: &str, files: &[(&str, &str)]) -> (Sandbox, attach::Vault) {
    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), label).expect("a sandbox");
    let root = sandbox.work_dir().join("attached");
    std::fs::create_dir_all(root.join("vault")).expect("the vault root");
    for (at, content) in files {
        let path = root.join("vault").join(at);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("make the parent");
        std::fs::write(path, content).expect("write a file");
    }
    (sandbox, attach::Vault::adopt(&root))
}

/// The plan a verb's preview answered, which must plan something.
fn previewed_something(answered: Result<norn_host::PendingApply, ErrorEnvelope>) {
    let answered = answered
        .expect("a preview is answered")
        .wait()
        .expect("the verb previews");
    let ApplyReport::Previewed { plan, .. } = answered.report else {
        panic!("a preview answered {:?}", answered.report);
    };
    assert!(!plan.operations.is_empty(), "the preview planned nothing");
}

/// **Every write verb's preview leaves every entry of the vault as it was.**
/// Over a registered vault with no schema, a preview of `set` by path and by
/// `where`, `edit`, `new`, `move` of a document and of a folder, a rewriting
/// `delete`, `rewrite-wikilink`, `init` and `vault migrate` — each planning
/// something, but for the migration, which the shipped ladders leave already
/// current — and over a vault declaring a creation rule, a preview of
/// `new --as` (and, behind `induced-failure`, of a migration over a ladder
/// that rewrites its schema) leave every file and folder at the inode and
/// modification time it had.
#[test]
fn every_verbs_preview_leaves_every_entry_of_the_vault_as_it_was() {
    use norn_wire::{
        AuthoredValue, DeleteParams, DocumentEdit, EditParams, FieldChange, InitParams, InitReport,
        MigrateParams, MigrateReport, MoveParams, MoveSubject, NewParams, NewSubject, Predicate,
        RewriteWikilinkParams, SetParams, ValueMap, Variables, WriteTarget,
    };

    let (_sandbox, vault) = an_adopted_vault(
        "host-applies-every-preview",
        &[
            ("subject.md", "---\nstatus: draft\n---\n# Subject\n"),
            ("holder.md", "See [[subject]] and [[gone]].\n"),
            ("gone.md", "# Gone\n"),
            ("target.md", "# Target\n"),
            ("folder/one.md", "# One\n[[two]]\n"),
            ("folder/two.md", "# Two\n"),
        ],
    );
    let host = vault.host();
    let lease = attach::attach_and_wait(&host, vault.name());
    let address = || VaultAddress::name(vault.name().clone());
    let at = |text: &str| DocumentPath::new(text).expect("a document path");
    let folder = |text: &str| FolderPath::new(text).expect("a folder path");
    let named = |text: &str| ResolutionTarget::new(text).expect("a target");
    let preview = ApplyMode::Preview;
    let shelving = || vec![FieldChange::set("status", AuthoredValue::string("shelved"))];
    let before = tree_state(vault.path());

    previewed_something(host.set(SetParams::new(
        address(),
        preview,
        WriteTarget::path(at("subject.md")),
        shelving(),
    )));
    previewed_something(host.set(SetParams::new(
        address(),
        preview,
        WriteTarget::matching([Predicate::equal_to("status", "draft")]),
        shelving(),
    )));
    previewed_something(host.edit(EditParams::new(
        address(),
        preview,
        at("subject.md"),
        vec![DocumentEdit::replace_body("# Rewritten\n")],
    )));
    previewed_something(host.new_document(NewParams::new(
        address(),
        preview,
        at("fresh/new.md"),
        "# New\n",
    )));
    previewed_something(host.move_path(MoveParams::new(
        address(),
        preview,
        MoveSubject::document(at("subject.md"), at("moved/renamed.md")),
    )));
    previewed_something(host.move_path(MoveParams::new(
        address(),
        preview,
        MoveSubject::folder(folder("folder"), folder("moved/folder")),
    )));
    previewed_something(host.delete(
        DeleteParams::new(address(), preview, at("gone.md")).rewriting_to(named("target")),
    ));
    previewed_something(host.rewrite_wikilink(RewriteWikilinkParams::new(
        address(),
        preview,
        named("subject"),
        named("target"),
    )));
    let init = host
        .init(&InitParams::new(address(), preview))
        .expect("an init preview answers");
    assert!(
        matches!(init, InitReport::Scaffolded { .. }),
        "init planned no starter: {init:?}"
    );
    assert_eq!(
        host.vault_migrate(&MigrateParams::new(address(), preview))
            .expect("a migrate preview answers"),
        MigrateReport::already_current()
    );
    assert_eq!(
        tree_state(vault.path()),
        before,
        "a preview wrote to the vault"
    );
    // The real-watcher lease is the host's and is not reentrant, so the
    // first host lets go before the second is built.
    drop(lease);
    drop(host);

    let (_ruled_sandbox, ruled) = an_adopted_vault(
        "host-applies-every-preview-ruled",
        &[
            (
                ".norn/schema.yaml",
                "version: 1\ncreatable:\n  task:\n    target: \"tasks/{{seq}}.md\"\n",
            ),
            ("a.md", "# A\n"),
        ],
    );
    let ruled_host = ruled.host();
    let _ruled_lease = attach::attach_and_wait(&ruled_host, ruled.name());
    let ruled_address = || VaultAddress::name(ruled.name().clone());
    let before = tree_state(ruled.path());

    previewed_something(ruled_host.new_document(NewParams::for_subject(
        ruled_address(),
        preview,
        NewSubject::by_rule(
            "task",
            Variables::default(),
            ValueMap::default(),
            Some("Body.\n".to_string()),
        ),
    )));
    #[cfg(feature = "induced-failure")]
    {
        use norn_config::migration::{Format, Ladder, Step};
        let schema = Ladder {
            format: Format::Yaml,
            current: 2,
            version_of: |text| Ok(if text.contains("# migrated\n") { 2 } else { 1 }),
            steps: vec![Step {
                from: 1,
                rewrite: |text| format!("{text}# migrated\n"),
            }],
        };
        let migrated = ruled_host
            .vault_migrate_with_ladders(
                &MigrateParams::new(ruled_address(), preview),
                &schema,
                &Ladder::config(),
            )
            .expect("a migrate preview answers");
        assert!(
            matches!(migrated, MigrateReport::Migrated { .. }),
            "the migration planned no rewrite: {migrated:?}"
        );
    }
    assert_eq!(
        tree_state(ruled.path()),
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

/// A vault schema closing the list field `status` over `[todo, done]`, with
/// `complete` a synonym of `done`.
const STATUS_SCHEMA: &str = "version: 1
fields:
  status: { type: text, shape: list }
rules:
  states: { one_of: { status: { values: [todo, done], synonyms: { complete: done } } } }
";

/// **A write refuses an element violation it introduces and not one standing,
/// end to end** (ADR 0037): rewriting the subject's `status: [complete,
/// bogus]` to `[done, bogus]` writes the field and leaves `bogus` holding the
/// identity it held, so it applies; a write adding `wrong` beside it is
/// refused, preview and apply alike, on `wrong` alone.
#[test]
fn a_write_refuses_an_element_violation_it_introduces_and_not_one_standing() {
    let (_sandbox, vault) = a_vault("host-applies-element-gate");
    std::fs::write(vault.path().join(".norn/schema.yaml"), STATUS_SCHEMA)
        .expect("write the schema");
    std::fs::write(
        vault.path().join(SUBJECT),
        "---\nstatus: [complete, bogus]\n---\n# Subject\n",
    )
    .expect("write the subject");
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let setting = |values: &[&str]| {
        PlanDocument::operations(AuthoredPlan::new(
            VaultAddress::name(vault.name().clone()),
            vec![Operation::new(OperationKind::set_frontmatter(
                norn_wire::WriteTarget::path(DocumentPath::new(SUBJECT).expect("a document path")),
                "status",
                norn_wire::AuthoredValue::List(
                    values
                        .iter()
                        .map(|value| norn_wire::AuthoredValue::string(*value))
                        .collect(),
                ),
            ))],
        ))
    };

    host.apply(ApplyParams::new(
        ApplyMode::Apply,
        setting(&["done", "bogus"]),
    ))
    .expect("the apply is admitted")
    .wait()
    .expect("a write leaving a standing violation as it stood applies");
    let written = "---\nstatus: [done, bogus]\n---\n# Subject\n";
    assert_eq!(
        std::fs::read_to_string(vault.path().join(SUBJECT)).unwrap(),
        written
    );

    let answer = |mode| {
        host.apply(ApplyParams::new(mode, setting(&["done", "bogus", "wrong"])))
            .expect("the request is answered")
            .wait()
            .expect_err("a write introducing a violation is refused")
    };
    let previewed = answer(ApplyMode::Preview);
    let applied = answer(ApplyMode::Apply);
    assert_eq!(previewed.detail(), applied.detail());
    let ErrorDetail::PlanRefused { checks, .. } = previewed.detail() else {
        panic!("the preview answered {:?}", previewed.detail());
    };
    let refused: Vec<(FindingKind, Option<&str>, Option<&str>)> = checks
        .iter()
        .map(|check| match check {
            RefusedCheck::SchemaViolation { violation, .. } => (
                violation.kind,
                violation.target.as_deref(),
                violation.value.as_ref().map(|head| head.text()),
            ),
            other => panic!("a schema check: {other:?}"),
        })
        .collect();
    assert_eq!(
        refused,
        [(FindingKind::NotOneOf, Some("status"), Some("wrong"))]
    );
    assert_eq!(
        std::fs::read_to_string(vault.path().join(SUBJECT)).unwrap(),
        written
    );
}

/// A vault schema whose rules a write can breach in every per-element and
/// whole-document way: a closed list field with a length limit, a field an
/// area requires and a field another area forbids.
const AGREEMENT_SCHEMA: &str = "version: 1
fields:
  status: { type: text, shape: list }
rules:
  states: { one_of: { status: { values: [todo, done], synonyms: { complete: done } } }, max_length: { status: 4 } }
  tasks: { match: { path: 'tasks/**' }, required: { status: } }
  notes: { match: { path: 'notes/**' }, forbidden: { owner: } }
";

/// A schema finding by its identity: kind, field and offending value.
type Identity = (&'static str, Option<String>, Option<String>);

/// The identities of the findings standing at `path`, every page read.
fn identities_at(
    host: &attach::ServingHost,
    vault: &attach::Vault,
    path: &str,
) -> BTreeSet<Identity> {
    let mut identities = BTreeSet::new();
    let mut after = None;
    loop {
        let mut params = norn_wire::ValidateParams::new(VaultAddress::name(vault.name().clone()));
        if let Some(cursor) = after.take() {
            params = params.with_after(cursor);
        }
        let answered = host.validate(&params).expect("a served vault answers a validate");
        let norn_wire::ValidateReport::Findings { page, .. } = answered.answer.report else {
            panic!("a validate answered {:?}", answered.answer.report);
        };
        identities.extend(
            page.rows
                .iter()
                .filter(|row| row.path.as_str() == path)
                .map(|row| {
                    (
                        row.kind.as_str(),
                        row.target.clone(),
                        row.value.as_ref().map(|head| head.text().to_string()),
                    )
                }),
        );
        match page.next {
            Some(next) => after = Some(next),
            None => return identities,
        }
    }
}

/// **The write gate and the validator agree** (ADR 0037): a write previews
/// as refused exactly when forcing it leaves its document under a finding it
/// did not stand under, and the refusal names exactly those findings — so a
/// write the gate admits is never flagged by the validator, across setting,
/// pushing and popping a list's elements, moving into a required area, and
/// creating or removing what an area requires or forbids.
#[test]
fn a_write_the_gate_admits_leaves_no_finding_the_validator_did_not_already_report() {
    let (_sandbox, vault) = a_vault("host-applies-gate-agreement");
    std::fs::write(vault.path().join(".norn/schema.yaml"), AGREEMENT_SCHEMA)
        .expect("write the schema");
    std::fs::write(
        vault.path().join(SUBJECT),
        "---\nstatus: [complete, bogus]\n---\n# Subject\n",
    )
    .expect("write the subject");
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let path = |path: &str| DocumentPath::new(path).expect("a document path");
    let target = |at: &str| norn_wire::WriteTarget::path(path(at));
    let text = norn_wire::AuthoredValue::string;
    let moved = "tasks/apply-subject.md";
    let writes: Vec<(&str, &str, &str, OperationKind, bool)> = vec![
        (
            "a set leaving a standing element",
            SUBJECT,
            SUBJECT,
            OperationKind::set_frontmatter(
                target(SUBJECT),
                "status",
                norn_wire::AuthoredValue::List(vec![text("done"), text("bogus")]),
            ),
            false,
        ),
        (
            "a push of an element outside the set and past the limit",
            SUBJECT,
            SUBJECT,
            OperationKind::push_frontmatter(target(SUBJECT), "status", text("wrong")),
            true,
        ),
        (
            "a pop of that element",
            SUBJECT,
            SUBJECT,
            OperationKind::pop_frontmatter(target(SUBJECT), "status", text("wrong")),
            false,
        ),
        (
            "a move into the area requiring the field it holds",
            SUBJECT,
            moved,
            OperationKind::move_document(path(SUBJECT), path(moved)),
            false,
        ),
        (
            "a removal of the field the area requires",
            moved,
            moved,
            OperationKind::remove_frontmatter(target(moved), "status"),
            true,
        ),
        (
            "a create lacking the field its area requires",
            "tasks/bare.md",
            "tasks/bare.md",
            OperationKind::create_document(path("tasks/bare.md"), "# Bare\n"),
            true,
        ),
        (
            "a create holding the field its area forbids",
            "notes/owned.md",
            "notes/owned.md",
            OperationKind::create_document(
                path("notes/owned.md"),
                "---\nowner: me\n---\n# Owned\n",
            ),
            true,
        ),
    ];

    for (write, from, to, operation, refuses) in writes {
        let plan = |force| {
            PlanDocument::operations(
                AuthoredPlan::new(
                    VaultAddress::name(vault.name().clone()),
                    vec![Operation::new(operation.clone())],
                )
                .with_force(force),
            )
        };
        let before = identities_at(&host, &vault, from);
        let refused: BTreeSet<Identity> =
            match host.apply(ApplyParams::new(ApplyMode::Preview, plan(false)))
                .expect("the request is answered")
                .wait()
            {
                Ok(_) => BTreeSet::new(),
                Err(error) => {
                    let ErrorDetail::PlanRefused { checks, .. } = error.detail() else {
                        panic!("{write}: the preview answered {:?}", error.detail());
                    };
                    checks
                        .iter()
                        .map(|check| match check {
                            RefusedCheck::SchemaViolation { violation, .. } => (
                                violation.kind.as_str(),
                                violation.target.clone(),
                                violation.value.as_ref().map(|head| head.text().to_string()),
                            ),
                            other => panic!("{write}: a schema check: {other:?}"),
                        })
                        .collect()
                }
            };
        host.apply(ApplyParams::new(ApplyMode::Apply, plan(true)))
            .expect("the apply is admitted")
            .wait()
            .unwrap_or_else(|error| panic!("{write}: the forced write applies: {error:?}"));
        let after = identities_at(&host, &vault, to);
        let introduced: BTreeSet<Identity> = after.difference(&before).cloned().collect();

        assert_eq!(refused, introduced, "{write}");
        assert_eq!(!refused.is_empty(), refuses, "{write}");
    }
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

/// **A previewed move records the link whose resolution it moves, and the
/// plan sent back applies as it previewed.** The preview's plan records
/// `[[apply-subject]]` going from the subject's old path to its new one, and
/// advises nothing: a link naming one document before and after is neither
/// broken, ambiguous nor retargeted. The apply computes the same set on its
/// own snapshot after its intake, lands the plan, and answers with the plan
/// it previewed.
#[test]
fn a_previewed_move_records_the_link_it_moves_and_applies_as_previewed() {
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

/// **A previewed delete leaving the links naming its document broken
/// advises that it leaves the link broken**, records the link going from the
/// document to none, and the same operations applied plan and land the same
/// set.
#[test]
fn a_delete_of_a_linked_document_previews_the_link_it_leaves_broken() {
    let (_sandbox, vault) = a_linked_vault("host-applies-delete-links");
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let deleting = || {
        PlanDocument::operations(AuthoredPlan::new(
            VaultAddress::name(vault.name().clone()),
            vec![Operation::new(
                OperationKind::delete_document_breaking_links(
                    DocumentPath::new(SUBJECT).expect("a document path"),
                ),
            )],
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

/// **A move among an ambiguous link's members advises that its cascade
/// skips the link, which the plan records no entry for.** `[[apply-twin]]`
/// names the two documents of that stem before the move and two after, one
/// of them at the moved path: which it names is not known, so the cascade
/// leaves it as written and the forecast says why; several on both sides is
/// no change the set records. The plan applies as previewed, and the link it
/// skipped stands as written.
#[test]
fn a_move_among_an_ambiguous_links_members_advises_its_cascade_skips_the_link() {
    let (_sandbox, vault) = a_vault("host-applies-retargeted");
    for (at, body) in [
        ("twin-a/apply-twin.md", "a twin\n"),
        ("twin-b/apply-twin.md", "b twin\n"),
        ("apply-twin-linker.md", "See [[apply-twin]].\n"),
    ] {
        let at = vault.path().join(at);
        std::fs::create_dir_all(at.parent().expect("a parent")).expect("make the folder");
        std::fs::write(at, body).expect("write the document");
    }
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let moving = PlanDocument::operations(AuthoredPlan::new(
        VaultAddress::name(vault.name().clone()),
        vec![Operation::new(OperationKind::move_document(
            DocumentPath::new("twin-a/apply-twin.md").expect("a document path"),
            DocumentPath::new("twin-c/apply-twin.md").expect("a document path"),
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
    assert_eq!(plan.conditions, vec![]);
    assert_eq!(
        forecast.links,
        vec![norn_wire::LinkAdvisory::skipped_ambiguous(
            norn_wire::LinkKey::new(
                DocumentPath::new("apply-twin-linker.md").expect("a document path"),
                norn_wire::LinkFamily::Wikilink,
                "apply-twin",
            )
        )]
    );

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
    assert_eq!(
        std::fs::read_to_string(vault.path().join("apply-twin-linker.md"))
            .expect("the linker reads"),
        "See [[apply-twin]].\n",
        "the apply rewrote the link its forecast skipped"
    );
}

/// Bytes that do not decode as a vault document: the host quarantines a file
/// holding them, derives no document from it, and reads every link naming it
/// broken.
const UNDECODABLE: &[u8] = b"\xff\xfe not utf-8\n";

/// A sandbox and a vault holding a quarantined `apply-quarantined.md` and a
/// document linking it by its stem.
fn a_vault_linking_a_quarantined_file(label: &str) -> (Sandbox, attach::Vault) {
    let (sandbox, vault) = a_vault(label);
    std::fs::write(vault.path().join(LINKER), "See [[apply-quarantined]].\n")
        .expect("write the linker");
    std::fs::write(vault.path().join("apply-quarantined.md"), UNDECODABLE)
        .expect("write the undecodable file");
    (sandbox, vault)
}

/// `operations` previewed over `vault`, then sent back resolved and
/// applied: the plan the preview answered and its forecast's link
/// advisories, after asserting the apply answered that same plan.
fn previewed_then_applied(
    host: &attach::ServingHost,
    vault: &attach::Vault,
    operations: Vec<Operation>,
) -> (norn_wire::ResolvedPlan, Vec<norn_wire::LinkAdvisory>) {
    let previewed = host
        .apply(ApplyParams::new(
            ApplyMode::Preview,
            PlanDocument::operations(AuthoredPlan::new(
                VaultAddress::name(vault.name().clone()),
                operations,
            )),
        ))
        .expect("a preview is answered")
        .wait()
        .expect("the plan previews");
    let ApplyReport::Previewed { plan, forecast, .. } = previewed.report else {
        panic!("a preview answered {:?}", previewed.report);
    };
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
    (plan, forecast.links)
}

/// **Deleting a quarantined document records no link change.** The host
/// derives no document from `apply-quarantined.md`, so `[[apply-quarantined]]`
/// reads broken before the plan as after it: the plan records no entry, the
/// forecast advises nothing, and the apply computes the same empty set.
#[test]
fn deleting_a_quarantined_document_records_no_link_change() {
    let (_sandbox, vault) = a_vault_linking_a_quarantined_file("host-applies-quarantine-delete");
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());

    let (plan, advised) = previewed_then_applied(
        &host,
        &vault,
        vec![Operation::new(OperationKind::delete_document(
            DocumentPath::new("apply-quarantined.md").expect("a document path"),
        ))],
    );
    assert!(
        !plan
            .conditions
            .iter()
            .any(|condition| matches!(condition, norn_wire::PlanCondition::LinkResolution { .. })),
        "{:?}",
        plan.conditions
    );
    assert!(advised.is_empty(), "{advised:?}");
    assert!(!plan.transitions[0].before.is_document());
    assert!(!vault.path().join("apply-quarantined.md").exists());
}

/// **An apply whose plan read no links refuses with a fresh plan that reads
/// them.** Moving a quarantined file changes no document's presence and
/// deletes none, so the previewed plan carries no link condition and its
/// apply needs no read handle before its check; another writer then repairs
/// the file's bytes, so the check refuses the drift and the operations,
/// resolved afresh, move a document `[[apply-quarantined]]` resolves to. The
/// apply answers `vault/plan-refused` with that fresh plan, carrying the
/// link condition it now needs, and writes nothing.
#[test]
fn an_apply_whose_plan_read_no_links_refuses_with_a_fresh_plan_reading_them() {
    let (_sandbox, vault) = a_vault_linking_a_quarantined_file("host-applies-quarantine-repaired");
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let previewed = host
        .apply(ApplyParams::new(
            ApplyMode::Preview,
            PlanDocument::operations(AuthoredPlan::new(
                VaultAddress::name(vault.name().clone()),
                vec![Operation::new(OperationKind::move_document(
                    DocumentPath::new("apply-quarantined.md").expect("a document path"),
                    DocumentPath::new("elsewhere/apply-quarantined.md").expect("a document path"),
                ))],
            )),
        ))
        .expect("a preview is answered")
        .wait()
        .expect("the plan previews");
    let ApplyReport::Previewed { plan, .. } = previewed.report else {
        panic!("a preview answered {:?}", previewed.report);
    };
    let reads_links = |plan: &ResolvedPlan| {
        plan.conditions
            .iter()
            .any(|condition| matches!(condition, norn_wire::PlanCondition::LinkResolution { .. }))
    };
    assert!(!reads_links(&plan), "{:?}", plan.conditions);
    std::fs::write(vault.path().join("apply-quarantined.md"), "# Repaired\n")
        .expect("another writer repairs the quarantined file");

    let refused = host
        .apply(ApplyParams::new(
            ApplyMode::Apply,
            PlanDocument::resolved(plan),
        ))
        .expect("an apply over a ready vault is admitted")
        .wait()
        .expect_err("a drifted plan applied");
    assert_eq!(refused.code(), &ReasonCode::VaultPlanRefused, "{refused:?}");
    let ErrorDetail::PlanRefused { plan: fresh, .. } = refused.detail() else {
        panic!("the refusal carries {:?}", refused.detail());
    };
    assert!(reads_links(fresh), "{:?}", fresh.conditions);
    assert_eq!(
        std::fs::read_to_string(vault.path().join("apply-quarantined.md")).unwrap(),
        "# Repaired\n"
    );
    assert!(!vault.path().join("elsewhere/apply-quarantined.md").exists());
}

/// **Moving a quarantined document records no link change**: no document
/// stands at its source before the plan or at its destination after it.
#[test]
fn moving_a_quarantined_document_records_no_link_change() {
    let (_sandbox, vault) = a_vault_linking_a_quarantined_file("host-applies-quarantine-move");
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());

    let (plan, advised) = previewed_then_applied(
        &host,
        &vault,
        vec![Operation::new(OperationKind::move_document(
            DocumentPath::new("apply-quarantined.md").expect("a document path"),
            DocumentPath::new("elsewhere/apply-quarantined.md").expect("a document path"),
        ))],
    );
    assert!(
        !plan
            .conditions
            .iter()
            .any(|condition| matches!(condition, norn_wire::PlanCondition::LinkResolution { .. })),
        "{:?}",
        plan.conditions
    );
    assert!(advised.is_empty(), "{advised:?}");
    assert!(vault.path().join("elsewhere/apply-quarantined.md").exists());
}

/// A plan writing the vault schema whole, guarded by the content its author
/// observed there.
fn rewriting_the_schema(vault: &attach::Vault, observed: &[u8], content: &str) -> PlanDocument {
    let schema = DocumentPath::new(".norn/schema.yaml").expect("a vault path");
    let hash = norn_wire::ContentHash::new(format!(
        "sha256:{}",
        norn_fs::ContentHash::of(observed).to_hex()
    ))
    .expect("a content hash");
    PlanDocument::operations(AuthoredPlan::new(
        VaultAddress::name(vault.name().clone()),
        vec![
            Operation::new(OperationKind::write_control_file(
                norn_wire::ControlFile::Schema,
                content,
            ))
            .with_conditions(vec![norn_wire::AuthorCondition::content_hash(schema, hash)]),
        ],
    ))
}

/// **A control-file write replacing the schema its author observed lands on
/// a served vault; one guarded by content the schema no longer holds is
/// refused.** The schema's path is one the entry's walk does not enter, yet
/// the write plans, applies and lands there, its changeset committed with no
/// document derived for it; the same write sent again, guarded by the bytes
/// it replaced, is left unresolved and writes nothing.
#[test]
fn a_schema_replace_guarded_by_its_before_state_lands_and_a_stale_guard_refuses() {
    let (_sandbox, vault) = a_vault("host-applies-control-file");
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let migrated = "version: 1\n# migrated\n";

    let applied = host
        .apply(ApplyParams::new(
            ApplyMode::Apply,
            rewriting_the_schema(&vault, attach::SCHEMA, migrated),
        ))
        .expect("an apply over a ready vault is admitted")
        .wait()
        .expect("the schema write applies");
    let ApplyReport::Applied {
        changeset, targets, ..
    } = applied.report
    else {
        panic!("an apply answered {:?}", applied.report);
    };
    assert_eq!(changeset, ChangesetOutcome::Committed);
    assert_eq!(
        targets,
        vec![AppliedTarget::new(
            DocumentPath::new(".norn/schema.yaml").unwrap(),
            TargetResult::Wrote
        )]
    );
    assert_eq!(
        std::fs::read_to_string(vault.path().join(".norn/schema.yaml")).unwrap(),
        migrated
    );

    let refused = host
        .apply(ApplyParams::new(
            ApplyMode::Apply,
            rewriting_the_schema(&vault, attach::SCHEMA, "version: 1\n# again\n"),
        ))
        .expect("an apply over a ready vault is admitted")
        .wait()
        .expect_err("a stale guard is refused");
    assert_eq!(refused.code(), &ReasonCode::VaultPlanRefused);
    let ErrorDetail::PlanRefused { unresolved, .. } = refused.detail() else {
        panic!("refused with {:?}", refused.detail());
    };
    assert_eq!(unresolved.len(), 1, "{unresolved:?}");
    assert_eq!(
        std::fs::read_to_string(vault.path().join(".norn/schema.yaml")).unwrap(),
        migrated
    );
}

/// **A schema write lands where the registration reads its schema from a
/// source** (ADR 0034): a `write_control_file` of the schema, sent through
/// `apply`, is written at the source outside the vault, and the default
/// `.norn/schema.yaml`, which that vault never reads, is left as it stands.
#[test]
fn a_schema_write_lands_where_the_registration_reads_a_schema_source() {
    let (sandbox, vault) = a_vault("host-applies-schema-elsewhere");
    let source = sandbox.work_dir().join("shared/schema.yaml");
    std::fs::create_dir_all(source.parent().expect("a parent")).expect("the folder");
    std::fs::write(&source, "version: 1\n").expect("the schema");
    let host = vault.host_reading_schema_from(
        norn_config::registry::SchemaSource::new(&source).expect("a schema source"),
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let default = std::fs::read(vault.path().join(".norn/schema.yaml")).expect("a default");

    host.apply(ApplyParams::new(
        ApplyMode::Apply,
        PlanDocument::operations(AuthoredPlan::new(
            VaultAddress::name(vault.name().clone()),
            vec![Operation::new(OperationKind::write_control_file(
                norn_wire::ControlFile::Schema,
                "version: 1\n# mine\n",
            ))],
        )),
    ))
    .expect("an apply over a ready vault is admitted")
    .wait()
    .expect("a schema write at the source lands");
    assert_eq!(
        std::fs::read_to_string(&source).expect("the source stands"),
        "version: 1\n# mine\n"
    );
    assert_eq!(
        std::fs::read(vault.path().join(".norn/schema.yaml")).expect("the default stands"),
        default
    );
}

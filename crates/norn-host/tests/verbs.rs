//! **The document-local write verbs, end to end**: `Host::set`,
//! `Host::edit` and `Host::new_document` over a real vault and a real
//! attachment.
//!
//! Each verb compiles its request to operations and enters the one `apply`
//! path, so what is pinned here is what a caller of each verb sees: a
//! preview that writes nothing and answers the plan its apply lands, a
//! `where` target expanded into exactly the documents the store matches, and
//! the refusals the planner and the applier answer for a write.
#![cfg(unix)]
#![allow(clippy::disallowed_methods)] // Harness scaffolding: this suite's own generated tree.

mod attach;

use std::path::Path;
use std::sync::Barrier;
use std::sync::atomic::{AtomicBool, Ordering};

use norn_testkit::process::Sandbox;
use norn_wire::{
    AppliedTarget, ApplyMode, ApplyParams, ApplyReport, AuthorCondition, AuthoredValue,
    ChangesetOutcome, DocumentEdit, DocumentPath, EditParams, ErrorDetail, ErrorEnvelope,
    ExpectedField, FieldChange, FindParams, FindingKind, NewParams, OperationKind, PlanDocument,
    Predicate, ReasonCode, RefusedCheck, ResolvedPlan, SetParams, TargetResult, UnresolvedReason,
    VaultAddress, WriteTarget,
};

/// The generated profile every case here attaches.
const PROFILE: &str = "tiny";

/// A sandbox and a vault generated inside it, holding `files` beside the
/// profile's own documents.
fn a_vault(label: &str, files: &[(&str, &str)]) -> (Sandbox, attach::Vault) {
    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), label).expect("a sandbox");
    let vault = attach::Vault::generate(&sandbox.work_dir().join("attached"), PROFILE);
    for (at, content) in files {
        let path = vault.path().join(at);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("make the parent");
        std::fs::write(path, content).expect("write a document");
    }
    (sandbox, vault)
}

fn address(vault: &attach::Vault) -> VaultAddress {
    VaultAddress::name(vault.name().clone())
}

fn path(text: &str) -> DocumentPath {
    DocumentPath::new(text).expect("a document path")
}

fn read(vault: &attach::Vault, at: &str) -> String {
    std::fs::read_to_string(vault.path().join(at)).expect("read a document")
}

/// The plan and report a previewed verb answered.
fn previewed(answered: Result<norn_host::PendingApply, ErrorEnvelope>) -> ResolvedPlan {
    let answered = answered
        .expect("a preview is answered")
        .wait()
        .expect("the verb previews");
    let ApplyReport::Previewed { plan, .. } = answered.report else {
        panic!("a preview answered {:?}", answered.report);
    };
    plan
}

/// The plan, changeset and targets an applied verb answered.
fn applied(
    answered: Result<norn_host::PendingApply, ErrorEnvelope>,
) -> (ResolvedPlan, ChangesetOutcome, Vec<AppliedTarget>) {
    let answered = answered
        .expect("an apply is admitted")
        .wait()
        .expect("the verb applies");
    let ApplyReport::Applied {
        plan,
        changeset,
        targets,
        ..
    } = answered.report
    else {
        panic!("an apply answered {:?}", answered.report);
    };
    (plan, changeset, targets)
}

/// The refusal a verb answered.
fn refused(answered: Result<norn_host::PendingApply, ErrorEnvelope>) -> ErrorEnvelope {
    answered
        .expect("the verb is answered")
        .wait()
        .expect_err("the verb was refused")
}

fn wrote(paths: &[&str]) -> Vec<AppliedTarget> {
    paths
        .iter()
        .map(|at| AppliedTarget::new(path(at), TargetResult::Wrote))
        .collect()
}

/// The paths a find for `predicates` answers now, in path order.
fn found(
    host: &attach::ServingHost,
    vault: &attach::Vault,
    predicates: Vec<Predicate>,
) -> Vec<String> {
    host.find(
        &FindParams::new(address(vault))
            .with_predicates(predicates)
            .with_limit(1000),
    )
    .expect("a find answers")
    .answer
    .report
    .rows
    .into_iter()
    .map(|row| row.path.as_str().to_string())
    .collect()
}

/// **A set previews the field it writes and applies the same plan.** The
/// preview writes nothing; the apply lands the previewed plan, the field is
/// on disk, and a read after the answer reads it.
#[test]
fn a_set_previews_then_applies_the_plan_it_previewed() {
    let subject = "---\nstatus: draft\n---\n# Subject\n";
    let (_sandbox, vault) = a_vault("host-verbs-set", &[("subject.md", subject)]);
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let setting = |mode| {
        SetParams::new(
            address(&vault),
            mode,
            WriteTarget::path(path("subject.md")),
            vec![FieldChange::set("status", AuthoredValue::string("shelved"))],
        )
    };

    let plan = previewed(host.set(setting(ApplyMode::Preview)));
    assert_eq!(read(&vault, "subject.md"), subject, "a preview wrote");
    let (landed, changeset, targets) = applied(host.set(setting(ApplyMode::Apply)));
    assert_eq!(
        landed, plan,
        "the apply landed another plan than it previewed"
    );
    assert_eq!(changeset, ChangesetOutcome::Committed);
    assert_eq!(targets, wrote(&["subject.md"]));
    assert_eq!(
        read(&vault, "subject.md"),
        "---\nstatus: shelved\n---\n# Subject\n"
    );
    assert_eq!(
        found(
            &host,
            &vault,
            vec![Predicate::equal_to("status", "shelved")]
        ),
        vec!["subject.md".to_string()]
    );
}

/// **A set with a `where` target writes exactly the documents it matches,
/// in one changeset, and a read meanwhile sees the vault before or after
/// it.** Three of five documents carry `wave: flip`; the preview names the
/// three by path, the apply writes those three and no other, and every find
/// answered while it ran saw none or all of them written.
#[test]
fn a_set_where_writes_exactly_the_matched_documents_in_one_changeset() {
    let flip = "---\nwave: flip\n---\n";
    let keep = "---\nwave: keep\n---\n";
    let (_sandbox, vault) = a_vault(
        "host-verbs-set-where",
        &[
            ("wave/a.md", flip),
            ("wave/b.md", keep),
            ("wave/c.md", flip),
            ("wave/d.md", keep),
            ("wave/e.md", flip),
        ],
    );
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let flipping = |mode| {
        SetParams::new(
            address(&vault),
            mode,
            WriteTarget::matching([Predicate::equal_to("wave", "flip")]),
            vec![FieldChange::set("wave", AuthoredValue::string("flipped"))],
        )
    };

    let plan = previewed(host.set(flipping(ApplyMode::Preview)));
    let targets: Vec<&str> = plan.transitions.iter().map(|t| t.path.as_str()).collect();
    assert_eq!(targets, vec!["wave/a.md", "wave/c.md", "wave/e.md"]);
    assert!(
        plan.operations.iter().all(|operation| operation
            .kind
            .target()
            .is_some_and(|t| t.as_path().is_some())),
        "a resolved plan carries a `where` target: {:?}",
        plan.operations
    );

    let applying = AtomicBool::new(true);
    let reading = Barrier::new(2);
    let (before, seen) = std::thread::scope(|scope| {
        let reader = scope.spawn(|| {
            let flipped =
                || found(&host, &vault, vec![Predicate::equal_to("wave", "flipped")]).len();
            let before = flipped();
            reading.wait();
            // At least one read after the barrier, so the apply is racing a
            // read however quickly it lands; the rest sample until it ends.
            let mut seen = vec![flipped()];
            while applying.load(Ordering::SeqCst) {
                seen.push(flipped());
            }
            (before, seen)
        });
        reading.wait();
        let landed = applied(host.apply(ApplyParams::new(
            ApplyMode::Apply,
            PlanDocument::resolved(plan.clone()),
        )));
        applying.store(false, Ordering::SeqCst);
        assert_eq!(landed.0, plan);
        assert_eq!(landed.1, ChangesetOutcome::Committed);
        assert_eq!(landed.2, wrote(&["wave/a.md", "wave/c.md", "wave/e.md"]));
        reader.join().expect("the reader finished")
    });
    assert_eq!(before, 0, "a document matched before the apply ran");
    assert!(
        seen.iter().all(|count| *count == 0 || *count == 3),
        "a read saw part of the changeset: {seen:?}"
    );
    for at in ["wave/a.md", "wave/c.md", "wave/e.md"] {
        assert_eq!(read(&vault, at), "---\nwave: flipped\n---\n");
    }
    for at in ["wave/b.md", "wave/d.md"] {
        assert_eq!(read(&vault, at), keep);
    }
    assert_eq!(
        found(&host, &vault, vec![Predicate::equal_to("wave", "flipped")]),
        vec!["wave/a.md", "wave/c.md", "wave/e.md"]
    );
}

/// **A set whose `where` matches no document is refused, its operation
/// unresolved naming the predicate**, as its preview is, and nothing is
/// written.
#[test]
fn a_set_where_matching_no_document_is_refused_unresolved() {
    let (_sandbox, vault) = a_vault("host-verbs-set-where-none", &[]);
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let setting = |mode| {
        SetParams::new(
            address(&vault),
            mode,
            WriteTarget::matching([Predicate::equal_to("wave", "nowhere")]),
            vec![FieldChange::set("wave", AuthoredValue::string("flipped"))],
        )
    };

    let previewed = refused(host.set(setting(ApplyMode::Preview)));
    let applied = refused(host.set(setting(ApplyMode::Apply)));
    assert_eq!(previewed.code(), &ReasonCode::VaultPlanRefused);
    assert_eq!(previewed.detail(), applied.detail());
    let ErrorDetail::PlanRefused { unresolved, .. } = previewed.detail() else {
        panic!("the refusal carries {:?}", previewed.detail());
    };
    let [left] = unresolved.as_slice() else {
        panic!("one operation is unresolved: {unresolved:?}");
    };
    let UnresolvedReason::NoLongerResolves { detail, .. } = &left.reason else {
        panic!("the operation is unresolved for {:?}", left.reason);
    };
    assert!(
        detail.contains("nowhere"),
        "the reason names no predicate: {detail}"
    );
}

/// **A `where` naming a key the vault does not hold is unresolved naming the
/// key**, the write's form of the in-band report a find answers.
#[test]
fn a_set_where_naming_an_unknown_key_is_unresolved_naming_it() {
    let (_sandbox, vault) = a_vault("host-verbs-set-where-unknown", &[]);
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let refused = refused(host.set(SetParams::new(
        address(&vault),
        ApplyMode::Preview,
        WriteTarget::matching([Predicate::equal_to("no-such-key-anywhere", "x")]),
        vec![FieldChange::set("wave", AuthoredValue::string("flipped"))],
    )));
    let ErrorDetail::PlanRefused { unresolved, .. } = refused.detail() else {
        panic!("the refusal carries {:?}", refused.detail());
    };
    let UnresolvedReason::NoLongerResolves { detail, .. } = &unresolved[0].reason else {
        panic!("the operation is unresolved for {:?}", unresolved[0].reason);
    };
    assert!(
        detail.contains(r#"{part: unknown_predicate_key, key: "no-such-key-anywhere""#),
        "the reason names no unknown key in the wire's words: {detail}"
    );
}

/// **An edit replaces a section, inserts at a heading and replaces text
/// through `Host::edit`**, previewing the plan its apply lands.
#[test]
fn an_edit_applies_section_operations_and_a_str_replace() {
    let subject = "# Subject\n\n## Plan\n\nold plan\n\n## Log\n\nstatus draft\n";
    let (_sandbox, vault) = a_vault("host-verbs-edit", &[("subject.md", subject)]);
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let editing = |mode| {
        EditParams::new(
            address(&vault),
            mode,
            path("subject.md"),
            vec![
                DocumentEdit::replace_section("Plan", "new plan\n"),
                DocumentEdit::append_to_section("Log", "entry\n"),
                DocumentEdit::str_replace("draft", "final"),
            ],
        )
    };

    let plan = previewed(host.edit(editing(ApplyMode::Preview)));
    assert_eq!(read(&vault, "subject.md"), subject, "a preview wrote");
    let (landed, _, targets) = applied(host.edit(editing(ApplyMode::Apply)));
    assert_eq!(landed, plan);
    assert_eq!(targets, wrote(&["subject.md"]));
    assert_eq!(
        read(&vault, "subject.md"),
        "# Subject\n\n## Plan\n\nnew plan\n\n## Log\n\nstatus final\nentry\n"
    );
}

/// **A new document at a path is created with the folders above it**, its
/// preview naming the plan its apply lands; **its resolved plan sent again is
/// found landed**, writing nothing.
#[test]
fn a_new_document_is_created_with_its_folders_and_found_landed_on_a_resend() {
    let (_sandbox, vault) = a_vault("host-verbs-new", &[]);
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let creating = |mode| NewParams::new(address(&vault), mode, path("deep/er/new.md"), "# New\n");

    let plan = previewed(host.new_document(creating(ApplyMode::Preview)));
    assert!(
        !vault.path().join("deep").exists(),
        "a preview made a folder"
    );
    let (landed, _, targets) = applied(host.new_document(creating(ApplyMode::Apply)));
    assert_eq!(landed, plan);
    assert_eq!(targets, wrote(&["deep/er/new.md"]));
    assert_eq!(read(&vault, "deep/er/new.md"), "# New\n");

    let (_, _, targets) = applied(host.apply(ApplyParams::new(
        ApplyMode::Apply,
        PlanDocument::resolved(plan),
    )));
    assert_eq!(
        targets,
        vec![AppliedTarget::new(
            path("deep/er/new.md"),
            TargetResult::Found
        )]
    );
}

/// **A new document at a name another document holds is refused**, its
/// operation unresolved, and the document there is left as it was.
#[test]
fn a_new_document_at_an_occupied_name_is_refused() {
    let (_sandbox, vault) = a_vault("host-verbs-new-occupied", &[("taken.md", "other\n")]);
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let creating = |mode| NewParams::new(address(&vault), mode, path("taken.md"), "# New\n");

    let previewed = refused(host.new_document(creating(ApplyMode::Preview)));
    let applied = refused(host.new_document(creating(ApplyMode::Apply)));
    assert_eq!(previewed.code(), &ReasonCode::VaultPlanRefused);
    assert_eq!(previewed.detail(), applied.detail());
    let ErrorDetail::PlanRefused { unresolved, .. } = previewed.detail() else {
        panic!("the refusal carries {:?}", previewed.detail());
    };
    assert!(matches!(
        unresolved.as_slice(),
        [left] if matches!(left.operation.kind, OperationKind::CreateDocument { .. })
    ));
    assert_eq!(read(&vault, "taken.md"), "other\n");
}

/// A vault schema declaring the one tag `project` and reporting any other.
const TAG_SCHEMA: &str = "version: 1\ntags:\n  declared: [project]\n  undeclared: report\n";

/// **A set whose result breaks the schema is refused, preview and apply
/// alike; forced, it applies and lists the violation it let through.**
#[test]
fn a_set_breaking_the_schema_is_refused_and_forced_lists_the_violation() {
    let subject = "---\ntags: [project]\n---\n# Subject\n";
    let (_sandbox, vault) = a_vault("host-verbs-set-schema", &[("subject.md", subject)]);
    std::fs::write(vault.path().join(".norn/schema.yaml"), TAG_SCHEMA).expect("write the schema");
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let tagging = |mode, force| {
        SetParams::new(
            address(&vault),
            mode,
            WriteTarget::path(path("subject.md")),
            vec![FieldChange::push("tags", AuthoredValue::string("stray"))],
        )
        .with_force(force)
    };

    let previewed = refused(host.set(tagging(ApplyMode::Preview, false)));
    let applied_refusal = refused(host.set(tagging(ApplyMode::Apply, false)));
    assert_eq!(previewed.code(), &ReasonCode::VaultPlanRefused);
    assert_eq!(previewed.detail(), applied_refusal.detail());
    let ErrorDetail::PlanRefused { checks, .. } = previewed.detail() else {
        panic!("the refusal carries {:?}", previewed.detail());
    };
    assert!(
        matches!(
            checks.as_slice(),
            [RefusedCheck::SchemaViolation { violation, .. }]
                if violation.kind == FindingKind::UndeclaredTag
        ),
        "the set refused for {checks:?}"
    );
    assert_eq!(read(&vault, "subject.md"), subject);

    let answered = host
        .set(tagging(ApplyMode::Apply, true))
        .expect("a forced set is admitted")
        .wait()
        .expect("a forced set applies");
    let ApplyReport::Applied { forced, .. } = answered.report else {
        panic!("a forced set answered {:?}", answered.report);
    };
    assert!(
        matches!(forced.as_slice(), [violation] if violation.target.as_deref() == Some("stray")),
        "the forced set lists {forced:?}"
    );
    assert_eq!(
        read(&vault, "subject.md"),
        "---\ntags: [project, stray]\n---\n# Subject\n"
    );
}

/// **A set guarded by an expected-absent field writes where the field is
/// absent and is refused where it is present**: the add-only-if-absent
/// reading of a set.
#[test]
fn a_set_guarded_by_an_absent_field_writes_only_where_it_is_absent() {
    let bare = "---\nstatus: draft\n---\n";
    let owned = "---\nowner: ana\n---\n";
    let (_sandbox, vault) = a_vault(
        "host-verbs-set-absent",
        &[("bare.md", bare), ("owned.md", owned)],
    );
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let claiming = |at: &str| {
        SetParams::new(
            address(&vault),
            ApplyMode::Apply,
            WriteTarget::path(path(at)),
            vec![FieldChange::set("owner", AuthoredValue::string("bo"))],
        )
        .with_conditions(vec![AuthorCondition::expected_value(
            path(at),
            "owner",
            ExpectedField::absent(),
        )])
    };

    let (_, _, targets) = applied(host.set(claiming("bare.md")));
    assert_eq!(targets, wrote(&["bare.md"]));
    assert_eq!(
        read(&vault, "bare.md"),
        "---\nstatus: draft\nowner: bo\n---\n"
    );
    let refusal = refused(host.set(claiming("owned.md")));
    assert_eq!(refusal.code(), &ReasonCode::VaultPlanRefused);
    assert_eq!(read(&vault, "owned.md"), owned);
}

/// `count` documents under `folder`, each carrying `wave: flip`, named so
/// their path order is their number's.
fn flips(folder: &str, count: usize) -> Vec<(String, String)> {
    (0..count)
        .map(|at| {
            (
                format!("{folder}/{at:05}.md"),
                "---\nwave: flip\n---\n".to_string(),
            )
        })
        .collect()
}

/// A set flipping every `wave: flip` document to `wave: flipped`.
fn flipping(vault: &attach::Vault, mode: ApplyMode) -> SetParams {
    SetParams::new(
        address(vault),
        mode,
        WriteTarget::matching([Predicate::equal_to("wave", "flip")]),
        vec![FieldChange::set("wave", AuthoredValue::string("flipped"))],
    )
}

/// **A set with a `where` target sent straight to apply matches inside the
/// apply job and writes exactly the matched documents, in one changeset.**
/// No preview runs first: the operations reach the job as authored, and its
/// own match names the three documents it writes.
#[test]
fn a_set_where_sent_straight_to_apply_writes_exactly_the_matched_documents() {
    let flip = "---\nwave: flip\n---\n";
    let keep = "---\nwave: keep\n---\n";
    let (_sandbox, vault) = a_vault(
        "host-verbs-set-where-apply",
        &[
            ("wave/a.md", flip),
            ("wave/b.md", keep),
            ("wave/c.md", flip),
            ("wave/d.md", keep),
            ("wave/e.md", flip),
        ],
    );
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());

    let (landed, changeset, targets) = applied(host.set(flipping(&vault, ApplyMode::Apply)));
    assert_eq!(changeset, ChangesetOutcome::Committed);
    assert_eq!(targets, wrote(&["wave/a.md", "wave/c.md", "wave/e.md"]));
    let written: Vec<&str> = landed.transitions.iter().map(|t| t.path.as_str()).collect();
    assert_eq!(written, vec!["wave/a.md", "wave/c.md", "wave/e.md"]);
    for at in ["wave/a.md", "wave/c.md", "wave/e.md"] {
        assert_eq!(read(&vault, at), "---\nwave: flipped\n---\n");
    }
    for at in ["wave/b.md", "wave/d.md"] {
        assert_eq!(read(&vault, at), keep);
    }
}

/// **A matching document written after the attach is matched by an apply
/// sent as soon as the vault's intake has derived it**: the apply's match
/// reads the store the intake wrote, so the new document is written with the
/// one that was there before.
#[test]
fn a_matching_document_the_intake_derived_is_matched_by_the_next_apply() {
    let flip = "---\nwave: flip\n---\n";
    let (_sandbox, vault) = a_vault("host-verbs-set-where-fresh", &[("wave/a.md", flip)]);
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());

    std::fs::write(vault.path().join("wave/b.md"), flip).expect("write a new document");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while found(&host, &vault, vec![Predicate::equal_to("wave", "flip")]).len() < 2 {
        assert!(
            std::time::Instant::now() < deadline,
            "the intake never derived the new document"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    let (_, changeset, targets) = applied(host.set(flipping(&vault, ApplyMode::Apply)));
    assert_eq!(changeset, ChangesetOutcome::Committed);
    assert_eq!(targets, wrote(&["wave/a.md", "wave/b.md"]));
    assert_eq!(read(&vault, "wave/b.md"), "---\nwave: flipped\n---\n");
}

/// **A `where` matching more documents than one page holds expands to every
/// one of them**, preview and apply alike: the match is paged to its end, so
/// the documents past the first page are written with the rest.
#[test]
fn a_where_matching_past_one_page_expands_to_every_matched_document() {
    let count = norn_store::MAX_PAGE + 3;
    let files = flips("many", count);
    let borrowed: Vec<(&str, &str)> = files
        .iter()
        .map(|(at, content)| (at.as_str(), content.as_str()))
        .collect();
    let (_sandbox, vault) = a_vault("host-verbs-set-where-pages", &borrowed);
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());

    let plan = previewed(host.set(flipping(&vault, ApplyMode::Preview)));
    assert_eq!(plan.transitions.len(), count, "the preview stopped short");
    let (landed, changeset, targets) = applied(host.set(flipping(&vault, ApplyMode::Apply)));
    assert_eq!(
        landed, plan,
        "the apply matched another set than its preview"
    );
    assert_eq!(changeset, ChangesetOutcome::Committed);
    assert_eq!(targets.len(), count, "the apply stopped short");
    for (at, _) in &files {
        assert_eq!(read(&vault, at), "---\nwave: flipped\n---\n", "{at}");
    }
}

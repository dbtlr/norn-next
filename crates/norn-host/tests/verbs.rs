//! **The write verbs, end to end**: `Host::set`, `Host::edit`,
//! `Host::new_document`, `Host::move_path`, `Host::delete` and
//! `Host::rewrite_wikilink` over a real vault and a real attachment.
//!
//! Each verb compiles its request to operations and enters the one `apply`
//! path, so what is pinned here is what a caller of each verb sees: a
//! preview that writes nothing and answers the plan its apply lands, a
//! `where` target expanded into exactly the documents the store matches, a
//! move carrying the link cascade it plans and a folder move naming what it
//! leaves behind, a delete refused while links name its document unless it
//! rewrites them or leaves them broken, a wikilink rewrite carrying the
//! cascade that retargets every wikilink naming its `old`, and the refusals
//! the planner and the applier answer for a write.
#![cfg(unix)]
#![allow(clippy::disallowed_methods)] // Harness scaffolding: this suite's own generated tree.

mod attach;

use std::path::Path;
use std::sync::Barrier;
use std::sync::atomic::{AtomicBool, Ordering};

use norn_testkit::process::Sandbox;
use norn_wire::{
    AppliedTarget, ApplyMode, ApplyParams, ApplyReport, AuthorCondition, AuthoredPlan,
    AuthoredValue, ChangesetOutcome, DeleteParams, DocumentEdit, DocumentPath, EditParams,
    ErrorDetail, ErrorEnvelope, ExpectedField, FieldChange, FilePath, FindParams, FindingKind,
    FolderPath, LinkAdvisory, LinkFamily, LinkKey, LinkRewrite, MoveParams, MoveSubject, NewParams,
    NewSubject, Operation, OperationKind, PlanDocument, Predicate, ReasonCode, RefusedCheck,
    ResolutionTarget, ResolvedPlan, RewriteWikilinkParams, SetParams, TargetResult,
    UnresolvedReason, ValueMap, Variables, VaultAddress, WriteTarget,
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

/// **A new document by a rule, or captured into the inbox, is refused until
/// a planner expands it**: the host passes the compiled `create_by_rule`
/// through unchanged, so it is left unresolved, preview and apply alike, and
/// nothing is written.
#[test]
fn a_new_document_by_rule_is_refused_until_it_is_planned() {
    let (_sandbox, vault) = a_vault("host-verbs-new-by-rule", &[("a.md", "a\n")]);
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    for subject in [
        NewSubject::by_rule(
            "meeting",
            Variables::default(),
            ValueMap::default(),
            Some("Agenda.\n".to_string()),
        ),
        NewSubject::inbox(ValueMap::default(), None),
    ] {
        for mode in [ApplyMode::Preview, ApplyMode::Apply] {
            let creating = NewParams::for_subject(address(&vault), mode, subject.clone());
            let refusal = refused(host.new_document(creating));
            assert_eq!(refusal.code(), &ReasonCode::VaultPlanRefused);
            let ErrorDetail::PlanRefused { unresolved, .. } = refusal.detail() else {
                panic!("the refusal carries {:?}", refusal.detail());
            };
            assert!(matches!(
                unresolved.as_slice(),
                [left] if matches!(left.operation.kind, OperationKind::CreateByRule { .. })
            ));
        }
    }
    assert_eq!(read(&vault, "a.md"), "a\n");
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

/// **A move previews its link cascade, then applies the plan it previewed.**
/// The linker names the moved document by its bare stem and by a relative
/// path; the preview writes nothing and carries both rewrites on the move,
/// and the apply lands the previewed plan, the document at its new path and
/// both links naming it there.
#[test]
fn a_move_previews_its_cascade_then_applies_it() {
    let linker = "See [[move-gate-subject]] and [s](move-gate-subject.md).\n";
    let (_sandbox, vault) = a_vault(
        "host-verbs-move",
        &[
            ("move-gate/move-gate-subject.md", "# Subject\n"),
            ("move-gate/linker.md", linker),
        ],
    );
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let moving = |mode| {
        MoveParams::new(
            address(&vault),
            mode,
            MoveSubject::document(
                path("move-gate/move-gate-subject.md"),
                path("move-gate/archive/move-gate-moved.md"),
            ),
        )
    };

    let plan = previewed(host.move_path(moving(ApplyMode::Preview)));
    assert_eq!(
        read(&vault, "move-gate/linker.md"),
        linker,
        "a preview wrote"
    );
    assert_eq!(
        plan.operations[0].cascade,
        vec![
            LinkRewrite::new(
                path("move-gate/linker.md"),
                LinkFamily::Markdown,
                "move-gate-subject.md",
                "archive/move-gate-moved.md",
            ),
            LinkRewrite::new(
                path("move-gate/linker.md"),
                LinkFamily::Wikilink,
                "move-gate-subject",
                "move-gate-moved",
            ),
        ]
    );
    let (landed, changeset, _) = applied(host.move_path(moving(ApplyMode::Apply)));
    assert_eq!(
        landed, plan,
        "the apply landed another plan than it previewed"
    );
    assert_eq!(changeset, ChangesetOutcome::Committed);
    assert_eq!(
        read(&vault, "move-gate/linker.md"),
        "See [[move-gate-moved]] and [s](archive/move-gate-moved.md).\n"
    );
    assert_eq!(
        read(&vault, "move-gate/archive/move-gate-moved.md"),
        "# Subject\n"
    );
    assert!(!vault.path().join("move-gate/move-gate-subject.md").exists());
}

/// **A folder move previews what it leaves behind.** The folder's two
/// documents move together, so the relative link between them stays as
/// written and no cascade is planned; the attachment beside them is named
/// as left behind, and stays in the folder once the move applies.
#[test]
fn a_folder_move_previews_what_it_leaves_behind() {
    let (_sandbox, vault) = a_vault(
        "host-verbs-move-folder",
        &[
            ("move-folder/a.md", "[b](b.md)\n"),
            ("move-folder/b.md", "# B\n"),
            ("move-folder/diagram.png", "png"),
        ],
    );
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let moving = |mode| {
        MoveParams::new(
            address(&vault),
            mode,
            MoveSubject::folder(
                FolderPath::new("move-folder").expect("a folder path"),
                FolderPath::new("moved/move-folder").expect("a folder path"),
            ),
        )
    };

    let answered = host
        .move_path(moving(ApplyMode::Preview))
        .expect("a preview is answered")
        .wait()
        .expect("the folder move previews");
    let ApplyReport::Previewed { plan, forecast, .. } = answered.report else {
        panic!("a preview answered {:?}", answered.report);
    };
    assert_eq!(
        forecast.left_behind,
        vec![FilePath::new("move-folder/diagram.png").expect("a file path")]
    );
    assert_eq!(plan.operations.len(), 2);
    assert!(
        plan.operations
            .iter()
            .all(|operation| operation.cascade.is_empty()),
        "{:?}",
        plan.operations
    );
    let (landed, _, _) = applied(host.move_path(moving(ApplyMode::Apply)));
    assert_eq!(landed, plan);
    assert_eq!(read(&vault, "moved/move-folder/a.md"), "[b](b.md)\n");
    assert_eq!(read(&vault, "move-folder/diagram.png"), "png");
    assert!(!vault.path().join("move-folder/a.md").exists());
}

/// The linker and the second holder the delete cases plant, each naming the
/// subject.
const DELETE_LINKER: &str = "See [[delete-gate-subject]] and [s](delete-gate-subject.md).\n";
const DELETE_HOLDER: &str = "Also ![[delete-gate-subject#Part]].\n";

/// A vault holding the delete cases' subject, its target, and the two
/// documents naming the subject.
fn a_delete_vault(label: &str) -> (Sandbox, attach::Vault) {
    a_vault(
        label,
        &[
            ("delete-gate/delete-gate-subject.md", "# Subject\n"),
            ("delete-gate/kept/delete-gate-target.md", "# Target\n"),
            ("delete-gate/linker.md", DELETE_LINKER),
            ("delete-gate/holder.md", DELETE_HOLDER),
        ],
    )
}

fn deleting_the_subject(vault: &attach::Vault, mode: ApplyMode) -> DeleteParams {
    DeleteParams::new(
        address(vault),
        mode,
        path("delete-gate/delete-gate-subject.md"),
    )
}

/// **A delete saying neither flag is refused while links name its
/// document**, preview and apply alike: its operation is unresolved naming
/// both holders, once each, and the three links, and nothing is written.
#[test]
fn a_plain_delete_of_a_linked_document_is_refused_naming_every_holder() {
    let (_sandbox, vault) = a_delete_vault("host-verbs-delete-refused");
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());

    let previewed = refused(host.delete(deleting_the_subject(&vault, ApplyMode::Preview)));
    let applied = refused(host.delete(deleting_the_subject(&vault, ApplyMode::Apply)));
    assert_eq!(previewed.code(), &ReasonCode::VaultPlanRefused);
    assert_eq!(previewed.detail(), applied.detail());
    let ErrorDetail::PlanRefused { unresolved, .. } = previewed.detail() else {
        panic!("the refusal carries {:?}", previewed.detail());
    };
    let [left] = unresolved.as_slice() else {
        panic!("one operation is unresolved: {unresolved:?}");
    };
    assert_eq!(
        left.reason,
        UnresolvedReason::has_backlinks(
            vec![path("delete-gate/holder.md"), path("delete-gate/linker.md")],
            3,
        )
    );
    assert!(
        vault
            .path()
            .join("delete-gate/delete-gate-subject.md")
            .exists()
    );
}

/// **A delete rewriting its links previews its cascade, then applies the
/// plan it previewed**: each link naming the subject is respelled, in its
/// own form, to name the target, the embed keeping its anchor, and the
/// subject is gone.
#[test]
fn a_delete_rewriting_its_links_previews_its_cascade_then_applies_it() {
    let (_sandbox, vault) = a_delete_vault("host-verbs-delete-rewrite");
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let rewriting = |mode| {
        deleting_the_subject(&vault, mode)
            .rewriting_to(ResolutionTarget::new("delete-gate-target").expect("a target"))
    };

    let plan = previewed(host.delete(rewriting(ApplyMode::Preview)));
    assert_eq!(
        read(&vault, "delete-gate/linker.md"),
        DELETE_LINKER,
        "a preview wrote"
    );
    assert_eq!(
        plan.operations[0].cascade,
        vec![
            LinkRewrite::new(
                path("delete-gate/holder.md"),
                LinkFamily::Wikilink,
                "delete-gate-subject",
                "delete-gate-target",
            ),
            LinkRewrite::new(
                path("delete-gate/linker.md"),
                LinkFamily::Markdown,
                "delete-gate-subject.md",
                "kept/delete-gate-target.md",
            ),
            LinkRewrite::new(
                path("delete-gate/linker.md"),
                LinkFamily::Wikilink,
                "delete-gate-subject",
                "delete-gate-target",
            ),
        ]
    );
    let (landed, changeset, _) = applied(host.delete(rewriting(ApplyMode::Apply)));
    assert_eq!(
        landed, plan,
        "the apply landed another plan than it previewed"
    );
    assert_eq!(changeset, ChangesetOutcome::Committed);
    assert_eq!(
        read(&vault, "delete-gate/linker.md"),
        "See [[delete-gate-target]] and [s](kept/delete-gate-target.md).\n"
    );
    assert_eq!(
        read(&vault, "delete-gate/holder.md"),
        "Also ![[delete-gate-target#Part]].\n"
    );
    assert!(
        !vault
            .path()
            .join("delete-gate/delete-gate-subject.md")
            .exists()
    );
}

/// **A delete leaving its links broken lands, advising each**, and a plain
/// delete of a document no link names lands with nothing to say.
#[test]
fn a_delete_leaving_its_links_broken_lands_and_an_unlinked_delete_lands() {
    let (_sandbox, vault) = a_delete_vault("host-verbs-delete-broken");
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let breaking = |mode| deleting_the_subject(&vault, mode).breaking_links();

    let answered = host
        .delete(breaking(ApplyMode::Preview))
        .expect("a preview is answered")
        .wait()
        .expect("the delete previews");
    let ApplyReport::Previewed { plan, forecast, .. } = answered.report else {
        panic!("a preview answered {:?}", answered.report);
    };
    let link = |holder: &str, syntax, address: &str| LinkKey::new(path(holder), syntax, address);
    assert_eq!(
        forecast.links,
        vec![
            LinkAdvisory::left_broken(link(
                "delete-gate/holder.md",
                LinkFamily::Wikilink,
                "delete-gate-subject"
            )),
            LinkAdvisory::left_broken(link(
                "delete-gate/linker.md",
                LinkFamily::Markdown,
                "delete-gate-subject.md"
            )),
            LinkAdvisory::left_broken(link(
                "delete-gate/linker.md",
                LinkFamily::Wikilink,
                "delete-gate-subject"
            )),
        ]
    );
    let (landed, _, _) = applied(host.delete(breaking(ApplyMode::Apply)));
    assert_eq!(landed, plan);
    assert_eq!(read(&vault, "delete-gate/linker.md"), DELETE_LINKER);

    let (landed, _, targets) = applied(host.delete(DeleteParams::new(
        address(&vault),
        ApplyMode::Apply,
        path("delete-gate/kept/delete-gate-target.md"),
    )));
    assert_eq!(landed.conditions, vec![]);
    assert_eq!(targets.len(), 1);
    assert!(
        !vault
            .path()
            .join("delete-gate/kept/delete-gate-target.md")
            .exists()
    );
}

/// **A wikilink rewrite previews its cascade, then applies the plan it
/// previewed**: every wikilink naming the subject is retargeted to the
/// target, in its own form, the embed keeping its anchor, while a Markdown
/// link naming the subject is no wikilink and stays. An `old` naming several
/// documents is refused with the head of them, preview and apply alike.
#[test]
fn a_wikilink_rewrite_previews_its_cascade_then_applies_it() {
    let (_sandbox, vault) = a_vault(
        "host-verbs-rewrite-wikilink",
        &[
            ("rewrite-gate/rewrite-gate-subject.md", "# Subject\n"),
            ("rewrite-gate/kept/rewrite-gate-target.md", "# Target\n"),
            (
                "rewrite-gate/linker.md",
                "See [[rewrite-gate-subject]] and [s](rewrite-gate-subject.md).\n",
            ),
            (
                "rewrite-gate/holder.md",
                "Also ![[rewrite-gate/rewrite-gate-subject#Part]].\n",
            ),
            ("rewrite-gate/x/rewrite-gate-twin.md", "x\n"),
            ("rewrite-gate/y/rewrite-gate-twin.md", "y\n"),
        ],
    );
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let target = |text: &str| ResolutionTarget::new(text).expect("a target");
    let rewriting = |mode| {
        RewriteWikilinkParams::new(
            address(&vault),
            mode,
            target("rewrite-gate-subject"),
            target("rewrite-gate-target"),
        )
    };

    let plan = previewed(host.rewrite_wikilink(rewriting(ApplyMode::Preview)));
    assert_eq!(
        plan.operations[0].cascade,
        vec![
            LinkRewrite::new(
                path("rewrite-gate/holder.md"),
                LinkFamily::Wikilink,
                "rewrite-gate/rewrite-gate-subject",
                "kept/rewrite-gate-target",
            ),
            LinkRewrite::new(
                path("rewrite-gate/linker.md"),
                LinkFamily::Wikilink,
                "rewrite-gate-subject",
                "rewrite-gate-target",
            ),
        ]
    );
    let (landed, changeset, _) = applied(host.rewrite_wikilink(rewriting(ApplyMode::Apply)));
    assert_eq!(
        landed, plan,
        "the apply landed another plan than it previewed"
    );
    assert_eq!(changeset, ChangesetOutcome::Committed);
    assert_eq!(
        read(&vault, "rewrite-gate/linker.md"),
        "See [[rewrite-gate-target]] and [s](rewrite-gate-subject.md).\n"
    );
    assert_eq!(
        read(&vault, "rewrite-gate/holder.md"),
        "Also ![[kept/rewrite-gate-target#Part]].\n"
    );

    let twin = |mode| {
        RewriteWikilinkParams::new(
            address(&vault),
            mode,
            target("rewrite-gate-twin"),
            target("rewrite-gate-target"),
        )
    };
    let previewed = refused(host.rewrite_wikilink(twin(ApplyMode::Preview)));
    let applied = refused(host.rewrite_wikilink(twin(ApplyMode::Apply)));
    assert_eq!(previewed.code(), &ReasonCode::VaultPlanRefused);
    assert_eq!(previewed.detail(), applied.detail());
    let ErrorDetail::PlanRefused { unresolved, .. } = previewed.detail() else {
        panic!("the refusal carries {:?}", previewed.detail());
    };
    let [left] = unresolved.as_slice() else {
        panic!("one operation is unresolved: {unresolved:?}");
    };
    let UnresolvedReason::AmbiguousTarget { candidates, .. } = &left.reason else {
        panic!("an ambiguous `old`: {:?}", left.reason);
    };
    assert_eq!(candidates.total(), 2);
}

/// Wait until the attachment has derived the document another writer wrote
/// at `at`, as the watcher delivers it.
fn derived(vault: &attach::Vault, at: &str) {
    let document = norn_store::DocumentPath::new(at).expect("a stored document path");
    norn_testkit::wait::wait_until(
        "the other writer's document is derived",
        attach::state_budget(std::time::Duration::from_secs(10)),
        || match vault.store().begin_request().stored_facts(&document) {
            Ok(Some(_)) => norn_testkit::wait::Observed::Met(()),
            _ => norn_testkit::wait::Observed::pending("not derived yet"),
        },
    )
    .expect("the other writer's document is derived");
}

/// **A delete whose path the plan refills is refused for a backlink another
/// writer adds after its preview**, its plan sent back resolved: nothing
/// named the document at preview, so the delete needed no flag, but the
/// document the plan replaces is named when the apply's check reads its
/// links again — the job mints a read handle for the check though every
/// presence stays as it was — and nothing is written.
#[test]
fn a_delete_refilling_its_path_is_refused_for_a_backlink_added_after_its_preview() {
    let (_sandbox, vault) = a_vault(
        "host-verbs-delete-refill",
        &[("delete-refill/delete-refill-subject.md", "# Old\n")],
    );
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let subject = path("delete-refill/delete-refill-subject.md");
    let plan = previewed(host.apply(ApplyParams::new(
        ApplyMode::Preview,
        PlanDocument::operations(AuthoredPlan::new(
            address(&vault),
            vec![
                Operation::new(OperationKind::delete_document(subject.clone())),
                Operation::new(OperationKind::create_document(subject, "# New\n")),
            ],
        )),
    )));
    assert_eq!(plan.conditions, vec![]);

    std::fs::write(
        vault.path().join("delete-refill/linker.md"),
        "[[delete-refill-subject]]\n",
    )
    .expect("another writer links the subject");
    derived(&vault, "delete-refill/linker.md");
    let refusal = refused(host.apply(ApplyParams::new(
        ApplyMode::Apply,
        PlanDocument::resolved(plan),
    )));
    assert_eq!(refusal.code(), &ReasonCode::VaultPlanRefused);
    let ErrorDetail::PlanRefused { checks, .. } = refusal.detail() else {
        panic!("the refusal carries {:?}", refusal.detail());
    };
    assert!(
        matches!(&checks[..], [RefusedCheck::ConditionUnrecorded { .. }]),
        "{checks:?}"
    );
    assert_eq!(
        read(&vault, "delete-refill/delete-refill-subject.md"),
        "# Old\n"
    );
}

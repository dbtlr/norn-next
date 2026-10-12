//! **A repair plan the process dies applying is finished by sending it again.**
//!
//! A repair is planned into one resolved plan and applied through the one
//! applier, so a crash in the middle of its publications leaves the vault where
//! the applier's convergence rule finishes it. This suite binds that to a plan a
//! real repair preview produced, rather than one written by hand: two fields
//! fixed in two documents, and a misplaced document routed into its rule's
//! folder with a holder whose link the move's cascade respells.
//!
//! **The apply runs in a child.** `norn-fs` reads its fault arms once per
//! process and a process that dies cannot report on itself, so the parent plans
//! the repair, a child of this test binary applies the plan under an arm that
//! ends it at one publication, and the parent attaches the vault again and sends
//! the same plan.
#![cfg(all(unix, feature = "induced-failure"))]
#![allow(clippy::disallowed_methods)] // Harness scaffolding: the child's inputs and this suite's tree.

mod attach;

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use norn_testkit::equivalence::StoreProjection;
use norn_testkit::process::{Run, RunStatus, Sandbox};
use norn_wire::{
    ApplyMode, ApplyParams, ApplyReport, ChangesetOutcome, PlanDocument, Predicate, RepairParams,
    ResolvedPlan, VaultAddress,
};

const CHILD: &str = "NORN_REPAIR_CRASH_CHILD";
const ROOT: &str = "NORN_REPAIR_CRASH_ROOT";
const PLAN: &str = "NORN_REPAIR_CRASH_PLAN";
const PROFILE: &str = "tiny";

/// One rule: a task holds a `status` (defaulting to `todo`) and belongs in
/// `tasks/`, where it is routed.
const SCHEMA: &str = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    required:\n      status: {default: todo}\n    allowed_paths: {paths: ['zz-repair/tasks/**'], route: 'zz-repair/tasks/'}\n";

/// The documents the repair works on: two tasks missing their `status`, a task
/// outside `tasks/`, and a holder linking to it that is no task.
const DOCUMENTS: [(&str, &str); 4] = [
    ("tasks/a.md", "---\ntype: task\n---\n# A\n"),
    ("tasks/b.md", "---\ntype: task\n---\n# B\n"),
    ("loose/m.md", "---\ntype: task\nstatus: todo\n---\n# M\n"),
    ("h.md", "Up: [m](loose/m.md)\n"),
];

/// A sandbox and a vault in it holding [`DOCUMENTS`] under [`SCHEMA`].
fn a_vault(label: &str) -> (Sandbox, attach::Vault) {
    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), label).expect("a sandbox");
    let vault = attach::Vault::generate(&sandbox.work_dir().join("attached"), PROFILE);
    std::fs::write(vault.path().join(".norn/schema.yaml"), SCHEMA).expect("write the schema");
    for (name, bytes) in DOCUMENTS {
        let at = vault.path().join("zz-repair").join(name);
        std::fs::create_dir_all(at.parent().expect("a parent folder")).expect("make the folder");
        std::fs::write(at, bytes).expect("write a document");
    }
    (sandbox, vault)
}

fn repairing(vault: &attach::Vault, mode: ApplyMode) -> RepairParams {
    RepairParams::new(VaultAddress::name(vault.name().clone()), mode)
        .with_predicates([Predicate::path("zz-repair/**")])
}

/// The resolved plan a repair preview of the vault answers, the host that
/// planned it let go.
fn planned(vault: &attach::Vault) -> ResolvedPlan {
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let answered = host
        .repair(repairing(vault, ApplyMode::Preview))
        .expect("the repair is answered")
        .wait()
        .expect("the repair previews");
    let ApplyReport::Previewed { plan, .. } = answered.report else {
        panic!("a preview answered {:?}", answered.report);
    };
    plan
}

/// What sending `plan` to a host freshly attached to the vault answers.
fn sent(vault: &attach::Vault, plan: &ResolvedPlan) -> ApplyReport {
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    host.apply(ApplyParams::new(
        ApplyMode::Apply,
        PlanDocument::resolved(plan.clone()),
    ))
    .expect("the plan is admitted")
    .wait()
    .expect("the plan applies")
    .report
}

/// The child: send the plan it is handed, under whatever the environment arms.
#[test]
fn repair_crash_child() {
    let Some(root) = std::env::var_os(CHILD) else {
        return;
    };
    let root = attach::accepted_harness_root(&root, ROOT);
    let plan = std::fs::read(std::env::var_os(PLAN).expect("the plan's file")).expect("the plan");
    let plan: ResolvedPlan = serde_json::from_slice(&plan).expect("a plan");
    sent(&attach::Vault::adopt(&root), &plan);
}

/// Run a child sending `plan` to the vault under `armed`, and report whether the
/// arm fired and ended it.
fn crashed_by(sandbox: &Sandbox, plan: &ResolvedPlan, armed: &str) -> bool {
    let root = sandbox.work_dir().join("attached");
    let token = attach::issue_harness_token(&root);
    let (plan_file, hits) = (root.join("plan.json"), root.join("hits"));
    std::fs::write(&plan_file, serde_json::to_vec(plan).expect("a plan")).expect("the plan file");
    std::fs::write(&hits, "").expect("the record file");
    let output = Run::new(sandbox, std::env::current_exe().expect("this test binary"))
        .arg("--exact")
        .arg("repair_crash_child")
        .arg("--nocapture")
        .env(CHILD, &root)
        .env(ROOT, token)
        .env(PLAN, &plan_file)
        .env("NORN_FS_ARMED_STAGES", armed)
        .env("NORN_FS_ARM_HITS", &hits)
        .deadline(Duration::from_secs(300))
        .wait()
        .expect("the child runs");
    let fired = !std::fs::read_to_string(&hits)
        .expect("the record")
        .is_empty();
    assert_eq!(
        output.status == RunStatus::Exited(0),
        !fired,
        "{armed}: the child ends where the arm fires and nowhere else: {}",
        output.stderr_text()
    );
    fired
}

/// Every Markdown document and the schema beneath the vault, with its bytes.
fn tree_bytes(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, at: &Path, into: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(at).expect("read a folder") {
            let path = entry.expect("an entry").path();
            let kind = std::fs::symlink_metadata(&path)
                .expect("a kind")
                .file_type();
            if kind.is_dir() {
                walk(root, &path, into);
            } else if kind.is_file()
                && (path.extension().is_some_and(|ext| ext == "md")
                    || path.ends_with(".norn/schema.yaml"))
            {
                let name = path.strip_prefix(root).expect("beneath the root");
                into.insert(
                    name.display().to_string(),
                    std::fs::read(&path).expect("read"),
                );
            }
        }
    }
    let mut into = BTreeMap::new();
    walk(root, root, &mut into);
    into
}

/// Assert the vault's store equals a derivation of its tree from zero.
fn assert_store_is_a_build_from_zero(sandbox: &Sandbox, vault: &attach::Vault, label: &str) {
    let fresh = vault.beside(&sandbox.work_dir().join("zero-machine"));
    let host = fresh.host();
    drop(attach::attach_and_wait(&host, fresh.name()));
    drop(host);
    let mut kept = vault.store();
    let mut built = fresh.store();
    StoreProjection::read(&mut kept)
        .expect("project the kept store")
        .assert_equivalent(
            &StoreProjection::read(&mut built).expect("project the built store"),
            label,
        );
}

/// **A crash at any publication of a repair plan, then a re-send.** The repair
/// plans five publications: two field fixes, and a routed move that creates the
/// document in `tasks/`, respells the holder's link and removes the original.
/// For each, the process ends before the publication lands and again after it
/// lands; sending the same resolved plan then answers it applied, leaves the
/// vault bytes an uninterrupted apply leaves, and leaves a store that equals a
/// build from zero.
#[test]
fn a_repair_plan_a_crash_cut_short_at_any_publication_is_finished_by_sending_it_again() {
    let (baseline_box, baseline) = a_vault("repair-crash-baseline");
    let plan = planned(&baseline);
    let ApplyReport::Applied { targets, .. } = sent(&baseline, &plan) else {
        panic!("the uninterrupted apply answers applied");
    };
    let publications = targets.len();
    assert_eq!(
        publications, 5,
        "two fixes and a route's three: {targets:?}"
    );
    let wanted = tree_bytes(baseline.path());
    assert_eq!(
        wanted["zz-repair/h.md"], b"Up: [m](tasks/m.md)\n",
        "the cascade respelled the holder"
    );
    assert_store_is_a_build_from_zero(&baseline_box, &baseline, "the uninterrupted apply");

    for ordinal in 1..=publications {
        // Before the publication lands (a swap or an unlink, whichever it is),
        // and after it lands and its folder is synced.
        for armed in [
            format!("swap@{ordinal}=ends,unlink@{ordinal}=ends"),
            format!("parent-sync@{ordinal}=ends"),
        ] {
            let (sandbox, vault) = a_vault("repair-crash");
            // Each vault is its own tree, so its plan names its own root.
            let sent_plan = planned(&vault);
            assert_eq!(sent_plan.operations, plan.operations, "{armed}");
            assert_eq!(sent_plan.transitions, plan.transitions, "{armed}");
            assert!(
                crashed_by(&sandbox, &sent_plan, &armed),
                "{armed}: the arm fired"
            );

            let ApplyReport::Applied { changeset, .. } = sent(&vault, &sent_plan) else {
                panic!("{armed}: the re-send answers applied");
            };
            assert!(
                matches!(
                    changeset,
                    ChangesetOutcome::Committed | ChangesetOutcome::Healing
                ),
                "{armed}: {changeset:?}"
            );
            assert_eq!(tree_bytes(vault.path()), wanted, "{armed}");
            assert_store_is_a_build_from_zero(&sandbox, &vault, &armed);
        }
    }
}

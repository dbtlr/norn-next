//! What a plan pays to confirm its targets' spellings, separate from content
//! reads.
//!
//! Fresh replacements keep folder width constant. Crossing target count with
//! total sibling count shows the confirmation follows the targets and not the
//! folder: on a folding macOS root each target costs a fixed number of
//! stored-name queries and no directory entry, and the same workload on a
//! root that distinguishes case asks nothing.
#![cfg(unix)]
#![cfg(feature = "induced-failure")]
#![allow(clippy::disallowed_methods)] // Harness scaffolding: this suite's own vault tree.

mod attach;
mod baselines;

use std::path::Path;

use norn_testkit::churn::{Folding, folding};
use norn_testkit::process::Sandbox;
use norn_wire::{
    ApplyMode, ApplyParams, ApplyReport, AuthoredPlan, ChangesetOutcome, DocumentPath, Operation,
    OperationKind, PlanDocument, TargetResult, VaultAddress,
};

const BEFORE: &str = "# Document\n\nstatus draft\n";
const AFTER: &str = "# Document\n\nstatus final\n";

/// A replaced target's spellings cost the same whatever its folder's width.
/// The host's job account excludes attachment and watcher-thread echo checks.
#[test]
#[ignore = "counter-lane case: wide-folder spelling cost on folding and distinct roots"]
fn a_wide_replace_plan_confirms_its_spellings_at_a_constant_cost_per_target() {
    baselines::assert_the_profile_the_bars_were_authored_on();
    for (targets, siblings) in [(32, 256), (128, 256), (32, 2048), (128, 2048)] {
        measure(targets, siblings);
    }
}

/// Measure one fresh plan after attachment, with all targets among the S
/// siblings. Replacements preserve S through both write-kernel phases.
fn measure(targets: usize, siblings: usize) {
    let sandbox = Sandbox::new(
        Path::new(env!("CARGO_TARGET_TMPDIR")),
        "folding-listing-cost",
    )
    .expect("a sandbox");
    let root = sandbox.work_dir().join("attached");
    let folder = root.join("vault/wide");
    std::fs::create_dir_all(&folder).expect("the measured folder");
    std::fs::create_dir_all(root.join("vault/.norn")).expect("the schema folder");
    std::fs::write(root.join("vault/.norn/schema.yaml"), attach::SCHEMA).expect("the schema");
    for at in 0..siblings {
        std::fs::write(folder.join(format!("note-{at:04}.md")), BEFORE).expect("a sibling");
    }
    let case = folding(&folder).expect("the measured volume's case behavior");
    // This panics on a distinct macOS scratch volume: its CI run must measure
    // the folding branch. A folding root elsewhere answers no stored name and
    // reads listings, which this bar does not state.
    if cfg!(target_os = "macos") {
        assert_eq!(case, Folding::Folded, "macOS must measure a folding root");
    } else {
        assert_eq!(
            case,
            Folding::Distinct,
            "a folding root off macOS reads listings"
        );
    }
    let folds = case == Folding::Folded;
    let vault = attach::Vault::adopt(&root);
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let operations = (0..targets)
        .map(|at| {
            Operation::new(OperationKind::str_replace(
                DocumentPath::new(format!("wide/note-{at:04}.md")).expect("a document path"),
                "draft",
                "final",
            ))
        })
        .collect();
    let plan = PlanDocument::operations(AuthoredPlan::new(
        VaultAddress::name(vault.name().clone()),
        operations,
    ));
    let mark = host.evidence();
    let answered = host
        .apply(ApplyParams::new(ApplyMode::Apply, plan))
        .expect("an admitted apply")
        .wait()
        .expect("the replacements apply");
    let spent = host.evidence().since(mark);
    let ApplyReport::Applied {
        plan,
        changeset,
        targets: landed,
        ..
    } = answered.report
    else {
        panic!("the plan answered {:?}", answered.report);
    };
    assert_eq!(changeset, ChangesetOutcome::Committed);
    assert_eq!(plan.transitions.len(), targets);
    assert_eq!(landed.len(), targets);
    assert!(
        landed
            .iter()
            .all(|target| target.result == TargetResult::Wrote)
    );
    assert!(
        plan.transitions.iter().all(|transition| {
            transition.before.is_document() && transition.after.is_document()
        })
    );
    assert_eq!(spent.changesets_applied, 1);
    assert_eq!(spent.documents_upserted, targets as u64);
    assert_eq!(spent.documents_deleted, 0);
    assert_eq!(std::fs::read_dir(&folder).unwrap().count(), siblings);
    for at in 0..siblings {
        assert_eq!(
            std::fs::read_to_string(folder.join(format!("note-{at:04}.md"))).unwrap(),
            if at < targets { AFTER } else { BEFORE },
            "sibling {at} at N={targets}, S={siblings}"
        );
    }
    let queries = if folds {
        baselines::APPLY_NAME_QUERIES_PER_REPLACED_TARGET * targets as u64
    } else {
        0
    };
    norn_testkit::readings::record(
        &format!("replace spelling cost: N={targets}, S={siblings}, {case}"),
        &[
            ("name_queries", spent.name_queries.to_string()),
            ("name_queries expected", queries.to_string()),
            ("write_dirents", spent.write_dirents.to_string()),
            ("walk_dirents", spent.walk_dirents.to_string()),
        ],
    );
    assert!(
        baselines::fits(spent.name_queries, queries)
            && baselines::fits(queries, spent.name_queries),
        "N={targets}, S={siblings} on {case}: {} stored-name queries, expected exactly {queries}",
        spent.name_queries
    );
    assert_eq!(
        (spent.write_dirents, spent.walk_dirents),
        (0, 0),
        "N={targets}, S={siblings} on {case}: confirming spellings read directory entries"
    );
}

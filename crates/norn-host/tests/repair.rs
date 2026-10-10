//! **The `repair` verb, end to end**: `Host::repair` over a real vault and a
//! real attachment.
//!
//! The verb's core is pinned over a schema declaring no fix: the findings a
//! request selects are read in batches of whole documents in path order,
//! every one is left alone for the reason it has, the plan that comes back is
//! the resolved plan an apply would land (none of it), and the provenance it
//! carries counts what remains, names where the next batch continues, and
//! decides nothing an apply does. Declared fixes are pinned over schemas of
//! their own: a missing required field filled from its rule default, and each
//! reason such a fill is skipped for.
#![cfg(unix)]
#![allow(clippy::disallowed_methods)] // Harness scaffolding: this suite's own generated tree.

mod attach;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use norn_testkit::process::Sandbox;
use norn_wire::{
    ApplyMode, ApplyParams, ApplyReport, AuthoredValue, ChangesetOutcome, Citation, CitedFinding,
    Confidence, Cursor, DocumentPath, ErrorDetail, ErrorEnvelope, FieldChange, FindingKind,
    NewParams, Operation, OperationId, OperationKind, PagedRows, PlanDocument, Predicate,
    Provenance, ReasonCode, RepairParams, RequiredField, RequiredFieldHead, ResolvedPlan,
    SetParams, Severity, SkipReason, SkippedCandidates, SkippedFinding, Unsatisfied,
    ValidateParams, ValidateReport, ValueCandidate, ValueCandidateHead, VaultAddress, WriteTarget,
};

/// The generated profile every case here attaches.
const PROFILE: &str = "tiny";

/// The schema the batch cases pin: one rule, so a task missing its `status`
/// or holding one outside its closed set is a finding that cites it. It
/// declares no default, so no finding here has a fix.
const SCHEMA: &str = "version: 1\nrules:\n  tasks:\n    severity: error\n    match: {frontmatter: {type: task}}\n    required:\n      status:\n    one_of:\n      status: {values: [todo, done]}\n";

/// Where every document the cases judge stands, so a path predicate leaves
/// the profile's own documents out.
const FOLDER: &str = "zz-repair/";

/// The documents the cases judge, in the path order a repair reads them, and
/// the findings each one stands under:
///
/// - `a.md` is a task missing its `status` (`field/required-missing`) and
///   names a document nothing holds (`link/broken`);
/// - `b.md` is a task holding `status: stalled` (`field/not-one-of`);
/// - `c.md` opens a frontmatter block it never closes
///   (`document/frontmatter-unclosed`);
/// - `d.md` is a task holding `status: stalled` and names a document nothing
///   holds;
/// - `e.md` has a body that is not UTF-8 (`document/body-bytes-not-utf8`).
const DOCUMENTS: [(&str, &[u8]); 5] = [
    ("a.md", b"---\ntype: task\n---\nSee [[nowhere-a]].\n"),
    ("b.md", b"---\ntype: task\nstatus: stalled\n---\n# B\n"),
    ("c.md", b"---\ntype: task\n# C\n"),
    (
        "d.md",
        b"---\ntype: task\nstatus: stalled\n---\nSee [[nowhere-d]].\n",
    ),
    (
        "e.md",
        b"---\ntype: task\nstatus: done\n---\n\xff\xfe body\n",
    ),
];

/// How many findings the documents stand under: two, one, one, two, one.
const FINDINGS: usize = 7;

/// A sandbox and a vault holding [`DOCUMENTS`] under [`SCHEMA`], attached.
fn a_vault(label: &str) -> (Sandbox, attach::Vault, attach::ServingHost) {
    a_vault_holding(label, &DOCUMENTS)
}

/// A sandbox and a vault holding `documents`, each named beneath [`FOLDER`],
/// under [`SCHEMA`], attached.
fn a_vault_holding(
    label: &str,
    documents: &[(&str, &[u8])],
) -> (Sandbox, attach::Vault, attach::ServingHost) {
    a_vault_under(label, SCHEMA, documents)
}

/// A sandbox and a vault holding `documents`, each named beneath [`FOLDER`],
/// under `schema`, attached.
fn a_vault_under(
    label: &str,
    schema: &str,
    documents: &[(&str, &[u8])],
) -> (Sandbox, attach::Vault, attach::ServingHost) {
    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), label).expect("a sandbox");
    let vault = attach::Vault::generate(&sandbox.work_dir().join("attached"), PROFILE);
    std::fs::write(vault.path().join(".norn/schema.yaml"), schema).expect("write the schema");
    for (name, bytes) in documents {
        let at = vault.path().join(FOLDER).join(name);
        std::fs::create_dir_all(at.parent().expect("a parent folder")).expect("make the folder");
        std::fs::write(at, bytes).expect("write a document");
    }
    let host = vault.host();
    (sandbox, vault, host)
}

fn address(vault: &attach::Vault) -> VaultAddress {
    VaultAddress::name(vault.name().clone())
}

/// A repair of the documents [`DOCUMENTS`] holds, in `mode`.
fn repairing(vault: &attach::Vault, mode: ApplyMode) -> RepairParams {
    RepairParams::new(address(vault), mode).with_predicates([Predicate::path(format!("{FOLDER}*"))])
}

/// The plan a repair answered, previewed or applied.
fn planned(answered: Result<norn_host::PendingApply, ErrorEnvelope>) -> ApplyReport {
    answered
        .expect("the repair is answered")
        .wait()
        .expect("the repair answers a report")
        .report
}

/// The resolved plan a previewed repair answered.
fn previewed(answered: Result<norn_host::PendingApply, ErrorEnvelope>) -> ResolvedPlan {
    match planned(answered) {
        ApplyReport::Previewed { plan, .. } => plan,
        other => panic!("a preview answered {other:?}"),
    }
}

/// The provenance a plan carries.
fn provenance(plan: &ResolvedPlan) -> &Provenance {
    plan.provenance
        .as_ref()
        .expect("a repair plan carries its provenance")
}

/// The refusal a repair answered.
fn refused(answered: Result<norn_host::PendingApply, ErrorEnvelope>) -> ErrorEnvelope {
    match answered {
        Ok(pending) => pending.wait().expect_err("the repair was refused"),
        Err(refusal) => refusal,
    }
}

/// Every finding a validate of `kinds` over the repair's documents stands
/// under: its id, and the path and kind it is filed at. The validate is the
/// independent account of what a repair selects.
fn standing(
    host: &attach::ServingHost,
    vault: &attach::Vault,
    narrowing: impl FnOnce(ValidateParams) -> ValidateParams,
) -> BTreeMap<u64, (String, FindingKind)> {
    let request = ValidateParams::new(address(vault))
        .with_predicates([Predicate::path(format!("{FOLDER}*"))])
        .with_limit(1000);
    let answered = host
        .validate(&narrowing(request))
        .expect("a validate answers");
    let ValidateReport::Findings { page, .. } = answered.answer.report else {
        panic!("a validate answered a tally");
    };
    page.rows
        .into_iter()
        .map(|row| (row.id, (row.path.as_str().to_string(), row.kind)))
        .collect()
}

/// The finding ids a plan's provenance skipped, in the order it lists them.
fn skipped_ids(plan: &ResolvedPlan) -> Vec<u64> {
    provenance(plan)
        .skipped
        .iter()
        .map(|skipped| skipped.finding)
        .collect()
}

/// The paths of the findings a plan skipped, resolved through `by_id`.
fn skipped_paths(plan: &ResolvedPlan, by_id: &BTreeMap<u64, (String, FindingKind)>) -> Vec<String> {
    skipped_ids(plan)
        .into_iter()
        .map(|id| {
            by_id
                .get(&id)
                .unwrap_or_else(|| panic!("finding {id} is no finding of the selection"))
                .0
                .rsplit('/')
                .next()
                .expect("a file name")
                .to_string()
        })
        .collect()
}

/// **A preview answers the plan with no operations and every selected
/// finding skipped as one no rule declares a fix for**, each with the value it
/// judged, in path order.
#[test]
fn a_preview_skips_every_selected_finding_as_having_no_declared_fix_in_path_order() {
    let (_sandbox, vault, host) = a_vault("host-repair-preview");
    let _lease = attach::attach_and_wait(&host, vault.name());
    let by_id = standing(&host, &vault, |request| request);
    assert_eq!(by_id.len(), FINDINGS, "the fixture's findings: {by_id:?}");

    let plan = previewed(host.repair(repairing(&vault, ApplyMode::Preview)));

    assert!(plan.operations.is_empty());
    assert!(plan.transitions.is_empty());
    assert_eq!(
        skipped_paths(&plan, &by_id),
        ["a.md", "a.md", "b.md", "c.md", "d.md", "d.md", "e.md"]
    );
    let mut ids = skipped_ids(&plan);
    ids.sort_unstable();
    assert_eq!(ids, by_id.keys().copied().collect::<Vec<_>>());
    let not_one_of: Vec<_> = provenance(&plan)
        .skipped
        .iter()
        .filter(|skipped| by_id[&skipped.finding].1 == FindingKind::NotOneOf)
        .map(|skipped| {
            (
                skipped.reason,
                skipped.value.as_ref().map(|value| value.text().to_string()),
            )
        })
        .collect();
    assert_eq!(
        not_one_of,
        [
            (SkipReason::NoDeclaredFix, Some("stalled".to_string())),
            (SkipReason::NoDeclaredFix, Some("stalled".to_string())),
        ]
    );
    assert!(provenance(&plan).citations.is_empty());
}

/// **A document that does not read is skipped as unreadable**: a frontmatter
/// block that never closes, and a body that is not UTF-8, which the derivation
/// quarantines.
#[test]
fn a_document_that_does_not_read_is_skipped_as_unreadable() {
    let (_sandbox, vault, host) = a_vault("host-repair-unreadable");
    let _lease = attach::attach_and_wait(&host, vault.name());
    let by_id = standing(&host, &vault, |request| request);

    let plan = previewed(host.repair(repairing(&vault, ApplyMode::Preview)));

    let unreadable: Vec<_> = provenance(&plan)
        .skipped
        .iter()
        .filter(|skipped| skipped.reason == SkipReason::Unreadable)
        .map(|skipped| by_id[&skipped.finding].clone())
        .collect();
    assert_eq!(
        unreadable,
        [
            (format!("{FOLDER}c.md"), FindingKind::FrontmatterUnclosed),
            (format!("{FOLDER}e.md"), FindingKind::BodyBytesNotUtf8),
        ]
    );
}

/// **An apply of a repair with nothing to write answers `Applied`, leaves
/// every file as it was, and carries the provenance its preview did.**
#[test]
fn an_apply_writes_nothing_and_carries_the_provenance_its_preview_did() {
    let (_sandbox, vault, host) = a_vault("host-repair-apply");
    let _lease = attach::attach_and_wait(&host, vault.name());
    let before = tree_bytes(vault.path());

    let preview = previewed(host.repair(repairing(&vault, ApplyMode::Preview)));
    let report = planned(host.repair(repairing(&vault, ApplyMode::Apply)));

    let ApplyReport::Applied {
        plan,
        changeset,
        targets,
        ..
    } = report
    else {
        panic!("an apply answered {report:?}");
    };
    assert_eq!(changeset, ChangesetOutcome::Committed);
    assert!(targets.is_empty());
    assert_eq!(tree_bytes(vault.path()), before, "a repair wrote");
    assert_eq!(plan.provenance, preview.provenance);
}

/// Every Markdown document under `root` and the schema, with the bytes each
/// holds: the vault's own files, which the host's bookkeeping files beside
/// them come and go among.
fn tree_bytes(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, at: &Path, into: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(at).expect("read a folder") {
            let path = entry.expect("an entry").path();
            let kind = std::fs::symlink_metadata(&path)
                .expect("a file's kind")
                .file_type();
            if kind.is_dir() {
                walk(root, &path, into);
            } else if kind.is_file()
                && (path.extension().is_some_and(|ext| ext == "md")
                    || path.ends_with(".norn/schema.yaml"))
            {
                let name = path
                    .strip_prefix(root)
                    .expect("beneath the root")
                    .display()
                    .to_string();
                into.insert(name, std::fs::read(&path).expect("read a file"));
            }
        }
    }
    let mut into = BTreeMap::new();
    walk(root, root, &mut into);
    into
}

/// **A batch whose limit falls inside a document extends through it, the next
/// batch begins at the next document, and across the batches every selected
/// finding appears exactly once.**
#[test]
fn a_batch_never_splits_a_document_and_the_batches_cover_every_finding_once() {
    let (_sandbox, vault, host) = a_vault("host-repair-batches");
    let _lease = attach::attach_and_wait(&host, vault.name());
    let by_id = standing(&host, &vault, |request| request);

    // A limit of one falls inside a.md's two findings.
    let mut batches = Vec::new();
    let mut request = repairing(&vault, ApplyMode::Preview).with_limit(1);
    loop {
        let plan = previewed(host.repair(request.clone()));
        let next = provenance(&plan).cursor.clone();
        batches.push(skipped_paths(&plan, &by_id));
        match next {
            Some(cursor) => request = request.with_after(*cursor),
            None => break,
        }
    }

    assert_eq!(
        batches,
        [
            vec!["a.md", "a.md"],
            vec!["b.md"],
            vec!["c.md"],
            vec!["d.md", "d.md"],
            vec!["e.md"],
        ]
    );
}

/// **The first batch carries the exact count that remains after it; a
/// continuation carries none; the last batch carries no cursor; one batch that
/// covers everything says nothing remains and carries no cursor.**
#[test]
fn the_first_batch_counts_what_remains_and_the_last_names_no_cursor() {
    let (_sandbox, vault, host) = a_vault("host-repair-remaining");
    let _lease = attach::attach_and_wait(&host, vault.name());

    let first = previewed(host.repair(repairing(&vault, ApplyMode::Preview).with_limit(3)));
    // a.md's two findings and b.md's: three read, and c.md, d.md and e.md's
    // four remain.
    assert_eq!(provenance(&first).skipped.len(), 3);
    assert_eq!(provenance(&first).remaining, Some(4));
    let cursor = provenance(&first)
        .cursor
        .clone()
        .expect("more remain, so the batch names where the next begins");

    let rest = previewed(host.repair(repairing(&vault, ApplyMode::Preview).with_after(*cursor)));
    assert_eq!(provenance(&rest).skipped.len(), 4);
    assert_eq!(provenance(&rest).remaining, None);
    assert_eq!(provenance(&rest).cursor, None);

    let whole = previewed(host.repair(repairing(&vault, ApplyMode::Preview)));
    assert_eq!(provenance(&whole).skipped.len(), FINDINGS);
    assert_eq!(provenance(&whole).remaining, Some(0));
    assert_eq!(provenance(&whole).cursor, None);
}

/// **A cursor survives the findings it covered being derived again.** After
/// a batch minted its cursor at a.md, a write to a.md files its findings anew
/// under new identities at a later generation; the continuation reads only the
/// documents after a.md, whatever became of a.md's findings.
#[test]
fn a_cursor_continues_after_the_documents_before_it_are_derived_again() {
    let (_sandbox, vault, host) = a_vault("host-repair-rederived");
    let _lease = attach::attach_and_wait(&host, vault.name());
    let first = previewed(host.repair(repairing(&vault, ApplyMode::Preview).with_limit(2)));
    let cursor = provenance(&first)
        .cursor
        .clone()
        .expect("b.md and the rest remain");
    let generation = provenance(&first).finding_generation;

    let write = SetParams::new(
        address(&vault),
        ApplyMode::Apply,
        WriteTarget::path(
            norn_wire::DocumentPath::new(format!("{FOLDER}a.md")).expect("a document path"),
        ),
        vec![FieldChange::set("status", AuthoredValue::string("done"))],
    );
    host.set(write)
        .expect("the write is admitted")
        .wait()
        .expect("a.md is written");
    let by_id = standing(&host, &vault, |request| request);

    let rest = previewed(host.repair(repairing(&vault, ApplyMode::Preview).with_after(*cursor)));

    assert!(provenance(&rest).finding_generation > generation);
    assert_eq!(
        skipped_paths(&rest, &by_id),
        ["b.md", "c.md", "d.md", "d.md", "e.md"]
    );
}

/// **A validate's cursor sent as `after` is refused as naming no position
/// among a repair's documents.**
#[test]
fn a_validates_cursor_is_refused_as_a_repairs_after() {
    let (_sandbox, vault, host) = a_vault("host-repair-foreign-cursor");
    let _lease = attach::attach_and_wait(&host, vault.name());
    let validated = host
        .validate(
            &ValidateParams::new(address(&vault))
                .with_predicates([Predicate::path(format!("{FOLDER}*"))])
                .with_limit(1),
        )
        .expect("a validate answers");
    let ValidateReport::Findings { page, .. } = validated.answer.report else {
        panic!("a validate answered a tally");
    };
    let foreign: Cursor = page.next.expect("more findings stand than one");

    let refusal = refused(host.repair(repairing(&vault, ApplyMode::Preview).with_after(foreign)));

    assert_eq!(refusal.code(), &ReasonCode::RequestCursorNotTaken);
    assert_eq!(
        refusal.detail(),
        &norn_wire::ErrorDetail::cursor_not_taken(PagedRows::Finding, PagedRows::RepairDocument)
    );
}

/// **The selection narrows to the findings it names**: a kind, a severity, a
/// rule and a path predicate each select only theirs.
#[test]
fn the_selection_narrows_by_kind_severity_rule_and_path() {
    let (_sandbox, vault, host) = a_vault("host-repair-narrowing");
    let _lease = attach::attach_and_wait(&host, vault.name());
    let by_id = standing(&host, &vault, |request| request);
    let paths_of = |plan: &ResolvedPlan| skipped_paths(plan, &by_id);

    let by_kind = previewed(
        host.repair(repairing(&vault, ApplyMode::Preview).with_kinds([FindingKind::NotOneOf])),
    );
    assert_eq!(paths_of(&by_kind), ["b.md", "d.md"]);

    // Errors: the rule's two, the unclosed block and the undecodable body.
    let by_severity = previewed(
        host.repair(repairing(&vault, ApplyMode::Preview).with_severity(Severity::Error)),
    );
    assert_eq!(
        paths_of(&by_severity),
        ["a.md", "b.md", "c.md", "d.md", "e.md"]
    );

    // The rule's findings: a.md's missing status and the two stalled ones.
    let by_rule = previewed(host.repair(repairing(&vault, ApplyMode::Preview).with_rule("tasks")));
    assert_eq!(paths_of(&by_rule), ["a.md", "b.md", "d.md"]);

    let by_path = previewed(
        host.repair(
            RepairParams::new(address(&vault), ApplyMode::Preview)
                .with_predicates([Predicate::path(format!("{FOLDER}d.md"))]),
        ),
    );
    assert_eq!(paths_of(&by_path), ["d.md", "d.md"]);
}

/// **A plan's provenance decides nothing an apply does.** Altered — its
/// skipped findings emptied, its cursor and count taken off, a finding
/// generation that never stood — the preview's plan applies as the plan it
/// came back as.
#[test]
fn a_plan_applies_the_same_whatever_its_provenance_says() {
    let (_sandbox, vault, host) = a_vault("host-repair-provenance");
    let _lease = attach::attach_and_wait(&host, vault.name());
    let plan = previewed(host.repair(repairing(&vault, ApplyMode::Preview).with_limit(1)));
    let mut altered = plan.clone();
    altered.provenance = Some(Provenance::new(u64::MAX, Vec::new()));

    let apply = |plan: ResolvedPlan| {
        host.apply(ApplyParams::new(
            ApplyMode::Apply,
            PlanDocument::resolved(plan),
        ))
        .expect("the plan is admitted")
        .wait()
        .expect("the plan applies")
        .report
    };
    let unaltered = apply(plan);
    let altered = apply(altered);

    let without_provenance = |report: ApplyReport| match report {
        ApplyReport::Applied {
            mut plan,
            changeset,
            targets,
            ..
        } => {
            plan.provenance = None;
            (plan, changeset, targets)
        }
        other => panic!("an apply answered {other:?}"),
    };
    assert_eq!(without_provenance(unaltered), without_provenance(altered));
}

/// **A selection the builder cannot apply as asked is refused, in both modes,
/// naming the part.** A read answers a predicate on a key the vault does not
/// hold in-band and matches nothing; a repair goes no further than that
/// nothing, so it is refused as `request/unsatisfied` with every such part,
/// and nothing is written.
#[test]
fn a_selection_naming_a_key_the_vault_does_not_hold_is_refused_in_both_modes() {
    let (_sandbox, vault, host) = a_vault("host-repair-unsatisfied");
    let _lease = attach::attach_and_wait(&host, vault.name());
    let before = tree_bytes(vault.path());

    for mode in [ApplyMode::Preview, ApplyMode::Apply] {
        let refusal = refused(host.repair(
            RepairParams::new(address(&vault), mode).with_predicates([
                Predicate::path(format!("{FOLDER}*")),
                Predicate::equal_to("stauts", "open"),
                Predicate::equal_to("prioritee", "high"),
            ]),
        ));

        assert_eq!(refusal.code(), &ReasonCode::RequestUnsatisfied, "{mode:?}");
        let ErrorDetail::Unsatisfied { parts, .. } = refusal.detail() else {
            panic!("{mode:?} refused with {:?}", refusal.detail());
        };
        let keys: Vec<&str> = parts
            .iter()
            .map(|part| match part {
                Unsatisfied::UnknownPredicateKey { key, .. } => key.as_str(),
                other => panic!("{mode:?} named another part: {other:?}"),
            })
            .collect();
        assert_eq!(keys, ["stauts", "prioritee"], "{mode:?}");
        assert_eq!(tree_bytes(vault.path()), before, "{mode:?} wrote");
    }
}

/// **A preview and an apply of the same batch plan the same whole plan, at a
/// limit that leaves a cursor and a count, and again for the continuation.**
/// An apply of nothing to write answers the plan its preview did, provenance
/// included.
#[test]
fn a_preview_and_an_apply_plan_the_same_whole_plan_at_every_batch() {
    let (_sandbox, vault, host) = a_vault("host-repair-modes-agree");
    let _lease = attach::attach_and_wait(&host, vault.name());
    let applied = |answered| match planned(answered) {
        ApplyReport::Applied { plan, .. } => plan,
        other => panic!("an apply answered {other:?}"),
    };

    let preview = previewed(host.repair(repairing(&vault, ApplyMode::Preview).with_limit(3)));
    let apply = applied(host.repair(repairing(&vault, ApplyMode::Apply).with_limit(3)));

    assert!(provenance(&preview).cursor.is_some());
    assert_eq!(provenance(&preview).remaining, Some(4));
    assert_eq!(apply, preview);

    let cursor = provenance(&preview).cursor.clone().expect("a cursor");
    let continued = repairing(&vault, ApplyMode::Preview).with_after(*cursor.clone());
    let preview = previewed(host.repair(continued));
    let apply = applied(host.repair(repairing(&vault, ApplyMode::Apply).with_after(*cursor)));

    assert_eq!(apply, preview);
}

/// **A vault drained in apply mode, batch by batch along each plan's cursor,
/// covers every selected finding once.** The cursor an applied plan carries is
/// the one that continues the repair.
#[test]
fn an_apply_drains_the_selection_along_its_cursors_covering_every_finding_once() {
    let (_sandbox, vault, host) = a_vault("host-repair-apply-drain");
    let _lease = attach::attach_and_wait(&host, vault.name());
    let by_id = standing(&host, &vault, |request| request);

    let mut batches = Vec::new();
    let mut seen = Vec::new();
    let mut request = repairing(&vault, ApplyMode::Apply).with_limit(2);
    // Each batch covers at least one finding, so a drain that has not ended
    // after one batch per finding is following a cursor that does not advance.
    for drained in 0.. {
        assert!(
            drained <= by_id.len(),
            "the drain did not end after {drained} batches: {batches:?}"
        );
        let ApplyReport::Applied { plan, .. } = planned(host.repair(request.clone())) else {
            panic!("an apply answered another report");
        };
        batches.push(skipped_paths(&plan, &by_id));
        seen.extend(skipped_ids(&plan));
        match provenance(&plan).cursor.clone() {
            Some(cursor) => request = request.with_after(*cursor),
            None => break,
        }
    }

    assert!(
        batches.len() >= 2,
        "one batch drained the vault: {batches:?}"
    );
    seen.sort_unstable();
    assert_eq!(seen, by_id.keys().copied().collect::<Vec<_>>());
}

/// **A block that changes nothing about what applies, shown on a plan that
/// writes.** The resolved plan of a status edit lands the same bytes and the
/// same target outcomes with no provenance, with the block a repair attached,
/// and with it altered: skipped findings emptied, cursor and count taken off,
/// a finding generation that never stood.
#[test]
fn a_plan_with_operations_applies_the_same_whatever_its_provenance_says() {
    const EDITED: &str = "edited.md";
    let documents: Vec<(&str, &[u8])> = DOCUMENTS
        .iter()
        .copied()
        .chain([(
            EDITED,
            &b"---\ntype: task\nstatus: todo\n---\n# Edited\n"[..],
        )])
        .collect();
    let edit = |vault: &attach::Vault, host: &attach::ServingHost| {
        previewed(host.set(SetParams::new(
            address(vault),
            ApplyMode::Preview,
            WriteTarget::path(
                norn_wire::DocumentPath::new(format!("{FOLDER}{EDITED}")).expect("a document path"),
            ),
            vec![FieldChange::set("status", AuthoredValue::string("done"))],
        )))
    };
    let landing = |label: &str, block: &dyn Fn(&ResolvedPlan) -> Option<Provenance>| {
        let (_sandbox, vault, host) = a_vault_holding(label, &documents);
        let _lease = attach::attach_and_wait(&host, vault.name());
        let mut plan = edit(&vault, &host);
        assert_eq!(plan.operations.len(), 1, "the edit is an operation");
        let repair = previewed(host.repair(repairing(&vault, ApplyMode::Preview).with_limit(1)));
        plan.provenance = block(&repair);
        let applied = host
            .apply(ApplyParams::new(
                ApplyMode::Apply,
                PlanDocument::resolved(plan),
            ))
            .expect("the plan is admitted")
            .wait()
            .expect("the plan applies")
            .report;
        let ApplyReport::Applied {
            plan,
            changeset,
            targets,
            ..
        } = applied
        else {
            panic!("an apply answered {applied:?}");
        };
        // The plan's root is the fixture's own, so it is the one thing that
        // differs between three equivalent vaults.
        let written = std::fs::read(vault.path().join(FOLDER).join(EDITED)).expect("read");
        (
            plan.operations,
            plan.transitions,
            changeset,
            targets,
            written,
        )
    };

    let none = landing("host-repair-block-none", &|_| None);
    let original = landing("host-repair-block-original", &|repair| {
        repair.provenance.clone()
    });
    let altered = landing("host-repair-block-altered", &|_| {
        Some(Provenance::new(u64::MAX, Vec::new()))
    });

    assert!(String::from_utf8_lossy(&none.4).contains("status: done"));
    assert_eq!(original, none);
    assert_eq!(altered, none);
}

/// The documents a previewed repair of the vault's ambiguous links skipped
/// the one ambiguous link with, as paths, and how many it said there were.
fn ambiguous_candidates(plan: &ResolvedPlan) -> (Vec<String>, u64) {
    let [skipped] = provenance(plan).skipped.as_slice() else {
        panic!("one ambiguous link stands: {:?}", provenance(plan).skipped);
    };
    assert_eq!(skipped.reason, SkipReason::AmbiguousLink);
    let Some(SkippedCandidates::Documents { head, .. }) = &skipped.candidates else {
        panic!("the skip names no documents: {skipped:?}");
    };
    let named = head
        .candidates()
        .iter()
        .map(|candidate| candidate.path.as_str().to_string())
        .collect();
    (named, head.total())
}

/// The documents the ambiguity fixture holds: two twins and a pointer to them.
const TWINS: [(&str, &[u8]); 3] = [
    ("a/twin.md", b"---\ntype: note\n---\n# A\n"),
    ("b/twin.md", b"---\ntype: note\n---\n# B\n"),
    ("pointer.md", b"---\ntype: note\n---\nSee [[twin]].\n"),
];

/// **An ambiguous link is skipped as ambiguous, naming the documents its
/// address could name.** `[[twin]]` names `a/twin.md` and `b/twin.md`; the
/// finding is skipped as `ambiguous_link` with both as its candidates, the
/// head the finding is filed with.
#[test]
fn an_ambiguous_link_is_skipped_as_ambiguous_naming_the_documents_it_could_name() {
    let (_sandbox, vault, host) = a_vault_holding("host-repair-ambiguous", &TWINS);
    let _lease = attach::attach_and_wait(&host, vault.name());

    let plan = previewed(
        host.repair(repairing(&vault, ApplyMode::Preview).with_kinds([FindingKind::Ambiguous])),
    );

    assert_eq!(
        ambiguous_candidates(&plan),
        (
            vec![format!("{FOLDER}a/twin.md"), format!("{FOLDER}b/twin.md")],
            2
        )
    );
}

/// **A third twin added after a repair names all three on the next one.** The
/// changeset that adds `c/twin.md` files the pointer's finding again, so the
/// head a repair reads is the class at its own snapshot.
#[test]
fn an_ambiguous_link_names_a_twin_added_since_the_last_repair() {
    let (_sandbox, vault, host) = a_vault_holding("host-repair-third-twin", &TWINS);
    let _lease = attach::attach_and_wait(&host, vault.name());
    let narrowed = || repairing(&vault, ApplyMode::Preview).with_kinds([FindingKind::Ambiguous]);
    assert_eq!(
        ambiguous_candidates(&previewed(host.repair(narrowed()))).1,
        2
    );

    host.new_document(NewParams::new(
        address(&vault),
        ApplyMode::Apply,
        norn_wire::DocumentPath::new(format!("{FOLDER}c/twin.md")).expect("a document path"),
        "# C\n",
    ))
    .expect("the write is admitted")
    .wait()
    .expect("c/twin.md is written");

    assert_eq!(
        ambiguous_candidates(&previewed(host.repair(narrowed()))),
        (
            vec![
                format!("{FOLDER}a/twin.md"),
                format!("{FOLDER}b/twin.md"),
                format!("{FOLDER}c/twin.md")
            ],
            3
        )
    );
}

/// The document beneath [`FOLDER`] at `name`, as a path.
fn at(name: &str) -> DocumentPath {
    DocumentPath::new(format!("{FOLDER}{name}")).expect("a document path")
}

/// The operation `repair-<n>`, setting `field` of the document at `name` to
/// `value`.
fn setting(n: usize, name: &str, field: &str, value: &str) -> Operation {
    Operation::new(OperationKind::set_frontmatter(
        WriteTarget::path(at(name)),
        field,
        AuthoredValue::string(value),
    ))
    .with_id(OperationId::new(format!("repair-{n}")).expect("an id"))
}

/// The id of the one finding of `kind` on `field` the validate of the
/// repair's documents reports for the document at `name`.
fn finding_of(
    host: &attach::ServingHost,
    vault: &attach::Vault,
    name: &str,
    kind: FindingKind,
    field: &str,
) -> u64 {
    let request = ValidateParams::new(address(vault))
        .with_predicates([Predicate::path(format!("{FOLDER}*"))])
        .with_limit(1000);
    let answered = host.validate(&request).expect("a validate answers");
    let ValidateReport::Findings { page, .. } = answered.answer.report else {
        panic!("a validate answered a tally");
    };
    let ids: Vec<u64> = page
        .rows
        .iter()
        .filter(|row| {
            row.path == at(name) && row.kind == kind && row.target.as_deref() == Some(field)
        })
        .map(|row| row.id)
        .collect();
    let [id] = ids.as_slice() else {
        panic!("one {kind:?} on `{field}` of {name}: {:?}", page.rows);
    };
    *id
}

/// The value candidates `values` name, each with the rule proposing it.
fn candidates(values: &[(&str, &str)]) -> SkippedCandidates {
    SkippedCandidates::values(
        ValueCandidateHead::new(
            values.iter().map(|(value, rule)| {
                ValueCandidate::new(norn_store::value_head(value)).by_rule(*rule)
            }),
            values.len() as u64,
        )
        .expect("a head"),
    )
}

/// One rule requiring a task's `status`, defaulting it to `todo`.
const DEFAULTING: &str = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    required:\n      status: {default: todo}\n";

/// **A missing required field fills from its rule default.** The preview's
/// plan sets the default, cited at the declared level; its apply writes it,
/// and the finding no longer stands.
#[test]
fn a_missing_required_field_fills_from_its_rule_default() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-default",
        DEFAULTING,
        &[("a.md", b"---\ntype: task\n---\n# A\n")],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let missing = finding_of(
        &host,
        &vault,
        "a.md",
        FindingKind::RequiredMissing,
        "status",
    );

    let plan = previewed(host.repair(repairing(&vault, ApplyMode::Preview)));

    assert_eq!(plan.operations, vec![setting(1, "a.md", "status", "todo")]);
    assert_eq!(
        provenance(&plan).citations,
        vec![Citation::new(
            OperationId::new("repair-1").expect("an id"),
            vec![CitedFinding::new(missing, Confidence::Declared)]
        )]
    );
    assert!(provenance(&plan).skipped.is_empty());

    let ApplyReport::Applied { changeset, .. } =
        planned(host.repair(repairing(&vault, ApplyMode::Apply)))
    else {
        panic!("the repair applies");
    };
    assert_eq!(changeset, ChangesetOutcome::Committed);
    let written = std::fs::read_to_string(vault.path().join(FOLDER).join("a.md")).expect("read");
    assert_eq!(written, "---\ntype: task\nstatus: todo\n---\n# A\n");
    assert!(standing(&host, &vault, |request| request).is_empty());
}

/// **Co-selecting rules whose defaults disagree skip the field as
/// conflicting defaults with both candidates, while an unrelated fix on the
/// same document applies**: `priority`, which one rule defaults, fills.
#[test]
fn disagreeing_defaults_skip_as_conflicting_defaults_while_an_unrelated_fix_applies() {
    let schema = "version: 1\nrules:\n  a-rule:\n    match: {frontmatter: {type: task}}\n    required:\n      status: {default: todo}\n      priority: {default: normal}\n  b-rule:\n    match: {frontmatter: {type: task}}\n    required:\n      status: {default: doing}\n";
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-conflicting-defaults",
        schema,
        &[("a.md", b"---\ntype: task\n---\n")],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let status = finding_of(
        &host,
        &vault,
        "a.md",
        FindingKind::RequiredMissing,
        "status",
    );

    let ApplyReport::Applied { plan, .. } =
        planned(host.repair(repairing(&vault, ApplyMode::Apply)))
    else {
        panic!("the repair applies");
    };

    assert_eq!(
        plan.operations,
        vec![setting(1, "a.md", "priority", "normal")]
    );
    assert_eq!(
        provenance(&plan).skipped,
        vec![
            SkippedFinding::new(status, SkipReason::ConflictingDefaults)
                .with_candidates(candidates(&[("todo", "a-rule"), ("doing", "b-rule")]))
        ]
    );
    let written = std::fs::read_to_string(vault.path().join(FOLDER).join("a.md")).expect("read");
    assert_eq!(written, "---\ntype: task\npriority: normal\n---\n");
}

/// **A default reading a capture its rule's `match.path` binds several ways
/// skips as an ambiguous capture**: `<area>` binds `red` and `blue` in
/// `red/blue/a.md`.
#[test]
fn a_default_reading_a_capture_bound_several_ways_skips_as_ambiguous_capture() {
    let schema = "version: 1\nrules:\n  areas:\n    match: {path: 'zz-repair/**/<area>/**'}\n    required:\n      area: {default: '{{path.area}}'}\n";
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-ambiguous-capture",
        schema,
        &[("red/blue/a.md", b"---\ntitle: A\n---\n")],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());

    // `zz-repair/*` names one level beneath the folder; the document stands
    // two below it.
    let plan = previewed(
        host.repair(
            RepairParams::new(address(&vault), ApplyMode::Preview)
                .with_predicates([Predicate::path(format!("{FOLDER}**"))]),
        ),
    );

    assert!(plan.operations.is_empty());
    let [skipped] = provenance(&plan).skipped.as_slice() else {
        panic!("one skip: {:?}", provenance(&plan).skipped);
    };
    assert_eq!(skipped.reason, SkipReason::AmbiguousCapture);
    let note = skipped
        .note
        .as_deref()
        .expect("the skip notes the bindings");
    assert!(
        note.contains("{area=red}") && note.contains("{area=blue}"),
        "{note}"
    );
}

/// **A default fill bringing a document under a forbidding rule skips as one
/// the judge would refuse**, and nothing is written: `kind: special` brings
/// in the rule forbidding the `scratch` the document holds.
#[test]
fn a_default_fill_bringing_a_document_under_a_forbidding_rule_skips_as_judge_would_refuse() {
    let schema = "version: 1\nrules:\n  kinds:\n    match: {frontmatter: {type: task}}\n    required:\n      kind: {default: special}\n  specials:\n    match: {frontmatter: {kind: special}}\n    forbidden:\n      scratch:\n";
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-forbidding",
        schema,
        &[("a.md", b"---\ntype: task\nscratch: x\n---\n")],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let kind = finding_of(&host, &vault, "a.md", FindingKind::RequiredMissing, "kind");
    let before = tree_bytes(vault.path());

    let ApplyReport::Applied { plan, .. } =
        planned(host.repair(repairing(&vault, ApplyMode::Apply)))
    else {
        panic!("the repair applies");
    };

    assert!(plan.operations.is_empty());
    assert_eq!(
        provenance(&plan).skipped,
        vec![
            SkippedFinding::new(kind, SkipReason::JudgeWouldRefuse)
                .with_candidates(candidates(&[("special", "kinds")]))
        ]
    );
    assert_eq!(tree_bytes(vault.path()), before, "a refused fill wrote");
}

/// **Filling `kind` where `kind: task` brings in a rule requiring `status`
/// skips as bringing in required fields, naming `status` and its declared
/// default, and fills no `status`.**
#[test]
fn filling_kind_where_kind_task_brings_in_a_rule_requiring_status_skips_and_fills_no_status() {
    let schema = "version: 1\nrules:\n  kinds:\n    match: {frontmatter: {type: work}}\n    required:\n      kind: {default: task}\n  tasks:\n    match: {frontmatter: {kind: task}}\n    required:\n      status: {default: todo}\n";
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-brings-in",
        schema,
        &[("a.md", b"---\ntype: work\n---\n")],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let kind = finding_of(&host, &vault, "a.md", FindingKind::RequiredMissing, "kind");
    let before = tree_bytes(vault.path());

    let ApplyReport::Applied { plan, .. } =
        planned(host.repair(repairing(&vault, ApplyMode::Apply)))
    else {
        panic!("the repair applies");
    };

    assert!(plan.operations.is_empty());
    let fields = RequiredFieldHead::new(
        [RequiredField::new("status").with_default(norn_store::value_head("todo"))],
        1,
    )
    .expect("a head");
    assert_eq!(
        provenance(&plan).skipped,
        vec![
            SkippedFinding::new(kind, SkipReason::BringsInRequiredFields)
                .with_required_fields(fields)
        ]
    );
    assert_eq!(tree_bytes(vault.path()), before, "a skipped fill wrote");
}

/// **Fixes compose in finding order and a fix the judge refuses skips while
/// earlier ones stand**: `a_owner` fills, `kind` would bring in the rule
/// forbidding `scratch` and skips, and `z_due` composes without it; the apply
/// lands both fills.
#[test]
fn fixes_compose_in_finding_order_and_a_fix_the_judge_refuses_skips_while_earlier_ones_stand() {
    let schema = "version: 1\nrules:\n  base:\n    match: {frontmatter: {type: task}}\n    required:\n      a_owner: {default: me}\n      kind: {default: special}\n      z_due: {default: soon}\n  specials:\n    match: {frontmatter: {kind: special}}\n    forbidden:\n      scratch:\n";
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-composition",
        schema,
        &[("a.md", b"---\ntype: task\nscratch: x\n---\n")],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let kind = finding_of(&host, &vault, "a.md", FindingKind::RequiredMissing, "kind");

    let ApplyReport::Applied { plan, .. } =
        planned(host.repair(repairing(&vault, ApplyMode::Apply)))
    else {
        panic!("the repair applies");
    };

    assert_eq!(
        plan.operations,
        vec![
            setting(1, "a.md", "a_owner", "me"),
            setting(2, "a.md", "z_due", "soon")
        ]
    );
    assert_eq!(
        provenance(&plan).skipped,
        vec![
            SkippedFinding::new(kind, SkipReason::JudgeWouldRefuse)
                .with_candidates(candidates(&[("special", "base")]))
        ]
    );
    let written = std::fs::read_to_string(vault.path().join(FOLDER).join("a.md")).expect("read");
    assert_eq!(
        written,
        "---\ntype: task\nscratch: x\na_owner: me\nz_due: soon\n---\n"
    );
}

/// One rule stamping each task it requires `created` of with the clock.
const STAMPED: &str = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    required:\n      created: {default: '{{now}}'}\n      day: {default: '{{date}}'}\n";

/// **Every default a repair plan fills comes from one clock reading**: five
/// clock defaults across three documents read the host's clock once, read off
/// its read account, in both modes, and write one instant, each noting it is
/// the repair's time.
#[test]
fn every_default_a_repair_plan_fills_comes_from_one_clock_reading() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-one-clock-reading",
        STAMPED,
        &[
            ("a.md", b"---\ntype: task\n---\n"),
            ("b.md", b"---\ntype: task\ncreated: then\n---\n"),
            ("c.md", b"---\ntype: task\n---\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());

    let account = host.read_evidence();
    let plan = previewed(host.repair(repairing(&vault, ApplyMode::Preview)));
    assert_eq!(host.read_evidence().since(account).clock_reads, 1);
    assert_eq!(plan.operations.len(), 5, "{:?}", plan.operations);
    let created: BTreeSet<String> = plan
        .operations
        .iter()
        .filter_map(|operation| match &operation.kind {
            OperationKind::SetFrontmatter { field, value, .. } if field == "created" => {
                Some(format!("{value:?}"))
            }
            _ => None,
        })
        .collect();
    assert_eq!(created.len(), 1, "one instant: {created:?}");
    for citation in &provenance(&plan).citations {
        assert!(
            citation.findings[0]
                .notes
                .iter()
                .any(|note| note.contains("the repair's time")),
            "{citation:?}"
        );
    }

    let account = host.read_evidence();
    planned(host.repair(repairing(&vault, ApplyMode::Apply)));
    assert_eq!(host.read_evidence().since(account).clock_reads, 1);
}

/// **A repair plan filling no default that reads the clock reads it not at
/// all.**
#[test]
fn a_repair_plan_filling_no_clock_default_reads_no_clock() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-no-clock-reading",
        DEFAULTING,
        &[("a.md", b"---\ntype: task\n---\n")],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let account = host.read_evidence();
    let plan = previewed(host.repair(repairing(&vault, ApplyMode::Preview)));
    assert_eq!(plan.operations.len(), 1);
    assert_eq!(host.read_evidence().since(account).clock_reads, 0);
}

/// **A `new` reads the clock through the same counted reading**: a creation
/// whose rule default reads the clock takes one reading for its plan.
#[test]
fn a_new_reads_the_clock_once_through_the_counted_reading() {
    let (_sandbox, vault, host) = a_vault_under("host-repair-new-clock-reading", STAMPED, &[]);
    let _lease = attach::attach_and_wait(&host, vault.name());

    let account = host.read_evidence();
    let report = planned(host.new_document(NewParams::new(
        address(&vault),
        ApplyMode::Preview,
        at("fresh.md"),
        "---\ntype: task\n---\n# Fresh\n",
    )));

    assert!(
        matches!(report, ApplyReport::Previewed { .. }),
        "{report:?}"
    );
    assert_eq!(host.read_evidence().since(account).clock_reads, 1);
}

/// **Findings the rules conflict over skip as a rules conflict**, in batch
/// order: a field one rule requires and another forbids, and placement rules
/// allowing no path in common.
#[test]
fn rules_conflict_findings_skip_as_rules_conflict() {
    let schema = "version: 1\nrules:\n  needs:\n    match: {frontmatter: {type: task}}\n    required:\n      status:\n    allowed_paths: {paths: ['zz-repair/x/**']}\n  bans:\n    match: {frontmatter: {priority: high}}\n    forbidden:\n      status:\n    allowed_paths: {paths: ['zz-repair/y/**']}\n";
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-rules-conflict",
        schema,
        &[("a.md", b"---\ntype: task\npriority: high\nstatus: x\n---\n")],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let by_id = standing(&host, &vault, |request| request);

    let plan = previewed(host.repair(repairing(&vault, ApplyMode::Preview)));

    let skipped: Vec<(FindingKind, SkipReason)> = provenance(&plan)
        .skipped
        .iter()
        .map(|skipped| (by_id[&skipped.finding].1, skipped.reason))
        .collect();
    assert_eq!(
        skipped,
        [
            (FindingKind::FieldRulesConflict, SkipReason::RulesConflict),
            (
                FindingKind::DocumentRulesConflict,
                SkipReason::RulesConflict
            ),
        ],
        "{by_id:?}"
    );
}

/// **Operations are numbered `repair-1`, `repair-2` and on in plan order
/// across documents, each cited at the declared level with the finding it
/// fixes.**
#[test]
fn operations_are_numbered_repair_n_and_cited_at_declared_confidence() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-numbering",
        DEFAULTING,
        &[
            ("a.md", b"---\ntype: task\n---\n"),
            ("b.md", b"---\ntype: task\n---\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let a = finding_of(
        &host,
        &vault,
        "a.md",
        FindingKind::RequiredMissing,
        "status",
    );
    let b = finding_of(
        &host,
        &vault,
        "b.md",
        FindingKind::RequiredMissing,
        "status",
    );

    let plan = previewed(host.repair(repairing(&vault, ApplyMode::Preview)));

    assert_eq!(
        plan.operations,
        vec![
            setting(1, "a.md", "status", "todo"),
            setting(2, "b.md", "status", "todo")
        ]
    );
    let cited = |n: usize, id: u64| {
        Citation::new(
            OperationId::new(format!("repair-{n}")).expect("an id"),
            vec![CitedFinding::new(id, Confidence::Declared)],
        )
    };
    assert_eq!(provenance(&plan).citations, vec![cited(1, a), cited(2, b)]);
}

//! **The `repair` verb, end to end**: `Host::repair` over a real vault and a
//! real attachment.
//!
//! The verb's core is pinned over a schema declaring no fix: the findings a
//! request selects are read in batches of whole documents in path order,
//! every one is left alone for the reason it has, the plan that comes back is
//! the resolved plan an apply would land (none of it), and the provenance it
//! carries counts what remains, names where the next batch continues, and
//! decides nothing an apply does. Declared fixes are pinned over schemas of
//! their own: a missing required field filled from its rule default, a value
//! outside a closed set replaced by its synonym's member (a list's elements
//! one by one into one change), a forbidden field removed or renamed, and
//! each reason such a fix is skipped for; a misplaced document moved into the
//! folder its rule's route names, its move's link cascade with it, and each
//! reason such a route is skipped for, a cascade that may respell a link a
//! rule reads by value among them; and over a schema declaring every kind of
//! fix, that the validator flags nothing a repair wrote.
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

/// The id of the one finding of `kind` on `field` the validate of the
/// repair's documents reports for the document at `name`, offending with
/// `value`.
fn finding_valued(
    host: &attach::ServingHost,
    vault: &attach::Vault,
    name: &str,
    kind: FindingKind,
    field: &str,
    value: &str,
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
            row.path == at(name)
                && row.kind == kind
                && row.target.as_deref() == Some(field)
                && row.value.as_ref().map(|head| head.text()) == Some(value)
        })
        .map(|row| row.id)
        .collect();
    let [id] = ids.as_slice() else {
        panic!(
            "one {kind:?} on `{field}` of {name} offending with `{value}`: {:?}",
            page.rows
        );
    };
    *id
}

/// What the document at `name` holds now.
fn written(vault: &attach::Vault, name: &str) -> String {
    std::fs::read_to_string(vault.path().join(FOLDER).join(name)).expect("read a document")
}

/// The operation `repair-<n>`, setting `field` of the document at `name` to
/// the list of strings `items`.
fn setting_list(n: usize, name: &str, field: &str, items: &[&str]) -> Operation {
    Operation::new(OperationKind::set_frontmatter(
        WriteTarget::path(at(name)),
        field,
        AuthoredValue::List(
            items
                .iter()
                .map(|item| AuthoredValue::string(*item))
                .collect(),
        ),
    ))
    .with_id(OperationId::new(format!("repair-{n}")).expect("an id"))
}

/// The citation of `repair-<n>` fixing the findings `ids`, each at the
/// declared level.
fn citing(n: usize, ids: &[u64]) -> Citation {
    Citation::new(
        OperationId::new(format!("repair-{n}")).expect("an id"),
        ids.iter()
            .map(|id| CitedFinding::new(*id, Confidence::Declared))
            .collect(),
    )
}

/// The finding `id` as a citation carries it, with its offending `value`.
fn cited_valued(id: u64, value: &str) -> CitedFinding {
    CitedFinding::new(id, Confidence::Declared).with_value(norn_store::value_head(value))
}

/// A list-shaped `status`, closed over `todo` and `done`, with `complete`
/// mapped to `done`.
const LISTED: &str = "version: 1\nfields:\n  status: {type: text, shape: list}\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    one_of:\n      status: {values: [todo, done], synonyms: {complete: done}}\n";

/// **A list's element with a synonym is repaired and the element without one
/// stands, still reported**: `status: [complete, bogus]` repairs to
/// `[done, bogus]` in one operation, `bogus` is skipped as having no declared
/// fix, and a validate afterwards reports it and nothing else; and
/// `status: [todo, done]` has no finding at all.
#[test]
fn a_list_repairs_the_element_with_a_synonym_and_the_other_stands_reported() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-list-synonym",
        LISTED,
        &[
            (
                "a.md",
                b"---\ntype: task\nstatus: [complete, bogus]\n---\n# A\n",
            ),
            ("b.md", b"---\ntype: task\nstatus: [todo, done]\n---\n# B\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let by_id = standing(&host, &vault, |request| request);
    assert!(
        by_id.values().all(|(path, _)| path.ends_with("a.md")),
        "`[todo, done]` stands under no finding: {by_id:?}"
    );
    let complete = finding_valued(
        &host,
        &vault,
        "a.md",
        FindingKind::NotOneOf,
        "status",
        "complete",
    );
    let bogus = finding_valued(
        &host,
        &vault,
        "a.md",
        FindingKind::NotOneOf,
        "status",
        "bogus",
    );

    let plan = previewed(host.repair(repairing(&vault, ApplyMode::Preview)));

    assert_eq!(
        plan.operations,
        vec![setting_list(1, "a.md", "status", &["done", "bogus"])]
    );
    assert_eq!(
        provenance(&plan).citations,
        vec![Citation::new(
            OperationId::new("repair-1").expect("an id"),
            vec![cited_valued(complete, "complete")]
        )]
    );
    assert_eq!(
        provenance(&plan).skipped,
        vec![
            SkippedFinding::new(bogus, SkipReason::NoDeclaredFix)
                .with_value(norn_store::value_head("bogus"))
        ]
    );

    let ApplyReport::Applied { changeset, .. } =
        planned(host.repair(repairing(&vault, ApplyMode::Apply)))
    else {
        panic!("the repair applies");
    };
    assert_eq!(changeset, ChangesetOutcome::Committed);
    let after = standing(&host, &vault, |request| request);
    let still: Vec<_> = after.values().cloned().collect();
    assert_eq!(
        still,
        [(format!("{FOLDER}a.md"), FindingKind::NotOneOf)],
        "{after:?}"
    );
    // The finding is filed again with the re-derived document, under the value
    // that stands.
    finding_valued(
        &host,
        &vault,
        "a.md",
        FindingKind::NotOneOf,
        "status",
        "bogus",
    );
    assert_eq!(
        written(&vault, "a.md"),
        "---\ntype: task\nstatus: [done, bogus]\n---\n# A\n"
    );
}

/// **A repeated offending element is one finding whose fix rewrites every
/// occurrence**: `[complete, todo, complete]` stands under one finding and
/// repairs to `[done, todo, done]`.
#[test]
fn a_repeated_offending_element_is_one_finding_whose_fix_rewrites_every_occurrence() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-list-repeated",
        LISTED,
        &[(
            "a.md",
            b"---\ntype: task\nstatus: [complete, todo, complete]\n---\n",
        )],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let by_id = standing(&host, &vault, |request| request);
    assert_eq!(
        by_id.len(),
        1,
        "one finding for the repeated element: {by_id:?}"
    );
    let complete = finding_valued(
        &host,
        &vault,
        "a.md",
        FindingKind::NotOneOf,
        "status",
        "complete",
    );

    let plan = previewed(host.repair(repairing(&vault, ApplyMode::Preview)));

    assert_eq!(
        plan.operations,
        vec![setting_list(1, "a.md", "status", &["done", "todo", "done"])]
    );
    assert_eq!(
        provenance(&plan).citations,
        vec![Citation::new(
            OperationId::new("repair-1").expect("an id"),
            vec![cited_valued(complete, "complete")]
        )]
    );
    assert!(provenance(&plan).skipped.is_empty());

    planned(host.repair(repairing(&vault, ApplyMode::Apply)));
    assert!(standing(&host, &vault, |request| request).is_empty());
    assert_eq!(
        written(&vault, "a.md"),
        "---\ntype: task\nstatus: [done, todo, done]\n---\n"
    );
}

/// **Co-selecting rules mapping one value to different members skip as a tie
/// with both candidates and their rules.**
#[test]
fn co_selecting_rules_mapping_a_value_to_different_members_skip_as_a_tie() {
    let schema = "version: 1\nrules:\n  a-rule:\n    match: {frontmatter: {type: task}}\n    one_of:\n      status: {values: [todo, done], synonyms: {complete: done}}\n  b-rule:\n    match: {frontmatter: {type: task}}\n    one_of:\n      status: {values: [todo, done], synonyms: {complete: todo}}\n";
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-synonym-tie",
        schema,
        &[("a.md", b"---\ntype: task\nstatus: complete\n---\n")],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let complete = finding_valued(
        &host,
        &vault,
        "a.md",
        FindingKind::NotOneOf,
        "status",
        "complete",
    );
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
            SkippedFinding::new(complete, SkipReason::Tie)
                .with_value(norn_store::value_head("complete"))
                .with_candidates(candidates(&[("done", "a-rule"), ("todo", "b-rule")]))
        ]
    );
    assert_eq!(tree_bytes(vault.path()), before, "a tie wrote");
}

/// **A synonym the judge refuses skips as one it would refuse, while an
/// earlier fix on the document stands**: `done` is outside the closed set
/// `strict` narrows `status` to, and `a_priority` repairs.
#[test]
fn a_synonym_the_judge_refuses_skips_while_an_earlier_fix_stands() {
    let schema = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    one_of:\n      a_priority: {values: [low, high], synonyms: {hi: high}}\n      status: {values: [todo, done], synonyms: {complete: done}}\n  strict:\n    match: {frontmatter: {type: task}}\n    one_of:\n      status: {values: [todo]}\n";
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-synonym-refused",
        schema,
        &[(
            "a.md",
            b"---\ntype: task\na_priority: hi\nstatus: complete\n---\n",
        )],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let complete = finding_valued(
        &host,
        &vault,
        "a.md",
        FindingKind::NotOneOf,
        "status",
        "complete",
    );

    let ApplyReport::Applied { plan, .. } =
        planned(host.repair(repairing(&vault, ApplyMode::Apply)))
    else {
        panic!("the repair applies");
    };

    assert_eq!(
        plan.operations,
        vec![setting(1, "a.md", "a_priority", "high")]
    );
    assert_eq!(
        provenance(&plan).skipped,
        vec![
            SkippedFinding::new(complete, SkipReason::JudgeWouldRefuse)
                .with_value(norn_store::value_head("complete"))
                .with_candidates(candidates(&[("done", "tasks")]))
        ]
    );
    assert_eq!(
        written(&vault, "a.md"),
        "---\ntype: task\na_priority: high\nstatus: complete\n---\n"
    );
}

/// **A synonym that brings a document under a rule requiring `status` skips
/// as bringing in required fields, naming `status` and its default**: mapping
/// `kind` to `task` selects the rule requiring it.
#[test]
fn a_synonym_that_brings_in_a_rule_requiring_status_skips_as_brings_in_required_fields() {
    let schema = "version: 1\nrules:\n  kinds:\n    match: {frontmatter: {type: work}}\n    one_of:\n      kind: {values: [task, note], synonyms: {todo: task}}\n  tasks:\n    match: {frontmatter: {kind: task}}\n    required:\n      status: {default: todo}\n";
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-synonym-brings-in",
        schema,
        &[("a.md", b"---\ntype: work\nkind: todo\n---\n")],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let todo = finding_valued(&host, &vault, "a.md", FindingKind::NotOneOf, "kind", "todo");
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
            SkippedFinding::new(todo, SkipReason::BringsInRequiredFields)
                .with_value(norn_store::value_head("todo"))
                .with_required_fields(fields)
        ]
    );
    assert_eq!(tree_bytes(vault.path()), before, "a skipped synonym wrote");
}

/// One rule fixing three forbidden fields: `scratch` is removed, `due_date`
/// renamed to `due`, and `legacy` has no fix.
const BANNED: &str = "version: 1\nrules:\n  bans:\n    match: {frontmatter: {type: task}}\n    forbidden:\n      scratch: remove\n      due_date: {rename_to: due}\n      legacy:\n";

/// **A forbidden field a rule removes is removed.**
#[test]
fn a_forbidden_field_a_rule_removes_is_removed() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-forbidden-remove",
        BANNED,
        &[("a.md", b"---\ntype: task\nscratch: x\n---\n# A\n")],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let scratch = finding_of(&host, &vault, "a.md", FindingKind::Forbidden, "scratch");

    let plan = previewed(host.repair(repairing(&vault, ApplyMode::Preview)));

    assert_eq!(
        plan.operations,
        vec![
            Operation::new(OperationKind::remove_frontmatter(
                WriteTarget::path(at("a.md")),
                "scratch"
            ))
            .with_id(OperationId::new("repair-1").expect("an id"))
        ]
    );
    assert_eq!(
        provenance(&plan).citations,
        vec![Citation::new(
            OperationId::new("repair-1").expect("an id"),
            vec![cited_valued(scratch, "x")]
        )]
    );

    planned(host.repair(repairing(&vault, ApplyMode::Apply)));
    assert_eq!(written(&vault, "a.md"), "---\ntype: task\n---\n# A\n");
    assert!(standing(&host, &vault, |request| request).is_empty());
}

/// **A forbidden field a rule renames is set under the new name and removed
/// under the old, the removal requiring the set, both cited for the
/// finding.**
#[test]
fn a_forbidden_field_a_rule_renames_is_set_under_the_new_name_and_removed() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-forbidden-rename",
        BANNED,
        &[("a.md", b"---\ntype: task\ndue_date: soon\n---\n# A\n")],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let due_date = finding_of(&host, &vault, "a.md", FindingKind::Forbidden, "due_date");

    let plan = previewed(host.repair(repairing(&vault, ApplyMode::Preview)));

    let first = OperationId::new("repair-1").expect("an id");
    assert_eq!(
        plan.operations,
        vec![
            setting(1, "a.md", "due", "soon"),
            Operation::new(OperationKind::remove_frontmatter(
                WriteTarget::path(at("a.md")),
                "due_date"
            ))
            .with_id(OperationId::new("repair-2").expect("an id"))
            .with_requires(vec![first])
        ]
    );
    assert_eq!(
        provenance(&plan).citations,
        vec![citing(1, &[due_date]), citing(2, &[due_date])]
            .into_iter()
            .map(|citation| Citation::new(citation.operation, vec![cited_valued(due_date, "soon")]))
            .collect::<Vec<_>>()
    );

    planned(host.repair(repairing(&vault, ApplyMode::Apply)));
    assert_eq!(
        written(&vault, "a.md"),
        "---\ntype: task\ndue: soon\n---\n# A\n"
    );
    assert!(standing(&host, &vault, |request| request).is_empty());
}

/// **A rename onto a field the document already holds is skipped as one onto
/// an occupied field, and writes nothing.**
#[test]
fn a_rename_onto_an_occupied_field_is_skipped() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-forbidden-occupied",
        BANNED,
        &[(
            "a.md",
            b"---\ntype: task\ndue_date: soon\ndue: later\n---\n",
        )],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let due_date = finding_of(&host, &vault, "a.md", FindingKind::Forbidden, "due_date");
    let before = tree_bytes(vault.path());

    let ApplyReport::Applied { plan, .. } =
        planned(host.repair(repairing(&vault, ApplyMode::Apply)))
    else {
        panic!("the repair applies");
    };

    assert!(plan.operations.is_empty());
    let [skipped] = provenance(&plan).skipped.as_slice() else {
        panic!("one skip: {:?}", provenance(&plan).skipped);
    };
    assert_eq!(skipped.finding, due_date);
    assert_eq!(skipped.reason, SkipReason::RenameOntoOccupiedField);
    assert_eq!(skipped.value, Some(norn_store::value_head("soon")));
    assert_eq!(tree_bytes(vault.path()), before, "a skipped rename wrote");
}

/// **Rules that remove and rename one forbidden field skip it as a tie with
/// each candidate and its rule.**
#[test]
fn rules_that_remove_and_rename_a_forbidden_field_skip_it_as_a_tie() {
    let schema = "version: 1\nrules:\n  a-rule:\n    match: {frontmatter: {type: task}}\n    forbidden:\n      scratch: remove\n  b-rule:\n    match: {frontmatter: {type: task}}\n    forbidden:\n      scratch: {rename_to: notes}\n";
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-forbidden-tie",
        schema,
        &[("a.md", b"---\ntype: task\nscratch: x\n---\n")],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let scratch = finding_of(&host, &vault, "a.md", FindingKind::Forbidden, "scratch");
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
            SkippedFinding::new(scratch, SkipReason::Tie)
                .with_value(norn_store::value_head("x"))
                .with_candidates(candidates(&[
                    ("remove", "a-rule"),
                    ("rename_to: notes", "b-rule")
                ]))
        ]
    );
    assert_eq!(tree_bytes(vault.path()), before, "a tie wrote");
}

/// One rule stamping each task it requires `created` of with the clock.
const STAMPED: &str = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    required:\n      created: {default: '{{now}}'}\n      day: {default: '{{date}}'}\n";

/// [`STAMPED`], its rule also routing each task into the day's folder of
/// `log/`.
const STAMPED_AND_ROUTED: &str = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    required:\n      created: {default: '{{now}}'}\n      day: {default: '{{date}}'}\n    allowed_paths: {paths: ['zz-repair/log/**'], route: 'zz-repair/log/{{date}}/'}\n";

/// **Every route and default a repair plan fills comes from one clock
/// reading**: three clock routes and five clock defaults across three
/// documents read the host's clock once, read off its read account, in both
/// modes, and write one instant — each route's folder the day each `day`
/// default writes — each noting it is the repair's time.
#[test]
fn every_route_and_default_a_repair_plan_fills_comes_from_one_clock_reading() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-one-clock-reading",
        STAMPED_AND_ROUTED,
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
    assert_eq!(plan.operations.len(), 8, "{:?}", plan.operations);
    let days: BTreeSet<String> = plan
        .operations
        .iter()
        .filter_map(|operation| match &operation.kind {
            OperationKind::MoveDocument { to, .. } => Some(
                to.as_str()
                    .split('/')
                    .nth(2)
                    .expect("a day's folder")
                    .to_string(),
            ),
            OperationKind::SetFrontmatter { field, value, .. } if field == "day" => {
                Some(value.scalar_text().expect("a day"))
            }
            _ => None,
        })
        .collect();
    assert_eq!(days.len(), 1, "one day: {days:?}");
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

/// A repair of every document beneath [`FOLDER`], at any depth, in `mode`.
fn repairing_beneath(vault: &attach::Vault, mode: ApplyMode) -> RepairParams {
    RepairParams::new(address(vault), mode)
        .with_predicates([Predicate::path(format!("{FOLDER}**"))])
}

/// Every finding a validate reports beneath [`FOLDER`], at any depth.
fn findings_beneath(
    host: &attach::ServingHost,
    vault: &attach::Vault,
) -> Vec<norn_wire::FindingRow> {
    let request = ValidateParams::new(address(vault))
        .with_predicates([Predicate::path(format!("{FOLDER}**"))])
        .with_limit(1000);
    let answered = host.validate(&request).expect("a validate answers");
    let ValidateReport::Findings { page, .. } = answered.answer.report else {
        panic!("a validate answered a tally");
    };
    page.rows
}

/// The id of the one finding that the document at `name` is misplaced.
fn misplaced_of(host: &attach::ServingHost, vault: &attach::Vault, name: &str) -> u64 {
    let ids: Vec<u64> = findings_beneath(host, vault)
        .iter()
        .filter(|row| row.path == at(name) && row.kind == FindingKind::Misplaced)
        .map(|row| row.id)
        .collect();
    let [id] = ids.as_slice() else {
        panic!("one misplaced finding of {name}: {ids:?}");
    };
    *id
}

/// Whether a document stands at `name` beneath [`FOLDER`].
fn stands(vault: &attach::Vault, name: &str) -> bool {
    vault.path().join(FOLDER).join(name).exists()
}

/// The kinds of `plan`'s operations, in plan order, without their cascades.
fn kinds(plan: &ResolvedPlan) -> Vec<OperationKind> {
    plan.operations
        .iter()
        .map(|operation| operation.kind.clone())
        .collect()
}

/// One rule placing a task in `tasks/` beneath [`FOLDER`] and routing it
/// there.
const TASKED: &str = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    allowed_paths: {paths: ['zz-repair/tasks/**'], route: 'zz-repair/tasks/'}\n";

/// One rule declaring every kind of fix a repair applies: a plain default
/// (`status`), a default filled from a capture (`area`) and one from the
/// clock (`created`), a synonym on a scalar (`status`) and on a list
/// (`labels`), a forbidden field removed (`scratch`), one renamed
/// (`due_date`), one with no fix (`legacy`), and a route into the area's
/// `tasks/` folder; and a rule forbidding `noisy` there, so a route can be
/// refused.
const EVERY_FIX: &str = "version: 1\nfields:\n  labels: {type: text, shape: list}\nrules:\n  tasks:\n    match: {frontmatter: {type: task}, path: 'zz-repair/<area>/**'}\n    required:\n      status: {default: todo}\n      area: {default: '{{path.area}}'}\n      created: {default: '{{date}}'}\n    one_of:\n      status: {values: [todo, done], synonyms: {complete: done}}\n      labels: {values: [red, blue], synonyms: {crimson: red}}\n    forbidden:\n      scratch: remove\n      due_date: {rename_to: due}\n      legacy:\n    allowed_paths: {paths: ['zz-repair/*/tasks/**'], route: 'zz-repair/{{path.area}}/tasks/'}\n  quiet:\n    match: {path: 'zz-repair/home/tasks/**'}\n    forbidden:\n      noisy:\n";

/// A finding as the validator reports it, apart from its identity: where it
/// stands, its kind, its field and the value it judged.
type Reported = (String, String, Option<String>, Option<String>);

/// `row` as [`Reported`] names it, standing at `path`.
fn reported(row: &norn_wire::FindingRow, path: &str) -> Reported {
    (
        path.to_string(),
        row.kind.as_str().to_string(),
        row.target.clone(),
        row.value.as_ref().map(|value| value.text().to_string()),
    )
}

/// **A configured repair never writes what the validator flags.** Over a
/// schema declaring every kind of fix — a default, a templated default, a
/// synonym on a scalar and on a list, a forbidden field removed and one
/// renamed, and a route — a repair applies, and a validate afterwards
/// reports no finding on any field or path the repair wrote but the ones it
/// skipped, each still standing as it stood where the plan's moves left it:
/// what is skipped is skipped, not written. Each moved document's fixes are
/// written where it lands, and a document whose route the judge refuses at
/// its destination (`noisy` in `home/tasks/`) stays, its finding skipped.
#[test]
fn a_configured_repair_never_writes_what_the_validator_flags() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-never-writes-flagged",
        EVERY_FIX,
        &[
            (
                "work/a.md",
                b"---\ntype: task\nstatus: complete\nlabels: [crimson, bogus]\nscratch: x\ndue_date: soon\nlegacy: old\n---\n# A\n",
            ),
            ("home/b.md", b"---\ntype: task\n---\n# B\n"),
            ("work/tasks/c.md", b"---\ntype: task\nstatus: complete\narea: work\ncreated: '2026-01-01'\n---\n# C\n"),
            ("home/d.md", b"---\ntype: task\nnoisy: yes\nstatus: todo\narea: home\ncreated: '2026-01-01'\n---\n# D\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let before: BTreeMap<u64, norn_wire::FindingRow> = findings_beneath(&host, &vault)
        .into_iter()
        .map(|row| (row.id, row))
        .collect();

    let ApplyReport::Applied {
        plan, changeset, ..
    } = planned(host.repair(repairing_beneath(&vault, ApplyMode::Apply)))
    else {
        panic!("the repair applies");
    };
    assert_eq!(changeset, ChangesetOutcome::Committed);

    // Every kind of fix was made: two routes, the defaults, both synonyms,
    // the removal and the rename.
    let mut moved: BTreeMap<String, String> = BTreeMap::new();
    let mut wrote: BTreeSet<(String, Option<String>)> = BTreeSet::new();
    for operation in &plan.operations {
        match &operation.kind {
            OperationKind::MoveDocument { from, to } => {
                moved.insert(from.as_str().to_string(), to.as_str().to_string());
                wrote.insert((to.as_str().to_string(), None));
            }
            OperationKind::SetFrontmatter {
                target: WriteTarget::Path(at),
                field,
                ..
            }
            | OperationKind::RemoveFrontmatter {
                target: WriteTarget::Path(at),
                field,
            } => {
                wrote.insert((at.as_str().to_string(), Some(field.clone())));
            }
            other => panic!("a repair made {other:?}"),
        }
    }
    let to = |name: &str| format!("{FOLDER}{name}");
    assert_eq!(
        moved,
        BTreeMap::from([
            (to("home/b.md"), to("home/tasks/b.md")),
            (to("work/a.md"), to("work/tasks/a.md")),
        ])
    );
    for (name, field) in [
        ("work/tasks/a.md", "status"),
        ("work/tasks/a.md", "labels"),
        ("work/tasks/a.md", "scratch"),
        ("work/tasks/a.md", "due"),
        ("work/tasks/a.md", "due_date"),
        ("work/tasks/a.md", "area"),
        ("work/tasks/a.md", "created"),
        ("home/tasks/b.md", "status"),
        ("work/tasks/c.md", "status"),
    ] {
        assert!(
            wrote.contains(&(to(name), Some(field.to_string()))),
            "`{field}` of {name} was not written: {wrote:?}"
        );
    }

    // What was skipped, where it stands after the plan's moves.
    let skipped: BTreeSet<Reported> = provenance(&plan)
        .skipped
        .iter()
        .map(|skipped| {
            let row = &before[&skipped.finding];
            let path = moved
                .get(row.path.as_str())
                .cloned()
                .unwrap_or_else(|| row.path.as_str().to_string());
            reported(row, &path)
        })
        .collect();
    let expected_skips: BTreeSet<Reported> = [
        (
            "work/tasks/a.md",
            "field/not-one-of",
            Some("labels"),
            Some("bogus"),
        ),
        (
            "work/tasks/a.md",
            "field/forbidden",
            Some("legacy"),
            Some("old"),
        ),
        ("home/d.md", "document/misplaced", None, None),
    ]
    .into_iter()
    .map(|(name, kind, field, value)| {
        (
            to(name),
            kind.to_string(),
            field.map(str::to_string),
            value.map(str::to_string),
        )
    })
    .collect();
    assert_eq!(skipped, expected_skips);

    // The validator flags nothing the repair wrote: every finding standing
    // after it is one it skipped, unchanged, and every one it skipped still
    // stands.
    let after: BTreeSet<Reported> = findings_beneath(&host, &vault)
        .iter()
        .map(|row| reported(row, row.path.as_str()))
        .collect();
    for finding in &after {
        let (path, _, field, _) = finding;
        let on_written =
            wrote.contains(&(path.clone(), field.clone())) || wrote.contains(&(path.clone(), None));
        assert!(
            skipped.contains(finding),
            "the validator flags {finding:?}, which the repair did not skip (on what it wrote: \
             {on_written})"
        );
    }
    assert_eq!(
        after, skipped,
        "a skipped finding no longer stands as it stood"
    );
    assert!(
        written(&vault, "work/tasks/a.md").contains("labels: [red, bogus]"),
        "{}",
        written(&vault, "work/tasks/a.md")
    );
}

/// **A misplaced document whose rules declare no route is skipped as having no
/// declared fix, with no note.**
#[test]
fn a_misplaced_document_no_rule_routes_is_skipped_with_no_note() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-misplaced-unrouted",
        "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    allowed_paths: {paths: ['zz-repair/tasks/**']}\n",
        &[("loose/a.md", b"---\ntype: task\n---\n")],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let misplaced = misplaced_of(&host, &vault, "loose/a.md");
    let plan = previewed(host.repair(repairing_beneath(&vault, ApplyMode::Preview)));
    assert_eq!(
        provenance(&plan).skipped,
        vec![SkippedFinding::new(misplaced, SkipReason::NoDeclaredFix)]
    );
}

/// One rule placing a task in its area's `tasks/` folder, the area the
/// segment beneath [`FOLDER`] it stands in, and routing it there.
const AREAS: &str = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}, path: 'zz-repair/<area>/**'}\n    allowed_paths: {paths: ['zz-repair/*/tasks/**'], route: 'zz-repair/{{path.area}}/tasks/'}\n";

/// **A route moves a misplaced document into its rule's route folder,
/// keeping its file name, a capture filled from where it stands**: the
/// preview's plan is one move cited at the declared level, its apply moves
/// the document, and the finding no longer stands.
#[test]
fn a_route_moves_a_misplaced_document_to_its_route_folder_with_a_capture_filled() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-route",
        AREAS,
        &[("work/a.md", b"---\ntype: task\n---\n# A\n")],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let misplaced = misplaced_of(&host, &vault, "work/a.md");

    let plan = previewed(host.repair(repairing_beneath(&vault, ApplyMode::Preview)));

    assert_eq!(
        kinds(&plan),
        vec![OperationKind::move_document(
            at("work/a.md"),
            at("work/tasks/a.md")
        )]
    );
    assert_eq!(provenance(&plan).citations, vec![citing(1, &[misplaced])]);
    assert!(provenance(&plan).skipped.is_empty());

    let ApplyReport::Applied { changeset, .. } =
        planned(host.repair(repairing_beneath(&vault, ApplyMode::Apply)))
    else {
        panic!("the repair applies");
    };
    assert_eq!(changeset, ChangesetOutcome::Committed);
    assert!(!stands(&vault, "work/a.md"));
    assert_eq!(
        written(&vault, "work/tasks/a.md"),
        "---\ntype: task\n---\n# A\n"
    );
    assert!(findings_beneath(&host, &vault).is_empty());
}

/// **A route reading a capture its rule's `match.path` binds several ways
/// skips as an ambiguous capture**, noting two of the bindings, and moves
/// nothing.
#[test]
fn a_route_reading_a_capture_bound_several_ways_skips_as_ambiguous_capture() {
    let schema = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}, path: 'zz-repair/**/<area>/**'}\n    allowed_paths: {paths: ['zz-repair/**/tasks/**'], route: 'zz-repair/{{path.area}}/tasks/'}\n";
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-route-ambiguous-capture",
        schema,
        &[("red/blue/a.md", b"---\ntype: task\n---\n")],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let misplaced = misplaced_of(&host, &vault, "red/blue/a.md");

    let plan = previewed(host.repair(repairing_beneath(&vault, ApplyMode::Preview)));

    assert!(plan.operations.is_empty());
    let (skipped, note) = the_one_skip(&plan);
    assert_eq!(
        (skipped.finding, skipped.reason),
        (misplaced, SkipReason::AmbiguousCapture)
    );
    assert!(
        note.contains("{area=red}") && note.contains("{area=blue}"),
        "{note}"
    );
}

/// **Co-selecting rules routing a document to different folders skip it as
/// a tie**, with each destination and its rule.
#[test]
fn disagreeing_routes_skip_as_a_tie() {
    let schema = "version: 1\nrules:\n  a-rule:\n    match: {frontmatter: {type: task}}\n    allowed_paths: {paths: ['zz-repair/a/**', 'zz-repair/b/**'], route: 'zz-repair/a/'}\n  b-rule:\n    match: {frontmatter: {type: task}}\n    allowed_paths: {paths: ['zz-repair/a/**', 'zz-repair/b/**'], route: 'zz-repair/b/'}\n";
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-route-tie",
        schema,
        &[("c/x.md", b"---\ntype: task\n---\n")],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let misplaced = misplaced_of(&host, &vault, "c/x.md");

    let plan = previewed(host.repair(repairing_beneath(&vault, ApplyMode::Preview)));

    assert!(plan.operations.is_empty());
    assert_eq!(
        provenance(&plan).skipped,
        vec![
            SkippedFinding::new(misplaced, SkipReason::Tie).with_candidates(candidates(&[
                ("zz-repair/a/x.md", "a-rule"),
                ("zz-repair/b/x.md", "b-rule")
            ]))
        ]
    );
}

/// **A route whose destination is taken skips as destination taken, and of
/// two routes of one batch to one destination the first in batch order
/// moves and the later skips as destination taken**: `loose/b.md` would
/// land on the `tasks/b.md` that stands, and `x/a.md` and `y/a.md` both
/// route to `tasks/a.md`.
#[test]
fn a_taken_destination_and_the_later_of_two_routes_to_one_skip_as_destination_taken() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-route-taken",
        TASKED,
        &[
            ("loose/b.md", b"---\ntype: task\n---\n# Loose\n"),
            ("tasks/b.md", b"---\ntype: task\n---\n# Placed\n"),
            ("x/a.md", b"---\ntype: task\n---\n# X\n"),
            ("y/a.md", b"---\ntype: task\n---\n# Y\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let loose = misplaced_of(&host, &vault, "loose/b.md");
    let y = misplaced_of(&host, &vault, "y/a.md");

    let plan = applied_leaving_only_its_skips(&host, &vault);

    assert_eq!(
        kinds(&plan),
        vec![OperationKind::move_document(at("x/a.md"), at("tasks/a.md"))]
    );
    let skipped: Vec<(u64, SkipReason)> = provenance(&plan)
        .skipped
        .iter()
        .map(|skipped| (skipped.finding, skipped.reason))
        .collect();
    assert_eq!(
        skipped,
        [
            (loose, SkipReason::DestinationTaken),
            (y, SkipReason::DestinationTaken)
        ]
    );
    assert_eq!(
        written(&vault, "tasks/b.md"),
        "---\ntype: task\n---\n# Placed\n"
    );
    assert_eq!(written(&vault, "tasks/a.md"), "---\ntype: task\n---\n# X\n");
    assert!(stands(&vault, "loose/b.md") && stands(&vault, "y/a.md"));
}

/// A rule routing a `p` into `tasks/`, and one routing a `q` standing in
/// `q/<area>/` into `tasks/<area>/`, so one route's destination can be a
/// folder another names as a document.
const NESTED_ROUTES: &str = "version: 1\nrules:\n  ps:\n    match: {frontmatter: {type: p}}\n    allowed_paths: {paths: ['zz-repair/tasks/**'], route: 'zz-repair/tasks/'}\n  qs:\n    match: {frontmatter: {type: q}, path: 'zz-repair/q/<area>/**'}\n    allowed_paths: {paths: ['zz-repair/tasks/**'], route: 'zz-repair/tasks/{{path.area}}/'}\n";

/// A case of [`NESTED_ROUTES`]: its label, its documents, the document the
/// plan moves, and the document whose route skips.
type Nested<'a> = (&'a str, &'a [(&'a str, &'a [u8])], &'a str, &'a str);

/// **A route whose destination lies above or beneath another route's of the
/// batch, or beneath a document that stands, skips as destination taken**,
/// in either batch order, and the earlier route moves: `tasks/a.md` and
/// `tasks/a.md/x.md` cannot both be documents.
#[test]
fn a_route_above_or_beneath_another_destination_skips_as_destination_taken() {
    let cases: [Nested<'_>; 3] = [
        (
            "host-repair-route-above-a-claimed-destination",
            &[
                ("q/a.md/x.md", b"---\ntype: q\n---\n"),
                ("r/a.md", b"---\ntype: p\n---\n"),
            ],
            "q/a.md/x.md",
            "r/a.md",
        ),
        (
            "host-repair-route-beneath-a-claimed-destination",
            &[
                ("b/a.md", b"---\ntype: p\n---\n"),
                ("q/a.md/x.md", b"---\ntype: q\n---\n"),
            ],
            "b/a.md",
            "q/a.md/x.md",
        ),
        (
            "host-repair-route-beneath-a-standing-document",
            &[
                ("q/a.md/x.md", b"---\ntype: q\n---\n"),
                ("tasks/a.md", b"---\ntype: p\n---\n"),
            ],
            "",
            "q/a.md/x.md",
        ),
    ];
    for (label, documents, moves, taken) in cases {
        let (_sandbox, vault, host) = a_vault_under(label, NESTED_ROUTES, documents);
        let _lease = attach::attach_and_wait(&host, vault.name());
        let skipped_route = misplaced_of(&host, &vault, taken);

        let plan = applied_leaving_only_its_skips(&host, &vault);

        let moved: Vec<String> = plan
            .operations
            .iter()
            .filter_map(|operation| match &operation.kind {
                OperationKind::MoveDocument { from, .. } => Some(from.as_str().to_string()),
                _ => None,
            })
            .collect();
        let expected: Vec<String> = if moves.is_empty() {
            Vec::new()
        } else {
            vec![format!("{FOLDER}{moves}")]
        };
        assert_eq!(moved, expected, "{label}");
        let skipped: Vec<(u64, SkipReason)> = provenance(&plan)
            .skipped
            .iter()
            .map(|skipped| (skipped.finding, skipped.reason))
            .collect();
        assert_eq!(
            skipped,
            [(skipped_route, SkipReason::DestinationTaken)],
            "{label}"
        );
    }
}

/// A rule routing a task into its area's `tasks/` folder by the area as it
/// is spelled, and one routing a note there by its area slugged.
const FILLED_ROUTES: &str = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}, path: 'zz-repair/<area>/**'}\n    allowed_paths: {paths: ['zz-repair/*/tasks/**'], route: 'zz-repair/{{path.area}}/tasks/'}\n  notes:\n    match: {frontmatter: {type: note}, path: 'zz-repair/<area>/**'}\n    allowed_paths: {paths: ['zz-repair/*/tasks/**'], route: 'zz-repair/{{path.area|slug}}/tasks/'}\n";

/// **A route that fills to no folder a document can be moved into skips as
/// one the judge would refuse, noting the route and why**, rather than
/// moving or failing: a capture holding `:`, which no route writes, and one
/// slugging to nothing, which would leave its segment empty.
#[test]
fn a_route_filling_to_no_folder_skips_as_judge_would_refuse() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-route-unfillable",
        FILLED_ROUTES,
        &[
            ("a:b/a.md", b"---\ntype: task\n---\n"),
            ("!!!/n.md", b"---\ntype: note\n---\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let colon = misplaced_of(&host, &vault, "a:b/a.md");
    let empty = misplaced_of(&host, &vault, "!!!/n.md");

    let plan = applied_leaving_only_its_skips(&host, &vault);

    assert!(plan.operations.is_empty(), "{:?}", plan.operations);
    let skipped: BTreeMap<u64, (SkipReason, String)> = provenance(&plan)
        .skipped
        .iter()
        .map(|skipped| {
            (
                skipped.finding,
                (
                    skipped.reason,
                    skipped.note.clone().expect("the skip carries a note"),
                ),
            )
        })
        .collect();
    let (reason, note) = &skipped[&colon];
    assert_eq!(*reason, SkipReason::JudgeWouldRefuse);
    assert!(
        note.contains("zz-repair/{{path.area}}/tasks/") && note.contains(':'),
        "{note}"
    );
    let (reason, note) = &skipped[&empty];
    assert_eq!(*reason, SkipReason::JudgeWouldRefuse);
    assert!(note.contains("{{path.area|slug}}"), "{note}");
}

/// **A route the judge refuses at its destination skips as one the judge
/// would refuse**, with its candidate, and nothing moves: the rule on
/// `tasks/` forbids the `scratch` the document holds. **One bringing the
/// document under a rule requiring fields it lacks skips as bringing in
/// required fields, naming each field and its declared default.**
#[test]
fn a_route_the_judge_refuses_at_its_destination_skips_as_judge_would_refuse() {
    let schema = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    allowed_paths: {paths: ['zz-repair/tasks/**'], route: 'zz-repair/tasks/'}\n  shelved:\n    match: {path: 'zz-repair/tasks/**'}\n    forbidden:\n      scratch:\n  owned:\n    match: {path: 'zz-repair/tasks/**', frontmatter: {kind: owned}}\n    required:\n      owner: {default: me}\n";
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-route-refused",
        schema,
        &[
            ("loose/a.md", b"---\ntype: task\nscratch: x\n---\n"),
            ("loose/b.md", b"---\ntype: task\nkind: owned\n---\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let refused_route = misplaced_of(&host, &vault, "loose/a.md");
    let owned = misplaced_of(&host, &vault, "loose/b.md");
    let before = tree_bytes(vault.path());

    let ApplyReport::Applied { plan, .. } =
        planned(host.repair(repairing_beneath(&vault, ApplyMode::Apply)))
    else {
        panic!("the repair applies");
    };

    assert!(plan.operations.is_empty());
    let fields = RequiredFieldHead::new(
        [RequiredField::new("owner").with_default(norn_store::value_head("me"))],
        1,
    )
    .expect("a head");
    assert_eq!(
        provenance(&plan).skipped,
        vec![
            SkippedFinding::new(refused_route, SkipReason::JudgeWouldRefuse)
                .with_candidates(candidates(&[("zz-repair/tasks/a.md", "tasks")])),
            SkippedFinding::new(owned, SkipReason::BringsInRequiredFields)
                .with_required_fields(fields)
                .with_candidates(candidates(&[("zz-repair/tasks/b.md", "tasks")])),
        ]
    );
    assert_eq!(tree_bytes(vault.path()), before, "a refused route wrote");
}

/// **A route out of a rule's area drops that rule's selected
/// `required-missing` finding**: the inbox rule would fill `triage`, but it
/// does not select the document where the route takes it, so the finding is
/// in neither the operations nor the skipped findings, and no `triage` is
/// written.
#[test]
fn a_route_out_of_a_rules_area_drops_its_selected_required_missing_finding() {
    let schema = "version: 1\nrules:\n  inbox:\n    match: {path: 'zz-repair/inbox/**'}\n    required:\n      triage: {default: later}\n  tasks:\n    match: {frontmatter: {type: task}}\n    allowed_paths: {paths: ['zz-repair/tasks/**'], route: 'zz-repair/tasks/'}\n";
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-route-drops",
        schema,
        &[("inbox/a.md", b"---\ntype: task\n---\n")],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let misplaced = misplaced_of(&host, &vault, "inbox/a.md");
    let kinds_selected: BTreeSet<&str> = findings_beneath(&host, &vault)
        .iter()
        .map(|row| row.kind.as_str())
        .collect();
    assert_eq!(
        kinds_selected,
        BTreeSet::from(["document/misplaced", "field/required-missing"]),
        "both findings are selected"
    );

    let plan = applied_leaving_only_its_skips(&host, &vault);

    assert_eq!(
        kinds(&plan),
        vec![OperationKind::move_document(
            at("inbox/a.md"),
            at("tasks/a.md")
        )]
    );
    assert_eq!(provenance(&plan).citations, vec![citing(1, &[misplaced])]);
    assert!(provenance(&plan).skipped.is_empty());
    assert_eq!(written(&vault, "tasks/a.md"), "---\ntype: task\n---\n");
    assert!(findings_beneath(&host, &vault).is_empty());
}

/// **A route's move rewrites every link the text layer can respell, and
/// every other link is listed with the reason a Layer 4 move's forecast
/// gives it; an unrespellable link never holds the route back.** The
/// holder's Markdown link to the moved document is respelled; its
/// frontmatter wikilink in a flow sequence has no bytes of its own to
/// respell, so the repair's preview forecasts it skipped as unplaced, as a
/// `move` of the document forecasts it, and the apply leaves it as written.
#[test]
fn a_routes_move_rewrites_every_respellable_link_and_lists_the_rest_as_a_move_forecasts_them() {
    let holder = "---\nsee: [\"[[zz-repair/loose/a]]\"]\n---\nUp: [a](loose/a.md)\n";
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-route-cascade",
        TASKED,
        &[
            ("loose/a.md", b"---\ntype: task\n---\n# A\n"),
            ("h.md", holder.as_bytes()),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());

    let ApplyReport::Previewed { plan, forecast, .. } =
        planned(host.repair(repairing_beneath(&vault, ApplyMode::Preview)))
    else {
        panic!("a preview answers a preview");
    };
    let [route] = plan.operations.as_slice() else {
        panic!("one route: {:?}", plan.operations);
    };
    assert_eq!(
        route.kind,
        OperationKind::move_document(at("loose/a.md"), at("tasks/a.md"))
    );
    assert!(
        !route.cascade.is_empty(),
        "the route's move carries its cascade"
    );
    let unplaced = norn_wire::LinkAdvisory::skipped_unplaced(norn_wire::LinkKey::new(
        at("h.md"),
        norn_wire::LinkFamily::Wikilink,
        "zz-repair/loose/a",
    ));
    assert_eq!(forecast.links, vec![unplaced]);

    // The same move, asked of `move`, forecasts the same links.
    let ApplyReport::Previewed {
        forecast: moved, ..
    } = planned(host.move_path(norn_wire::MoveParams::new(
        address(&vault),
        ApplyMode::Preview,
        norn_wire::MoveSubject::document(at("loose/a.md"), at("tasks/a.md")),
    )))
    else {
        panic!("a preview answers a preview");
    };
    assert_eq!(moved.links, forecast.links);

    planned(host.repair(repairing_beneath(&vault, ApplyMode::Apply)));
    assert_eq!(
        written(&vault, "h.md"),
        "---\nsee: [\"[[zz-repair/loose/a]]\"]\n---\nUp: [a](tasks/a.md)\n"
    );
    assert!(stands(&vault, "tasks/a.md"));
}

/// **An unrespellable link never holds a route back, even in a field a rule
/// reads by value**: the hub's closed `up` holds the link to the moved
/// document in a flow list, which no cascade can respell, so the route moves
/// and its forecast advises on the link as unplaced, as a `move` of the
/// document forecasts it.
#[test]
fn an_unrespellable_link_in_a_field_read_by_value_never_holds_a_route_back() {
    let schema = "version: 1\nfields:\n  up: {type: text, shape: list}\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    allowed_paths: {paths: ['zz-repair/tasks/**'], route: 'zz-repair/tasks/'}\n  hubs:\n    match: {frontmatter: {type: hub}}\n    one_of:\n      up: {values: ['[[zz-repair/loose/a]]']}\n";
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-route-unplaced-judged",
        schema,
        &[
            ("loose/a.md", b"---\ntype: task\n---\n"),
            (
                "h.md",
                b"---\ntype: hub\nup: [\"[[zz-repair/loose/a]]\"]\n---\n",
            ),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let unplaced = norn_wire::LinkAdvisory::skipped_unplaced(norn_wire::LinkKey::new(
        at("h.md"),
        norn_wire::LinkFamily::Wikilink,
        "zz-repair/loose/a",
    ));
    let ApplyReport::Previewed {
        forecast: moved, ..
    } = planned(host.move_path(norn_wire::MoveParams::new(
        address(&vault),
        ApplyMode::Preview,
        norn_wire::MoveSubject::document(at("loose/a.md"), at("tasks/a.md")),
    )))
    else {
        panic!("a preview answers a preview");
    };
    assert_eq!(moved.links, std::slice::from_ref(&unplaced));

    let ApplyReport::Previewed { plan, forecast, .. } =
        planned(host.repair(repairing_beneath(&vault, ApplyMode::Preview)))
    else {
        panic!("a preview answers a preview");
    };

    assert_eq!(
        kinds(&plan),
        [OperationKind::move_document(
            at("loose/a.md"),
            at("tasks/a.md")
        )]
    );
    assert!(provenance(&plan).skipped.is_empty(), "{plan:?}");
    assert_eq!(forecast.links, [unplaced]);
}

/// **A holder is judged at the state its own fixes compose where it
/// stands**: the hub reads `up` by value only once its own default fills
/// `phase`, which brings it under the rule limiting `up`, so the route that
/// would respell `up` skips while the hub's default is filled.
#[test]
fn a_holders_own_fix_bringing_its_field_under_a_rule_reading_it_by_value_skips_the_route() {
    let schema = format!(
        "{TASKED}  hubs:\n    match: {{frontmatter: {{type: hub}}}}\n    required:\n      phase: {{default: judged}}\n  judged:\n    match: {{frontmatter: {{phase: judged}}}}\n    max_length:\n      up: 100\n"
    );
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-route-holder-own-fix",
        &schema,
        &[
            ("loose/a.md", b"---\ntype: task\n---\n"),
            (
                "h.md",
                b"---\ntype: hub\nup: '[[zz-repair/loose/a]]'\n---\n",
            ),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());

    let plan = previewed(host.repair(repairing_beneath(&vault, ApplyMode::Preview)));

    assert_eq!(kinds(&plan), [setting(1, "h.md", "phase", "judged").kind]);
    let skipped: Vec<SkipReason> = provenance(&plan)
        .skipped
        .iter()
        .map(|skip| skip.reason)
        .collect();
    assert_eq!(skipped, [SkipReason::RespellsAJudgedLink]);
}

/// **A route its respell check skips claims no destination**: `x/a.md`'s
/// route would respell the hub's limited `up`, so it skips, and `y/a.md`'s
/// route to the same destination moves.
#[test]
fn a_route_its_respell_check_skips_claims_no_destination() {
    let schema = format!(
        "{TASKED}  hubs:\n    match: {{frontmatter: {{type: hub}}}}\n    max_length:\n      up: 100\n"
    );
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-route-skipped-claims-none",
        &schema,
        &[
            ("x/a.md", b"---\ntype: task\n---\n"),
            ("y/a.md", b"---\ntype: task\n---\n"),
            ("h.md", b"---\ntype: hub\nup: '[[zz-repair/x/a]]'\n---\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());

    let plan = previewed(host.repair(repairing_beneath(&vault, ApplyMode::Preview)));

    assert_eq!(
        kinds(&plan),
        [OperationKind::move_document(at("y/a.md"), at("tasks/a.md"))]
    );
    let skipped: Vec<SkipReason> = provenance(&plan)
        .skipped
        .iter()
        .map(|skip| skip.reason)
        .collect();
    assert_eq!(skipped, [SkipReason::RespellsAJudgedLink]);
}

/// A rule routing a task into `tasks/`, and a hub rule closing a hub's `up`
/// link field over the link to `loose/a.md` as it is written now.
const CLOSED_UP: &str = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    required:\n      status: {default: todo}\n    allowed_paths: {paths: ['zz-repair/tasks/**'], route: 'zz-repair/tasks/'}\n  hubs:\n    match: {frontmatter: {type: hub}}\n    one_of:\n      up: {values: ['[[zz-repair/loose/a]]']}\n";

/// **A route whose cascade may respell a frontmatter link in a field a rule
/// reads by value skips as respelling a judged link**, with its destination,
/// noting the holder and the field, and the plan stays applicable: moving
/// `loose/a.md` would respell the hub's closed `up`, so nothing moves and
/// the hub stands as written.
#[test]
fn a_route_whose_cascade_may_respell_a_judged_frontmatter_link_skips() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-route-respells-a-judged-link",
        CLOSED_UP,
        &[
            ("loose/a.md", b"---\ntype: task\nstatus: todo\n---\n# A\n"),
            (
                "h.md",
                b"---\ntype: hub\nup: \"[[zz-repair/loose/a]]\"\n---\n",
            ),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let misplaced = misplaced_of(&host, &vault, "loose/a.md");

    let plan = applied_leaving_only_its_skips(&host, &vault);

    assert!(plan.operations.is_empty(), "{:?}", plan.operations);
    let (skipped, note) = the_one_skip(&plan);
    assert_eq!(
        skipped,
        SkippedFinding::new(misplaced, SkipReason::RespellsAJudgedLink)
            .with_candidates(candidates(&[("zz-repair/tasks/a.md", "tasks")]))
    );
    assert!(
        note.contains("zz-repair/h.md") && note.contains("`up`"),
        "{note}"
    );
    assert_eq!(
        written(&vault, "h.md"),
        "---\ntype: hub\nup: \"[[zz-repair/loose/a]]\"\n---\n"
    );
}

/// **A route whose cascade may respell the moved document's own judged link
/// skips**, noting the document and the field.
#[test]
fn a_route_whose_cascade_may_respell_the_moved_documents_own_judged_link_skips() {
    let schema = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    allowed_paths: {paths: ['zz-repair/tasks/**'], route: 'zz-repair/tasks/'}\n    one_of:\n      me: {values: ['[[zz-repair/loose/a]]', '[[zz-repair/tasks/a]]']}\n";
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-route-self-respell",
        schema,
        &[(
            "loose/a.md",
            b"---\ntype: task\nme: \"[[zz-repair/loose/a]]\"\n---\n# A\n",
        )],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let misplaced = misplaced_of(&host, &vault, "loose/a.md");

    let plan = applied_leaving_only_its_skips(&host, &vault);

    assert!(plan.operations.is_empty(), "{:?}", plan.operations);
    let (skipped, note) = the_one_skip(&plan);
    assert_eq!(
        (skipped.finding, skipped.reason),
        (misplaced, SkipReason::RespellsAJudgedLink)
    );
    assert!(
        note.contains("zz-repair/loose/a.md") && note.contains("`me`"),
        "{note}"
    );
    assert!(stands(&vault, "loose/a.md"));
}

/// **A route its respell check skips leaves the batch's other fixes
/// applied**: the routed document's own default is filled where it stands,
/// since it no longer moves, and another document's default is filled as
/// before; only the route's finding stands after.
#[test]
fn a_route_its_respell_check_skips_leaves_the_batchs_other_fixes_applied() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-route-respell-skipped-others-stand",
        CLOSED_UP,
        &[
            ("loose/a.md", b"---\ntype: task\n---\n# A\n"),
            (
                "h.md",
                b"---\ntype: hub\nup: \"[[zz-repair/loose/a]]\"\n---\n",
            ),
            ("tasks/c.md", b"---\ntype: task\n---\n# C\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let misplaced = misplaced_of(&host, &vault, "loose/a.md");

    let plan = applied_leaving_only_its_skips(&host, &vault);

    assert_eq!(
        plan.operations,
        vec![
            setting(1, "loose/a.md", "status", "todo"),
            setting(2, "tasks/c.md", "status", "todo"),
        ]
    );
    let (skipped, _) = the_one_skip(&plan);
    assert_eq!(
        (skipped.finding, skipped.reason),
        (misplaced, SkipReason::RespellsAJudgedLink)
    );
    assert_eq!(
        written(&vault, "loose/a.md"),
        "---\ntype: task\nstatus: todo\n---\n# A\n"
    );
}

/// A rule routing a task into `tasks/`, and a hub rule closing a hub's `up`
/// link field over the link to `loose/a.md` as it is written now.
const HUB_UP: &str = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    allowed_paths: {paths: ['zz-repair/tasks/**'], route: 'zz-repair/tasks/'}\n  hubs:\n    match: {frontmatter: {type: hub}}\n    one_of:\n      up: {values: ['[[zz-repair/loose/a]]']}\n";

/// **Of two routes whose cascades rewrite one hub, only the one respelling a
/// judged link skips, and the other's cascade lands**: moving `loose/a.md`
/// would respell the hub's closed `up`, moving `loose/b.md` respells its
/// open `down` and its body's links, so `b` moves, the hub respelled, and
/// `a` skips, noting the hub and `up`; a bare backlink in the closed field
/// holds no route back.
#[test]
fn of_two_routes_rewriting_one_hub_only_the_one_respelling_a_judged_link_skips() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-two-routes-one-hub",
        HUB_UP,
        &[
            (
                "h.md",
                b"---\ntype: hub\nup: \"[[zz-repair/loose/a]]\"\ndown: \"[[zz-repair/loose/b]]\"\n---\nSee [[zz-repair/loose/b]] and [b](loose/b.md).\n",
            ),
            ("loose/a.md", b"---\ntype: task\n---\n# A\n"),
            ("loose/b.md", b"---\ntype: task\n---\n# B\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let a = misplaced_of(&host, &vault, "loose/a.md");
    let b = misplaced_of(&host, &vault, "loose/b.md");

    let plan = applied_leaving_only_its_skips(&host, &vault);

    assert_eq!(
        kinds(&plan),
        vec![OperationKind::move_document(
            at("loose/b.md"),
            at("tasks/b.md")
        )]
    );
    assert_eq!(provenance(&plan).citations, vec![citing(1, &[b])]);
    let (skipped, note) = the_one_skip(&plan);
    assert_eq!(
        skipped,
        SkippedFinding::new(a, SkipReason::RespellsAJudgedLink)
            .with_candidates(candidates(&[("zz-repair/tasks/a.md", "tasks")]))
    );
    assert!(
        note.contains("zz-repair/h.md") && note.contains("`up`"),
        "{note}"
    );
    assert_eq!(
        written(&vault, "h.md"),
        "---\ntype: hub\nup: \"[[zz-repair/loose/a]]\"\ndown: \"[[tasks/b]]\"\n---\nSee [[tasks/b]] and [b](tasks/b.md).\n"
    );
}

/// **A preview's route plan lands through `apply`**: the resolved plan a
/// repair's preview answers — two routes, their cascades, and a routed
/// document's fix where it lands — applies as sent, and leaves no finding.
/// (A preview and an apply of one repair plan the same operations wherever a
/// case holds the repair to what it guarantees, the routes' cases here
/// among them.)
#[test]
fn a_previews_route_plan_lands_through_apply() {
    let schema = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    required:\n      status: {default: todo}\n    allowed_paths: {paths: ['zz-repair/tasks/**'], route: 'zz-repair/tasks/'}\n";
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-route-preview-applies",
        schema,
        &[
            ("loose/a.md", b"---\ntype: task\n---\n# A\n"),
            ("loose/b.md", b"---\ntype: task\nstatus: done\n---\n# B\n"),
            (
                "h.md",
                b"---\nsee: \"[[zz-repair/loose/a]]\"\n---\nUp [[zz-repair/loose/b]]\n",
            ),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let preview = previewed(host.repair(repairing_beneath(&vault, ApplyMode::Preview)));
    assert_eq!(
        kinds(&preview),
        vec![
            OperationKind::move_document(at("loose/a.md"), at("tasks/a.md")),
            OperationKind::set_frontmatter(
                WriteTarget::path(at("tasks/a.md")),
                "status",
                AuthoredValue::string("todo"),
            ),
            OperationKind::move_document(at("loose/b.md"), at("tasks/b.md")),
        ]
    );

    let ApplyReport::Applied { changeset, .. } = planned(host.apply(ApplyParams::new(
        ApplyMode::Apply,
        PlanDocument::resolved(preview),
    ))) else {
        panic!("the preview's plan applies");
    };
    assert_eq!(changeset, ChangesetOutcome::Committed);
    assert_eq!(
        written(&vault, "tasks/a.md"),
        "---\ntype: task\nstatus: todo\n---\n# A\n"
    );
    assert_eq!(
        written(&vault, "h.md"),
        "---\nsee: \"[[tasks/a]]\"\n---\nUp [[tasks/b]]\n"
    );
    assert!(!stands(&vault, "loose/a.md") && !stands(&vault, "loose/b.md"));
    assert!(findings_beneath(&host, &vault).is_empty());
}

/// **A repair plan routing by no route that reads the clock reads it not at
/// all.**
#[test]
fn a_repair_plan_routing_by_no_clock_route_reads_no_clock() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-route-no-clock-reading",
        TASKED,
        &[("loose/a.md", b"---\ntype: task\n---\n")],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let account = host.read_evidence();
    let plan = previewed(host.repair(repairing_beneath(&vault, ApplyMode::Preview)));
    assert_eq!(plan.operations.len(), 1);
    assert_eq!(host.read_evidence().since(account).clock_reads, 0);
}

/// Apply the repair of every document beneath [`FOLDER`], and hold it to
/// what a configured repair guarantees: it answers a preview and an apply
/// alike, its apply commits, every finding standing after it is one it
/// skipped, standing where the plan's moves left it, and every finding it
/// fixed or dropped no longer stands. The applied plan.
fn applied_leaving_only_its_skips(
    host: &attach::ServingHost,
    vault: &attach::Vault,
) -> ResolvedPlan {
    let before: BTreeMap<u64, norn_wire::FindingRow> = findings_beneath(host, vault)
        .into_iter()
        .map(|row| (row.id, row))
        .collect();
    let preview = previewed(host.repair(repairing_beneath(vault, ApplyMode::Preview)));
    let ApplyReport::Applied {
        plan, changeset, ..
    } = planned(host.repair(repairing_beneath(vault, ApplyMode::Apply)))
    else {
        panic!("the repair applies");
    };
    assert_eq!(changeset, ChangesetOutcome::Committed);
    assert_eq!(
        preview.operations, plan.operations,
        "preview and apply differ"
    );
    let moved: BTreeMap<String, String> = plan
        .operations
        .iter()
        .filter_map(|operation| match &operation.kind {
            OperationKind::MoveDocument { from, to } => {
                Some((from.as_str().to_string(), to.as_str().to_string()))
            }
            _ => None,
        })
        .collect();
    let standing_at = |row: &norn_wire::FindingRow| {
        let path = moved
            .get(row.path.as_str())
            .cloned()
            .unwrap_or_else(|| row.path.as_str().to_string());
        reported(row, &path)
    };
    let skipped: BTreeSet<Reported> = provenance(&plan)
        .skipped
        .iter()
        .map(|skipped| standing_at(&before[&skipped.finding]))
        .collect();
    let after: BTreeSet<Reported> = findings_beneath(host, vault)
        .iter()
        .map(|row| reported(row, row.path.as_str()))
        .collect();
    for finding in &after {
        assert!(
            skipped.contains(finding),
            "the validator flags {finding:?}, which the repair did not skip: {plan:?}"
        );
    }
    let skipped_ids: BTreeSet<u64> = provenance(&plan)
        .skipped
        .iter()
        .map(|skipped| skipped.finding)
        .collect();
    for (id, row) in &before {
        if !skipped_ids.contains(id) {
            assert!(
                !after.contains(&standing_at(row)),
                "finding {id}, fixed or dropped, still stands: {row:?}"
            );
        }
    }
    plan
}

/// The one skip of `plan`, its note taken off so the rest compares whole,
/// and the note.
fn the_one_skip(plan: &ResolvedPlan) -> (SkippedFinding, String) {
    let [skipped] = provenance(plan).skipped.as_slice() else {
        panic!("one skip: {:?}", provenance(plan).skipped);
    };
    let mut skipped = skipped.clone();
    let note = skipped.note.take().expect("the skip carries a note");
    (skipped, note)
}

/// **A fix never brings back what an earlier fix took away**: removing `k`
/// fixes its finding, so renaming `z_k` onto `k` would make the document
/// forbid `k` again, and skips as one the judge would refuse, with its
/// candidate and its value.
#[test]
fn a_rename_onto_the_field_an_earlier_removal_fixed_skips_as_judge_would_refuse() {
    let schema = "version: 1\nrules:\n  s:\n    match: {frontmatter: {type: task}}\n    forbidden:\n      k: remove\n  t:\n    match: {path: 'zz-repair/**'}\n    forbidden:\n      z_k: {rename_to: k}\n";
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-rename-onto-a-removed-field",
        schema,
        &[("a.md", b"---\ntype: task\nk: on\nz_k: on\n---\n")],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let k = finding_of(&host, &vault, "a.md", FindingKind::Forbidden, "k");
    let z_k = finding_of(&host, &vault, "a.md", FindingKind::Forbidden, "z_k");

    let plan = applied_leaving_only_its_skips(&host, &vault);

    assert_eq!(
        plan.operations,
        vec![
            Operation::new(OperationKind::remove_frontmatter(
                WriteTarget::path(at("a.md")),
                "k"
            ))
            .with_id(OperationId::new("repair-1").expect("an id"))
        ]
    );
    assert_eq!(
        provenance(&plan).citations,
        vec![Citation::new(
            OperationId::new("repair-1").expect("an id"),
            vec![cited_valued(k, "on")]
        )]
    );
    assert_eq!(
        provenance(&plan).skipped,
        vec![
            SkippedFinding::new(z_k, SkipReason::JudgeWouldRefuse)
                .with_value(norn_store::value_head("on"))
                .with_candidates(candidates(&[("rename_to: k", "t")]))
        ]
    );
}

/// **A finding an earlier fix dropped is not made to hold again by a later
/// one**: removing `k` takes the document out of the rule forbidding `m`, so
/// `m`'s finding is dropped, and the rename of `z_k` onto `k`, which would
/// bring that rule back, skips; `m` stands unflagged after.
#[test]
fn a_finding_an_earlier_fix_dropped_is_not_made_to_hold_again() {
    let schema = "version: 1\nrules:\n  r:\n    match: {frontmatter: {k: on}}\n    forbidden:\n      m:\n  s:\n    match: {frontmatter: {type: task}}\n    forbidden:\n      k: remove\n  t:\n    match: {path: 'zz-repair/**'}\n    forbidden:\n      z_k: {rename_to: k}\n";
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-dropped-stays-dropped",
        schema,
        &[("a.md", b"---\ntype: task\nk: on\nm: x\nz_k: on\n---\n")],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let z_k = finding_of(&host, &vault, "a.md", FindingKind::Forbidden, "z_k");

    let plan = applied_leaving_only_its_skips(&host, &vault);

    assert_eq!(skipped_ids(&plan), vec![z_k]);
    assert_eq!(
        written(&vault, "a.md"),
        "---\ntype: task\nm: x\nz_k: on\n---\n"
    );
}

/// **A fix that leaves its finding standing is no fix**: the synonym maps
/// `complete` onto itself, a member of its own rule's set but not of the
/// set the two rules narrow `status` to, so it skips as one the judge would
/// refuse, with its candidate and its value, and nothing is written.
#[test]
fn a_synonym_that_leaves_its_finding_standing_skips_as_judge_would_refuse() {
    let schema = "version: 1\nrules:\n  a-rule:\n    match: {frontmatter: {type: task}}\n    one_of:\n      status: {values: [todo, complete], synonyms: {complete: complete}}\n  b-rule:\n    match: {frontmatter: {type: task}}\n    one_of:\n      status: {values: [todo]}\n";
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-fix-leaving-its-finding",
        schema,
        &[("a.md", b"---\ntype: task\nstatus: complete\n---\n")],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let status = finding_of(&host, &vault, "a.md", FindingKind::NotOneOf, "status");

    let plan = applied_leaving_only_its_skips(&host, &vault);

    assert!(plan.operations.is_empty(), "{:?}", plan.operations);
    let (skipped, _) = the_one_skip(&plan);
    assert_eq!(
        skipped,
        SkippedFinding::new(status, SkipReason::JudgeWouldRefuse)
            .with_value(norn_store::value_head("complete"))
            .with_candidates(candidates(&[("complete", "a-rule")]))
    );
}

/// A declared tag vocabulary reporting every other tag, and a rule removing
/// a task's `tags`.
const UNTAGGED: &str = "version: 1\ntags:\n  declared: [project]\n  undeclared: report\nrules:\n  bans:\n    match: {frontmatter: {type: task}}\n    forbidden:\n      tags: remove\n";

/// **A finding of a kind no fix answers is dropped where a fix of the batch
/// eliminates it, and skipped where it still stands**: removing `a.md`'s
/// `tags` takes its undeclared `draft` with it, so that finding is neither
/// fixed nor skipped; `b.md` writes `#draft` in its body too, so its tag
/// finding still stands after the removal and is skipped as having no
/// declared fix.
#[test]
fn a_finding_a_fix_eliminates_is_dropped_whatever_its_kind() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-fix-eliminates-a-tag",
        UNTAGGED,
        &[
            ("a.md", b"---\ntype: task\ntags: [draft]\n---\n# A\n"),
            (
                "b.md",
                b"---\ntype: task\ntags: [draft]\n---\nA #draft body\n",
            ),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let tag_of = |name: &str| {
        let ids: Vec<u64> = findings_beneath(&host, &vault)
            .iter()
            .filter(|row| row.path == at(name) && row.kind == FindingKind::UndeclaredTag)
            .map(|row| row.id)
            .collect();
        let [id] = ids.as_slice() else {
            panic!("one tag finding of {name}: {ids:?}");
        };
        *id
    };
    let b_tag = tag_of("b.md");
    let _ = tag_of("a.md");

    let plan = applied_leaving_only_its_skips(&host, &vault);

    assert_eq!(plan.operations.len(), 2, "{:?}", plan.operations);
    let skipped: Vec<(u64, SkipReason)> = provenance(&plan)
        .skipped
        .iter()
        .map(|skipped| (skipped.finding, skipped.reason))
        .collect();
    assert_eq!(skipped, [(b_tag, SkipReason::NoDeclaredFix)]);
}

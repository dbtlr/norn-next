//! **The `repair` verb, end to end**: `Host::repair` over a real vault and a
//! real attachment.
//!
//! No finding has a fix yet, so what is pinned here is the verb's core: the
//! findings a request selects are read in batches of whole documents in path
//! order, every one is left alone for the reason it has, the plan that comes
//! back is the resolved plan an apply would land (none of it), and the
//! provenance it carries counts what remains, names where the next batch
//! continues, and decides nothing an apply does.
#![cfg(unix)]
#![allow(clippy::disallowed_methods)] // Harness scaffolding: this suite's own generated tree.

mod attach;

use std::collections::BTreeMap;
use std::path::Path;

use norn_testkit::process::Sandbox;
use norn_wire::{
    ApplyMode, ApplyParams, ApplyReport, AuthoredValue, ChangesetOutcome, Cursor, ErrorDetail,
    ErrorEnvelope, FieldChange, FindingKind, PagedRows, PlanDocument, Predicate, Provenance,
    ReasonCode, RepairParams, ResolvedPlan, SetParams, Severity, SkipReason, Unsatisfied,
    ValidateParams, ValidateReport, VaultAddress, WriteTarget,
};

/// The generated profile every case here attaches.
const PROFILE: &str = "tiny";

/// The schema the cases pin: one rule, so a task missing its `status` or
/// holding one outside its closed set is a finding that cites it.
const SCHEMA: &str = "version: 1\nrules:\n  tasks:\n    severity: error\n    match: {frontmatter: {type: task}}\n    required:\n      status: {default: todo}\n    one_of:\n      status: {values: [todo, done]}\n";

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
    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), label).expect("a sandbox");
    let vault = attach::Vault::generate(&sandbox.work_dir().join("attached"), PROFILE);
    std::fs::write(vault.path().join(".norn/schema.yaml"), SCHEMA).expect("write the schema");
    std::fs::create_dir_all(vault.path().join(FOLDER)).expect("make the folder");
    for (name, bytes) in DOCUMENTS {
        std::fs::write(vault.path().join(FOLDER).join(name), bytes).expect("write a document");
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

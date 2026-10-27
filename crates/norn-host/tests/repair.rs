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
//! rule reads by value among them. Derived fixes are pinned over schemas of
//! their own, each through a preview then an apply: a shape mismatch wrapped
//! or unwrapped, a boolean spelled by case written as the boolean, a value
//! differing from a closed set's member by case or white space written as the
//! member, the excluded classes, the request's threshold taking effect alike
//! in both modes, and a declared fix outranking the derived answer. Over a
//! schema declaring every kind of fix, that the validator flags nothing a repair wrote.
#![cfg(unix)]
#![allow(clippy::disallowed_methods)] // Harness scaffolding: this suite's own generated tree.

mod attach;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use norn_host::ReloadRefusal;
use norn_testkit::process::Sandbox;
use norn_testkit::wait::{Observed, wait_until};
use norn_wire::{
    ApplyMode, ApplyParams, ApplyReport, AuthoredValue, ChangesetOutcome, Citation, CitedFinding,
    Confidence, Cursor, DocumentPath, ErrorDetail, ErrorEnvelope, FieldChange, FindingKind,
    NewParams, Operation, OperationId, OperationKind, PagedRows, PlanDocument, Predicate,
    Provenance, ReasonCode, RepairAdvisories, RepairParams, RequiredField, RequiredFieldHead,
    ResolvedPlan, SetParams, Severity, SkipReason, SkippedCandidates, SkippedFinding, Unsatisfied,
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

/// The write generation a validate of the repair's documents reads: the
/// independent account of the generation the findings stand at.
fn validated_generation(host: &attach::ServingHost, vault: &attach::Vault) -> u64 {
    let request = ValidateParams::new(address(vault))
        .with_predicates([Predicate::path(format!("{FOLDER}*"))])
        .with_limit(1000);
    host.validate(&request)
        .expect("a validate answers")
        .answer
        .reading
        .generation
}

/// **A first batch records the generation the store's findings stand at.**
/// It equals the write generation a validate reads, before a write and again
/// after one has moved it.
#[test]
fn the_first_batch_records_the_generation_the_findings_stand_at() {
    let (_sandbox, vault, host) = a_vault("host-repair-generation");
    let _lease = attach::attach_and_wait(&host, vault.name());

    let standing_at = validated_generation(&host, &vault);
    let first = previewed(host.repair(repairing(&vault, ApplyMode::Preview).with_limit(3)));
    assert_eq!(provenance(&first).finding_generation, standing_at);

    host.set(SetParams::new(
        address(&vault),
        ApplyMode::Apply,
        WriteTarget::path(at("a.md")),
        vec![FieldChange::set("status", AuthoredValue::string("done"))],
    ))
    .expect("the write is admitted")
    .wait()
    .expect("a.md is written");

    let moved_to = validated_generation(&host, &vault);
    assert!(moved_to > standing_at, "the write moved no generation");
    let after = previewed(host.repair(repairing(&vault, ApplyMode::Preview).with_limit(3)));
    assert_eq!(provenance(&after).finding_generation, moved_to);
}

/// **An apply's first batch counts what remains as its preview did, before
/// its own writes change the findings.** a.md's missing `status` fills from
/// its default and its broken link stays, so the first batch (a.md, at a limit
/// of two) fixes one selected finding and skips one; the three tasks after it
/// each stand under one. The count is the selection's total before the batch
/// less the batch's findings, though the apply leaves the selection one
/// smaller.
#[test]
fn an_applys_first_batch_counts_what_remains_before_its_own_writes() {
    let defaulting = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    required:\n      status: {default: todo}\n";
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-remaining-before-writes",
        defaulting,
        &[
            ("a.md", b"---\ntype: task\n---\nSee [[nowhere-a]].\n"),
            ("b.md", b"---\ntype: task\n---\n"),
            ("c.md", b"---\ntype: task\n---\n"),
            ("d.md", b"---\ntype: task\n---\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let selected = standing(&host, &vault, |request| request).len() as u64;
    assert_eq!(selected, 5);

    let preview = previewed(host.repair(repairing(&vault, ApplyMode::Preview).with_limit(2)));
    let ApplyReport::Applied { plan: apply, .. } =
        planned(host.repair(repairing(&vault, ApplyMode::Apply).with_limit(2)))
    else {
        panic!("the repair applies");
    };

    let in_batch = |plan: &ResolvedPlan| {
        let cited: usize = provenance(plan)
            .citations
            .iter()
            .map(|citation| citation.findings.len())
            .sum();
        (cited + provenance(plan).skipped.len()) as u64
    };
    assert_eq!(in_batch(&apply), 2, "{:?}", provenance(&apply));
    assert_eq!(provenance(&apply).remaining, provenance(&preview).remaining);
    assert_eq!(
        provenance(&apply).remaining,
        Some(selected - in_batch(&apply))
    );
    let after = standing(&host, &vault, |request| request).len() as u64;
    assert_eq!(after, selected - 1, "the apply changed no finding");
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
/// generation that never stood, and advisories the repair never computed —
/// the preview's plan applies as the plan it came back as.
#[test]
fn a_plan_applies_the_same_whatever_its_provenance_says() {
    let (_sandbox, vault, host) = a_vault("host-repair-provenance");
    let _lease = attach::attach_and_wait(&host, vault.name());
    let plan = previewed(host.repair(repairing(&vault, ApplyMode::Preview).with_limit(1)));
    let mut altered = plan.clone();
    altered.provenance = Some(
        Provenance::new(u64::MAX, Vec::new()).with_advisories(RepairAdvisories::omitted_narrowed()),
    );

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

/// **A block that changes nothing about what applies, shown on a real repair
/// plan.** A repair under [`DEFAULTING`] writes two cited operations and skips
/// a finding. Every part of its block is then altered: the generation, a
/// citation (an operation the plan does not hold, a finding that never stood,
/// another confidence, a value and notes), a skipped finding carrying every
/// kind of decision data (a proposed delete included), the count and the
/// cursor. The altered plan applies with the operations, transitions,
/// changeset, targets and written bytes the unaltered plan does.
#[test]
fn a_cited_repair_plan_applies_the_same_whatever_every_part_of_its_block_says() {
    let docs: [(&str, &[u8]); 2] = [
        ("a.md", b"---\ntype: task\n---\nSee [[nowhere-a]].\n"),
        ("b.md", b"---\ntype: task\n---\n# B\n"),
    ];
    let landing = |label: &str, alter: bool| {
        let (_sandbox, vault, host) = a_vault_under(label, DEFAULTING, &docs);
        let _lease = attach::attach_and_wait(&host, vault.name());
        let mut plan = previewed(host.repair(repairing(&vault, ApplyMode::Preview)));
        let original = provenance(&plan).clone();
        assert_eq!(plan.operations.len(), 2, "{:?}", plan.operations);
        assert_eq!(original.citations.len(), 2, "the plan cites: {original:?}");
        assert!(!original.skipped.is_empty(), "the plan skips: {original:?}");
        assert!(original.cursor.is_none());
        if alter {
            let cursor = provenance(&previewed(
                host.repair(repairing(&vault, ApplyMode::Preview).with_limit(1)),
            ))
            .cursor
            .clone()
            .expect("a cursor");
            let altered = norn_store::value_head("altered");
            plan.provenance = Some(
                Provenance::new(
                    original.finding_generation.wrapping_add(1_000_000),
                    vec![
                        SkippedFinding::new(u64::MAX, SkipReason::Tie)
                            .with_value(altered.clone())
                            .with_candidates(candidates(&[("x", "nobody"), ("y", "nobody")]))
                            .with_required_fields(
                                RequiredFieldHead::new([RequiredField::new("ghost")], 1)
                                    .expect("a head"),
                            )
                            .with_proposed(Operation::new(OperationKind::delete_document(at(
                                "b.md",
                            ))))
                            .with_note("altered"),
                    ],
                )
                .with_citations(vec![Citation::new(
                    OperationId::new("repair-99").expect("an id"),
                    vec![
                        CitedFinding::new(u64::MAX, Confidence::Suggested)
                            .with_value(altered)
                            .with_notes(vec!["altered".into()]),
                    ],
                )])
                .with_remaining(12345)
                .continued_by(*cursor),
            );
        }
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
        // The plan's root is the fixture's own, so only the parts that
        // would differ with the block are compared, not the whole plan.
        (
            plan.operations,
            plan.transitions,
            changeset,
            targets,
            written(&vault, "a.md"),
            written(&vault, "b.md"),
        )
    };

    let unaltered = landing("host-repair-block-unaltered", false);
    let altered = landing("host-repair-block-altered", true);

    assert!(unaltered.4.contains("status: todo"), "{}", unaltered.4);
    assert!(unaltered.5.contains("status: todo"), "{}", unaltered.5);
    assert_eq!(altered, unaltered);
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

/// [`STAMPED_AND_ROUTED`] beside a rule filling a `review`'s `status` from a
/// plain default, a fix that reads no clock.
const STAMPED_ROUTED_AND_PLAIN: &str = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    required:\n      created: {default: '{{now}}'}\n      day: {default: '{{date}}'}\n    allowed_paths: {paths: ['zz-repair/log/**'], route: 'zz-repair/log/{{date}}/'}\n  reviews:\n    match: {frontmatter: {type: review}}\n    required:\n      status: {default: todo}\n";

/// The folder a route files a stamped document under, before its date.
const ROUTE_ROOT: &str = "zz-repair/log/";

/// Whether `text` reads as a `YYYY-MM-DD` date.
fn is_a_date(text: &str) -> bool {
    let b = text.as_bytes();
    let number = |from: usize, to: usize| text.get(from..to)?.parse::<u32>().ok();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| matches!(i, 4 | 7) || c.is_ascii_digit())
        && matches!(number(5, 7), Some(1..=12))
        && matches!(number(8, 10), Some(1..=31))
}

/// Whether `text` reads as an RFC 3339 instant to the second, `Z` or with an
/// offset: `YYYY-MM-DDTHH:MM:SS+HH:MM`.
fn is_an_instant(text: &str) -> bool {
    let Some((date, time)) = text.split_once('T') else {
        return false;
    };
    let (clock, offset) = match time.strip_suffix('Z') {
        Some(clock) => (clock, None),
        None => match time.rfind(['+', '-']) {
            Some(at) => (&time[..at], Some(&time[at + 1..])),
            None => return false,
        },
    };
    let pair =
        |text: &str, bound: u32| text.len() == 2 && text.parse::<u32>().is_ok_and(|n| n < bound);
    let parts: Vec<&str> = clock.split(':').collect();
    let offset_parts: Vec<&str> = offset.map_or_else(Vec::new, |o| o.split(':').collect());
    is_a_date(date)
        && matches!(parts.as_slice(), [h, m, s] if pair(h, 24) && pair(m, 60) && pair(s, 61))
        && (offset.is_none()
            || matches!(offset_parts.as_slice(), [h, m] if pair(h, 24) && pair(m, 60)))
}

/// What the clock filled in a plan, read off it while masking, and how much
/// of the plan was masked for it.
struct ClockFilled {
    /// Each `created` value, in plan order.
    instants: Vec<String>,
    /// Each `day` value, in plan order.
    days: Vec<String>,
    /// The date folder of each route's destination, in plan order.
    folders: Vec<String>,
    /// The after-state of each document's transition a clock default fills,
    /// as planned, with whether the document is stamped with an instant.
    filled_after: Vec<(bool, norn_wire::FileState)>,
}

/// `path` with the date folder a route reads the clock for set to `DATE`, the
/// rest of the path as planned; the date it replaced is pushed to `folders`.
/// Panics where a path under the route's root holds no date there.
fn without_the_route_date(path: &DocumentPath, folders: &mut Vec<String>) -> DocumentPath {
    let Some(under) = path.as_str().strip_prefix(ROUTE_ROOT) else {
        return path.clone();
    };
    let (date, rest) = under.split_once('/').expect("a document under a date");
    assert!(is_a_date(date), "a route's folder is a date: {path:?}");
    folders.push(date.to_string());
    DocumentPath::new(format!("{ROUTE_ROOT}DATE/{rest}")).expect("a document path")
}

/// `plan` with only what the clock fills set apart: the date folder in a
/// route's paths (a move's destination, a write's target, the transitions
/// landing there, the citation notes naming a destination), the value of a `created` or `day` default, and the
/// after-state of every document a clock default fills with an instant, whose
/// bytes hold it. The after-state of a document a default fills with a day
/// alone is masked only for `across_days`, as two readings that straddle local
/// midnight differ in it and two on one day do not.
/// Each masked value is checked to be a well-formed clock reading. Everything
/// else, the rest of each destination path included, stays as planned.
fn with_the_clock_masked(plan: &ResolvedPlan, across_days: bool) -> (ResolvedPlan, ClockFilled) {
    let mut masked = plan.clone();
    let mut filled = ClockFilled {
        instants: Vec::new(),
        days: Vec::new(),
        folders: Vec::new(),
        filled_after: Vec::new(),
    };
    let mut stamped = BTreeSet::new();
    let mut clock_filled = BTreeSet::new();
    for operation in &mut masked.operations {
        match &mut operation.kind {
            OperationKind::MoveDocument { to, .. } => {
                assert!(
                    to.as_str().starts_with(ROUTE_ROOT),
                    "a route's destination is under its dated root: {to:?}"
                );
                *to = without_the_route_date(to, &mut filled.folders);
            }
            OperationKind::SetFrontmatter {
                target: WriteTarget::Path(path),
                field,
                value,
            } => {
                if field == "created" || field == "day" {
                    let reading = value.scalar_text().expect("a clock reading");
                    clock_filled.insert(path.as_str().to_string());
                    if field == "created" {
                        assert!(is_an_instant(&reading), "an instant: {reading}");
                        stamped.insert(path.as_str().to_string());
                        filled.instants.push(reading);
                    } else {
                        assert!(is_a_date(&reading), "a date: {reading}");
                        filled.days.push(reading);
                    }
                    *value = AuthoredValue::string("masked");
                }
                *path = without_the_route_date(path, &mut Vec::new());
            }
            _ => {}
        }
    }
    for transition in &mut masked.transitions {
        let was = transition.path.as_str().to_string();
        transition.path = without_the_route_date(&transition.path, &mut Vec::new());
        if clock_filled.contains(&was) {
            let with_an_instant = stamped.contains(&was);
            filled
                .filled_after
                .push((with_an_instant, transition.after.clone()));
            if with_an_instant || across_days {
                transition.after =
                    norn_wire::FileState::present(norn_wire::ContentHash::from_sha256([0; 32]));
            }
        }
    }
    // A citation's note names a route's destination, date folder included.
    if let Some(block) = &mut masked.provenance {
        for note in block
            .citations
            .iter_mut()
            .flat_map(|citation| &mut citation.findings)
            .flat_map(|finding| &mut finding.notes)
        {
            for date in &filled.folders {
                *note = note.replace(date.as_str(), "DATE");
            }
        }
    }
    (masked, filled)
}

/// Both plans with only what the clock fills masked, the hash of a day-only
/// document masked only where the two plans' days differ: each plan's masked
/// form with what it held, the first plan's then the second's.
fn masked_alike(
    first: &ResolvedPlan,
    second: &ResolvedPlan,
) -> ((ResolvedPlan, ClockFilled), (ResolvedPlan, ClockFilled)) {
    let day_of = |plan| with_the_clock_masked(plan, true).1.days[0].clone();
    let across_days = day_of(first) != day_of(second);
    (
        with_the_clock_masked(first, across_days),
        with_the_clock_masked(second, across_days),
    )
}

/// `plan` as the clock would have filled it on `then`'s day instead of `now`'s,
/// every date it holds (folders, `day` values, the day of each instant)
/// respelled.
fn read_on_another_day(plan: &ResolvedPlan, now: &str, then: &str) -> ResolvedPlan {
    let text = serde_json::to_string(plan).expect("a plan serializes");
    serde_json::from_str(&text.replace(now, then)).expect("a plan deserializes")
}

/// **An apply plans what its preview did, apart from what the clock fills.**
/// Three routes and five clock defaults across three tasks, beside a review
/// whose default reads no clock: a second apart, the preview and the apply
/// read the clock at different instants, so the `created` instants differ.
/// With only what the clock fills masked (each default's value, the date
/// folder of each route's paths, and the after-state of the two documents a
/// clock default fills with an instant, and of the one it fills with a day
/// alone only if the two readings' days differ, as they do across local
/// midnight), each checked to be a well-formed reading, and the
/// rest of every destination path left as planned, the two plans are equal in
/// operations, transitions, conditions and provenance (citations, skipped
/// findings, remaining count and cursor), and the review's fix stands alike
/// in both.
#[test]
fn an_apply_plans_what_its_preview_did_apart_from_the_values_the_clock_fills() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-clock-masked",
        STAMPED_ROUTED_AND_PLAIN,
        &[
            ("a.md", b"---\ntype: task\n---\n"),
            ("b.md", b"---\ntype: task\ncreated: then\n---\n"),
            ("c.md", b"---\ntype: task\n---\n"),
            ("r.md", b"---\ntype: review\n---\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());

    let preview = previewed(host.repair(repairing(&vault, ApplyMode::Preview)));
    // The host's clock is the system's, which a test cannot drive; `{{now}}`
    // reads to the second, so a wait past one makes the two readings differ.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let ApplyReport::Applied {
        plan: apply,
        changeset,
        ..
    } = planned(host.repair(repairing(&vault, ApplyMode::Apply)))
    else {
        panic!("the repair applies");
    };
    assert_eq!(changeset, ChangesetOutcome::Committed);

    let ((preview_masked, preview_filled), (apply_masked, apply_filled)) =
        masked_alike(&preview, &apply);
    for (plan, filled) in [(&preview, &preview_filled), (&apply, &apply_filled)] {
        assert_eq!(
            (
                filled.folders.len(),
                filled.instants.len(),
                filled.days.len()
            ),
            (3, 2, 3),
            "{plan:?}"
        );
        // One reading of the clock fills a plan: every route's folder is the
        // day every `day` default holds.
        let day = &filled.days[0];
        assert!(filled.days.iter().chain(&filled.folders).all(|d| d == day));
        assert!(
            filled
                .instants
                .iter()
                .all(|at| at.starts_with(day.as_str()))
        );
    }
    assert_ne!(
        preview_filled.instants, apply_filled.instants,
        "the two readings of the clock are one instant"
    );
    // `a.md` and `c.md` hold an instant, so their hashes differ between the
    // plans and are masked. `b.md` holds a day alone, which two readings share
    // unless they straddle local midnight, so its hash is masked only then.
    let instants_of = |filled: &ClockFilled| -> Vec<norn_wire::FileState> {
        filled
            .filled_after
            .iter()
            .filter(|(stamped, _)| *stamped)
            .map(|(_, after)| after.clone())
            .collect()
    };
    assert_eq!(preview_filled.filled_after.len(), 3);
    assert_eq!(
        preview_filled
            .filled_after
            .iter()
            .map(|(stamped, _)| *stamped)
            .collect::<Vec<_>>(),
        apply_filled
            .filled_after
            .iter()
            .map(|(stamped, _)| *stamped)
            .collect::<Vec<_>>()
    );
    assert!(
        instants_of(&preview_filled)
            .iter()
            .zip(instants_of(&apply_filled))
            .all(|(then, now)| *then != now),
        "a stamped document's hash is its instant's"
    );
    // A day-only document's hash is the day's: with the two readings on one
    // day, a hash that differs is a difference, not the clock's.
    let set_day_only_hash = |plan: &mut ResolvedPlan, hash: [u8; 32]| {
        plan.transitions
            .iter_mut()
            .find(|transition| {
                transition.path.as_str().starts_with(ROUTE_ROOT)
                    && transition.path.as_str().ends_with("/b.md")
            })
            .expect("the day-only document's transition")
            .after = norn_wire::FileState::present(norn_wire::ContentHash::from_sha256(hash));
    };
    let mut other = apply.clone();
    set_day_only_hash(&mut other, [7; 32]);
    let (apply_alike, other_alike) = masked_alike(&apply, &other);
    assert_ne!(
        other_alike.0, apply_alike.0,
        "a day-only document's hash passed the mask on one day"
    );
    // The readings on different days, as across local midnight, differ in the
    // day-only hash, which then passes the mask, and in nothing else.
    let day = apply_filled.days[0].clone();
    let mut straddling = read_on_another_day(&apply, &day, "2099-01-01");
    set_day_only_hash(&mut straddling, [7; 32]);
    let (preview_alike, straddling_alike) = masked_alike(&preview, &straddling);
    assert_eq!(
        straddling_alike.1.days[0], "2099-01-01",
        "the plan reads another day"
    );
    assert_eq!(
        straddling_alike.0, preview_alike.0,
        "a day-only document's hash was not masked across days"
    );
    // A difference that is not the clock's is not masked: the apply landing
    // a document under another name, or in a subfolder, is a difference.
    for altered in ["zz-repair/log/DATE/other.md", "zz-repair/log/DATE/sub/a.md"] {
        let mut other = apply.clone();
        let OperationKind::MoveDocument { to, .. } = &mut other.operations[0].kind else {
            panic!("the first operation is a move");
        };
        *to = DocumentPath::new(altered.replace("DATE", "2026-01-01")).expect("a path");
        assert_ne!(
            masked_alike(&preview, &other).1.0,
            preview_masked,
            "{altered} passed the mask"
        );
    }
    // Nor is a document no clock default fills: the review's after-state
    // differing is a difference.
    let mut other = apply.clone();
    let review = other
        .transitions
        .iter_mut()
        .find(|transition| transition.path.as_str().ends_with("/r.md"))
        .expect("the review's transition");
    review.after = norn_wire::FileState::present(norn_wire::ContentHash::from_sha256([7; 32]));
    assert_ne!(
        masked_alike(&preview, &other).1.0,
        preview_masked,
        "an unfilled document's hash passed the mask"
    );
    assert!(
        preview
            .operations
            .iter()
            .any(|operation| matches!(&operation.kind,
                OperationKind::SetFrontmatter { field, .. } if field == "status")),
        "the review's plain default is in the plan: {:?}",
        preview.operations
    );
    assert_eq!(apply_masked.operations, preview_masked.operations);
    assert_eq!(apply_masked.transitions, preview_masked.transitions);
    assert_eq!(apply_masked.conditions, preview_masked.conditions);
    assert_eq!(provenance(&apply_masked), provenance(&preview_masked));
    assert_eq!(apply_masked, preview_masked);
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

/// An unreadable file still occupies a route's destination. That route is
/// skipped while an independent route remains in the preview.
#[cfg(feature = "induced-failure")]
#[test]
fn an_unreadable_destination_skips_only_its_route_as_destination_taken() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-route-unreadable-destination",
        TASKED,
        &[
            ("loose/a.md", b"---\ntype: task\n---\n# Independent\n"),
            ("loose/b.md", b"---\ntype: task\n---\n# Loose\n"),
            ("tasks/b.md", b"---\ntype: task\n---\n# Placed\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let loose = misplaced_of(&host, &vault, "loose/b.md");
    let _denied = norn_fs::deny_document_opens(&vault.path().join(FOLDER).join("tasks/b.md"));

    let plan = previewed(host.repair(repairing_beneath(&vault, ApplyMode::Preview)));

    assert_eq!(
        kinds(&plan),
        vec![OperationKind::move_document(
            at("loose/a.md"),
            at("tasks/a.md")
        )]
    );
    let skipped: Vec<_> = provenance(&plan)
        .skipped
        .iter()
        .map(|skip| (skip.finding, skip.reason))
        .collect();
    assert_eq!(skipped, [(loose, SkipReason::DestinationTaken)]);
    assert_eq!(
        written(&vault, "tasks/b.md"),
        "---\ntype: task\n---\n# Placed\n"
    );
    assert!(stands(&vault, "loose/a.md") && stands(&vault, "loose/b.md"));
    assert!(!stands(&vault, "tasks/a.md"));
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
/// reads by value**: the hub's limited `up` holds the link to the moved
/// document in a flow list, which no cascade can respell, so the route moves
/// and its forecast advises on the link as unplaced, as a `move` of the
/// document forecasts it.
#[test]
fn an_unrespellable_link_in_a_field_read_by_value_never_holds_a_route_back() {
    let schema = "version: 1\nfields:\n  up: {type: text, shape: list}\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    allowed_paths: {paths: ['zz-repair/tasks/**'], route: 'zz-repair/tasks/'}\n  hubs:\n    match: {frontmatter: {type: hub}}\n    max_length:\n      up: 40\n";
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

/// A rule routing a task into `tasks/`, and a hub rule limiting a hub's `up`
/// link field, which reads it by value.
const LIMITED_UP: &str = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    required:\n      status: {default: todo}\n    allowed_paths: {paths: ['zz-repair/tasks/**'], route: 'zz-repair/tasks/'}\n  hubs:\n    match: {frontmatter: {type: hub}}\n    max_length:\n      up: 40\n";

/// **A route whose cascade may respell a frontmatter link in a field a rule
/// reads by value skips as respelling a judged link**, with its destination,
/// noting the holder and the field, and the plan stays applicable: moving
/// `loose/a.md` would respell the hub's limited `up`, so nothing moves and
/// the hub stands as written.
#[test]
fn a_route_whose_cascade_may_respell_a_judged_frontmatter_link_skips() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-route-respells-a-judged-link",
        LIMITED_UP,
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
    let schema = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    allowed_paths: {paths: ['zz-repair/tasks/**'], route: 'zz-repair/tasks/'}\n    max_length:\n      me: 40\n";
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
        LIMITED_UP,
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

/// A rule routing a task into `tasks/`, and a hub rule limiting a hub's `up`
/// link field, which reads it by value.
const HUB_UP: &str = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    allowed_paths: {paths: ['zz-repair/tasks/**'], route: 'zz-repair/tasks/'}\n  hubs:\n    match: {frontmatter: {type: hub}}\n    max_length:\n      up: 40\n";

/// **Of two routes whose cascades rewrite one hub, only the one respelling a
/// judged link skips, and the other's cascade lands**: moving `loose/a.md`
/// would respell the hub's limited `up`, moving `loose/b.md` respells its
/// open `down` and its body's links, so `b` moves, the hub respelled, and
/// `a` skips, noting the hub and `up`; a bare backlink in the limited field
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
        "---\ntype: hub\nup: \"[[zz-repair/loose/a]]\"\ndown: \"[[zz-repair/tasks/b]]\"\n---\nSee [[zz-repair/tasks/b]] and [b](tasks/b.md).\n"
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
        "---\nsee: \"[[zz-repair/tasks/a]]\"\n---\nUp [[zz-repair/tasks/b]]\n"
    );
    assert!(!stands(&vault, "loose/a.md") && !stands(&vault, "loose/b.md"));
    assert!(findings_beneath(&host, &vault).is_empty());
}

/// **A repair preview answers what `Host::apply`'s preview of the same plan
/// answers.** Repair judges its resolved plan by the applier's own preview
/// judgment on a read hold of its own, and the architecture states the two
/// answers equal. The plan here routes a document, fills a default and skips
/// a finding, and the route's move leaves a link it cannot respell (a
/// wikilink in a flow sequence) that the forecast advises on, so the
/// comparison holds something in every part of the report: the plan's
/// operations and provenance, and the forecast's removed folders and link
/// advisories. Nothing is excluded: a preview's report is the plan and its
/// forecast, and neither is per-call.
#[test]
fn a_repair_preview_answers_what_a_preview_of_its_plan_through_apply_answers() {
    let schema = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    required:\n      status: {default: todo}\n    allowed_paths: {paths: ['zz-repair/tasks/**'], route: 'zz-repair/tasks/'}\n";
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-preview-equals-apply-preview",
        schema,
        &[
            ("loose/a.md", b"---\ntype: task\n---\n# A\n"),
            ("tasks/b.md", b"---\ntype: task\n---\nSee [[nowhere]].\n"),
            (
                "h.md",
                b"---\nsee: \"[[zz-repair/loose/a]]\"\n---\nUp [[zz-repair/loose/a]]\n",
            ),
            ("g.md", b"---\nsee: [\"[[zz-repair/loose/a]]\"]\n---\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());

    let repair = planned(host.repair(repairing_beneath(&vault, ApplyMode::Preview)));
    let ApplyReport::Previewed { plan, forecast, .. } = &repair else {
        panic!("a repair preview answered {repair:?}");
    };
    assert!(
        !forecast.links.is_empty(),
        "the forecast advises on a link the move cannot respell: {forecast:?}"
    );
    assert!(
        plan.operations
            .iter()
            .any(|operation| matches!(operation.kind, OperationKind::MoveDocument { .. })),
        "the plan routes: {:?}",
        plan.operations
    );
    assert!(
        plan.operations
            .iter()
            .any(|operation| matches!(operation.kind, OperationKind::SetFrontmatter { .. })),
        "the plan fills a default: {:?}",
        plan.operations
    );
    assert!(
        !provenance(plan).skipped.is_empty(),
        "the plan skips: {:?}",
        provenance(plan)
    );

    let through_apply = planned(host.apply(ApplyParams::new(
        ApplyMode::Preview,
        PlanDocument::resolved(plan.clone()),
    )));

    assert_eq!(through_apply, repair);
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

/// The finding `id` as a citation carries it at the derived level, with its
/// offending `value`.
fn cited_derived(id: u64, value: &str) -> CitedFinding {
    CitedFinding::new(id, Confidence::Derived).with_value(norn_store::value_head(value))
}

/// The operation `repair-<n>`, setting `field` of the document at `name` to
/// `value`.
fn setting_value(n: usize, name: &str, field: &str, value: AuthoredValue) -> Operation {
    Operation::new(OperationKind::set_frontmatter(
        WriteTarget::path(at(name)),
        field,
        value,
    ))
    .with_id(OperationId::new(format!("repair-{n}")).expect("an id"))
}

/// The plan `host` previews for the repair, then the one it applies, which
/// must be the same whole plan: the planner is shared, and an apply holds no
/// more than its preview.
fn previewed_then_applied(
    host: &attach::ServingHost,
    vault: &attach::Vault,
    request: impl Fn(RepairParams) -> RepairParams,
) -> ResolvedPlan {
    let preview = previewed(host.repair(request(repairing(vault, ApplyMode::Preview))));
    let ApplyReport::Applied {
        plan, changeset, ..
    } = planned(host.repair(request(repairing(vault, ApplyMode::Apply))))
    else {
        panic!("the repair applies");
    };
    assert_eq!(changeset, ChangesetOutcome::Committed);
    assert_eq!(plan, preview, "an apply plans what its preview did");
    preview
}

/// Each repair probe previews and applies one derived operation, recording its written links.
fn probe_repaired(
    label: &str,
    schema: &str,
    document: &[u8],
    field: &str,
    kind: FindingKind,
    value: AuthoredValue,
    conditions: Vec<norn_wire::PlanCondition>,
) {
    let (_sandbox, vault, host) = a_vault_under(
        label,
        schema,
        &[("zz-holder.md", document), ("zz-b.md", b"# B\n")],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let mismatch = finding_of(&host, &vault, "zz-holder.md", kind, field);
    let plan = previewed_then_applied(&host, &vault, |request| request);
    assert_eq!(
        plan.operations,
        vec![setting_value(1, "zz-holder.md", field, value)]
    );
    assert_eq!(plan.conditions, conditions);
    let citations = &provenance(&plan).citations;
    assert_eq!(citations.len(), 1);
    assert_eq!(citations[0].findings.len(), 1);
    assert_eq!(
        (
            citations[0].findings[0].finding,
            citations[0].findings[0].confidence
        ),
        (mismatch, Confidence::Derived)
    );
    assert!(provenance(&plan).skipped.is_empty());
    assert!(standing(&host, &vault, |request| request).is_empty());
}

/// **An unquoted wikilink on a link field without a shape is written as one link string.**
#[test]
fn probe_p01_an_unquoted_wikilink_in_a_single_link_field_is_written_as_the_link_string() {
    probe_repaired(
        "host-repair-p01",
        "version: 1\nfields:\n  up: {type: link}\n",
        b"---\nup: [[zz-b]]\n---\n",
        "up",
        FindingKind::TypeMismatch,
        AuthoredValue::string("[[zz-b]]"),
        vec![norn_wire::PlanCondition::address_resolution(
            norn_wire::LinkKey::new(at("zz-holder.md"), norn_wire::LinkFamily::Wikilink, "zz-b"),
            norn_wire::Resolves::one(at("zz-b.md")),
        )],
    );
}

/// **A one-string list on a list link field stays a finding with no declared fix.**
#[test]
fn probe_p02_a_one_string_list_in_a_list_link_field_stays_skipped_with_no_declared_fix() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-p02",
        "version: 1\nfields:\n  ups: {type: link, shape: list}\n",
        &[
            ("zz-holder.md", b"---\nups: [[zz-b]]\n---\n"),
            ("zz-b.md", b"# B\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let mismatch = finding_of(
        &host,
        &vault,
        "zz-holder.md",
        FindingKind::TypeMismatch,
        "ups",
    );

    let plan = previewed(host.repair(repairing(&vault, ApplyMode::Preview)));

    assert!(plan.operations.is_empty());
    assert!(plan.conditions.is_empty());
    assert_eq!(skipped_ids(&plan), vec![mismatch]);
    assert_eq!(
        provenance(&plan).skipped[0].reason,
        SkipReason::NoDeclaredFix
    );
    assert_eq!(
        standing(&host, &vault, |request| request)
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        vec![mismatch]
    );
}

/// **An unquoted wikilink in a block list is written as its link element.**
#[test]
fn an_unquoted_wikilink_in_a_block_list_is_written_as_the_link_element() {
    probe_repaired(
        "host-repair-unquoted-block-list",
        "version: 1\nfields:\n  ups: {type: link, shape: list}\n",
        b"---\nups:\n  - [[zz-b]]\n---\n",
        "ups",
        FindingKind::TypeMismatch,
        AuthoredValue::List(vec![AuthoredValue::string("[[zz-b]]")]),
        vec![norn_wire::PlanCondition::address_resolution(
            norn_wire::LinkKey::new(at("zz-holder.md"), norn_wire::LinkFamily::Wikilink, "zz-b"),
            norn_wire::Resolves::one(at("zz-b.md")),
        )],
    );
}

/// **A bare link value is wrapped and linked in one derived operation.**
#[test]
fn probe_p04_a_bare_value_in_a_list_link_field_is_wrapped_and_linked_in_one_operation() {
    probe_repaired(
        "host-repair-p04",
        "version: 1\nfields:\n  ups: {type: link, shape: list}\n",
        b"---\nups: zz-b\n---\n",
        "ups",
        FindingKind::ShapeMismatch,
        AuthoredValue::List(vec![AuthoredValue::string("[[zz-b]]")]),
        vec![norn_wire::PlanCondition::address_resolution(
            norn_wire::LinkKey::new(at("zz-holder.md"), norn_wire::LinkFamily::Wikilink, "zz-b"),
            norn_wire::Resolves::one(at("zz-b.md")),
        )],
    );
}

/// **A boolean case is wrapped and corrected in one derived operation.**
#[test]
fn probe_p10_a_boolean_case_in_a_list_field_is_wrapped_and_corrected_in_one_operation() {
    probe_repaired(
        "host-repair-p10",
        "version: 1\nfields:\n  flags: {type: boolean, shape: list}\n",
        b"---\nflags: \"True\"\n---\n",
        "flags",
        FindingKind::ShapeMismatch,
        AuthoredValue::List(vec![AuthoredValue::Bool(true)]),
        vec![],
    );
}

/// **A scalar in a list field is wrapped as a one-item list, through preview
/// then apply.** The plan sets the one-item list, its YAML type kept (`3` is
/// the number in the list), cited at the derived level; the apply writes it
/// and the validator flags nothing.
#[test]
fn a_scalar_in_a_list_field_is_wrapped_through_preview_then_apply() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-wrap",
        "version: 1\nfields:\n  ranks: {type: number, shape: list}\n",
        &[("a.md", b"---\nranks: 3\n---\n# A\n")],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let mismatch = finding_of(&host, &vault, "a.md", FindingKind::ShapeMismatch, "ranks");

    let plan = previewed_then_applied(&host, &vault, |request| request);

    assert_eq!(
        plan.operations,
        vec![setting_value(
            1,
            "a.md",
            "ranks",
            AuthoredValue::List(vec![AuthoredValue::Integer(3)])
        )]
    );
    let citations = &provenance(&plan).citations;
    assert_eq!(citations.len(), 1);
    assert_eq!(
        citations[0]
            .findings
            .iter()
            .map(|cited| (cited.finding, cited.confidence))
            .collect::<Vec<_>>(),
        vec![(mismatch, Confidence::Derived)]
    );
    assert!(provenance(&plan).skipped.is_empty());
    assert!(
        written(&vault, "a.md").contains("3") && !written(&vault, "a.md").contains("\"3\""),
        "{}",
        written(&vault, "a.md")
    );
    assert!(standing(&host, &vault, |request| request).is_empty());
}

/// **A one-item list in a single field is unwrapped to its item, through
/// preview then apply; a list of two items has no fix and stands reported.**
#[test]
fn a_one_item_list_in_a_single_field_is_unwrapped_through_preview_then_apply() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-unwrap",
        "version: 1\nfields:\n  owner: {type: text, shape: single}\n",
        &[
            ("a.md", b"---\nowner: [drew]\n---\n# A\n"),
            ("b.md", b"---\nowner: [drew, sam]\n---\n# B\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let a = finding_of(&host, &vault, "a.md", FindingKind::ShapeMismatch, "owner");
    let b = finding_of(&host, &vault, "b.md", FindingKind::ShapeMismatch, "owner");

    let plan = previewed_then_applied(&host, &vault, |request| request);

    assert_eq!(plan.operations, vec![setting(1, "a.md", "owner", "drew")]);
    assert_eq!(
        provenance(&plan).citations[0].findings[0].confidence,
        Confidence::Derived
    );
    assert_eq!(provenance(&plan).citations[0].findings[0].finding, a);
    assert_eq!(
        skipped_ids(&plan),
        vec![b],
        "{:?}",
        provenance(&plan).skipped
    );
    assert_eq!(
        provenance(&plan).skipped[0].reason,
        SkipReason::NoDeclaredFix
    );
    let after = standing(&host, &vault, |request| request);
    let still: Vec<_> = after.values().cloned().collect();
    assert_eq!(
        still,
        [(format!("{FOLDER}b.md"), FindingKind::ShapeMismatch)],
        "{after:?}"
    );
}

/// **A boolean spelled with another case or padding is written as the YAML
/// boolean, through preview then apply, and a yes/no spelling is skipped as an
/// excluded class and stands reported**: a scalar, and the elements of a
/// list into one write.
#[test]
fn a_boolean_spelled_by_case_is_written_as_the_boolean_through_preview_then_apply() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-boolean",
        "version: 1\nfields:\n  done: {type: boolean}\n  flags: {type: boolean, shape: list}\n",
        &[
            ("a.md", b"---\ndone: \"True\"\n---\n# A\n"),
            ("b.md", b"---\nflags: [\"FALSE\", \"yes\"]\n---\n# B\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let scalar = finding_valued(
        &host,
        &vault,
        "a.md",
        FindingKind::TypeMismatch,
        "done",
        "True",
    );
    let element = finding_valued(
        &host,
        &vault,
        "b.md",
        FindingKind::TypeMismatch,
        "flags",
        "FALSE",
    );
    let yes = finding_valued(
        &host,
        &vault,
        "b.md",
        FindingKind::TypeMismatch,
        "flags",
        "yes",
    );

    let plan = previewed_then_applied(&host, &vault, |request| request);

    assert_eq!(
        plan.operations,
        vec![
            setting_value(1, "a.md", "done", AuthoredValue::Bool(true)),
            setting_value(
                2,
                "b.md",
                "flags",
                AuthoredValue::List(vec![
                    AuthoredValue::Bool(false),
                    AuthoredValue::string("yes")
                ])
            ),
        ]
    );
    assert_eq!(
        provenance(&plan).citations,
        vec![
            Citation::new(
                OperationId::new("repair-1").expect("an id"),
                vec![cited_derived(scalar, "True")]
            ),
            Citation::new(
                OperationId::new("repair-2").expect("an id"),
                vec![cited_derived(element, "FALSE")]
            ),
        ]
    );
    let skipped = &provenance(&plan).skipped;
    assert_eq!(skipped.len(), 1, "{skipped:?}");
    assert_eq!(
        (skipped[0].finding, skipped[0].reason),
        (yes, SkipReason::ExcludedClass)
    );
    assert!(
        skipped[0]
            .note
            .as_deref()
            .is_some_and(|note| note.contains("yes/no coercion")),
        "{skipped:?}"
    );
    assert!(
        written(&vault, "a.md").contains("done: true"),
        "{}",
        written(&vault, "a.md")
    );
    let after = standing(&host, &vault, |request| request);
    let still: Vec<_> = after.values().cloned().collect();
    assert_eq!(
        still,
        [(format!("{FOLDER}b.md"), FindingKind::TypeMismatch)],
        "{after:?}"
    );
}

/// A closed `status` over `todo`, `done` and `in progress`, a list `labels`
/// closed over `red` and `blue`.
const FOLDING: &str = "version: 1\nfields:\n  labels: {type: text, shape: list}\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    one_of:\n      status: {values: [todo, done, 'in progress']}\n      labels: {values: [red, blue]}\n";

/// **A value differing from exactly one member only by case or white space is
/// written as that member, through preview then apply**: a scalar, and a
/// list's element into one write beside a neighbour that stands reported.
#[test]
fn a_value_differing_from_a_member_by_case_or_white_space_is_written_as_the_member() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-fold",
        FOLDING,
        &[
            (
                "a.md",
                b"---\ntype: task\nstatus: \" In   Progress \"\n---\n# A\n",
            ),
            ("b.md", b"---\ntype: task\nlabels: [Red, mauve]\n---\n# B\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let scalar = finding_valued(
        &host,
        &vault,
        "a.md",
        FindingKind::NotOneOf,
        "status",
        " In   Progress ",
    );
    let element = finding_valued(
        &host,
        &vault,
        "b.md",
        FindingKind::NotOneOf,
        "labels",
        "Red",
    );
    let mauve = finding_valued(
        &host,
        &vault,
        "b.md",
        FindingKind::NotOneOf,
        "labels",
        "mauve",
    );

    let plan = previewed_then_applied(&host, &vault, |request| request);

    assert_eq!(
        plan.operations,
        vec![
            setting(1, "a.md", "status", "in progress"),
            setting_list(2, "b.md", "labels", &["red", "mauve"]),
        ]
    );
    assert_eq!(
        provenance(&plan).citations,
        vec![
            Citation::new(
                OperationId::new("repair-1").expect("an id"),
                vec![cited_derived(scalar, " In   Progress ")]
            ),
            Citation::new(
                OperationId::new("repair-2").expect("an id"),
                vec![cited_derived(element, "Red")]
            ),
        ]
    );
    assert_eq!(
        provenance(&plan).skipped,
        vec![
            SkippedFinding::new(mauve, SkipReason::NoDeclaredFix)
                .with_value(norn_store::value_head("mauve"))
        ]
    );
    assert!(
        written(&vault, "a.md").contains("status: \"in progress\""),
        "{}",
        written(&vault, "a.md")
    );
    let after = standing(&host, &vault, |request| request);
    let still: Vec<_> = after.values().cloned().collect();
    assert_eq!(
        still,
        [(format!("{FOLDER}b.md"), FindingKind::NotOneOf)],
        "{after:?}"
    );
}

/// **A type mismatch on a date or a number field is skipped as an excluded
/// class in both modes, and stands reported**: the repair writes nothing.
#[test]
fn a_date_or_number_type_mismatch_is_skipped_as_an_excluded_class_and_stands() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-excluded",
        "version: 1\nfields:\n  due: {type: date}\n  points: {type: number}\n",
        &[("a.md", b"---\ndue: 1/2/26\npoints: three\n---\n# A\n")],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let before = standing(&host, &vault, |request| request);
    assert_eq!(before.len(), 2, "{before:?}");

    let plan = previewed_then_applied(&host, &vault, |request| request);

    assert!(plan.operations.is_empty());
    assert_eq!(
        provenance(&plan)
            .skipped
            .iter()
            .map(|skipped| skipped.reason)
            .collect::<Vec<_>>(),
        vec![SkipReason::ExcludedClass, SkipReason::ExcludedClass]
    );
    assert_eq!(
        standing(&host, &vault, |request| request)
            .values()
            .cloned()
            .collect::<Vec<_>>(),
        before.values().cloned().collect::<Vec<_>>()
    );
}

/// **A derived fix below the request's threshold is skipped in both modes,
/// carrying its value and the operation the plan would have held; the same
/// plan at the default threshold writes it.** A declared fix is written at
/// every threshold.
#[test]
fn a_derived_fix_below_the_threshold_is_skipped_in_preview_and_apply_alike() {
    let schema = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    one_of:\n      status: {values: [todo, done], synonyms: {complete: done}}\n";
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-threshold",
        schema,
        &[
            ("a.md", b"---\ntype: task\nstatus: Done\n---\n# A\n"),
            ("b.md", b"---\ntype: task\nstatus: complete\n---\n# B\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let folded = finding_valued(
        &host,
        &vault,
        "a.md",
        FindingKind::NotOneOf,
        "status",
        "Done",
    );
    let synonym = finding_valued(
        &host,
        &vault,
        "b.md",
        FindingKind::NotOneOf,
        "status",
        "complete",
    );
    let before = tree_bytes(vault.path());

    let plan = previewed_then_applied(&host, &vault, |request| {
        request.with_threshold(Confidence::Declared)
    });

    assert_eq!(plan.operations, vec![setting(1, "b.md", "status", "done")]);
    assert_eq!(
        provenance(&plan).citations,
        vec![Citation::new(
            OperationId::new("repair-1").expect("an id"),
            vec![cited_valued(synonym, "complete")]
        )]
    );
    let proposed = Operation::new(OperationKind::set_frontmatter(
        WriteTarget::path(at("a.md")),
        "status",
        AuthoredValue::string("done"),
    ));
    assert_eq!(
        provenance(&plan).skipped,
        vec![
            SkippedFinding::new(folded, SkipReason::BelowThreshold)
                .with_value(norn_store::value_head("Done"))
                .with_proposed(proposed)
        ]
    );
    let after = tree_bytes(vault.path());
    assert_eq!(
        after.get(&format!("{FOLDER}a.md")),
        before.get(&format!("{FOLDER}a.md"))
    );
    assert_ne!(
        after.get(&format!("{FOLDER}b.md")),
        before.get(&format!("{FOLDER}b.md"))
    );

    // At the default threshold the fold is written.
    let plan = previewed_then_applied(&host, &vault, |request| request);
    assert_eq!(plan.operations.len(), 1, "{:?}", plan.operations);
    assert_eq!(
        written(&vault, "a.md"),
        "---\ntype: task\nstatus: done\n---\n# A\n"
    );
}

/// **A declared synonym decides over the fold, and the citation notes the
/// derived answer when it differs; a declared fix the judge refuses blocks
/// the derived answer, and the finding skips with both candidates.**
#[test]
fn a_declared_fix_outranks_the_derived_answer_through_preview_then_apply() {
    let schema = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    one_of:\n      status: {values: [todo, done, finished], synonyms: {Done: finished, Todo: finished}}\n  strict:\n    match: {frontmatter: {type: task, strict: yes}}\n    one_of:\n      status: {values: [todo, done]}\n";
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-precedence",
        schema,
        &[
            ("a.md", b"---\ntype: task\nstatus: Done\n---\n# A\n"),
            (
                "b.md",
                b"---\ntype: task\nstrict: yes\nstatus: Todo\n---\n# B\n",
            ),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let decided = finding_valued(
        &host,
        &vault,
        "a.md",
        FindingKind::NotOneOf,
        "status",
        "Done",
    );
    let blocked = finding_valued(
        &host,
        &vault,
        "b.md",
        FindingKind::NotOneOf,
        "status",
        "Todo",
    );

    let plan = previewed_then_applied(&host, &vault, |request| request);

    assert_eq!(
        plan.operations,
        vec![setting(1, "a.md", "status", "finished")]
    );
    let cited = &provenance(&plan).citations[0].findings[0];
    assert_eq!(
        (cited.finding, cited.confidence),
        (decided, Confidence::Declared)
    );
    assert_eq!(cited.notes.len(), 1, "{:?}", cited.notes);
    assert!(cited.notes[0].contains("`done`"), "{:?}", cited.notes);
    let skipped = &provenance(&plan).skipped;
    assert_eq!(skipped.len(), 1, "{skipped:?}");
    assert_eq!(
        (skipped[0].finding, skipped[0].reason),
        (blocked, SkipReason::JudgeWouldRefuse)
    );
    let Some(SkippedCandidates::Values { head, .. }) = &skipped[0].candidates else {
        panic!("candidates: {skipped:?}");
    };
    let named: Vec<(String, Option<String>)> = head
        .candidates()
        .iter()
        .map(|candidate| (candidate.value.text().to_string(), candidate.rule.clone()))
        .collect();
    assert_eq!(
        named,
        vec![
            ("finished".to_string(), Some("tasks".to_string())),
            ("todo".to_string(), None)
        ]
    );
    assert_eq!(
        written(&vault, "b.md"),
        "---\ntype: task\nstrict: yes\nstatus: Todo\n---\n# B\n"
    );
}

/// A `link` field `up`, which a bare value leaves a type mismatch.
const LINKING: &str = "version: 1\nfields:\n  up: {type: link}\n";

/// A document holding `up: zz-target` and the one document that value names.
const LINKED_PAIR: [(&str, &[u8]); 2] = [
    ("zz-holder.md", b"---\nup: zz-target\n---\n# Holder\n"),
    ("zz-target.md", b"# Target\n"),
];

/// The address resolution a repair of [`LINKED_PAIR`] records: `zz-target`,
/// written in the holder, names the target at `lands`.
fn linking_to(lands: &str) -> norn_wire::PlanCondition {
    linking_from("zz-holder.md", lands)
}

/// The address resolution a repair records for `zz-target`, written in the
/// holder at `holder`, naming the target at `lands`.
fn linking_from(holder: &str, lands: &str) -> norn_wire::PlanCondition {
    norn_wire::PlanCondition::address_resolution(
        norn_wire::LinkKey::new(at(holder), norn_wire::LinkFamily::Wikilink, "zz-target"),
        norn_wire::Resolves::one(at(lands)),
    )
}

/// The refusal an apply of `plan` met, with the plan the refusal carries.
fn apply_refused(
    host: &attach::ServingHost,
    mode: ApplyMode,
    plan: &ResolvedPlan,
) -> (ErrorEnvelope, ResolvedPlan, Vec<norn_wire::RefusedCheck>) {
    let refusal = refused(host.apply(ApplyParams::new(mode, PlanDocument::resolved(plan.clone()))));
    assert_eq!(refusal.code(), &ReasonCode::VaultPlanRefused, "{refusal:?}");
    let ErrorDetail::PlanRefused { plan, checks, .. } = refusal.detail() else {
        panic!("refused with {:?}", refusal.detail());
    };
    let (plan, checks) = (plan.clone(), checks.clone());
    (refusal, plan, checks)
}

/// Whether `checks` fail the address resolution `condition`.
fn fails(checks: &[norn_wire::RefusedCheck], condition: &norn_wire::PlanCondition) -> bool {
    checks.iter().any(|check| {
        matches!(check, norn_wire::RefusedCheck::ConditionFailed { condition: failed, .. } if failed == condition)
    })
}

/// **A bare value is made a link through preview then apply, and the plan
/// records the resolution of the address it wrote.** The condition reaches
/// the resolved plan in both modes, only the link fix records one, and the
/// validator no longer reports the finding.
#[test]
fn a_bare_value_is_made_a_link_through_preview_then_apply() {
    let (_sandbox, vault, host) = a_vault_under("host-repair-bare-link", LINKING, &LINKED_PAIR);
    let _lease = attach::attach_and_wait(&host, vault.name());
    let mismatch = finding_valued(
        &host,
        &vault,
        "zz-holder.md",
        FindingKind::TypeMismatch,
        "up",
        "zz-target",
    );

    let plan = previewed_then_applied(&host, &vault, |request| request);

    assert_eq!(
        plan.operations,
        vec![setting(1, "zz-holder.md", "up", "[[zz-target]]")]
    );
    assert_eq!(plan.conditions, vec![linking_to("zz-target.md")]);
    let cited = &provenance(&plan).citations[0].findings[0];
    assert_eq!(
        (cited.finding, cited.confidence),
        (mismatch, Confidence::Derived)
    );
    assert!(
        written(&vault, "zz-holder.md").contains("up: '[[zz-target]]'")
            || written(&vault, "zz-holder.md").contains("up: \"[[zz-target]]\""),
        "{}",
        written(&vault, "zz-holder.md")
    );
    assert!(standing(&host, &vault, |request| request).is_empty());
}

/// **Deleting the target between preview and apply refuses the apply, in
/// apply mode and in a preview that judges the previewed plan again.**
#[test]
fn deleting_the_target_after_the_preview_refuses_the_plan() {
    let (_sandbox, vault, host) =
        a_vault_under("host-repair-bare-link-deleted", LINKING, &LINKED_PAIR);
    let _lease = attach::attach_and_wait(&host, vault.name());
    let plan = previewed(host.repair(repairing(&vault, ApplyMode::Preview)));
    host.delete(norn_wire::DeleteParams::new(
        address(&vault),
        ApplyMode::Apply,
        at("zz-target.md"),
    ))
    .expect("the delete is admitted")
    .wait()
    .expect("the target is deleted");

    for mode in [ApplyMode::Apply, ApplyMode::Preview] {
        let (_, _, checks) = apply_refused(&host, mode, &plan);
        assert!(
            fails(&checks, &linking_to("zz-target.md")),
            "{mode:?}: {checks:?}"
        );
    }
    assert_eq!(
        written(&vault, "zz-holder.md"),
        "---\nup: zz-target\n---\n# Holder\n"
    );
}

/// **A second document of the same stem, created between preview and apply,
/// refuses the plan**: the address names two documents now.
#[test]
fn a_competing_target_created_after_the_preview_refuses_the_plan() {
    let (_sandbox, vault, host) =
        a_vault_under("host-repair-bare-link-competitor", LINKING, &LINKED_PAIR);
    let _lease = attach::attach_and_wait(&host, vault.name());
    let plan = previewed(host.repair(repairing(&vault, ApplyMode::Preview)));
    host.new_document(NewParams::new(
        address(&vault),
        ApplyMode::Apply,
        at("elsewhere/zz-target.md"),
        "# Competitor\n",
    ))
    .expect("the write is admitted")
    .wait()
    .expect("the competitor is written");

    for mode in [ApplyMode::Apply, ApplyMode::Preview] {
        let (_, _, checks) = apply_refused(&host, mode, &plan);
        assert!(
            fails(&checks, &linking_to("zz-target.md")),
            "{mode:?}: {checks:?}"
        );
    }
    assert_eq!(
        written(&vault, "zz-holder.md"),
        "---\nup: zz-target\n---\n# Holder\n"
    );
}

/// **A refused plan's fresh plan carries the recorded resolution as it was.**
/// A holder changed after the preview refuses the plan; where the evidence is
/// unchanged the fresh plan applies and lands the link, and where the target
/// is gone the fresh plan refuses again for the same condition.
#[test]
fn a_refused_plans_fresh_plan_carries_the_resolution_and_refuses_where_the_evidence_changed() {
    for target_deleted in [false, true] {
        let label = format!("host-repair-bare-link-fresh-{target_deleted}");
        let (_sandbox, vault, host) = a_vault_under(&label, LINKING, &LINKED_PAIR);
        let _lease = attach::attach_and_wait(&host, vault.name());
        let plan = previewed(host.repair(repairing(&vault, ApplyMode::Preview)));
        std::fs::write(
            vault.path().join(FOLDER).join("zz-holder.md"),
            "---\nup: zz-target\nnote: x\n---\n# Holder\n",
        )
        .expect("another writer changes the holder");
        if target_deleted {
            host.delete(norn_wire::DeleteParams::new(
                address(&vault),
                ApplyMode::Apply,
                at("zz-target.md"),
            ))
            .expect("the delete is admitted")
            .wait()
            .expect("the target is deleted");
        }

        let (_, fresh, _) = apply_refused(&host, ApplyMode::Apply, &plan);

        assert_eq!(fresh.conditions, vec![linking_to("zz-target.md")]);
        if target_deleted {
            let (_, _, checks) = apply_refused(&host, ApplyMode::Apply, &fresh);
            assert!(fails(&checks, &linking_to("zz-target.md")), "{checks:?}");
        } else {
            let report = planned(host.apply(ApplyParams::new(
                ApplyMode::Apply,
                PlanDocument::resolved(fresh),
            )));
            assert!(matches!(report, ApplyReport::Applied { .. }), "{report:?}");
            let holder = written(&vault, "zz-holder.md");
            assert!(
                holder.contains("[[zz-target]]") && holder.contains("note: x"),
                "{holder}"
            );
        }
    }
}

/// **Declared limit (NORN-371): a refused plan's fresh plan also carries the
/// address resolution of an operation it leaves unresolved.** A condition
/// names no operation, so where another writer removes one holder of a
/// two-link repair plan the fresh plan lists that holder's operation as
/// unresolved and still carries the condition recorded for its link, and may
/// refuse on evidence for an operation it no longer holds.
#[test]
fn declared_limit_a_refused_repair_plans_fresh_plan_carries_the_condition_of_an_operation_it_left_unresolved()
 {
    const HOLDER_B: &str = "zz-holder-b.md";
    let documents: [(&str, &[u8]); 3] = [
        ("zz-holder.md", b"---\nup: zz-target\n---\n# Holder\n"),
        (HOLDER_B, b"---\nup: zz-target\n---\n# Holder B\n"),
        ("zz-target.md", b"# Target\n"),
    ];
    let (_sandbox, vault, host) = a_vault_under("host-repair-fresh-limit", LINKING, &documents);
    let _lease = attach::attach_and_wait(&host, vault.name());
    let plan = previewed(host.repair(repairing(&vault, ApplyMode::Preview)));
    assert_eq!(plan.operations.len(), 2, "{:?}", plan.operations);
    assert_eq!(
        plan.conditions,
        vec![
            linking_from(HOLDER_B, "zz-target.md"),
            linking_to("zz-target.md")
        ]
    );
    // Another writer changes one holder, which refuses the plan, and removes
    // the other, which leaves its operation nothing to write to.
    std::fs::write(
        vault.path().join(FOLDER).join("zz-holder.md"),
        "---\nup: zz-target\nnote: x\n---\n# Holder\n",
    )
    .expect("another writer changes a holder");
    std::fs::remove_file(vault.path().join(FOLDER).join(HOLDER_B))
        .expect("another writer removes the other holder");

    let (refusal, fresh, _) = apply_refused(&host, ApplyMode::Apply, &plan);

    let ErrorDetail::PlanRefused { unresolved, .. } = refusal.detail() else {
        panic!("refused with {:?}", refusal.detail());
    };
    assert_eq!(
        unresolved.len(),
        1,
        "the removed holder's operation is left to the caller: {unresolved:?}"
    );
    assert_eq!(
        kinds(&fresh),
        vec![setting(1, "zz-holder.md", "up", "[[zz-target]]").kind],
        "the fresh plan holds the changed holder's operation alone"
    );
    // The limit: the condition for the operation the fresh plan no longer
    // holds is carried all the same.
    assert_eq!(
        fresh.conditions,
        vec![
            linking_from(HOLDER_B, "zz-target.md"),
            linking_to("zz-target.md")
        ]
    );
}

/// **A route in the same batch moving the target leaves a plan that applies
/// with the link resolving**: the plan records the path the target lands at.
#[test]
fn a_route_in_the_batch_moving_the_target_leaves_a_plan_that_applies_with_the_link_resolving() {
    let schema = "version: 1\nfields:\n  up: {type: link}\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    allowed_paths: {paths: ['zz-repair/tasks/**'], route: 'zz-repair/tasks/'}\n";
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-bare-link-routed",
        schema,
        &[
            ("zz-holder.md", b"---\nup: zz-target\n---\n# Holder\n"),
            ("zz-target.md", b"---\ntype: task\n---\n# Target\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());

    let plan = previewed_then_applied(&host, &vault, |request| request);

    // The resolution change set records the written link's own move with the
    // target; the repair records the resolution of the address it wrote.
    let recorded: Vec<_> = plan
        .conditions
        .iter()
        .filter(|condition| {
            matches!(
                condition,
                norn_wire::PlanCondition::AddressResolution { .. }
            )
        })
        .collect();
    assert_eq!(recorded, vec![&linking_to("tasks/zz-target.md")]);
    assert!(
        vault
            .path()
            .join(FOLDER)
            .join("tasks/zz-target.md")
            .exists()
    );
    assert!(written(&vault, "zz-holder.md").contains("[[zz-target]]"));
    assert!(standing(&host, &vault, |request| request).is_empty());
}

/// What the link judgments of a repair's preview cost on the store's
/// resolution door, over a vault where a route moves `loose/x.md` and
/// `holders` documents each hold a bare value of `up`: every even one naming
/// a document of its own, every odd one a stem two documents share.
fn link_judgments_of_a_preview(label: &str, holders: usize) -> u64 {
    let mut documents: Vec<(String, Vec<u8>)> = vec![
        ("loose/x.md".to_string(), b"---\ntype: task\n---\n".to_vec()),
        ("a/zz-twin.md".to_string(), b"# A\n".to_vec()),
        ("b/zz-twin.md".to_string(), b"# B\n".to_vec()),
    ];
    for at in 0..holders {
        let up = if at % 2 == 0 {
            documents.push((format!("zz-t{at}.md"), b"# T\n".to_vec()));
            format!("zz-t{at}")
        } else {
            "zz-twin".to_string()
        };
        documents.push((
            format!("h{at}.md"),
            format!("---\nup: {up}\n---\n").into_bytes(),
        ));
    }
    let documents: Vec<(&str, &[u8])> = documents
        .iter()
        .map(|(at, bytes)| (at.as_str(), bytes.as_slice()))
        .collect();
    let schema = format!("{TASKED}fields:\n  up: {{type: link}}\n");
    let (_sandbox, vault, host) = a_vault_under(label, &schema, &documents);
    let _lease = attach::attach_and_wait(&host, vault.name());
    let account = host.read_evidence();
    let plan = previewed(host.repair(repairing_beneath(&vault, ApplyMode::Preview)));
    let judged = host.read_evidence().since(account).preview_link_judgments;
    let written = plan
        .operations
        .iter()
        .filter(|operation| matches!(operation.kind, OperationKind::SetFrontmatter { .. }))
        .count();
    assert_eq!(written, holders.div_ceil(2), "{plan:?}");
    assert_eq!(
        provenance(&plan).skipped.len(),
        holders / 2,
        "every odd holder's value names two documents"
    );
    judged.judgments
}

/// **A repair resolves every link its fixes write in one judgment, however
/// many there are**: the preview of two link fixes and of six, beside a
/// route, judges as often — once for the batch's links, once more for the
/// heads of those naming several — so the cost of resolving them follows the
/// links, never the links times the routes.
#[test]
fn every_link_a_repair_writes_is_resolved_in_one_judgment_however_many() {
    let few = link_judgments_of_a_preview("host-repair-link-judgments-few", 2);
    let many = link_judgments_of_a_preview("host-repair-link-judgments-many", 6);
    assert_eq!(few, many);
}

/// A route moving `loose/x.md` into `tasks-all/`, whose longer folder name
/// lengthens a link to it, and a rule selecting a document by the link its
/// `up` holds, limiting its `parent` to the length of the link as written now.
const SELECTED_BY_LINK: &str = "version: 1\nfields:\n  up: {type: link}\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    allowed_paths: {paths: ['zz-repair/tasks-all/**'], route: 'zz-repair/tasks-all/'}\n  projects:\n    match: {frontmatter: {up: '[[zz-projects]]'}}\n    max_length:\n      parent: 21\n";

/// **A link fix bringing its holder under a rule that reads a link a route
/// respells skips; the route moves.** Making `up` the link `[[zz-projects]]`
/// brings `h.md` under the rule limiting `parent`, and the route of
/// `loose/x.md` respells `parent`'s link: the two together would introduce a
/// value past the limit. The link fix skips as one the judge would refuse,
/// and the plan applies.
#[test]
fn a_link_fix_bringing_its_holder_under_a_rule_reading_a_respelled_link_skips() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-link-fix-respelled",
        SELECTED_BY_LINK,
        &[
            ("loose/x.md", b"---\ntype: task\n---\n# X\n"),
            (
                "h.md",
                b"---\nup: zz-projects\nparent: '[[zz-repair/loose/x]]'\n---\n# H\n",
            ),
            ("zz-projects.md", b"# Projects\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());

    let plan = applied_leaving_only_its_skips(&host, &vault);

    assert!(stands(&vault, "tasks-all/x.md"));
    let skipped: Vec<SkipReason> = provenance(&plan)
        .skipped
        .iter()
        .map(|skip| skip.reason)
        .collect();
    assert_eq!(skipped, [SkipReason::JudgeWouldRefuse], "{plan:?}");
    assert!(written(&vault, "h.md").contains("up: zz-projects"));
}

/// **What a below-threshold skip proposes is what lowering the threshold
/// writes, through the host**: each of a list's two folded elements proposes
/// the one write of the field the derived threshold's plan holds.
#[test]
fn below_threshold_proposals_of_a_lists_elements_are_what_lowering_the_threshold_writes() {
    let schema = "version: 1\nfields:\n  labels: {type: text, shape: list}\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    one_of:\n      labels: {values: [red, blue]}\n";
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-below-threshold-proposals",
        schema,
        &[("h.md", b"---\ntype: task\nlabels: [BLUE, RED]\n---\n# H\n")],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());

    let below = previewed(
        host.repair(repairing(&vault, ApplyMode::Preview).with_threshold(Confidence::Declared)),
    );
    let lowered = previewed(host.repair(repairing(&vault, ApplyMode::Preview)));

    let proposed: Vec<Option<OperationKind>> = provenance(&below)
        .skipped
        .iter()
        .map(|skipped| {
            skipped
                .proposed
                .as_ref()
                .map(|operation| operation.kind.clone())
        })
        .collect();
    let written = kinds(&lowered);
    assert_eq!(written.len(), 1, "{written:?}");
    assert_eq!(proposed, vec![Some(written[0].clone()); 2]);
}

/// **A bare value naming a document whose route the batch refuses resolves
/// where that document stays**: two routes land at one destination, so the
/// later is refused as taken and its document stays; `b/zz-b`, path-qualified
/// to it, still names it once the plan lands, and the plan applies leaving
/// only its skips.
#[test]
fn a_bare_value_naming_a_document_whose_route_the_batch_refuses_resolves_where_it_stays() {
    let schema = format!("{TASKED}fields:\n  up: {{type: link}}\n");
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-bare-link-refused-route",
        &schema,
        &[
            ("a/zz-b.md", b"---\ntype: task\n---\n# First\n"),
            ("b/zz-b.md", b"---\ntype: task\n---\n# Second\n"),
            ("h.md", b"---\nup: b/zz-b\n---\n# H\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());

    let plan = applied_leaving_only_its_skips(&host, &vault);

    assert!(stands(&vault, "b/zz-b.md"));
    assert!(written(&vault, "h.md").contains("[[b/zz-b]]"));
    let recorded: Vec<&norn_wire::PlanCondition> = plan
        .conditions
        .iter()
        .filter(|condition| {
            matches!(
                condition,
                norn_wire::PlanCondition::AddressResolution { .. }
            )
        })
        .collect();
    assert_eq!(
        recorded,
        vec![&norn_wire::PlanCondition::address_resolution(
            norn_wire::LinkKey::new(at("h.md"), norn_wire::LinkFamily::Wikilink, "b/zz-b"),
            norn_wire::Resolves::one(at("b/zz-b.md")),
        )]
    );
}

// ---- link findings a plan settles ----

/// A forbidden removal drops each kind of link finding in its field, while
/// identical links in another field and in the body stay skipped.
#[test]
fn a_forbidden_removal_drops_its_link_findings_and_keeps_identical_links_elsewhere() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-settle-forbidden-remove",
        BANNED,
        &[
            ("one/zz-twin.md", b"# One\n"),
            ("two/zz-twin.md", b"# Two\n"),
            ("zz-peer.md", b"# Peer\n"),
            (
                "h.md",
                b"---\ntype: task\nscratch: '[[zz-absent]] [[zz-twin]] [[zz-peer#Nope]]'\nkept: '[[zz-absent]] [[zz-twin]] [[zz-peer#Nope]]'\n---\n[[zz-absent]] [[zz-twin]] [[zz-peer#Nope]]\n",
            ),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let linked = link_findings_of(&host, &vault, "h.md");
    assert_eq!(linked.len(), 9);
    for kind in [
        FindingKind::Broken,
        FindingKind::Ambiguous,
        FindingKind::MissingAnchor,
    ] {
        assert_eq!(linked.iter().filter(|row| row.kind == kind).count(), 3);
    }
    let kept: BTreeSet<u64> = linked
        .iter()
        .filter(|row| row.span.expect("a literal link has a span").line != 3)
        .map(|row| row.id)
        .collect();
    assert_eq!(kept.len(), 6);

    let preview = previewed(host.repair(repairing_beneath(&vault, ApplyMode::Preview)));
    assert_eq!(
        provenance(&preview)
            .skipped
            .iter()
            .map(|skip| skip.finding)
            .collect::<BTreeSet<_>>(),
        kept,
    );
    let ApplyReport::Applied { plan, .. } =
        planned(host.repair(repairing_beneath(&vault, ApplyMode::Apply)))
    else {
        panic!("the repair applies");
    };
    assert_eq!(preview.operations, plan.operations);
    assert_eq!(provenance(&preview).skipped, provenance(&plan).skipped);
    assert_eq!(
        written(&vault, "h.md"),
        "---\ntype: task\nkept: '[[zz-absent]] [[zz-twin]] [[zz-peer#Nope]]'\n---\n[[zz-absent]] [[zz-twin]] [[zz-peer#Nope]]\n"
    );
    let after = link_findings_of(&host, &vault, "h.md");
    assert_eq!(after.len(), 6);
    for kind in [
        FindingKind::Broken,
        FindingKind::Ambiguous,
        FindingKind::MissingAnchor,
    ] {
        assert_eq!(after.iter().filter(|row| row.kind == kind).count(), 2);
    }
}

/// A forbidden rename keeps each link finding skipped with the link in its
/// new field.
#[test]
fn a_forbidden_rename_keeps_its_link_findings_skipped() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-settle-forbidden-rename",
        BANNED,
        &[
            ("one/zz-twin.md", b"# One\n"),
            ("two/zz-twin.md", b"# Two\n"),
            ("zz-peer.md", b"# Peer\n"),
            (
                "h.md",
                b"---\ntype: task\ndue_date: '[[zz-absent]] [[zz-twin]] [[zz-peer#Nope]]'\n---\n",
            ),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let linked = link_findings_of(&host, &vault, "h.md");
    assert_eq!(linked.len(), 3);
    for kind in [
        FindingKind::Broken,
        FindingKind::Ambiguous,
        FindingKind::MissingAnchor,
    ] {
        assert_eq!(linked.iter().filter(|row| row.kind == kind).count(), 1);
    }
    let kept: BTreeSet<u64> = linked.iter().map(|row| row.id).collect();

    let preview = previewed(host.repair(repairing_beneath(&vault, ApplyMode::Preview)));
    assert_eq!(
        provenance(&preview)
            .skipped
            .iter()
            .map(|skip| skip.finding)
            .collect::<BTreeSet<_>>(),
        kept
    );
    let ApplyReport::Applied { plan, .. } =
        planned(host.repair(repairing_beneath(&vault, ApplyMode::Apply)))
    else {
        panic!("the repair applies");
    };
    assert_eq!(preview.operations, plan.operations);
    assert_eq!(provenance(&preview).skipped, provenance(&plan).skipped);
    assert_eq!(
        written(&vault, "h.md"),
        "---\ntype: task\ndue: '[[zz-absent]] [[zz-twin]] [[zz-peer#Nope]]'\n---\n"
    );
    let after = link_findings_of(&host, &vault, "h.md");
    assert_eq!(after.len(), 3);
    for kind in [
        FindingKind::Broken,
        FindingKind::Ambiguous,
        FindingKind::MissingAnchor,
    ] {
        assert_eq!(after.iter().filter(|row| row.kind == kind).count(), 1);
    }
}

/// A forbidden removal on a document its route moves drops the link findings
/// its field held: the removal names the field at the document's path before
/// the move, so the route does not hide it.
#[test]
fn a_forbidden_removal_on_a_routed_document_drops_its_link_findings() {
    let schema = format!(
        "{BANNED}    allowed_paths: {{paths: ['zz-repair/tasks/**'], route: 'zz-repair/tasks/'}}\n"
    );
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-settle-forbidden-remove-routed",
        &schema,
        &[
            ("one/zz-twin.md", b"# One\n"),
            ("two/zz-twin.md", b"# Two\n"),
            ("zz-peer.md", b"# Peer\n"),
            (
                "inbox/h.md",
                b"---\ntype: task\nscratch: '[[zz-absent]] [[zz-twin]] [[zz-peer#Nope]]'\nkept: '[[zz-absent]] [[zz-twin]] [[zz-peer#Nope]]'\n---\n[[zz-absent]]\n",
            ),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let linked = link_findings_of(&host, &vault, "inbox/h.md");
    assert_eq!(linked.len(), 7);
    let kept: BTreeSet<u64> = linked
        .iter()
        .filter(|row| row.span.expect("a literal link has a span").line != 3)
        .map(|row| row.id)
        .collect();
    assert_eq!(kept.len(), 4);

    let preview = previewed(host.repair(repairing_beneath(&vault, ApplyMode::Preview)));
    assert_eq!(
        provenance(&preview)
            .skipped
            .iter()
            .map(|skip| skip.finding)
            .collect::<BTreeSet<_>>(),
        kept,
    );
    let ApplyReport::Applied { plan, .. } =
        planned(host.repair(repairing_beneath(&vault, ApplyMode::Apply)))
    else {
        panic!("the repair applies");
    };
    assert_eq!(provenance(&preview).skipped, provenance(&plan).skipped);
    assert_eq!(
        written(&vault, "tasks/h.md"),
        "---\ntype: task\nkept: '[[zz-absent]] [[zz-twin]] [[zz-peer#Nope]]'\n---\n[[zz-absent]]\n"
    );
    assert_eq!(link_findings_of(&host, &vault, "tasks/h.md").len(), 4);
}

/// The `link/*` findings standing over the document at `name`, in id order.
fn link_findings_of(
    host: &attach::ServingHost,
    vault: &attach::Vault,
    name: &str,
) -> Vec<norn_wire::FindingRow> {
    let mut rows: Vec<_> = findings_beneath(host, vault)
        .into_iter()
        .filter(|row| {
            row.path == at(name)
                && matches!(
                    row.kind,
                    FindingKind::Broken | FindingKind::MissingAnchor | FindingKind::Ambiguous
                )
        })
        .collect();
    rows.sort_by_key(|row| row.id);
    rows
}

/// **A broken link a route's move makes resolve is dropped, neither fixed nor
/// skipped.** `h.md` names `tasks/zz-land`, which nothing ends with until the
/// route lands `loose/zz-land.md` at `tasks/zz-land.md`; the plan applies,
/// and the validator then flags nothing.
#[test]
fn a_broken_link_a_routes_move_resolves_is_dropped_from_the_skipped_findings() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-settle-broken",
        TASKED,
        &[
            ("loose/zz-land.md", b"---\ntype: task\n---\n# Land\n"),
            ("h.md", b"See [[tasks/zz-land]].\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let misplaced = misplaced_of(&host, &vault, "loose/zz-land.md");
    let linked = link_findings_of(&host, &vault, "h.md");
    let [broken] = linked.as_slice() else {
        panic!("one link finding");
    };
    assert_eq!(broken.kind, FindingKind::Broken);

    let preview = previewed(host.repair(repairing_beneath(&vault, ApplyMode::Preview)));
    assert_eq!(
        provenance(&preview).citations,
        vec![citing(1, &[misplaced])]
    );
    assert!(
        provenance(&preview).skipped.is_empty(),
        "the broken link resolves once the route lands: {:?}",
        provenance(&preview).skipped
    );

    let plan = applied_leaving_only_its_skips(&host, &vault);
    assert!(provenance(&plan).skipped.is_empty());
    assert!(stands(&vault, "tasks/zz-land.md"));
    assert!(findings_beneath(&host, &vault).is_empty());
}

/// **An ambiguous link a route's move makes name one document is dropped.**
/// `h.md` names `sub/zz-twin`, which two documents end with until the route
/// takes `other/sub/zz-twin.md` away; the link is no backlink of either, so
/// the move's cascade leaves it as written.
#[test]
fn an_ambiguous_link_a_routes_move_makes_name_one_document_is_dropped() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-settle-ambiguous",
        TASKED,
        &[
            ("sub/zz-twin.md", b"# Stays\n"),
            ("other/sub/zz-twin.md", b"---\ntype: task\n---\n# Leaves\n"),
            ("h.md", b"See [[sub/zz-twin]].\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let linked = link_findings_of(&host, &vault, "h.md");
    let [ambiguous] = linked.as_slice() else {
        panic!("one link finding");
    };
    assert_eq!(ambiguous.kind, FindingKind::Ambiguous);

    let plan = applied_leaving_only_its_skips(&host, &vault);

    assert!(
        provenance(&plan).skipped.is_empty(),
        "{:?}",
        provenance(&plan).skipped
    );
    assert!(stands(&vault, "tasks/zz-twin.md"));
    assert!(findings_beneath(&host, &vault).is_empty());
}

/// **Two links of one address in one holder are two links: the route that
/// resolves one drops that one's finding and not the other's.** `sub/h.md`
/// holds a wikilink written `tasks/zz-land.md` and a Markdown link written
/// `./tasks/zz-land.md`; the wikilink is read by suffix and the Markdown link
/// from the holder's folder, so the route landing `loose/zz-land.md` at
/// `tasks/zz-land.md` resolves the wikilink and leaves the Markdown link
/// broken.
#[test]
fn only_the_link_a_route_resolves_is_dropped_when_two_syntaxes_share_an_address() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-settle-syntaxes",
        TASKED,
        &[
            ("loose/zz-land.md", b"---\ntype: task\n---\n# Land\n"),
            (
                "sub/h.md",
                b"Wiki [[tasks/zz-land.md]].\n\nMark [m](./tasks/zz-land.md).\n",
            ),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let linked = link_findings_of(&host, &vault, "sub/h.md");
    let [wiki, mark] = linked.as_slice() else {
        panic!("two link findings");
    };
    assert_eq!(
        (wiki.kind, mark.kind),
        (FindingKind::Broken, FindingKind::Broken)
    );
    assert!(
        wiki.span.map(|span| span.line) < mark.span.map(|span| span.line),
        "the wikilink is written first"
    );
    let (wiki, mark) = (wiki.id, mark.id);

    let preview = previewed(host.repair(repairing_beneath(&vault, ApplyMode::Preview)));
    let ApplyReport::Applied { plan, .. } =
        planned(host.repair(repairing_beneath(&vault, ApplyMode::Apply)))
    else {
        panic!("the repair applies");
    };

    assert_eq!(preview.operations, plan.operations);
    for plan in [&preview, &plan] {
        assert_eq!(
            provenance(plan).skipped,
            vec![
                SkippedFinding::new(mark, SkipReason::BelowThreshold)
                    .with_proposed(Operation::new(OperationKind::rewrite_link(
                        at("sub/h.md"),
                        norn_wire::LinkFamily::Markdown,
                        "./tasks/zz-land.md",
                        "../tasks/zz-land.md"
                    )))
                    .with_note("similarity 1.00 (trigram, cutoff 0.50)")
            ],
            "wikilink {wiki} resolves and the Markdown link receives a suggestion"
        );
    }
    // The validator agrees: of the two links, the Markdown one stands.
    let after = link_findings_of(&host, &vault, "sub/h.md");
    assert_eq!(
        after
            .iter()
            .map(|row| row.span.map(|s| s.line))
            .collect::<Vec<_>>(),
        vec![Some(3)]
    );
}

/// **A broken link held by a document the batch routes is dropped when the
/// plan makes it resolve.** `loose/zz-holder.md` and `loose/zz-peer.md` are
/// both routed to `tasks/`; the holder's `tasks/zz-peer` resolves once the
/// peer lands, and the plan's change set keys that link at the holder's
/// destination.
#[test]
fn a_broken_link_a_routed_holder_holds_is_dropped_where_the_plan_makes_it_resolve() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-settle-routed-holder",
        TASKED,
        &[
            (
                "loose/zz-holder.md",
                b"---\ntype: task\n---\nSee [[tasks/zz-peer]].\n",
            ),
            ("loose/zz-peer.md", b"---\ntype: task\n---\n# Peer\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let linked = link_findings_of(&host, &vault, "loose/zz-holder.md");
    let [broken] = linked.as_slice() else {
        panic!("one link finding");
    };
    assert_eq!(broken.kind, FindingKind::Broken);

    let plan = applied_leaving_only_its_skips(&host, &vault);

    assert!(
        provenance(&plan).skipped.is_empty(),
        "{:?}",
        provenance(&plan).skipped
    );
    assert!(stands(&vault, "tasks/zz-holder.md"));
    assert!(stands(&vault, "tasks/zz-peer.md"));
    assert!(findings_beneath(&host, &vault).is_empty());
}

/// **A missing-anchor link whose target a route's landing makes ambiguous is
/// dropped.** The heading `Nope` is in no document. `tasks/zz-twin` names
/// `other/tasks/zz-twin.md` alone, with a missing anchor, until the route
/// lands a second document at `tasks/zz-twin.md`; the link then names two
/// documents, and the finding about its anchor is no longer the link's state.
/// The ambiguity the plan makes is a new finding, which the validator reports
/// beside the repair and no selected finding accounts for.
#[test]
fn a_missing_anchor_link_a_route_makes_ambiguous_is_dropped() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-settle-anchor-ambiguous",
        TASKED,
        &[
            ("other/tasks/zz-twin.md", b"# Real\n"),
            ("loose/zz-twin.md", b"---\ntype: task\n---\n# Lands\n"),
            ("h.md", b"See [[tasks/zz-twin#Nope]].\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let linked = link_findings_of(&host, &vault, "h.md");
    let [anchor] = linked.as_slice() else {
        panic!("one link finding");
    };
    assert_eq!(anchor.kind, FindingKind::MissingAnchor);

    let preview = previewed(host.repair(repairing_beneath(&vault, ApplyMode::Preview)));
    assert!(
        provenance(&preview).skipped.is_empty(),
        "{:?}",
        provenance(&preview).skipped
    );
    let ApplyReport::Applied { plan, .. } =
        planned(host.repair(repairing_beneath(&vault, ApplyMode::Apply)))
    else {
        panic!("the repair applies");
    };
    assert_eq!(preview.operations, plan.operations);
    assert!(provenance(&plan).skipped.is_empty());
    let after: Vec<FindingKind> = link_findings_of(&host, &vault, "h.md")
        .iter()
        .map(|row| row.kind)
        .collect();
    assert_eq!(after, vec![FindingKind::Ambiguous]);
}

/// **A missing-anchor link whose target is itself routed stays skipped**: the
/// link still names the one document it named, and repair edits no heading.
#[test]
fn a_missing_anchor_link_whose_target_is_routed_stays_skipped() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-settle-anchor-routed",
        TASKED,
        &[
            ("other/tasks/zz-twin.md", b"---\ntype: task\n---\n# Real\n"),
            ("h.md", b"See [[other/tasks/zz-twin#Nope]].\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let linked = link_findings_of(&host, &vault, "h.md");
    let [anchor] = linked.as_slice() else {
        panic!("one link finding");
    };
    assert_eq!(anchor.kind, FindingKind::MissingAnchor);

    let preview = previewed(host.repair(repairing_beneath(&vault, ApplyMode::Preview)));
    assert_eq!(skipped_ids(&preview), vec![anchor.id]);
    let ApplyReport::Applied { plan, .. } =
        planned(host.repair(repairing_beneath(&vault, ApplyMode::Apply)))
    else {
        panic!("the repair applies");
    };
    assert_eq!(preview.operations, plan.operations);
    assert_eq!(skipped_ids(&plan), vec![anchor.id]);
    assert!(stands(&vault, "tasks/zz-twin.md"));
    // The move's cascade respelled the link to name the document where it
    // lands, and its anchor is still missing there.
    assert!(written(&vault, "h.md").contains("[[zz-repair/tasks/zz-twin#Nope]]"));
    let after: Vec<FindingKind> = link_findings_of(&host, &vault, "h.md")
        .iter()
        .map(|row| row.kind)
        .collect();
    assert_eq!(after, vec![FindingKind::MissingAnchor]);
}

/// **A link finding whose link the plan does not change stays skipped as its
/// row reads**, beside a route that changes others: a broken link, an
/// ambiguous one and one naming a heading that is not there, none of them
/// about the moved document.
#[test]
fn a_link_finding_whose_link_the_plan_does_not_change_stays_skipped() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-settle-unchanged",
        TASKED,
        &[
            ("loose/zz-land.md", b"---\ntype: task\n---\n# Land\n"),
            ("p/zz-two.md", b"# P\n"),
            ("q/zz-two.md", b"# Q\n"),
            ("zz-solo.md", b"# Solo\n"),
            (
                "h.md",
                b"See [[zz-nowhere]], [[zz-two]] and [[zz-solo#Nope]].\n",
            ),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let linked = link_findings_of(&host, &vault, "h.md");
    let reasons: Vec<(FindingKind, u64)> = linked.iter().map(|row| (row.kind, row.id)).collect();
    assert_eq!(
        reasons.iter().map(|(kind, _)| *kind).collect::<Vec<_>>(),
        vec![
            FindingKind::Broken,
            FindingKind::Ambiguous,
            FindingKind::MissingAnchor
        ]
    );

    let plan = applied_leaving_only_its_skips(&host, &vault);

    let skipped: Vec<(u64, SkipReason)> = provenance(&plan)
        .skipped
        .iter()
        .map(|skipped| (skipped.finding, skipped.reason))
        .collect();
    assert_eq!(
        skipped,
        vec![
            (reasons[0].1, SkipReason::NoDeclaredFix),
            (reasons[1].1, SkipReason::AmbiguousLink),
            (reasons[2].1, SkipReason::NoDeclaredFix),
        ]
    );
}

/// Two rules, each placing and routing its own kind of document beneath
/// [`FOLDER`]: tasks in `tasks/` and notes in `loose/`.
const TWO_ROUTES: &str = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    allowed_paths: {paths: ['zz-repair/tasks/**'], route: 'zz-repair/tasks/'}\n  notes:\n    match: {frontmatter: {type: note}}\n    allowed_paths: {paths: ['zz-repair/loose/**'], route: 'zz-repair/loose/'}\n";

/// **A routed holder's relative link the cascade respells, and a second
/// route's landing makes resolve, is dropped.** `loose/zz-h.md` is routed to
/// `tasks/` and holds `[m](./zz-peer.md)`, which names `loose/zz-peer.md` and
/// finds nothing there. The holder's move respells the link to
/// `../loose/zz-peer.md`, and the note `elsewhere/zz-peer.md` is routed to
/// `loose/zz-peer.md`, so the respelled link resolves once the plan lands. The
/// respelling is found under the holder where the plan leaves it, not where it
/// stood.
#[test]
fn a_routed_holders_respelled_link_a_second_route_resolves_is_dropped() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-settle-respelled-routed-holder",
        TWO_ROUTES,
        &[
            (
                "loose/zz-h.md",
                b"---\ntype: task\n---\nSee [m](./zz-peer.md).\n",
            ),
            ("elsewhere/zz-peer.md", b"---\ntype: note\n---\n# Peer\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let linked = link_findings_of(&host, &vault, "loose/zz-h.md");
    let [broken] = linked.as_slice() else {
        panic!("one link finding");
    };
    assert_eq!(broken.kind, FindingKind::Broken);

    let plan = applied_leaving_only_its_skips(&host, &vault);

    assert!(
        provenance(&plan).skipped.is_empty(),
        "{:?}",
        provenance(&plan).skipped
    );
    assert!(stands(&vault, "tasks/zz-h.md"));
    assert!(stands(&vault, "loose/zz-peer.md"));
    assert!(written(&vault, "tasks/zz-h.md").contains("../loose/zz-peer.md"));
    assert!(link_findings_of(&host, &vault, "tasks/zz-h.md").is_empty());
}

/// **A link the cascade leaves as written beside a respelled twin keeps its
/// finding.** `loose/zz-h.md` is routed to `tasks/` and holds `[m](./zz-peer.md)`
/// and `[n](./zz\-peer.md)`, two links of one address that find nothing at
/// `loose/`; the note `elsewhere/zz-peer.md` is routed to `loose/zz-peer.md`.
/// The holder's move respells `[m]` to `../loose/zz-peer.md`, which resolves
/// once the plan lands, but the text layer leaves the escaped `[n]` as
/// written, so it still names nothing from `tasks/`. The plan does not say
/// which link of the address it respelled, so neither finding is dropped on
/// the strength of the respelled one alone: `[n]`'s stays skipped, as the
/// validator, which still reports it, shows.
#[test]
fn a_link_the_cascade_leaves_as_written_beside_a_respelled_twin_keeps_its_finding() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-settle-left-as-written-beside-twin",
        TWO_ROUTES,
        &[
            (
                "loose/zz-h.md",
                b"---\ntype: task\n---\nSee [m](./zz-peer.md) and [n](./zz\\-peer.md).\n",
            ),
            ("elsewhere/zz-peer.md", b"---\ntype: note\n---\n# Peer\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let linked = link_findings_of(&host, &vault, "loose/zz-h.md");
    let [_, escaped] = linked.as_slice() else {
        panic!("two link findings: {linked:?}");
    };
    assert!(linked.iter().all(|row| row.kind == FindingKind::Broken));

    let plan = applied_leaving_only_its_skips(&host, &vault);

    let skipped: Vec<u64> = provenance(&plan)
        .skipped
        .iter()
        .map(|skipped| skipped.finding)
        .collect();
    assert!(
        skipped.contains(&escaped.id),
        "the escaped link still names nothing, so its finding stays skipped: {skipped:?}"
    );
    assert!(written(&vault, "tasks/zz-h.md").contains("../loose/zz-peer.md"));
    assert!(written(&vault, "tasks/zz-h.md").contains("./zz\\-peer.md"));
    assert!(
        !link_findings_of(&host, &vault, "tasks/zz-h.md").is_empty(),
        "the validator still reports the escaped link"
    );
}

/// The schema the reload cases preview under: a task's `status` is required,
/// defaulting to `todo`, and one of `todo` and `done`.
const FILLING_STATUS: &str = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    required:\n      status: {default: todo}\n    one_of:\n      status: {values: [todo, done]}\n";

/// Rewrite the vault's schema to `schema` and wait for the host to reload it.
fn reloaded_under(host: &attach::ServingHost, vault: &attach::Vault, schema: &str) {
    std::fs::write(vault.path().join(".norn/schema.yaml"), schema).expect("rewrite the schema");
    wait_until(
        "the reload to activate the rewritten schema",
        attach::reload_budget(attach::READY_LIMIT),
        || match host.reload(vault.name()) {
            Ok(_) => Observed::Met(()),
            Err(ReloadRefusal::Unavailable(standing)) => {
                Observed::pending(format!("the entry stands at {standing:?}"))
            }
            Err(refused) => panic!("the reload was refused: {refused:?}"),
        },
    )
    .unwrap_or_else(|failure| panic!("{failure}"));
}

/// The plan [`FILLING_STATUS`] previews for `vault`'s task missing its
/// `status` (one operation, setting `status: todo`), the vault reloaded under
/// `schema` before the plan is returned to be sent back to apply.
fn previewed_across_a_reload_to(
    host: &attach::ServingHost,
    vault: &attach::Vault,
    schema: &str,
) -> ResolvedPlan {
    let plan = previewed(host.repair(repairing(vault, ApplyMode::Preview)));
    assert_eq!(plan.operations, vec![setting(1, "a.md", "status", "todo")]);
    reloaded_under(host, vault, schema);
    plan
}

/// A vault holding one task missing its `status`, under [`FILLING_STATUS`].
fn a_vault_missing_a_status(label: &str) -> (Sandbox, attach::Vault, attach::ServingHost) {
    a_vault_under(
        label,
        FILLING_STATUS,
        &[("a.md", b"---\ntype: task\n---\n# A\n")],
    )
}

/// **A repair plan previewed before a schema reload applies where the new
/// declaration still admits the value it writes.** The reload widens the
/// closed set; the plan, sent back unchanged, lands its `status`.
#[test]
fn a_repair_plan_previewed_before_a_reload_applies_where_the_new_declaration_admits_its_value() {
    let (_sandbox, vault, host) = a_vault_missing_a_status("host-repair-reload-admits");
    let _lease = attach::attach_and_wait(&host, vault.name());
    let plan = previewed_across_a_reload_to(
        &host,
        &vault,
        &FILLING_STATUS.replace("[todo, done]", "[todo, done, blocked]"),
    );

    let report = planned(host.apply(ApplyParams::new(
        ApplyMode::Apply,
        PlanDocument::resolved(plan),
    )));

    assert!(matches!(report, ApplyReport::Applied { .. }), "{report:?}");
    assert_eq!(
        written(&vault, "a.md"),
        "---\ntype: task\nstatus: todo\n---\n# A\n"
    );
}

/// **A repair plan previewed before a schema reload applies where the new
/// declaration adds a violation on a field the plan leaves alone.** The
/// reload requires `priority`, which the task lacks and the plan does not
/// write: that violation stood in the document before the plan, so the plan
/// introduces none.
#[test]
fn a_repair_plan_previewed_before_a_reload_applies_where_the_new_declaration_flags_another_field() {
    let (_sandbox, vault, host) = a_vault_missing_a_status("host-repair-reload-other-field");
    let _lease = attach::attach_and_wait(&host, vault.name());
    let plan = previewed_across_a_reload_to(
        &host,
        &vault,
        &FILLING_STATUS.replace(
            "      status: {default: todo}\n",
            "      status: {default: todo}\n      priority:\n",
        ),
    );

    let report = planned(host.apply(ApplyParams::new(
        ApplyMode::Apply,
        PlanDocument::resolved(plan),
    )));

    assert!(matches!(report, ApplyReport::Applied { .. }), "{report:?}");
    assert_eq!(
        written(&vault, "a.md"),
        "---\ntype: task\nstatus: todo\n---\n# A\n"
    );
    // The unwritten field's finding stands: `finding_of` finds exactly one.
    finding_of(
        &host,
        &vault,
        "a.md",
        FindingKind::RequiredMissing,
        "priority",
    );
}

/// **A repair plan previewed before a schema reload is refused where the new
/// declaration makes the value it writes a violation.** The reload drops
/// `todo` from the closed set, and the default that wrote it; the apply refuses
/// with the schema violation the plan would introduce, and the document is left
/// as it was.
#[test]
fn a_repair_plan_previewed_before_a_reload_is_refused_where_the_new_declaration_rejects_its_value()
{
    let (_sandbox, vault, host) = a_vault_missing_a_status("host-repair-reload-rejects");
    let _lease = attach::attach_and_wait(&host, vault.name());
    let plan = previewed_across_a_reload_to(
        &host,
        &vault,
        &FILLING_STATUS
            .replace("[todo, done]", "[done]")
            .replace(": {default: todo}", ":"),
    );

    let (_refusal, _plan, checks) = apply_refused(&host, ApplyMode::Apply, &plan);

    let [norn_wire::RefusedCheck::SchemaViolation { violation, .. }] = checks.as_slice() else {
        panic!("one introduced schema violation: {checks:?}");
    };
    assert_eq!(violation.path, at("a.md"));
    assert_eq!(violation.kind, FindingKind::NotOneOf);
    assert_eq!(violation.target.as_deref(), Some("status"));
    assert_eq!(written(&vault, "a.md"), "---\ntype: task\n---\n# A\n");
}

/// A holder with a missing heading anchor and its indexed target.
fn a_vault_with_a_missing_anchor(label: &str) -> (Sandbox, attach::Vault, attach::ServingHost) {
    a_vault_under(
        label,
        "version: 1\n",
        &[
            ("zz-holder.md", b"[[zz-target#Important Headinh]]\n"),
            ("zz-target.md", b"# Important Heading\n"),
        ],
    )
}

/// A target that stopped reading cannot supply a closest-heading suggestion;
/// independent fixes in the same batch still proceed.
#[cfg(feature = "induced-failure")]
#[test]
fn a_closest_heading_fix_into_a_broken_document_is_skipped_as_unreadable_target() {
    let (_sandbox, vault, host) = a_vault_under(
        "repair-anchor-unreadable-target",
        "version: 1\n",
        &[
            ("zz-holder.md", b"[[zz-target#Important Headinh]]\n"),
            ("zz-target.md", b"# Important Heading\n"),
            ("zz-other-holder.md", b"[[zz-other#Another Headinh]]\n"),
            ("zz-other.md", b"# Another Heading\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let before = previewed(
        host.repair(repairing(&vault, ApplyMode::Preview).with_threshold(Confidence::Suggested)),
    );
    assert_eq!(
        before.operations.len(),
        2,
        "both heading fixes are initially eligible"
    );
    let target = norn_store::DocumentPath::new(at("zz-target.md").as_str()).unwrap();
    let holder = norn_store::DocumentPath::new(at("zz-holder.md").as_str()).unwrap();
    let mut store = vault.store();
    let captured = store
        .begin_request()
        .stored_findings(&holder)
        .unwrap()
        .into_iter()
        .find(|finding| finding.kind == FindingKind::MissingAnchor.as_str())
        .expect("the original missing-anchor evidence");
    assert!(captured.value.is_none());
    let selected = norn_store::FindingFacts {
        kind: FindingKind::MissingAnchor,
        severity: Severity::try_from(captured.severity.as_str()).unwrap(),
        path: captured.path,
        class_keys: captured.class_keys,
        path_keys: captured.path_keys,
        target: captured.target,
        span: captured.span,
        ordinal: captured.ordinal,
        candidates: captured.candidates,
        candidates_total: captured.candidates_total,
        message: captured.message,
        detail: captured.detail,
        rules: captured.rules,
        value: None,
    };
    let unreadable = norn_store::FindingFacts {
        kind: FindingKind::UnreadableDocument,
        severity: Severity::Error,
        path: target.clone(),
        class_keys: BTreeSet::new(),
        path_keys: BTreeSet::new(),
        target: None,
        span: None,
        ordinal: None,
        candidates: Vec::new(),
        candidates_total: 0,
        message: "the document could not be read".to_owned(),
        detail: Some("Permission denied (os error 13)".to_owned()),
        rules: BTreeSet::new(),
        value: None,
    };
    let _denied = norn_fs::deny_document_opens(&vault.path().join(FOLDER).join("zz-target.md"));
    let declared = norn_store::ContentModel::under(
        store
            .begin_request()
            .vault_schema_pin()
            .unwrap()
            .unwrap()
            .fingerprint,
    );
    store
        .begin_request()
        .apply_increment(
            norn_store::IncrementProvenance::Derived,
            [norn_store::Change::Unreadable {
                path: target.clone(),
            }],
            &[norn_store::DerivedFinding {
                facts: unreadable,
                replaces: None,
                bytes_hash: None,
            }],
            &declared,
        )
        .expect("marking the target broken and filing its cause atomically");
    assert!(
        store
            .begin_request()
            .stored_document(&target)
            .unwrap()
            .unwrap()
            .content_hash
            .is_none()
    );
    assert!(
        store
            .begin_request()
            .stored_findings(&holder)
            .unwrap()
            .iter()
            .all(|finding| finding.kind != FindingKind::MissingAnchor.as_str())
    );
    norn_testkit::equivalence::assert_operationally_valid(
        &mut store,
        "the ordinary broken-target state",
    );
    // Ordinary link health withholds this judgment. Restore only the captured
    // input through the fenced seam to test repair's defensive evidence boundary.
    norn_store::induced_failure::record_finding_out_of_band(&mut store, &selected).unwrap();
    drop(store);
    let findings = standing(&host, &vault, |request| request);
    let finding = *findings
        .iter()
        .find(|(_, (path, kind))| {
            path == &format!("{FOLDER}zz-holder.md") && *kind == FindingKind::MissingAnchor
        })
        .expect("the explicitly supplied missing-anchor finding")
        .0;
    let plan = previewed(
        host.repair(repairing(&vault, ApplyMode::Preview).with_threshold(Confidence::Suggested)),
    );
    let skipped = &provenance(&plan).skipped;
    assert!(
        skipped
            .iter()
            .any(|skip| skip.finding == finding && skip.reason == SkipReason::UnreadableTarget),
        "{plan:?}"
    );
    assert_eq!(
        plan.operations.len(),
        1,
        "the independent heading fix proceeds: {plan:?}"
    );
    assert_eq!(
        plan.operations[0].kind,
        OperationKind::rewrite_anchor(
            at("zz-other-holder.md"),
            norn_wire::LinkFamily::Wikilink,
            "zz-other",
            "Another Headinh",
            "Another Heading"
        )
    );
    assert_eq!(
        written(&vault, "zz-holder.md"),
        "[[zz-target#Important Headinh]]\n"
    );
}

/// Tag comparisons count even when the read gate finds no possible fix.
#[test]
fn an_unfixable_undeclared_tag_counts_each_declared_tag_the_read_gate_compares() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-unfixable-tag-reading",
        "version: 1\ntags:\n  declared: [alpha, bravo, delta]\n  undeclared: report\n",
        &[("a.md", b"#zzzzzz\n")],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let before = host.read_evidence();

    let plan = previewed(
        host.repair(repairing(&vault, ApplyMode::Preview).with_threshold(Confidence::Suggested)),
    );

    assert!(plan.operations.is_empty());
    assert_eq!(
        host.read_evidence()
            .since(before)
            .repair_suggestions
            .declared_tags,
        3
    );
}

/// A Markdown suggestion scores the decoded anchor and rewrites its written fragment.
#[test]
fn a_percent_encoded_markdown_anchor_gets_its_closest_heading_fix_and_resolves() {
    let (_sandbox, vault, host) = a_vault_under(
        "repair-percent-encoded-anchor",
        "version: 1\n",
        &[
            ("zz-holder.md", b"[x](zz-target.md#Importnt%20Heading)\n"),
            ("zz-target.md", b"# Important Heading\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let plan = previewed(
        host.repair(repairing(&vault, ApplyMode::Preview).with_threshold(Confidence::Suggested)),
    );

    assert_eq!(plan.operations.len(), 1, "{plan:?}");
    assert_eq!(
        plan.operations[0].kind,
        OperationKind::rewrite_anchor(
            at("zz-holder.md"),
            norn_wire::LinkFamily::Markdown,
            "zz-target.md",
            "Importnt%20Heading",
            "important-heading",
        )
    );
    assert_eq!(
        provenance(&plan).citations[0].findings[0].confidence,
        Confidence::Suggested
    );
    let report = planned(host.apply(ApplyParams::new(
        ApplyMode::Apply,
        PlanDocument::resolved(plan),
    )));
    assert!(matches!(report, ApplyReport::Applied { .. }), "{report:?}");
    assert_eq!(
        written(&vault, "zz-holder.md"),
        "[x](zz-target.md#important-heading)\n"
    );
    assert!(standing(&host, &vault, |request| request).is_empty());
}

/// A changed target heading invalidates the snapshot evidence of an anchor fix.
#[test]
fn a_previewed_anchor_suggestion_refuses_apply_when_the_target_heading_changes() {
    {
        let (_sandbox, vault, host) = a_vault_with_a_missing_anchor("repair-anchor-target-change");
        let _lease = attach::attach_and_wait(&host, vault.name());
        let plan =
            previewed(host.repair(
                repairing(&vault, ApplyMode::Preview).with_threshold(Confidence::Suggested),
            ));
        assert_eq!(plan.operations.len(), 1);
        assert!(
            matches!(&plan.operations[0].conditions[0], norn_wire::AuthorCondition::ContentHash { path, .. } if path == &at("zz-target.md"))
        );
        std::fs::write(
            vault.path().join(FOLDER).join("zz-target.md"),
            "# Changed Heading\n",
        )
        .unwrap();
        let (_, _, checks) = apply_refused(&host, ApplyMode::Apply, &plan);
        assert!(!checks.is_empty());
        assert_eq!(
            written(&vault, "zz-holder.md"),
            "[[zz-target#Important Headinh]]\n"
        );
    }

    let (_sandbox, vault, host) = a_vault_with_a_missing_anchor("repair-anchor-target-unchanged");
    let _lease = attach::attach_and_wait(&host, vault.name());
    let preview = previewed(
        host.repair(repairing(&vault, ApplyMode::Preview).with_threshold(Confidence::Suggested)),
    );
    let ApplyReport::Applied {
        plan, changeset, ..
    } = planned(host.apply(ApplyParams::new(
        ApplyMode::Apply,
        PlanDocument::resolved(preview.clone()),
    )))
    else {
        panic!("the unchanged target admits the previewed plan");
    };
    assert_eq!(changeset, ChangesetOutcome::Committed);
    assert_eq!(plan, preview);
    assert_eq!(
        written(&vault, "zz-holder.md"),
        "[[zz-target#Important Heading]]\n"
    );
}

/// A fresh plan retains the operation's snapshot hash and names it unresolved.
#[test]
fn a_refused_anchor_suggestions_fresh_plan_keeps_its_hash_condition_and_unresolved_operation() {
    let (_sandbox, vault, host) = a_vault_with_a_missing_anchor("repair-anchor-fresh-condition");
    let _lease = attach::attach_and_wait(&host, vault.name());
    let plan = previewed(
        host.repair(repairing(&vault, ApplyMode::Preview).with_threshold(Confidence::Suggested)),
    );
    assert_eq!(plan.operations.len(), 1);
    let operation = plan.operations[0].clone();
    std::fs::write(
        vault.path().join(FOLDER).join("zz-target.md"),
        "# Changed Heading\n",
    )
    .unwrap();
    let (refusal, fresh, _) = apply_refused(&host, ApplyMode::Apply, &plan);
    let ErrorDetail::PlanRefused { unresolved, .. } = refusal.detail() else {
        panic!("a refused plan");
    };
    assert!(fresh.operations.is_empty());
    assert_eq!(unresolved.len(), 1);
    assert_eq!(unresolved[0].operation, operation);
    assert!(matches!(
        unresolved[0].operation.conditions[0],
        norn_wire::AuthorCondition::ContentHash { .. }
    ));
}

/// Snapshot headings are not suggested when the same plan rewrites the target's tags.
#[test]
fn a_closest_heading_is_not_proposed_when_the_plan_rewrites_tags_in_its_target() {
    for own in [false, true] {
        let documents: &[(&str, &[u8])] = if own {
            &[("zz-holder.md", b"# Notes #darft\n[[#Notes darft]]\n")]
        } else {
            &[
                ("zz-holder.md", b"[[zz-target#Notes darft]]\n"),
                ("zz-target.md", b"# Notes #darft\n"),
            ]
        };
        let (_sandbox, vault, host) = a_vault_under(
            "repair-anchor-target-tag-rewrite",
            "version: 1\ntags:\n  declared: [draft]\n  undeclared: report\n",
            documents,
        );
        let _lease = attach::attach_and_wait(&host, vault.name());
        for threshold in [Confidence::Suggested, Confidence::DEFAULT_THRESHOLD] {
            let plan = previewed(
                host.repair(repairing(&vault, ApplyMode::Preview).with_threshold(threshold)),
            );
            if threshold == Confidence::Suggested {
                assert!(
                    plan.operations.iter().any(|operation| matches!(
                        operation.kind,
                        OperationKind::RewriteTag { .. }
                    ))
                );
            }
            assert!(
                !plan
                    .operations
                    .iter()
                    .any(|operation| matches!(operation.kind, OperationKind::RewriteAnchor { .. }))
            );
            assert!(provenance(&plan).skipped.iter().all(|skip| !matches!(
                skip.proposed.as_ref().map(|operation| &operation.kind),
                Some(OperationKind::RewriteAnchor { .. })
            )));
        }
    }
}

/// Wait until the out-of-band move has been derived and the holder is broken.
fn moved_on_disk(host: &attach::ServingHost, vault: &attach::Vault, from: &str, to: &str) {
    let destination = vault.path().join(FOLDER).join(to);
    std::fs::create_dir_all(destination.parent().expect("a parent")).expect("make destination");
    std::fs::rename(vault.path().join(FOLDER).join(from), destination).expect("move outside norn");
    wait_until(
        "the move and broken link are derived",
        attach::read_budget(attach::READY_LIMIT),
        || {
            let mut store = vault.store();
            let snapshot = store.begin_request();
            let held = snapshot
                .stored_document(&norn_store::DocumentPath::new(at(to).as_str()).expect("path"))
                .expect("live document");
            let dead = snapshot
                .stored_tombstone(&norn_store::DocumentPath::new(at(from).as_str()).expect("path"))
                .expect("death");
            let broken = link_findings_of(host, vault, "zz-holder.md");
            if held.is_some()
                && dead.is_some()
                && broken.iter().any(|row| row.kind == FindingKind::Broken)
            {
                Observed::Met(())
            } else {
                Observed::pending(format!("{held:?} {dead:?} {broken:?}"))
            }
        },
    )
    .expect("the move settles");
}

/// **A broken wikilink follows one tombstone to one live holder outside its selection.**
#[test]
fn a_broken_wikilink_follows_one_tombstone_to_one_live_holder() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-follow-wiki",
        "version: 1\n",
        &[
            ("old.md", b"# Followed\n"),
            ("zz-holder.md", b"See [[old#Heading|Title]] and ![[old]].\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    moved_on_disk(&host, &vault, "old.md", "new.md");
    let findings = link_findings_of(&host, &vault, "zz-holder.md");
    let broken: Vec<_> = findings
        .iter()
        .filter(|row| row.kind == FindingKind::Broken)
        .collect();
    assert_eq!(broken.len(), 2);
    let strict = previewed(
        host.repair(
            RepairParams::new(address(&vault), ApplyMode::Preview)
                .with_predicates([Predicate::path(format!("{FOLDER}zz-holder.md"))])
                .with_threshold(Confidence::Declared),
        ),
    );
    assert!(strict.operations.is_empty(), "{strict:?}");
    assert_eq!(provenance(&strict).skipped.len(), 2);
    for skipped in &provenance(&strict).skipped {
        assert_eq!(skipped.reason, SkipReason::BelowThreshold);
        let proposed = skipped.proposed.as_ref().expect("the derived operation");
        assert_eq!(
            proposed.kind,
            OperationKind::rewrite_link(
                at("zz-holder.md"),
                norn_wire::LinkFamily::Wikilink,
                "old",
                "new"
            )
        );
        assert_eq!(proposed.conditions.len(), 1);
    }
    let plan = previewed(
        host.repair(
            RepairParams::new(address(&vault), ApplyMode::Preview)
                .with_predicates([Predicate::path(format!("{FOLDER}zz-holder.md"))]),
        ),
    );
    assert_eq!(plan.operations.len(), 1, "{plan:?}");
    let operation = &plan.operations[0];
    assert_eq!(
        operation.kind,
        OperationKind::rewrite_link(
            at("zz-holder.md"),
            norn_wire::LinkFamily::Wikilink,
            "old",
            "new"
        )
    );
    let hash = norn_wire::ContentHash::new(format!(
        "sha256:{}",
        norn_fs::ContentHash::of(b"# Followed\n").to_hex()
    ))
    .expect("hash");
    assert_eq!(
        operation.conditions,
        vec![norn_wire::AuthorCondition::content_hash(
            at("new.md"),
            hash.clone()
        )]
    );
    assert!(
        plan.conditions
            .contains(&norn_wire::PlanCondition::content_hash(at("new.md"), hash))
    );
    for (address, after) in [
        ("new", norn_wire::Resolves::one(at("new.md"))),
        ("old", norn_wire::Resolves::none()),
    ] {
        assert!(
            plan.conditions
                .contains(&norn_wire::PlanCondition::address_resolution(
                    norn_wire::LinkKey::new(
                        at("zz-holder.md"),
                        norn_wire::LinkFamily::Wikilink,
                        address
                    ),
                    after
                )),
            "{plan:?}"
        );
    }
    let citations = &provenance(&plan).citations;
    assert_eq!(citations.len(), 1);
    let cited: BTreeSet<_> = citations[0]
        .findings
        .iter()
        .map(|finding| {
            assert_eq!(finding.confidence, Confidence::Derived);
            finding.finding
        })
        .collect();
    assert_eq!(cited, broken.iter().map(|row| row.id).collect());
    let report = planned(host.apply(ApplyParams::new(
        ApplyMode::Apply,
        PlanDocument::resolved(plan),
    )));
    assert!(matches!(report, ApplyReport::Applied { .. }), "{report:?}");
    assert_eq!(
        written(&vault, "zz-holder.md"),
        "See [[new#Heading|Title]] and ![[new]].\n"
    );
}

/// **A relative Markdown link follows the exact tombstone path.**
#[test]
fn a_broken_relative_markdown_link_follows_one_tombstone_to_one_live_holder() {
    followed_spelling(
        "relative",
        "See [Title](old.md#Heading \"Title\") and [Other](old.md).\n",
        norn_wire::LinkFamily::Markdown,
        "old.md",
        "new.md",
        "See [Title](new.md#Heading \"Title\") and [Other](new.md).\n",
    );
}

/// **Each followed syntax carries the same evidence and lands the expected bytes.**
fn followed_spelling(
    label: &str,
    before: &str,
    syntax: norn_wire::LinkFamily,
    old: &str,
    new: &str,
    after: &str,
) {
    let (_sandbox, vault, host) = a_vault_under(
        &format!("host-repair-follow-{label}"),
        "version: 1\n",
        &[
            ("old.md", b"# Followed\n"),
            ("zz-holder.md", before.as_bytes()),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    moved_on_disk(&host, &vault, "old.md", "new.md");
    let broken: BTreeSet<_> = link_findings_of(&host, &vault, "zz-holder.md")
        .iter()
        .filter(|row| row.kind == FindingKind::Broken)
        .map(|row| row.id)
        .collect();
    let plan = previewed(host.repair(repairing(&vault, ApplyMode::Preview)));
    assert_eq!(plan.operations.len(), 1, "{plan:?}");
    assert_eq!(
        plan.operations[0].kind,
        OperationKind::rewrite_link(at("zz-holder.md"), syntax, old, new)
    );
    let hash = norn_wire::ContentHash::new(format!(
        "sha256:{}",
        norn_fs::ContentHash::of(b"# Followed\n").to_hex()
    ))
    .expect("hash");
    assert_eq!(
        plan.operations[0].conditions,
        vec![norn_wire::AuthorCondition::content_hash(
            at("new.md"),
            hash.clone()
        )]
    );
    assert!(
        plan.conditions
            .contains(&norn_wire::PlanCondition::content_hash(at("new.md"), hash))
    );
    for (address, resolves) in [
        (old, norn_wire::Resolves::none()),
        (new, norn_wire::Resolves::one(at("new.md"))),
    ] {
        assert!(
            plan.conditions
                .contains(&norn_wire::PlanCondition::address_resolution(
                    norn_wire::LinkKey::new(at("zz-holder.md"), syntax, address),
                    resolves
                )),
            "{plan:?}"
        );
    }
    let cited: BTreeSet<_> = provenance(&plan).citations[0]
        .findings
        .iter()
        .map(|finding| {
            assert_eq!(finding.confidence, Confidence::Derived);
            finding.finding
        })
        .collect();
    assert_eq!(cited, broken);
    let report = planned(host.apply(ApplyParams::new(
        ApplyMode::Apply,
        PlanDocument::resolved(plan),
    )));
    assert!(matches!(report, ApplyReport::Applied { .. }), "{report:?}");
    assert_eq!(written(&vault, "zz-holder.md"), after);
}

/// **A rooted Markdown link follows the exact tombstone path.**
#[test]
fn a_broken_rooted_markdown_link_follows_one_tombstone_to_one_live_holder() {
    followed_spelling(
        "rooted",
        "See [Title](/zz-repair/old.md#Heading).\n",
        norn_wire::LinkFamily::Markdown,
        "/zz-repair/old.md",
        "/zz-repair/new.md",
        "See [Title](/zz-repair/new.md#Heading).\n",
    );
}

/// **A vault link follows the exact tombstone path with its protocol kept.**
#[test]
fn a_broken_vault_link_follows_one_tombstone_to_one_live_holder() {
    followed_spelling(
        "vault",
        "See [[vault://zz-repair/old.md#Heading|Title]].\n",
        norn_wire::LinkFamily::Wikilink,
        "vault://zz-repair/old.md",
        "vault://zz-repair/new.md",
        "See [[vault://zz-repair/new.md#Heading|Title]].\n",
    );
}

/// **A tombstone whose hash has no live holder skips with the old path.**
#[test]
fn a_tombstone_no_live_document_holds_skips_as_no_holder() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-follow-no-holder",
        "version: 1\n",
        &[
            ("old.md", b"# Followed\n"),
            ("zz-holder.md", b"See [[old]].\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    std::fs::remove_file(vault.path().join(FOLDER).join("old.md")).expect("remove outside norn");
    wait_for_broken(&host, &vault);
    let plan = previewed(host.repair(repairing(&vault, ApplyMode::Preview)));
    assert_follow_skip(&plan, SkipReason::NoHolder, &["zz-repair/old.md"]);
}

/// **Wait for the holder's links to be judged broken after a disk change.**
fn wait_for_broken(host: &attach::ServingHost, vault: &attach::Vault) {
    wait_until(
        "the broken link is derived",
        attach::read_budget(attach::READY_LIMIT),
        || {
            let rows = link_findings_of(host, vault, "zz-holder.md");
            if rows.iter().any(|row| row.kind == FindingKind::Broken) {
                Observed::Met(())
            } else {
                Observed::pending(format!("{rows:?}"))
            }
        },
    )
    .expect("the broken link settles");
}

/// **A move-following skip carries its reason and paths, with no candidate head.**
fn assert_follow_skip(plan: &ResolvedPlan, reason: SkipReason, paths: &[&str]) {
    assert!(plan.operations.is_empty(), "{plan:?}");
    let skipped = &provenance(plan).skipped;
    assert_eq!(skipped.len(), 1, "{skipped:?}");
    assert_eq!(skipped[0].reason, reason);
    assert!(skipped[0].candidates.is_none());
    let note = skipped[0].note.as_ref().expect("a reason note");
    for path in paths {
        assert!(note.contains(path), "{note}");
    }
    if matches!(
        reason,
        SkipReason::SeveralHolders | SkipReason::SeveralTombstones
    ) {
        assert!(note.contains("at least two"), "{note}");
    }
}

/// **Two live hash holders skip with both paths and no candidate head.**
#[test]
fn a_tombstone_two_live_documents_hold_skips_as_several_holders() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-follow-twins",
        "version: 1\n",
        &[
            ("old.md", b"# Followed\n"),
            ("zz-holder.md", b"See [[old]].\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    std::fs::write(vault.path().join(FOLDER).join("twin.md"), b"# Followed\n").expect("copy bytes");
    wait_for_stored_path(&vault, "twin.md", false);
    moved_on_disk(&host, &vault, "old.md", "new.md");
    let plan = previewed(host.repair(repairing(&vault, ApplyMode::Preview)));
    assert_follow_skip(
        &plan,
        SkipReason::SeveralHolders,
        &["zz-repair/new.md", "zz-repair/twin.md"],
    );
}

/// **Wait for a live row or a death at an exact path.**
fn wait_for_stored_path(vault: &attach::Vault, name: &str, dead: bool) {
    wait_until(
        "the disk change is derived",
        attach::state_budget(attach::READY_LIMIT),
        || {
            let mut store = vault.store();
            let snapshot = store.begin_request();
            let path = norn_store::DocumentPath::new(at(name).as_str()).expect("path");
            let present = if dead {
                snapshot.stored_tombstone(&path).expect("death").is_some()
            } else {
                snapshot.stored_document(&path).expect("live row").is_some()
            };
            if present {
                Observed::Met(())
            } else {
                Observed::pending(name)
            }
        },
    )
    .expect("the row settles");
}

/// **Two suffix tombstones skip with both paths and no candidate head.**
#[test]
fn a_broken_link_matching_two_tombstones_skips_as_several_tombstones() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-follow-deaths",
        "version: 1\n",
        &[
            ("a/old.md", b"# First\n"),
            ("b/old.md", b"# Second\n"),
            ("zz-holder.md", b"See [[old]].\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    for path in ["a/old.md", "b/old.md"] {
        std::fs::remove_file(vault.path().join(FOLDER).join(path)).expect("remove outside norn");
        wait_for_stored_path(&vault, path, true);
    }
    wait_for_broken(&host, &vault);
    let plan = previewed(host.repair(repairing(&vault, ApplyMode::Preview)));
    assert_follow_skip(
        &plan,
        SkipReason::SeveralTombstones,
        &["zz-repair/a/old.md", "zz-repair/b/old.md"],
    );
}

/// **An unwritable follow skips with the spelling or text layer's reason.**
#[test]
fn a_follow_no_spelling_or_edit_can_write_skips_as_link_not_rewritable() {
    for (label, holder, destination, reason) in [
        (
            "unplaced",
            "---\nsee: [\"[[old]]\"]\n---\n",
            "new.md",
            "skipped_unplaced",
        ),
        ("no-spelling", "See [[old]].\n", "new.md.md", "no spelling"),
    ] {
        let (_sandbox, vault, host) = a_vault_under(
            &format!("host-repair-follow-{label}"),
            "version: 1\n",
            &[
                ("old.md", b"# Followed\n"),
                ("zz-holder.md", holder.as_bytes()),
                ("new.md", b"# Competitor\n"),
            ],
        );
        let _lease = attach::attach_and_wait(&host, vault.name());
        if destination == "new.md" {
            std::fs::remove_file(vault.path().join(FOLDER).join("new.md"))
                .expect("clear destination");
        }
        moved_on_disk(&host, &vault, "old.md", destination);
        let plan = previewed(host.repair(repairing(&vault, ApplyMode::Preview)));
        assert_follow_skip(&plan, SkipReason::LinkNotRewritable, &[reason]);
        assert_eq!(written(&vault, "zz-holder.md"), holder);
    }
}

/// **A quarantine tombstone never follows its hash to a live copy.**
#[test]
fn a_quarantine_tombstone_is_never_followed() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-follow-quarantine",
        "version: 1\n",
        &[
            ("old.md", b"# Followed\n"),
            ("zz-holder.md", b"See [[old]].\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    std::fs::write(vault.path().join(FOLDER).join("copy.md"), b"# Followed\n")
        .expect("copy good bytes");
    std::fs::write(vault.path().join(FOLDER).join("old.md"), b"\xff").expect("quarantine old path");
    wait_for_stored_path(&vault, "copy.md", false);
    wait_for_stored_path(&vault, "old.md", true);
    wait_for_broken(&host, &vault);
    let plan = previewed(
        host.repair(
            RepairParams::new(address(&vault), ApplyMode::Preview)
                .with_predicates([Predicate::path(format!("{FOLDER}zz-holder.md"))]),
        ),
    );
    assert!(plan.operations.is_empty(), "{plan:?}");
    assert_eq!(provenance(&plan).skipped.len(), 1);
    assert_eq!(
        provenance(&plan).skipped[0].reason,
        SkipReason::NoDeclaredFix
    );
}

/// **Run a check against a previewed follow while the attachment stays held.**
fn with_followed_plan(
    label: &str,
    check: impl FnOnce(&attach::ServingHost, &attach::Vault, ResolvedPlan),
) {
    let (_sandbox, vault, host) = a_vault_under(
        label,
        "version: 1\n",
        &[
            ("old.md", b"# Followed\n"),
            ("zz-holder.md", b"See [[old]].\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    moved_on_disk(&host, &vault, "old.md", "new.md");
    let plan = previewed(host.repair(repairing(&vault, ApplyMode::Preview)));
    assert_eq!(plan.operations.len(), 1, "{plan:?}");
    check(&host, &vault, plan);
}

/// **Changing the followed bytes refuses the recorded hash before any write.**
#[test]
fn a_change_to_the_followed_document_between_preview_and_apply_refuses() {
    with_followed_plan("host-repair-follow-changed", |host, vault, plan| {
        std::fs::write(vault.path().join(FOLDER).join("new.md"), b"# Changed\n")
            .expect("change followed document");
        let (_, _, checks) = apply_refused(host, ApplyMode::Apply, &plan);
        let hash = plan
            .conditions
            .iter()
            .find(|condition| {
                matches!(
                    condition,
                    norn_wire::PlanCondition::ContentHash { path, .. } if path == &at("new.md")
                )
            })
            .expect("hash condition");
        assert!(fails(&checks, hash), "{checks:?}");
        assert_eq!(written(vault, "zz-holder.md"), "See [[old]].\n");
    });
}

/// **Recreating the old target refuses the replaced address's recorded resolution.**
#[test]
fn recreating_the_old_target_between_preview_and_apply_refuses_through_the_replaced_address() {
    with_followed_plan("host-repair-follow-recreated", |host, vault, plan| {
        std::fs::write(vault.path().join(FOLDER).join("old.md"), b"# Recreated\n")
            .expect("recreate old target");
        wait_for_stored_path(vault, "old.md", false);
        let (_, _, checks) = apply_refused(host, ApplyMode::Apply, &plan);
        let replaced = norn_wire::PlanCondition::address_resolution(
            norn_wire::LinkKey::new(at("zz-holder.md"), norn_wire::LinkFamily::Wikilink, "old"),
            norn_wire::Resolves::none(),
        );
        assert!(fails(&checks, &replaced), "{checks:?}");
        assert_eq!(written(vault, "zz-holder.md"), "See [[old]].\n");
    });
}

/// **A fresh plan keeps the hash with the operation whose evidence changed.**
#[test]
fn a_refused_follows_fresh_plan_keeps_the_hash_with_its_operation_listed_unresolved() {
    with_followed_plan("host-repair-follow-fresh", |host, vault, plan| {
        std::fs::write(vault.path().join(FOLDER).join("new.md"), b"# Changed\n")
            .expect("change followed document");
        let (refusal, fresh, _) = apply_refused(host, ApplyMode::Apply, &plan);
        let ErrorDetail::PlanRefused { unresolved, .. } = refusal.detail() else {
            panic!("plan refusal");
        };
        assert_eq!(unresolved.len(), 1, "{unresolved:?}");
        assert_eq!(unresolved[0].operation, plan.operations[0]);
        assert_eq!(
            unresolved[0].operation.conditions,
            plan.operations[0].conditions
        );
        assert!(fresh.operations.is_empty(), "{fresh:?}");
    });
}

/// **A forbidden removal drops the followed link finding with its field.**
/// No rewrite remains to match once the removal deletes the holder's link.
#[test]
fn a_followed_link_whose_field_a_forbidden_removal_deletes_is_dropped_not_rewritten() {
    let schema = "version: 1\nrules:\n  bans:\n    match: {frontmatter: {type: task}}\n    forbidden:\n      scratch: remove\n";
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-follow-removed-field",
        schema,
        &[
            ("old.md", b"# Followed\n"),
            (
                "zz-holder.md",
                b"---\ntype: task\nscratch: '[[old]]'\n---\n# Holder\n",
            ),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    moved_on_disk(&host, &vault, "old.md", "new.md");

    let plan = previewed(host.repair(repairing(&vault, ApplyMode::Preview)));

    assert_eq!(
        kinds(&plan),
        vec![OperationKind::remove_frontmatter(
            WriteTarget::path(at("zz-holder.md")),
            "scratch",
        )],
    );
    assert!(provenance(&plan).skipped.is_empty(), "{plan:?}");
    let report = planned(host.apply(ApplyParams::new(
        ApplyMode::Apply,
        PlanDocument::resolved(plan),
    )));
    assert!(matches!(report, ApplyReport::Applied { .. }), "{report:?}");
    assert_eq!(
        written(&vault, "zz-holder.md"),
        "---\ntype: task\n---\n# Holder\n",
    );
}

/// **A follow bringing in a required field names that field and its default.**
#[test]
fn a_follow_bringing_in_a_required_field_names_its_default() {
    let schema = "version: 1\nfields:\n  see: {type: link}\nrules:\n  linked:\n    match: {frontmatter: {see: '[[new]]'}}\n    required:\n      status: {default: todo}\n";
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-follow-required",
        schema,
        &[
            ("old.md", b"# Followed\n"),
            ("zz-holder.md", b"---\nsee: '[[old]]'\n---\n# Holder\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    moved_on_disk(&host, &vault, "old.md", "new.md");

    let plan = previewed(host.repair(repairing(&vault, ApplyMode::Preview)));

    assert!(plan.operations.is_empty(), "{plan:?}");
    let skipped = &provenance(&plan).skipped;
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].reason, SkipReason::BringsInRequiredFields);
    assert_eq!(
        skipped[0].required_fields,
        Some(
            RequiredFieldHead::new(
                [RequiredField::new("status").with_default(norn_store::value_head("todo"))],
                1,
            )
            .expect("a head")
        ),
    );
    assert_eq!(
        written(&vault, "zz-holder.md"),
        "---\nsee: '[[old]]'\n---\n# Holder\n"
    );
}

/// **A follow cannot activate a rule reading a link a settled route respells.**
/// The follow skips with the bare-link veto's note, and the route still applies.
#[test]
fn a_follow_bringing_its_holder_under_a_rule_reading_a_respelled_link_skips() {
    let schema = SELECTED_BY_LINK.replace("[[zz-projects]]", "[[new]]");
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-follow-respelled",
        &schema,
        &[
            ("old.md", b"# Followed\n"),
            ("loose/x.md", b"---\ntype: task\n---\n# X\n"),
            (
                "zz-holder.md",
                b"---\nup: '[[old]]'\nparent: '[[zz-repair/loose/x]]'\n---\n# H\n",
            ),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    moved_on_disk(&host, &vault, "old.md", "new.md");

    let plan = applied_leaving_only_its_skips(&host, &vault);

    assert_eq!(
        kinds(&plan),
        vec![OperationKind::move_document(
            at("loose/x.md"),
            at("tasks-all/x.md")
        )]
    );
    assert!(stands(&vault, "tasks-all/x.md"));
    let skipped = &provenance(&plan).skipped;
    assert_eq!(skipped.len(), 1, "{plan:?}");
    assert_eq!(skipped[0].reason, SkipReason::JudgeWouldRefuse);
    assert_eq!(
        skipped[0].note.as_deref(),
        Some(
            "the link brings `zz-repair/zz-holder.md` under a rule reading `parent` by value, whose link the move of `zz-repair/loose/x.md` to `zz-repair/tasks-all/x.md` would respell"
        )
    );
    let holder = written(&vault, "zz-holder.md");
    assert!(holder.contains("up: '[[old]]'"), "{holder}");
    assert!(holder.contains("[[zz-repair/tasks-all/x]]"), "{holder}");
}

/// **A follow that exceeds its field's length limit skips as the judge refuses.**
#[test]
fn a_follow_exceeding_its_fields_length_limit_skips_as_judge_would_refuse() {
    let schema = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    max_length: {see: 7}\n";
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-follow-length",
        schema,
        &[
            ("old.md", b"# Followed\n"),
            (
                "zz-holder.md",
                b"---\ntype: task\nsee: '[[old]]'\n---\n# H\n",
            ),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    moved_on_disk(&host, &vault, "old.md", "much-longer.md");

    let plan = previewed(host.repair(repairing(&vault, ApplyMode::Preview)));

    assert!(plan.operations.is_empty(), "{plan:?}");
    let skipped = &provenance(&plan).skipped;
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].reason, SkipReason::JudgeWouldRefuse);
}

/// **A routed holder's follow guards the replaced address where it was read.**
/// Recreating that target refuses apply, before the holder is moved or rewritten.
#[test]
fn recreating_the_old_target_refuses_a_follow_from_a_routed_holder() {
    let before = b"---\ntype: task\n---\nSee [x](old.md).\n";
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-follow-routed-recreated",
        TASKED,
        &[("old.md", b"# Followed\n"), ("zz-holder.md", before)],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    moved_on_disk(&host, &vault, "old.md", "new.md");
    let plan = previewed(host.repair(repairing(&vault, ApplyMode::Preview)));
    assert_eq!(plan.operations.len(), 2, "{plan:?}");
    std::fs::write(vault.path().join(FOLDER).join("old.md"), b"# Recreated\n")
        .expect("recreate old target");
    wait_for_stored_path(&vault, "old.md", false);

    let (_, _, checks) = apply_refused(&host, ApplyMode::Apply, &plan);

    let replaced = norn_wire::PlanCondition::address_resolution(
        norn_wire::LinkKey::new(
            at("zz-holder.md"),
            norn_wire::LinkFamily::Markdown,
            "old.md",
        ),
        norn_wire::Resolves::none(),
    );
    assert!(fails(&checks, &replaced), "{checks:?}");
    assert_eq!(written(&vault, "zz-holder.md").as_bytes(), before);
    assert!(!stands(&vault, "tasks/zz-holder.md"));
}

/// Both complete paths named by a rooted `.md` link contribute their tombstones.
/// A live copy of the first death cannot turn the two deaths into one follow.
#[test]
fn a_rooted_md_link_matching_both_tombstones_skips_as_several_tombstones() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-vault-two-reductions",
        "version: 1\n",
        &[
            ("old.md", b"# First\n"),
            ("old.md.md", b"# Second\n"),
            ("zz-holder.md", b"See [[vault://zz-repair/old.md]].\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    for path in ["old.md", "old.md.md"] {
        std::fs::remove_file(vault.path().join(FOLDER).join(path)).expect("remove outside norn");
        wait_for_stored_path(&vault, path, true);
    }
    std::fs::write(vault.path().join(FOLDER).join("copy.md"), b"# First\n").expect("copy bytes");
    wait_for_stored_path(&vault, "copy.md", false);
    wait_for_broken(&host, &vault);

    let plan = previewed(host.repair(repairing(&vault, ApplyMode::Preview)));

    assert_follow_skip(
        &plan,
        SkipReason::SeveralTombstones,
        &["zz-repair/old.md", "zz-repair/old.md.md"],
    );
}

/// **A follow names the followed document where its settled route lands it.**
/// Its author hash remains on that document's before-state path, guarded by the move.
#[test]
fn a_follow_to_a_document_a_route_of_the_batch_moves_is_spelled_toward_where_it_lands() {
    let target = b"---\ntype: task\n---\n# Followed\n";
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-follow-routed-destination",
        TASKED,
        &[
            ("old.md", target),
            ("zz-holder.md", b"See [x](./old.md).\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    moved_on_disk(&host, &vault, "old.md", "loose/new.md");

    let plan = previewed(host.repair(repairing_beneath(&vault, ApplyMode::Preview)));

    assert_eq!(plan.operations.len(), 2, "{plan:?}");
    let rewrite = plan
        .operations
        .iter()
        .find(|operation| matches!(operation.kind, OperationKind::RewriteLink { .. }))
        .expect("one followed link rewrite");
    assert_eq!(
        rewrite.kind,
        OperationKind::rewrite_link(
            at("zz-holder.md"),
            norn_wire::LinkFamily::Markdown,
            "./old.md",
            "./tasks/new.md",
        )
    );
    let hash = norn_wire::ContentHash::new(format!(
        "sha256:{}",
        norn_fs::ContentHash::of(target).to_hex()
    ))
    .expect("hash");
    assert_eq!(
        rewrite.conditions,
        vec![norn_wire::AuthorCondition::content_hash(
            at("loose/new.md"),
            hash.clone()
        )]
    );
    assert!(
        plan.conditions
            .contains(&norn_wire::PlanCondition::address_resolution(
                norn_wire::LinkKey::new(
                    at("zz-holder.md"),
                    norn_wire::LinkFamily::Markdown,
                    "./tasks/new.md"
                ),
                norn_wire::Resolves::one(at("tasks/new.md")),
            )),
        "{plan:?}"
    );
    assert!(!plan.conditions.iter().any(|condition| {
        matches!(condition, norn_wire::PlanCondition::ContentHash { path, .. } if path == &at("loose/new.md"))
    }));
    let source = plan
        .transitions
        .iter()
        .find(|transition| transition.path == at("loose/new.md"))
        .expect("the move's source");
    assert!(
        matches!(&source.before, norn_wire::FileState::Present { hash: before, .. } if before == &hash)
    );
    assert!(provenance(&plan).skipped.is_empty(), "{plan:?}");
    let report = planned(host.apply(ApplyParams::new(
        ApplyMode::Apply,
        PlanDocument::resolved(plan),
    )));
    assert!(matches!(report, ApplyReport::Applied { .. }), "{report:?}");
    assert_eq!(
        written(&vault, "zz-holder.md"),
        "See [x](./tasks/new.md).\n"
    );
    assert!(stands(&vault, "tasks/new.md"));
    assert!(!stands(&vault, "loose/new.md"));
}

/// **A broken wikilink suggests its best stem, guards both addresses and applies only when suggestions are admitted.**
#[test]
fn a_broken_wikilink_suggests_its_most_similar_stem_through_preview_then_apply() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-similarity",
        "version: 1\n",
        &[
            ("abcdefghij.md", b"# Target\n"),
            ("zz-holder.md", b"See [[abcdefghix]].\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let strict = previewed(host.repair(repairing(&vault, ApplyMode::Preview)));
    let skipped = &provenance(&strict).skipped[0];
    assert_eq!(skipped.reason, SkipReason::BelowThreshold);
    assert_eq!(
        skipped.note.as_deref(),
        Some("similarity 0.69 (trigram, cutoff 0.50)")
    );
    let operation = skipped.proposed.as_ref().expect("suggested rewrite");
    assert_eq!(
        operation.kind,
        OperationKind::rewrite_link(
            at("zz-holder.md"),
            norn_wire::LinkFamily::Wikilink,
            "abcdefghix",
            "abcdefghij"
        )
    );
    assert!(operation.conditions.is_empty());
    let plan = previewed(
        host.repair(repairing(&vault, ApplyMode::Preview).with_threshold(Confidence::Suggested)),
    );
    assert_eq!(plan.operations.len(), 1);
    let cited = &provenance(&plan).citations[0].findings[0];
    assert_eq!(cited.confidence, Confidence::Suggested);
    assert_eq!(cited.notes, vec!["similarity 0.69 (trigram, cutoff 0.50)"]);
    for (address, after) in [
        ("abcdefghix", norn_wire::Resolves::none()),
        ("abcdefghij", norn_wire::Resolves::one(at("abcdefghij.md"))),
    ] {
        assert!(
            plan.conditions
                .contains(&norn_wire::PlanCondition::address_resolution(
                    norn_wire::LinkKey::new(
                        at("zz-holder.md"),
                        norn_wire::LinkFamily::Wikilink,
                        address
                    ),
                    after
                ))
        );
    }
    assert!(matches!(
        planned(host.apply(ApplyParams::new(
            ApplyMode::Apply,
            PlanDocument::resolved(plan.clone())
        ))),
        ApplyReport::Applied { .. }
    ));
    assert_eq!(written(&vault, "zz-holder.md"), "See [[abcdefghij]].\n");
}

/// **Equal best stems skip as a tie carrying both documents and the score.**
#[test]
fn equal_best_stems_skip_as_a_tie_with_both_documents() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-similarity-tie",
        "version: 1\n",
        &[
            ("abcdefghij.md", b"# J\n"),
            ("abcdefghik.md", b"# K\n"),
            ("zz-holder.md", b"See [[abcdefghix]].\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let plan = previewed(
        host.repair(repairing(&vault, ApplyMode::Preview).with_threshold(Confidence::Suggested)),
    );
    assert!(plan.operations.is_empty());
    let skipped = &provenance(&plan).skipped[0];
    assert_eq!(skipped.reason, SkipReason::Tie);
    assert_eq!(
        skipped.note.as_deref(),
        Some("tie at similarity 0.69 (trigram, cutoff 0.50)")
    );
    let head = norn_wire::CandidateHead::new(
        ["abcdefghij.md", "abcdefghik.md"]
            .map(|name| norn_wire::Candidate::new(at(name), name.strip_suffix(".md").unwrap())),
        2,
    )
    .unwrap();
    assert_eq!(
        skipped.candidates,
        Some(norn_wire::SkippedCandidates::documents(head))
    );
}

/// **The prefix filter retrieves equal best candidates sharing rare and common query trigrams.**
#[test]
fn rare_and_common_trigram_candidates_are_both_in_the_best_score_tie() {
    let mut documents = vec![
        ("ab.md".to_string(), b"# Short\n".to_vec()),
        ("abcaa.md".to_string(), b"# Rare\n".to_vec()),
        ("zz-holder.md".to_string(), b"See [[abcab]].\n".to_vec()),
    ];
    for n in 0..20 {
        documents.push((format!("abzz{n:02}ab.md"), b"# Filler\n".to_vec()));
    }
    let borrowed: Vec<_> = documents
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
        .collect();
    let (_sandbox, vault, host) =
        a_vault_under("host-similarity-prefix-tie", "version: 1\n", &borrowed);
    let _lease = attach::attach_and_wait(&host, vault.name());
    let plan = previewed(
        host.repair(repairing(&vault, ApplyMode::Preview).with_threshold(Confidence::Suggested)),
    );
    let skipped = &provenance(&plan).skipped[0];
    assert_eq!(skipped.reason, SkipReason::Tie);
    assert_eq!(
        skipped.note.as_deref(),
        Some("tie at similarity 0.50 (trigram, cutoff 0.50)")
    );
    let head = norn_wire::CandidateHead::new(
        ["ab.md", "abcaa.md"]
            .map(|name| norn_wire::Candidate::new(at(name), name.strip_suffix(".md").unwrap())),
        2,
    )
    .unwrap();
    assert_eq!(
        skipped.candidates,
        Some(norn_wire::SkippedCandidates::documents(head))
    );
}

/// **An over-cap query skips as too common with its stem, trigram and cap.**
#[test]
fn a_stem_past_the_postings_cap_skips_as_too_common() {
    let mut documents = vec![("zz-holder.md".to_string(), b"See [[aaaaa]].\n".to_vec())];
    for n in 0..257 {
        documents.push((format!("p{n}/aaaa.md"), b"# Common\n".to_vec()));
    }
    let borrowed: Vec<_> = documents
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
        .collect();
    let (_sandbox, vault, host) =
        a_vault_under("host-similarity-common", "version: 1\n", &borrowed);
    let _lease = attach::attach_and_wait(&host, vault.name());
    let plan = previewed(host.repair(repairing(&vault, ApplyMode::Preview)));
    let skipped = &provenance(&plan).skipped[0];
    assert_eq!(skipped.reason, SkipReason::TooCommon);
    assert_eq!(skipped.value, Some(norn_store::value_head("aaaaa")));
    assert_eq!(
        skipped.note.as_deref(),
        Some("trigram `  a` exceeds the postings cap 256")
    );
}

/// **A folded leaf stem shorter than three characters skips without reading the similarity index.**
#[test]
fn a_short_folded_leaf_stem_skips_without_reading_the_similarity_index() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-similarity-short",
        "version: 1\n",
        &[(
            "zz-holder.md",
            "See [[vault://folder/E\u{301}x.md]].\n".as_bytes(),
        )],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let before = host.read_evidence();
    let plan = previewed(host.repair(repairing(&vault, ApplyMode::Preview)));
    assert_eq!(
        host.read_evidence().since(before).similar_stems,
        norn_host::SimilarStemsCost::default()
    );
    let skipped = &provenance(&plan).skipped[0];
    assert_eq!(skipped.reason, SkipReason::TooShort);
    assert_eq!(skipped.value, Some(norn_store::value_head("E\u{301}x")));
    assert_eq!(
        skipped.note.as_deref(),
        Some("the shortest admitted query is 3 characters")
    );
}

/// **A stem below the similarity cutoff keeps no_declared_fix without proposing an operation.**
#[test]
fn a_stem_below_the_cutoff_has_no_proposed_operation() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-similarity-cutoff",
        "version: 1\n",
        &[
            ("abcdefghixqrstuvwxyz.md", b"# Low\n"),
            ("zz-holder.md", b"See [[abcdefghix]].\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let plan = previewed(
        host.repair(repairing(&vault, ApplyMode::Preview).with_threshold(Confidence::Suggested)),
    );
    assert!(plan.operations.is_empty());
    let skipped = &provenance(&plan).skipped[0];
    assert_eq!(skipped.reason, SkipReason::NoDeclaredFix);
    assert!(skipped.proposed.is_none());
}

/// **A unique move follow wins over a similar stem without reading the similarity index.**
#[test]
fn a_unique_move_follow_outranks_a_similar_stem_without_reading_it() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-similarity-follow-first",
        "version: 1\n",
        &[
            ("abcdefghix.md", b"# Moved\n"),
            ("abcdefghij.md", b"# Similar\n"),
            ("zz-holder.md", b"See [[abcdefghix]].\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    moved_on_disk(&host, &vault, "abcdefghix.md", "elsewhere.md");
    let before = host.read_evidence();
    let plan = previewed(
        host.repair(repairing(&vault, ApplyMode::Preview).with_threshold(Confidence::Suggested)),
    );
    assert_eq!(host.read_evidence().since(before).similar_stems.reads, 0);
    assert_eq!(
        plan.operations[0].kind,
        OperationKind::rewrite_link(
            at("zz-holder.md"),
            norn_wire::LinkFamily::Wikilink,
            "abcdefghix",
            "elsewhere"
        )
    );
    assert_eq!(
        provenance(&plan).citations[0].findings[0].confidence,
        Confidence::Derived
    );
}

/// **Recreating the old target refuses a suggestion through its replaced-address condition.**
#[test]
fn recreating_the_old_target_refuses_a_suggestion_through_the_replaced_address() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-similarity-recreated",
        "version: 1\n",
        &[
            ("abcdefghij.md", b"# Target\n"),
            ("zz-holder.md", b"See [[abcdefghix]].\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let plan = previewed(
        host.repair(repairing(&vault, ApplyMode::Preview).with_threshold(Confidence::Suggested)),
    );
    assert_eq!(plan.operations.len(), 1);
    std::fs::write(
        vault.path().join(FOLDER).join("abcdefghix.md"),
        b"# Recreated\n",
    )
    .unwrap();
    wait_for_stored_path(&vault, "abcdefghix.md", false);
    let (_, _, checks) = apply_refused(&host, ApplyMode::Apply, &plan);
    let replaced = norn_wire::PlanCondition::address_resolution(
        norn_wire::LinkKey::new(
            at("zz-holder.md"),
            norn_wire::LinkFamily::Wikilink,
            "abcdefghix",
        ),
        norn_wire::Resolves::none(),
    );
    assert!(fails(&checks, &replaced), "{checks:?}");
    assert_eq!(written(&vault, "zz-holder.md"), "See [[abcdefghix]].\n");
}

/// **A Markdown query decodes its leaf and drops a document extension in any ASCII case.**
/// The leaf differs from its target by more than case, so it is broken on a folding root too.
#[test]
fn a_percent_encoded_markdown_leaf_with_an_uppercase_extension_suggests_the_decoded_stem() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-similarity-decoded",
        "version: 1\n",
        &[
            ("my note.md", b"# Target\n"),
            ("zz-holder.md", b"See [x](My%20Notes.MD).\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let plan = previewed(
        host.repair(repairing(&vault, ApplyMode::Preview).with_threshold(Confidence::Suggested)),
    );
    assert_eq!(plan.operations.len(), 1, "{plan:?}");
    assert_eq!(
        plan.operations[0].kind,
        OperationKind::rewrite_link(
            at("zz-holder.md"),
            norn_wire::LinkFamily::Markdown,
            "My%20Notes.MD",
            "my%20note.md"
        )
    );
    assert_eq!(
        provenance(&plan).citations[0].findings[0].confidence,
        Confidence::Suggested
    );
    assert_eq!(
        provenance(&plan).citations[0].findings[0].notes,
        ["similarity 0.70 (trigram, cutoff 0.50)"]
    );
    assert!(
        plan.conditions
            .contains(&norn_wire::PlanCondition::address_resolution(
                norn_wire::LinkKey::new(
                    at("zz-holder.md"),
                    norn_wire::LinkFamily::Markdown,
                    "my%20note.md"
                ),
                norn_wire::Resolves::one(at("my note.md"))
            ))
    );
}

/// **A similarity suggestion excludes its own holder and chooses the next best document.**
#[test]
fn a_broken_link_never_suggests_its_own_holder() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-similarity-excludes-holder",
        "version: 1\n",
        &[
            ("meeting-2025.md", b"See [[meeting-2024]].\n"),
            ("meeting-2023.md", b"# Earlier\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let plan = previewed(
        host.repair(repairing(&vault, ApplyMode::Preview).with_threshold(Confidence::Suggested)),
    );
    assert_eq!(plan.operations.len(), 1, "{plan:?}");
    assert_eq!(
        plan.operations[0].kind,
        OperationKind::rewrite_link(
            at("meeting-2025.md"),
            norn_wire::LinkFamily::Wikilink,
            "meeting-2024",
            "meeting-2023"
        )
    );
    assert_eq!(
        provenance(&plan).citations[0].findings[0].notes,
        ["similarity 0.73 (trigram, cutoff 0.50)"]
    );
}

/// **A no-holder tombstone keeps its move-following skip without consulting a similar live target.**
#[test]
fn a_broken_link_with_a_no_holder_tombstone_stays_broken_without_a_suggestion() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-similarity-tombstone",
        "version: 1\n",
        &[
            ("abcdefghix.md", b"# Removed\n"),
            ("abcdefghij.md", b"# Target\n"),
            ("zz-holder.md", b"See [[abcdefghix]].\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    moved_on_disk(&host, &vault, "abcdefghix.md", "elsewhere.md");
    std::fs::write(
        vault.path().join(FOLDER).join("elsewhere.md"),
        b"# Changed\n",
    )
    .expect("change the moved bytes");
    wait_until(
        "changed holder hash is derived",
        attach::state_budget(attach::READY_LIMIT),
        || {
            let mut store = vault.store();
            let held = store
                .begin_request()
                .stored_document(
                    &norn_store::DocumentPath::new(at("elsewhere.md").as_str()).expect("a path"),
                )
                .expect("the moved document");
            if held.as_ref().is_some_and(|doc| {
                doc.content_hash.as_deref()
                    == Some(norn_fs::ContentHash::of(b"# Changed\n").to_hex().as_str())
            }) {
                Observed::Met(())
            } else {
                Observed::pending(format!("{held:?}"))
            }
        },
    )
    .expect("the changed bytes are derived");
    let before = host.read_evidence();
    let plan = previewed(
        host.repair(repairing(&vault, ApplyMode::Preview).with_threshold(Confidence::Suggested)),
    );
    assert_eq!(host.read_evidence().since(before).similar_stems.reads, 0);
    assert!(plan.operations.is_empty(), "{plan:?}");
    let skipped = &provenance(&plan).skipped;
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].reason, SkipReason::NoHolder);
    assert!(skipped[0].proposed.is_none());
    assert_eq!(
        skipped[0].note.as_deref(),
        Some(
            format!("no live document holds the hash recorded at `{FOLDER}abcdefghix.md`").as_str()
        )
    );
    assert_eq!(written(&vault, "zz-holder.md"), "See [[abcdefghix]].\n");
}

/// **An apply plans exactly what its preview planned with every 5C fix present.**
#[test]
fn an_apply_plans_what_its_preview_did_with_every_5c_fix_present() {
    let (_sandbox, vault, host, _lease) = a_vault_with_every_5c_fix("repair-5c-agreement");
    let request = |mode| repairing_beneath(&vault, mode).with_threshold(Confidence::Suggested);
    let preview = previewed(host.repair(request(ApplyMode::Preview)));
    let ApplyReport::Applied { plan, .. } = planned(host.repair(request(ApplyMode::Apply))) else {
        panic!("the repair applies");
    };
    assert_eq!(plan.operations, preview.operations);
    assert_eq!(plan.transitions, preview.transitions);
    assert_eq!(plan.conditions, preview.conditions);
    assert_eq!(plan.provenance, preview.provenance);
    let held = at("zz-holder.md");
    let expected = [
        OperationKind::rewrite_link(held.clone(), norn_wire::LinkFamily::Wikilink, "old", "new"),
        OperationKind::rewrite_link(
            held.clone(),
            norn_wire::LinkFamily::Wikilink,
            "abcdefghix",
            "abcdefghij",
        ),
        OperationKind::rewrite_anchor(
            held.clone(),
            norn_wire::LinkFamily::Wikilink,
            "zz-target",
            "Important Headinh",
            "Important Heading",
        ),
        OperationKind::set_frontmatter(
            WriteTarget::path(held.clone()),
            "status",
            AuthoredValue::string("todo"),
        ),
        OperationKind::rewrite_tag(held, "darft", "draft"),
    ];
    let actual = kinds(&plan);
    assert_eq!(actual.len(), expected.len(), "{actual:?}");
    for kind in expected {
        assert!(actual.contains(&kind), "missing {kind:?}: {actual:?}");
    }
}

/// **A resolved 5C preview and an apply-mode repair write every expected fix identically.**
#[test]
fn a_5c_previews_plan_lands_through_apply() {
    let (_first_box, first, first_host, first_lease) =
        a_vault_with_every_5c_fix("repair-5c-direct");
    let direct =
        planned(first_host.repair(
            repairing_beneath(&first, ApplyMode::Apply).with_threshold(Confidence::Suggested),
        ));
    assert!(matches!(direct, ApplyReport::Applied { .. }), "{direct:?}");
    drop(first_lease);
    drop(first_host);
    let (_second_box, second, second_host, _second_lease) =
        a_vault_with_every_5c_fix("repair-5c-resolved");
    let preview = previewed(second_host.repair(
        repairing_beneath(&second, ApplyMode::Preview).with_threshold(Confidence::Suggested),
    ));
    let sent = planned(second_host.apply(ApplyParams::new(
        ApplyMode::Apply,
        PlanDocument::resolved(preview),
    )));
    assert!(matches!(sent, ApplyReport::Applied { .. }), "{sent:?}");
    assert_eq!(
        std::fs::read(second.path().join(FOLDER).join("zz-holder.md")).expect("resolved holder"),
        std::fs::read(first.path().join(FOLDER).join("zz-holder.md")).expect("direct holder"),
    );
    assert_eq!(
        std::fs::read(second.path().join(FOLDER).join("zz-holder.md")).expect("repaired holder"),
        b"---\ntype: task\nstatus: todo\n---\n[[new]] [[abcdefghij]] [[zz-target#Important Heading]] #draft\n",
    );
}

/// One holder under one schema needs move-following and all four 5C suggestions.
fn a_vault_with_every_5c_fix(
    label: &str,
) -> (
    Sandbox,
    attach::Vault,
    attach::ServingHost,
    norn_host::DemandLease<norn_host::ProductionEntryOps>,
) {
    let (sandbox, vault, host) = a_vault_under(
        label,
        "version: 1\ntags: {declared: [draft], undeclared: report}\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    one_of:\n      status: {values: [todo, done]}\n",
        &[
            ("old.md", b"# Unique followed document\n"),
            ("abcdefghij.md", b"# Similar target\n"),
            ("zz-target.md", b"# Important Heading\n"),
            ("zz-holder.md", b"---\ntype: task\nstatus: todx\n---\n[[old]] [[abcdefghix]] [[zz-target#Important Headinh]] #darft\n"),
        ],
    );
    let lease = attach::attach_and_wait(&host, vault.name());
    moved_on_disk(&host, &vault, "old.md", "new.md");
    (sandbox, vault, host, lease)
}

#[test]
fn resolving_vault_links_have_a_derived_rooted_fix_in_their_own_family() {
    let before = "![[vault://zz-repair/space name#Heading|Alias]] [angle](<vault://zz-repair/space name.md#Heading>) [escaped](vault://zz-repair/space%20name#Heading)\n";
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-vault-scheme",
        "version: 1\n",
        &[
            ("space name.md", b"# Heading\n"),
            ("holder.md", before.as_bytes()),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let plan = previewed(
        host.repair(repairing(&vault, ApplyMode::Preview).with_kinds([FindingKind::VaultScheme])),
    );
    assert_eq!(plan.operations.len(), 3, "{plan:?}");
    let plan: ResolvedPlan =
        serde_json::from_value(serde_json::to_value(plan).expect("serialize rooted repair"))
            .expect("read rooted repair");
    assert!(
        provenance(&plan)
            .citations
            .iter()
            .flat_map(|citation| &citation.findings)
            .all(|finding| finding.confidence == Confidence::Derived)
    );
    let report = planned(host.apply(ApplyParams::new(
        ApplyMode::Apply,
        PlanDocument::resolved(plan),
    )));
    assert!(matches!(report, ApplyReport::Applied { .. }), "{report:?}");
    assert_eq!(
        written(&vault, "holder.md"),
        "![[/zz-repair/space name#Heading|Alias]] [angle](</zz-repair/space name.md#Heading>) [escaped](/zz-repair/space%20name#Heading)\n"
    );
    assert!(findings_beneath(&host, &vault).is_empty());
}

#[test]
fn markdown_repairs_encode_destinations_in_each_occurrences_form() {
    let (_sandbox, vault, host) = a_vault_under(
        "host-repair-destination-encoding",
        "version: 1\n",
        &[
            ("old.md", b"# Followed\n"),
            ("zz-holder.md", b"[plain](./old.md) [angle](<./old.md>)\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let changed = planned(host.apply(ApplyParams::new(
        ApplyMode::Apply,
        PlanDocument::operations(norn_wire::AuthoredPlan::new(
            address(&vault),
            vec![
                Operation::new(OperationKind::create_document(
                    at("new name.md"),
                    "# Followed\n",
                )),
                Operation::new(OperationKind::delete_document_breaking_links(at("old.md"))),
            ],
        )),
    )));
    assert!(
        matches!(changed, ApplyReport::Applied { .. }),
        "{changed:?}"
    );
    let plan = previewed(
        host.repair(repairing(&vault, ApplyMode::Preview).with_kinds([FindingKind::Broken])),
    );
    assert_eq!(plan.operations.len(), 1, "{plan:?}");
    let plan = serde_json::from_value(serde_json::to_value(plan).expect("serialize repair"))
        .expect("read repair");
    let report = planned(host.apply(ApplyParams::new(
        ApplyMode::Apply,
        PlanDocument::resolved(plan),
    )));
    assert!(matches!(report, ApplyReport::Applied { .. }), "{report:?}");
    assert_eq!(
        written(&vault, "zz-holder.md"),
        "[plain](./new%20name.md) [angle](<./new name.md>)\n"
    );
    assert!(findings_beneath(&host, &vault).is_empty());
}

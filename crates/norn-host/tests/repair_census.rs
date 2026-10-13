//! **Every statement a repair runs is planned as an index search**: the
//! census that binds the `EXPLAIN` bars to what `Host::repair` actually runs.
//!
//! Each store statement family carries its own plan bars, and each family's
//! census holds every statement it names to a bar. What no family census can
//! say is that a repair runs only statements some family names. So this suite
//! records the text of every statement SQLite begins on the vault's store —
//! its writer and its readers — while a real host previews and applies repairs
//! over selections reaching every arm the batch read has, and takes the plan
//! of each distinct one:
//!
//! - the unnarrowed first batch, cut inside a document, its tail and the probe
//!   for more past it; and its continuation from the cursor it answered;
//! - a batch of one narrowed by rule, by rule and severity, by kind, by
//!   severity, by a path part, by a path part and a rule, and by a document
//!   part, each cut inside a document (the suite asserts the cut), so its
//!   tail and the probe past it are in the record, and each continued from
//!   the cursor it minted. A continuation's page is the first page's text,
//!   its position a bound parameter, so what continuing adds to the record is
//!   any statement only a continuation begins;
//! - fixes whose planning reads what the snapshot holds: a route whose
//!   documents other documents link to, so the plan resolves the links it
//!   moves; a route whose cascade may respell a frontmatter link a rule reads
//!   by value, so the holder's frontmatter is read; a link finding the route
//!   resolves, judged by the stored link at its ordinal; and a bare value made
//!   a link, so its address is resolved;
//! - each previewed, then the batch applied, so the applier's own judgment, its
//!   read of the pinned declaration and the changeset it commits are in the
//!   record too.
//!
//! **What the census holds every statement to** is the scan bar the family
//! bars share (`norn_testkit::explain`): no step reads a relation end to end,
//! a covering index included, or builds an automatic index, and no step sorts
//! into a temporary b-tree but the three sorts a family's own bar names and
//! bounds ([`admitted_sort`]). The plan is taken of the statement's own text
//! with its parameters unbound, which is the plan it ran with: the store
//! gathers no statistics and no statement matches a `LIKE` or `GLOB` pattern,
//! so SQLite chooses no plan by a bound value, and the census checks that
//! premise over the record rather than assuming it.
//!
//! **Writes are held to the same bar.** What an apply's changeset runs is the
//! write path's registry (`norn_store`'s `WriteStatement`), whose own bar reads
//! every step a write's plan shows; the census holds the writes it records to
//! that same bar, so a write a repair runs outside the registry is judged too.
//! A trigger's statements are reported one by one, so the census plans them
//! as well — the full-text maintenance and the tombstone clear the registry
//! states it does not cover among them — and so are the statements the
//! full-text index runs for itself.
//!
//! The record is process-wide and names no connection, so this suite is one
//! case in a binary of its own: nothing else runs in its process.
#![cfg(unix)]
#![allow(clippy::disallowed_methods)] // Harness scaffolding: this suite's own generated tree.

mod attach;

use std::collections::BTreeSet;
use std::path::Path;

use norn_store::{Store, induced_failure};
use norn_testkit::explain::{PlanRow, QueryPlan};
use norn_testkit::process::Sandbox;
use norn_wire::{
    ApplyMode, ApplyReport, FindingKind, Predicate, Provenance, RepairParams, ResolvedPlan,
    Severity, VaultAddress,
};

/// The schema the census vault pins: a task rule with a default, a synonym
/// and a route, at `error`; a hub rule reading `up` by value, at `warning`;
/// and a `link` field a bare value is a type mismatch of.
const SCHEMA: &str = "version: 1\nfields:\n  ref: {type: link}\nrules:\n  tasks:\n    severity: error\n    match: {frontmatter: {type: task}}\n    required:\n      status: {default: todo}\n    one_of:\n      status: {values: [todo, done], synonyms: {complete: done}}\n    allowed_paths: {paths: ['zz-repair/tasks/**'], route: 'zz-repair/tasks/'}\n  hubs:\n    severity: warning\n    match: {frontmatter: {type: hub}}\n    max_length:\n      up: 40\n";

/// Where every document the census arranges stands.
const FOLDER: &str = "zz-repair/";

/// The document that sorts before every other the census selects: a task
/// missing its `status`, outside its route's folder, holding two links nothing
/// resolves, so it carries four findings of four sorts (a required field
/// missing, a misplacement, and two broken links). A batch of one over any
/// narrowing that reaches it is cut inside it, which is what runs the
/// document's tail and the probe for more past it.
const CUT_DOCUMENT: (&str, &[u8]) = (
    "0-cut.md",
    b"---\ntype: task\n---\nSee [[nowhere-a]] and [[nowhere-b]].\n",
);

/// The documents the census arranges, beneath [`FOLDER`]:
///
/// - `loose/a.md` is a task missing its `status`, outside its route's folder,
///   and linked to from `h.md`'s body and from `hub.md`'s `up`, which the hub
///   rule reads by value;
/// - `loose/b.md` is a task holding a synonym, outside its route's folder,
///   and linked to from `h.md`;
/// - `loose/zz-land.md` is a task outside its route's folder, which `h.md`
///   names where the route lands it, so that link is broken until it moves;
/// - `h.md` holds those links and one nothing resolves;
/// - `hub.md` is a hub whose `up` links `loose/a.md`, so the route that would
///   respell it is judged by the hub's frontmatter;
/// - `zz-holder.md` holds a bare value in a `link` field, naming
///   `zz-target.md`.
const DOCUMENTS: [(&str, &[u8]); 7] = [
    ("loose/a.md", b"---\ntype: task\n---\n# A\n"),
    (
        "loose/b.md",
        b"---\ntype: task\nstatus: complete\n---\n# B\n",
    ),
    (
        "loose/zz-land.md",
        b"---\ntype: task\nstatus: todo\n---\n# Land\n",
    ),
    (
        "h.md",
        b"See [[zz-repair/loose/a]], [b](loose/b.md), [[tasks/zz-land]] and [[nowhere]].\n",
    ),
    (
        "hub.md",
        b"---\ntype: hub\nup: \"[[zz-repair/loose/a]]\"\n---\n",
    ),
    ("zz-holder.md", b"---\nref: zz-target\n---\n# Holder\n"),
    ("zz-target.md", b"# Target\n"),
];

/// **Every statement a repair runs, previewed or applied, is planned as an
/// index search**, over every arm of the batch read and every read its fixes'
/// planning and the applier's judgment make.
///
/// Control: with the index the held-link read and the stored link at a
/// finding's ordinal seek dropped, the same record fails the census.
#[test]
fn every_statement_a_repair_runs_is_planned_as_an_index_search() {
    induced_failure::record_statements_begun();
    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), "host-repair-census")
        .expect("a sandbox");
    let vault = attach::Vault::generate(&sandbox.work_dir().join("attached"), "tiny");
    std::fs::write(vault.path().join(".norn/schema.yaml"), SCHEMA).expect("write the schema");
    for (name, bytes) in DOCUMENTS {
        let at = vault.path().join(FOLDER).join(name);
        std::fs::create_dir_all(at.parent().expect("a parent folder")).expect("make the folder");
        std::fs::write(at, bytes).expect("write a document");
    }
    std::fs::write(vault.path().join(CUT_DOCUMENT.0), CUT_DOCUMENT.1)
        .expect("write the cut document");
    let host = vault.host();
    let lease = attach::attach_and_wait(&host, vault.name());
    let attached = induced_failure::statements_begun();

    let ran = repairs_over_every_arm(&host, &VaultAddress::name(vault.name().clone()));
    drop(lease);
    drop(host);
    induced_failure::stop_recording_statements();

    // The premise the plans are taken on: nothing gathered statistics, so no
    // plan is chosen by a bound value.
    let gathered: Vec<&String> = attached
        .iter()
        .chain(&ran)
        .filter(|sql| sql.to_ascii_uppercase().contains("ANALYZE"))
        .collect();
    assert!(
        gathered.is_empty(),
        "the store gathered statistics: {gathered:?}"
    );

    let statements: BTreeSet<String> = ran.iter().filter_map(|sql| judged(sql)).collect();
    // Nor is one chosen by a pattern bound to a `LIKE` or a `GLOB`, the other
    // way SQLite plans by a bound value.
    let patterned: Vec<&String> = statements
        .iter()
        .filter(|sql| names_a_word(sql, "LIKE") || names_a_word(sql, "GLOB"))
        .collect();
    assert!(
        patterned.is_empty(),
        "a statement matches a pattern: {patterned:?}"
    );
    let mut store = vault.store();
    let plans = plans_of(&store, &statements);
    let unbarred = census(&plans);
    assert!(
        unbarred.is_empty(),
        "a repair ran {} statement(s) no index search plans:\n\n{}",
        unbarred.len(),
        unbarred.join("\n\n")
    );
    let reached: BTreeSet<&str> = plans
        .iter()
        .flat_map(|plan| plan.rows().iter().filter_map(PlanRow::index))
        .collect();
    for arms in REACHED {
        assert!(
            arms.iter().any(|index| reached.contains(index)),
            "the census reached no seek of {arms:?}: {reached:?}"
        );
    }

    induced_failure::execute_out_of_band(&mut store, "DROP INDEX links_document_ordinal")
        .expect("drop the index");
    let replanned = plans_of(&store, &statements);
    assert!(
        !census(&replanned).is_empty(),
        "with links_document_ordinal dropped, the census found every statement searched"
    );
}

/// The indexes the census's selections reach, each named with the arm whose
/// seek it is, any one of a group serving: the record is only a census of a
/// repair's statements where it holds every arm's.
const REACHED: &[&[&str]] = &[
    // A merged page and a document's tail by kind, and by kind and severity.
    &["findings_fingerprint_kind_nocase"],
    &["findings_fingerprint_kind_severity_nocase"],
    // The same, narrowed by rule: the first page, and the tail of a cut
    // document, which seeks the same indexes.
    &["finding_rules_fingerprint_rule_kind_nocase"],
    &["finding_rules_fingerprint_rule_kind_severity_nocase"],
    // A document part driving the batch.
    &["document_fields_raw"],
    // The stored link at a finding's ordinal, and a holder's held links.
    &["links_document_ordinal"],
    // A held document's row by its path.
    &["documents_path"],
    // The resolution of a route's moved links and a link fix's address: the
    // links held under a key and a class's heads, each under the key the root
    // decides (raw where it tells spellings apart, folded where it folds).
    &["link_keys_key", "link_keys_folded_key"],
    &[
        "documents_admitted_suffix_key",
        "documents_admitted_folded_suffix_key",
    ],
    // The apply's changeset: its written links re-decided, and the rule sets
    // its findings cite.
    &["documents_change_feed"],
    &["rule_sets_fingerprint_rules"],
];

/// Preview a repair over every arm of the batch read through `host`, then
/// apply one, and hand back the text of every statement each ran.
fn repairs_over_every_arm(host: &attach::ServingHost, address: &VaultAddress) -> Vec<String> {
    let repairing = |mode| RepairParams::new(address.clone(), mode);
    let beneath = || Predicate::path(format!("{FOLDER}**"));
    let mut ran = Vec::new();
    let mut repair = |params: RepairParams| -> ApplyReport {
        let report = host
            .repair(params.clone())
            .unwrap_or_else(|refused| panic!("{params:?} was refused: {refused:?}"))
            .wait()
            .unwrap_or_else(|refused| panic!("{params:?} answered no report: {refused:?}"))
            .report;
        ran.extend(induced_failure::statements_begun());
        report
    };

    // Each narrowing is run as a batch of one that cuts a document, so its
    // tail and the probe past it run, then continued from the cursor it
    // minted, so a statement only a continuation begins runs too.
    let narrowings = [
        repairing(ApplyMode::Preview),
        repairing(ApplyMode::Preview).with_rule("tasks"),
        repairing(ApplyMode::Preview)
            .with_rule("tasks")
            .with_severity(Severity::Error),
        repairing(ApplyMode::Preview).with_kinds([FindingKind::Broken]),
        repairing(ApplyMode::Preview)
            .with_kinds([FindingKind::Misplaced, FindingKind::RequiredMissing]),
        repairing(ApplyMode::Preview).with_severity(Severity::Error),
        repairing(ApplyMode::Preview).with_predicates([beneath()]),
        repairing(ApplyMode::Preview)
            .with_rule("tasks")
            .with_predicates([beneath()]),
        repairing(ApplyMode::Preview).with_predicates([Predicate::equal_to("type", "task")]),
    ];
    for narrowing in narrowings {
        let first = repair(narrowing.clone().with_limit(1));
        assert!(
            findings_in(planned(&first)) > 1,
            "a batch of one cut no document under {narrowing:?}: {:?}",
            provenance(planned(&first))
        );
        let next = provenance(planned(&first))
            .cursor
            .clone()
            .expect("a batch of one leaves more to repair");
        repair(narrowing.with_limit(1).with_after(*next));
    }
    for params in [
        repairing(ApplyMode::Preview),
        repairing(ApplyMode::Preview)
            .with_rule("hubs")
            .with_predicates([beneath()]),
    ] {
        repair(params);
    }
    let applied = repair(repairing(ApplyMode::Apply).with_predicates([beneath()]));
    assert!(
        matches!(applied, ApplyReport::Applied { .. }),
        "the census's apply landed: {applied:?}"
    );
    assert!(
        !planned(&applied).operations.is_empty(),
        "the census's apply wrote something"
    );
    ran
}

/// Whether `sql` spells `word` as a word of its own, in any case and beside
/// whatever separates it: a space, a newline, a parenthesis or a bracket. A
/// longer name that holds it, `norn_glob` among them, is not the word.
fn names_a_word(sql: &str, word: &str) -> bool {
    sql.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .any(|token| token.eq_ignore_ascii_case(word))
}

/// How many findings a repair plan's batch covers: those its operations fix
/// and those it left alone.
fn findings_in(plan: &ResolvedPlan) -> usize {
    let provenance = provenance(plan);
    provenance
        .citations
        .iter()
        .map(|citation| citation.findings.len())
        .sum::<usize>()
        + provenance.skipped.len()
}

/// The statement the census plans for one the trace recorded, or none for
/// the start of a trigger's program, whose statements the trace reports one
/// by one.
///
/// A statement SQLite runs inside another — a trigger's, or one a virtual
/// table runs for itself — is reported behind `-- `. A trigger's statement
/// reads the row it fired on as `old.<column>` and `new.<column>`, which only
/// the trigger can prepare, so each is planned as a parameter, as the write
/// path's registry plans the one trigger body it registers.
fn judged(recorded: &str) -> Option<String> {
    match recorded.strip_prefix("-- ") {
        Some(nested) if nested.starts_with("TRIGGER ") => None,
        Some(nested) => Some(with_row_references_unbound(nested)),
        None => Some(recorded.to_string()),
    }
}

/// `sql` with each `old.<column>` and `new.<column>` a parameter.
fn with_row_references_unbound(sql: &str) -> String {
    let identifier = |character: char| character.is_ascii_alphanumeric() || character == '_';
    let mut unbound = String::with_capacity(sql.len());
    let mut rest = sql;
    while let Some(character) = rest.chars().next() {
        if !identifier(character) {
            unbound.push(character);
            rest = &rest[character.len_utf8()..];
            continue;
        }
        let word = rest.find(|c: char| !identifier(c)).unwrap_or(rest.len());
        let (name, after) = rest.split_at(word);
        let column = after
            .strip_prefix('.')
            .filter(|_| name.eq_ignore_ascii_case("old") || name.eq_ignore_ascii_case("new"));
        match column {
            Some(column) => {
                unbound.push('?');
                rest = &column[column
                    .find(|c: char| !identifier(c))
                    .unwrap_or(column.len())..];
            }
            None => {
                unbound.push_str(name);
                rest = after;
            }
        }
    }
    unbound
}

/// Every plan in `plans` that reads a relation end to end, builds an
/// automatic index, or sorts into a temporary b-tree no family bar admits,
/// each spelled with its statement and its plan.
fn census(plans: &[QueryPlan]) -> Vec<String> {
    plans
        .iter()
        .filter_map(|plan| {
            let scans = plan.unbounded_steps();
            let unbounded: Vec<&PlanRow> = plan
                .rows()
                .iter()
                .filter(|row| {
                    scans.contains(row)
                        || row.uses_automatic_index()
                        || (row.detail.contains("TEMP B-TREE")
                            && admitted_sort(plan, row).is_none())
                })
                .collect();
            (!unbounded.is_empty()).then(|| {
                let rows: Vec<String> = plan.rows().iter().map(PlanRow::to_string).collect();
                let steps: Vec<String> = unbounded.iter().map(|row| row.to_string()).collect();
                format!(
                    "{}\nunbounded: {}\nplan: {}",
                    plan.sql(),
                    steps.join("; "),
                    rows.join("; ")
                )
            })
        })
        .collect()
}

/// The family whose own bar names `sort` in `plan`, and why the sort is
/// bounded by what that family reads rather than by the vault; `None` for a
/// sort no bar admits.
///
/// - A class's heads (`ExplainedStatement::LinkHealthHeads`): each admitting
///   count's run is read in ladder order off the admitted-key index, and the
///   runs' union — at most a head per count the key's segments reach — is
///   sorted and cut again.
/// - A page of a write's links (`ExplainedStatement::LinkHealthWrittenLinks`):
///   read in path order off the change-feed index, which does not say a path
///   is one document's, so the links of one path, which the changeset wrote,
///   are sorted by ordinal.
/// - **One document's findings on a page a document part drives (NORN-390).**
///   The merged page and a validate's kind or rule page a part keeping its
///   documents in the answer's path order narrows read the part's documents
///   in a co-routine in that order, from the page's place, and seek each
///   one's findings at its path; what SQLite orders is the last two terms,
///   position and id, among one document's findings, a run no longer than
///   the document's selected findings. Admitted only where a source drives
///   the statement (`CO-ROUTINE dv`), so a page that sorts what a part
///   matched — the cost NORN-390 removed — is found.
/// - **A summary a document part drives.** The summary
///   (`ValidateStatement::Summary` and its rule form) sorts its `GROUP BY`
///   into a temporary b-tree when a part on a field or a tag drives the
///   selection: the matched documents are reached by row id from the part's
///   own seek, and what is sorted is the groups of what the part matched. A
///   first batch's count costs the selected set, a declared limit; the
///   exemption is held to the statements that carry it, recognised by the
///   select list their family opens with, and a sort in any other statement
///   that searches those indexes is found.
fn admitted_sort(plan: &QueryPlan, sort: &PlanRow) -> Option<&'static str> {
    let searched_through = |table: &str, indexes: &[&str]| {
        plan.searches_of(table)
            .iter()
            .any(|row| row.index().is_some_and(|index| indexes.contains(&index)))
    };
    let driven = searched_through("document_fields", &["document_fields_raw"])
        || !plan.searches_of("document_tags").is_empty();
    let opens_with = |select_list: &str| {
        let spelled: Vec<&str> = plan.sql().split_whitespace().collect();
        spelled.join(" ").starts_with(select_list)
    };
    let a_page = opens_with("SELECT f.id, f.kind, f.severity, f.path,");
    let sourced = plan.rows().iter().any(|row| row.detail == "CO-ROUTINE dv");
    let a_summary = opens_with("SELECT f.kind, f.severity, COUNT(*)")
        || opens_with("SELECT fr.kind, fr.severity, COUNT(*)");
    match sort.detail.as_str() {
        "USE TEMP B-TREE FOR ORDER BY"
            if searched_through(
                "documents",
                &[
                    "documents_admitted_suffix_key",
                    "documents_admitted_folded_suffix_key",
                ],
            ) =>
        {
            Some("LinkHealthHeads")
        }
        "USE TEMP B-TREE FOR LAST TERM OF ORDER BY"
            if searched_through("documents", &["documents_change_feed"]) =>
        {
            Some("LinkHealthWrittenLinks")
        }
        "USE TEMP B-TREE FOR LAST 2 TERMS OF ORDER BY" if sourced && a_page => {
            Some("one document's findings on a page a document part drives")
        }
        "USE TEMP B-TREE FOR GROUP BY" if driven && a_summary => {
            Some("a summary a document part drives")
        }
        _ => None,
    }
}

/// The plan a repair answered, previewed or applied.
fn planned(report: &ApplyReport) -> &ResolvedPlan {
    match report {
        ApplyReport::Previewed { plan, .. } | ApplyReport::Applied { plan, .. } => plan,
        other => panic!("a repair answered {other:?}"),
    }
}

/// The provenance a plan carries.
fn provenance(plan: &ResolvedPlan) -> &Provenance {
    plan.provenance
        .as_ref()
        .expect("a repair's plan carries provenance")
}

/// The plan SQLite reports for each of `statements` on `store`, as the
/// testkit's plan of it.
fn plans_of(store: &Store, statements: &BTreeSet<String>) -> Vec<QueryPlan> {
    statements
        .iter()
        .map(|sql| {
            let explained = induced_failure::explain_out_of_band(store, sql)
                .unwrap_or_else(|error| panic!("explaining {sql}: {error:?}"));
            let rows = explained
                .steps
                .into_iter()
                .map(|step| PlanRow::new(step.id, step.parent, step.detail))
                .collect();
            QueryPlan::new(sql, rows)
        })
        .collect()
}

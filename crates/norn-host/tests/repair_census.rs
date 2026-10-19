//! **Every statement a repair runs is planned as an index search**: the
//! census that binds the `EXPLAIN` bars to what `Host::repair` actually runs.
//!
//! Each store statement family carries its own plan bars, and each family's
//! census holds every statement it names to a bar. What no family census can
//! say is that a repair runs only statements some family names. So this suite
//! records the text of every statement SQLite begins on the vault's store —
//! its writer and its readers — while a real host previews and applies
//! repairs, and takes the plan of each distinct one. The record reaches:
//!
//! - **every document-part shape, under every selection the batch read arms
//!   by.** A narrowing by one part of each filter shape the store enumerates
//!   (`norn_store::ReadFilter::all`) — an equality, an inequality, a
//!   membership of at most sixteen values and one of more, a presence and an
//!   absence, a range either way, a full-text match, a path part, a
//!   resolution, a tag, a finding and a links-to part — and by none, each
//!   under no selection, a rule, a rule and a severity, one kind, two kinds,
//!   and a severity, so every arm the batch read seeks, a rule's own rows
//!   among them, is in the record under every part. A resolution is a part a
//!   repair does not apply, refused once the batch read compiled it; what
//!   that ran is in the record too.
//! - **every drive the batch read settles on** (`Snapshot::drive_page`): a
//!   part keeping its documents in path order drives the batch from its
//!   source; excluding parts alone walk it, asking nothing; and a part keeping
//!   its documents in no path order — a range, a full-text match, a links-to
//!   part and the wider membership — drives it from what it matched at the
//!   first count, walks two rounds of its budget and then drives it from what
//!   it matched, walks two rounds to the ceiling and then on, unbounded, or
//!   fills it within a round's walk. Each narrowing's record is held to the
//!   drive it was arranged to take.
//! - **a document's tail and the read past it.** Each narrowing is a batch of
//!   one cut inside a document (the suite asserts the cut), so the cut
//!   document's tail and the read for more past it — which takes up the
//!   driver, or the round of the walk, the batch's first read settled on — are
//!   in the record; and each is continued from the cursor it minted. A
//!   continuation's page is the first page's text, its position a bound
//!   parameter, so what continuing adds to the record is any statement only a
//!   continuation begins, the walk a continuation past the matched documents'
//!   first takes among them.
//! - fixes whose planning reads what the snapshot holds: a route whose
//!   documents other documents link to, so the plan resolves the links it
//!   moves; a route whose cascade may respell a frontmatter link a rule reads
//!   by value, so the holder's frontmatter is read; a link finding the route
//!   resolves, judged by the stored link at its ordinal; and a bare value made
//!   a link, so its address is resolved; move-following through suffix and
//!   exact-path tombstones and live hash holders; a closest-heading anchor
//!   through the held-links holder and document headings; and a similarity
//!   tie through candidate document reads;
//! - each previewed, and batches applied under each drive a part keeping no
//!   path order takes and under a path part, so the applier's own judgment, its
//!   read of the pinned declaration and the changeset it commits are in the
//!   record too.
//!
//! What the record does not reach is a conjunction of two document parts, in
//! which one part drives and the page tests the other: a statement of that
//! shape is the drive's text with one more test, and the census narrows by
//! one part at a time.
//!
//! **The record is held to what it claims to reach.** The shapes it narrows by
//! are compared with the store's enumeration, and the drives it arranges each
//! under with the drives the batch read takes for the shape as the store
//! classes it; the two memberships with the widest one a page merges from its
//! source; the drive each narrowing's record shows with the one it was
//! arranged to take; and the statements it plans with every statement the
//! store began from the first repair to the last, so a statement begun
//! outside a repair's own record is found as well.
//!
//! **What the census holds every statement to** is the scan bar the family
//! bars share (`norn_testkit::explain`): no step reads a relation end to end,
//! a covering index included, or builds an automatic index, and no step sorts
//! into a temporary b-tree but the sorts the census admits by name and bound
//! ([`admitted_sort`]), nor scans but the one relation it admits
//! ([`admitted_scan`]). The plan is taken of the statement's own text
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

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::Path;

use norn_store::{
    FieldMatch, FieldOrder, MERGED_SOURCE_VALUES, ReadFilter, RepairAdvisoryStatement, Store,
    StoredPathOrder, SuffixKey, induced_failure,
};
use norn_testkit::explain::{Access, PlanRow, QueryPlan};
use norn_testkit::process::Sandbox;
use norn_testkit::wait::{Observed, wait_until};
use norn_wire::{
    ApplyMode, ApplyReport, Confidence, FindingKind, Predicate, Provenance, ReasonCode,
    RepairParams, ResolutionTarget, ResolvedPlan, Severity, VaultAddress,
};

/// The schema the census vault pins: a task rule with a default, a synonym
/// and a route, at `error`; a hub rule reading `up` by value, at `warning`;
/// and a `link` field a bare value is a type mismatch of.
/// A number field's unreadable synonym reaches a shaped raw membership seek.
const SCHEMA: &str = "version: 1\nfields:\n  ref: {type: link}\n  zz_syn: {type: number, shape: single}\nrules:\n  tasks:\n    severity: error\n    match: {frontmatter: {type: task}}\n    required:\n      status: {default: todo}\n    one_of:\n      status: {values: [todo, done], synonyms: {complete: done}}\n    allowed_paths: {paths: ['zz-repair/tasks/**'], route: 'zz-repair/tasks/'}\n  numbers:\n    one_of:\n      zz_syn: {values: ['1', '2'], synonyms: {one: '1'}}\n  hubs:\n    severity: warning\n    match: {frontmatter: {type: hub}}\n    exclude: {path: ['archive/**']}\n    max_length:\n      up: 40\n";

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
///   `zz-target.md`;
/// - `zzz-5c/` stands after the existing census subjects, holding a moved
///   document linked by both grammars, a closest-heading anchor and a
///   similarity tie.
const DOCUMENTS: [(&str, &[u8]); 15] = [
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
        b"---\ntype: hub\nup: \"[[zz-repair/loose/a]]\"\nzz-M: marker\n---\n",
    ),
    (
        "zz-holder.md",
        b"---\nref: zz-target\nzz_syn: one\n---\n# Holder\n",
    ),
    ("zz-target.md", b"---\nzz-M: marker\n---\n# Target\n"),
    ("zzz-5c/old.md", b"# Unique census followed document\n"),
    ("zzz-5c/holder-wiki.md", b"[[zz-repair/zzz-5c/old]]\n"),
    ("zzz-5c/holder-markdown.md", b"[old](old.md)\n"),
    ("zzz-5c/heading-target.md", b"# Important Heading\n"),
    (
        "zzz-5c/heading-holder.md",
        b"[[zz-repair/zzz-5c/heading-target#Important Headinh]]\n",
    ),
    ("zzz-5c/abcdefghij.md", b"# Census J\n"),
    ("zzz-5c/abcdefghik.md", b"# Census K\n"),
    ("zzz-5c/tie-holder.md", b"[[abcdefghix]]\n"),
];

/// How many documents the census stands under `n/`, each a task holding four
/// findings — two broken links, its missing `status` and its misplacement —
/// ahead of every `p/` document in path order: the rows a walk passes before
/// it reaches what a part over `p/` keeps, more than two rounds of its budget
/// hold on any arm, and a vault large enough that the ceiling a batch of one
/// stops its budget at stands past the first round's.
const NOISE: usize = 200;

/// How many documents the census stands under `p/`, each a task holding four
/// findings as an `n/` one does, past every `n/` one in path order.
const PARTED: usize = 40;

/// What a part over `p/` keeps for each drive it is arranged to take in a
/// batch of one, and the word the part names, in `p/` documents' bodies and
/// in the target under `t/` they link to. The batch reads two findings, so a
/// part keeping two drives it from its first count; its walk's first budget
/// is eight documents per finding, sixteen, and the ceiling the square root
/// of two findings times the vault's span, past eighteen and short of forty.
const KEPT: [(Reach, usize, &str); 3] = [
    (Reach::FirstCount, 2, "two"),
    (Reach::RoundsThenMatched, 18, "eighteen"),
    (Reach::PastCeiling, PARTED, "forty"),
];

/// The name of the `n/` document at `at`, which its `zz_m` holds too.
fn noise_name(at: usize) -> String {
    format!("n{at:03}")
}

/// The name of the `p/` document at `at`, which its `zz_n` holds too.
fn parted_name(at: usize) -> String {
    format!("p{at:02}")
}

/// The documents the census arranges beneath [`FOLDER`] beside
/// [`DOCUMENTS`]: [`NOISE`] tasks under `n/`, each with `zz_m` its own name,
/// the tag `census`, the key `noted`, the word `noiseword` and a link to
/// `t/noise`; [`PARTED`] tasks under `p/`, each with `zz_n` its own name,
/// `zz_r` counting down from `r99`, and for each row of [`KEPT`] that keeps
/// it, that row's word and a link to that row's target; and the targets.
fn arranged_documents() -> Vec<(String, String)> {
    let task = |fields: &str, body: &str| {
        format!("---\ntype: task\n{fields}---\nSee [[nowhere-a]] and [[nowhere-b]]. {body}\n")
    };
    let mut documents: Vec<(String, String)> = (0..NOISE)
        .map(|at| {
            (
                format!("n/{}.md", noise_name(at)),
                task(
                    &format!("zz_m: {}\ntags: [census]\nnoted: yes\n", noise_name(at)),
                    "noiseword [[zz-repair/t/noise]]",
                ),
            )
        })
        .collect();
    for at in 0..PARTED {
        let body: Vec<String> = KEPT
            .iter()
            .filter(|(_, kept, _)| at < *kept)
            .map(|(_, _, word)| format!("{word}word [[zz-repair/t/{word}]]"))
            .collect();
        documents.push((
            format!("p/{}.md", parted_name(at)),
            task(
                &format!("zz_n: {}\nzz_r: r{:02}\n", parted_name(at), 99 - at),
                &body.join(" "),
            ),
        ));
    }
    for (_, _, word) in KEPT {
        documents.push((format!("t/{word}.md"), format!("# {word}\n")));
    }
    documents.push(("t/noise.md".to_string(), "# noise\n".to_string()));
    documents
}

/// **Every statement a repair runs, previewed or applied, is planned as an
/// index search**, over every document-part shape, every drive of the batch
/// read and every arm it seeks, and every read its fixes' planning and the
/// applier's judgment make. It fails as well where its narrowings name other
/// shapes than the store enumerates, where a narrowing's record shows
/// another drive than it was arranged to take, and where the statements it
/// plans are not every statement the store began while the repairs ran.
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
    let arranged = DOCUMENTS
        .iter()
        .map(|(name, bytes)| ((*name).to_string(), bytes.to_vec()))
        .chain(
            arranged_documents()
                .into_iter()
                .map(|(name, text)| (name, text.into_bytes())),
        );
    for (name, bytes) in arranged {
        let at = vault.path().join(FOLDER).join(name);
        std::fs::create_dir_all(at.parent().expect("a parent folder")).expect("make the folder");
        std::fs::write(at, bytes).expect("write a document");
    }
    // The folder the route moves documents into stands before the vault is
    // attached. A create that makes a folder is reported back by the watcher
    // as a change of its own and reconciled after the repair answers, and the
    // statements that reconcile begins are no repair's.
    std::fs::create_dir_all(vault.path().join(FOLDER).join("tasks"))
        .expect("make the route's folder");
    std::fs::write(vault.path().join(CUT_DOCUMENT.0), CUT_DOCUMENT.1)
        .expect("write the cut document");
    let host = vault.host();
    let lease = attach::attach_and_wait(&host, vault.name());
    std::fs::rename(
        vault.path().join(FOLDER).join("zzz-5c/old.md"),
        vault.path().join(FOLDER).join("zzz-5c/new.md"),
    )
    .expect("move outside norn");
    wait_until(
        "the census move is derived before recording repairs",
        attach::state_budget(attach::READY_LIMIT),
        || {
            let mut store = vault.store();
            let snapshot = store.begin_request();
            let dead = snapshot
                .stored_tombstone(
                    &norn_store::DocumentPath::new("zz-repair/zzz-5c/old.md").expect("path"),
                )
                .expect("death");
            let live = snapshot
                .stored_document(
                    &norn_store::DocumentPath::new("zz-repair/zzz-5c/new.md").expect("path"),
                )
                .expect("live document");
            if dead.is_some() && live.is_some() {
                Observed::Met(())
            } else {
                Observed::pending(format!("{dead:?} {live:?}"))
            }
        },
    )
    .expect("the move settles");
    let attached = induced_failure::statements_begun();

    let ran = repairs_over_every_arm(&host, &VaultAddress::name(vault.name().clone()));
    // Whatever the store began after the last repair answered, while the
    // vault stayed attached: no repair's record holds it.
    let after = induced_failure::statements_begun();
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
    let health_head = plans
        .iter()
        .find(|plan| {
            plan.sql()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .starts_with("SELECT 0, j.key, dc.id, dc.path,")
        })
        .expect("a link-health head statement");
    let head_sort = health_head
        .rows()
        .iter()
        .find(|row| row.detail == "USE TEMP B-TREE FOR ORDER BY")
        .expect("the bounded head merge sorts");
    assert!(admitted_sort(health_head, head_sort).is_some());
    let count_only = QueryPlan::new(
        health_head.sql(),
        health_head
            .rows()
            .iter()
            .map(|row| {
                let detail = if row.searches() == Some("dl") {
                    row.detail.replace(
                        row.constraint().expect("an admitted run constraint"),
                        "(admitting_segments=?)",
                    )
                } else {
                    row.detail.clone()
                };
                PlanRow::new(row.id, row.parent, detail)
            })
            .collect(),
    );
    assert!(
        admitted_sort(&count_only, head_sort).is_none(),
        "a health head's count-only seek was admitted without its suffix bounds"
    );
    // The set the census plans is every statement the store began while the
    // repairs ran, and nothing besides: one begun outside a repair's own
    // record, or one the record holds and no plan was taken of, is a
    // statement no bar here judged.
    let begun: BTreeSet<String> = ran
        .iter()
        .chain(&after)
        .filter_map(|sql| judged(sql))
        .collect();
    let planned: BTreeSet<String> = plans.iter().map(|plan| plan.sql().to_string()).collect();
    // `judged` drops only the start of a trigger's program. Any other
    // statement it dropped would leave `begun` and `planned` alike, and the
    // difference below would not see it.
    let dropped: Vec<&String> = ran
        .iter()
        .chain(&after)
        .filter(|sql| judged(sql).is_none())
        .filter(|sql| !sql.starts_with("-- TRIGGER "))
        .collect();
    assert!(
        dropped.is_empty(),
        "the census drops a statement that is not a trigger's start: {dropped:?}"
    );
    // The statements nested in another are planned too: the full-text index's
    // configuration is read by the module, nested in a match.
    assert!(
        planned.contains("SELECT k, v FROM 'main'.'documents_fts_config'"),
        "the census planned no read of the full-text configuration: {planned:?}"
    );
    assert!(
        planned.contains("SELECT document FROM document_trigrams WHERE trigram = ?1 LIMIT ?2"),
        "the census planned no stem rarity read"
    );
    assert!(planned.iter().any(|sql| sql.split_whitespace().collect::<Vec<_>>().join(" ") == "SELECT d.path FROM document_trigrams AS t JOIN documents AS d ON d.id = t.document WHERE t.trigram = ?1"), "the census planned no stem postings read");
    let normalized: BTreeSet<String> = planned
        .iter()
        .map(|sql| sql.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    let (suffix_range, tombstone_at_path) = match store.path_order() {
        StoredPathOrder::Sensitive => (
            "suffix_key >= ?1 AND suffix_key < ?2",
            "SELECT path, last_content_hash, provenance FROM tombstones WHERE path = ?1 AND provenance <> 'quarantine'",
        ),
        StoredPathOrder::AsciiCaseInsensitive => (
            "folded_suffix_key >= ?1 AND folded_suffix_key < ?2",
            "SELECT path, last_content_hash, provenance FROM tombstones WHERE path = ?1 COLLATE NOCASE AND provenance <> 'quarantine' ORDER BY path LIMIT 2",
        ),
    };
    assert!(
        normalized.iter().any(|sql| sql
            .starts_with("SELECT path, last_content_hash, provenance FROM tombstones WHERE ")
            && sql.contains(suffix_range)
            && sql.contains("provenance <> 'quarantine'")
            && sql.ends_with("LIMIT 2")),
        "the census planned no TombstonesBySuffix read"
    );
    for (statement, sql) in [
        ("TombstoneAtPath", tombstone_at_path),
        (
            "LiveHolders",
            "SELECT path FROM documents WHERE content_hash = ?1 ORDER BY path LIMIT 2",
        ),
        (
            "held-links holder",
            "SELECT d.id, d.content_hash FROM documents AS d WHERE d.path = ?1",
        ),
        (
            "DocumentHeadings",
            "SELECT text, slug, level, span_line, span_column, span_offset, body_offset, inside_container, reading FROM headings WHERE document = ?1 ORDER BY ordinal",
        ),
        (
            "CandidateDocument",
            "SELECT id, path FROM documents WHERE path = ?1",
        ),
    ] {
        assert!(
            normalized.contains(sql),
            "the census planned no {statement} read"
        );
    }
    assert_eq!(
        begun.symmetric_difference(&planned).collect::<Vec<_>>(),
        Vec::<&String>::new(),
        "the statements the census plans are not the statements the repairs began"
    );
    let unbarred = census(&plans);
    assert!(
        unbarred.is_empty(),
        "a repair ran {} statement(s) no index search plans:\n\n{}",
        unbarred.len(),
        unbarred.join("\n\n")
    );
    for statement in RepairAdvisoryStatement::all() {
        assert!(
            plans
                .iter()
                .any(|plan| advisory_statement(plan.sql()) == Some(statement)),
            "the census reached no {statement:?} advisory statement"
        );
    }
    assert!(
        plans
            .iter()
            .any(|plan| { plan.sql().contains("fv.raw") && plan.sql().contains("fs.container =") }),
        "the synonym pass reached no shaped raw membership seek"
    );
    let excluding: Vec<_> = plans
        .iter()
        .filter(|plan| {
            plan.sql()
                .contains("NOT EXISTS (SELECT 1 FROM documents AS dg")
        })
        .collect();
    assert!(
        !excluding.is_empty(),
        "the advisory pass reached no excluding path filter"
    );
    for plan in excluding {
        let probe = QueryPlan::new(
            plan.sql(),
            plan.rows()
                .iter()
                .filter(|row| row.searches() == Some("dg"))
                .cloned()
                .collect(),
        );
        probe.assert_searches_through("documents", Access::RowId);
        probe.assert_search_constraint("documents", "(rowid=?)");
    }
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

/// The advisory statement family the recorded SQL runs, held to all five families.
fn advisory_statement(sql: &str) -> Option<RepairAdvisoryStatement> {
    if sql.starts_with("SELECT key FROM document_fields INDEXED BY document_fields_presence") {
        return Some(if sql.contains("key >=") {
            RepairAdvisoryStatement::FirstKey
        } else {
            RepairAdvisoryStatement::NextKey
        });
    }
    if sql.starts_with("SELECT value_type, document FROM document_fields") {
        return Some(RepairAdvisoryStatement::ValueTypes);
    }
    if sql.starts_with(
        "SELECT path, container FROM document_fields INDEXED BY document_fields_presence",
    ) && sql.split(" UNION ALL ").count() == 3
    {
        return Some(RepairAdvisoryStatement::FieldPresence);
    }
    if sql.starts_with("SELECT path FROM document_fields INDEXED BY document_fields_presence") {
        return Some(RepairAdvisoryStatement::SpellingPresence);
    }
    None
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
    // A document part driving the batch, from its source or from what it
    // matched: an equality, a range and a membership on the raw value, a
    // presence and a tag. A links-to part's link keys are the route's below.
    &["document_fields_raw"],
    &["document_fields_presence"],
    // Parsed scalar types read by the first-batch advisory pass.
    &["document_fields_value_type"],
    &["document_tags_folded_name"],
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
    // A links-to part's target: its class's head and total.
    &[
        "documents_admitted_suffix_key",
        "documents_admitted_folded_suffix_key",
    ],
    // The apply's changeset: its written links re-decided, and the rule sets
    // its findings cite.
    &["documents_change_feed"],
    // A moved document's death cascades into its stem trigrams by document.
    &["document_trigrams_document_trigram"],
    &["rule_sets_fingerprint_rules"],
];

/// How the batch read reaches the findings a narrowing selects, as it
/// settles that for a batch of one (`Snapshot::drive_page`).
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Reach {
    /// A part keeping its documents in path order drives the batch from its
    /// source, asking nothing.
    Ordered,
    /// No document part keeps what it seeks, so the batch walks the merged
    /// findings — within a path part's range, where one bounds it — testing
    /// each part at every document, asking nothing.
    Unasked,
    /// The first count finds a part keeping no more documents than the batch
    /// reads, which drives it from what it matched, walking nothing.
    FirstCount,
    /// The batch walks two rounds of its budget, each short of a finding the
    /// part keeps, and the count after the second finds the part within the
    /// ceiling, which drives the rest of the batch from what it matched.
    RoundsThenMatched,
    /// The batch walks two rounds to the ceiling and no count finds the part
    /// within it, so the batch walks on unbounded.
    PastCeiling,
    /// The batch fills within its first round's walk, and the read past a cut
    /// document takes that round up rather than ask again.
    FilledInRound,
}

impl Reach {
    /// Every drive the census declares.
    const ALL: [Reach; 6] = [
        Reach::Ordered,
        Reach::Unasked,
        Reach::FirstCount,
        Reach::RoundsThenMatched,
        Reach::PastCeiling,
        Reach::FilledInRound,
    ];

    /// The drive a repair's `record` shows its batch read took, or `None`
    /// where it shows none of them. Every statement is read off its spelling:
    /// a part's count opens `SELECT count(*) <=`; the vault's span is one
    /// fixed text; a window reads one path past an offset; and a merged page
    /// reads its documents from a part's source, from the documents a part
    /// matched by row id, or neither, testing each at its findings.
    fn of(record: &[String]) -> Option<Reach> {
        let (mut counts, mut spans, mut windows) = (0, 0, 0);
        let (mut sourced, mut matched) = (false, false);
        for sql in record {
            let sql = sql.split_whitespace().collect::<Vec<_>>().join(" ");
            if sql.starts_with("SELECT count(*) <=") {
                counts += 1;
            } else if sql == "SELECT coalesce(max(id), 0) FROM documents" {
                spans += 1;
            } else if (sql.starts_with("SELECT f.path FROM")
                || sql.starts_with("SELECT fr.path FROM"))
                && sql.contains(" OFFSET ")
            {
                windows += 1;
            } else if sql.starts_with("SELECT f.id, f.kind, f.severity, f.path,") {
                sourced |= sql.contains(") AS dv CROSS JOIN");
                matched |= sql.contains("FROM documents AS dv CROSS JOIN");
            }
        }
        match (counts, spans, windows, sourced, matched) {
            (0, 0, 0, true, false) => Some(Reach::Ordered),
            (0, 0, 0, false, false) => Some(Reach::Unasked),
            (1.., 0, 0, false, true) => Some(Reach::FirstCount),
            (3.., 1.., 2.., false, true) => Some(Reach::RoundsThenMatched),
            (3.., 1.., 2.., false, false) => Some(Reach::PastCeiling),
            (1.., 0, 1.., false, false) => Some(Reach::FilledInRound),
            _ => None,
        }
    }
}

/// One part a narrowing holds: its shape, the part, and the drive a batch of
/// one narrowed by it takes.
struct Part {
    shape: ReadFilter,
    part: Predicate,
    reach: Reach,
}

/// A part of each shape, each with the drive a batch of one narrowed by it is
/// arranged to take; and the widths of the two memberships.
///
/// Every part keeps documents the first of which in path order holds at
/// least two findings under every selection, so a batch of one cuts it: a
/// part keeping its documents in path order narrows to `n/` or to what the
/// cut document stands among, as an excluding part and a path part do. A
/// part keeping them in no path order narrows, for each row of [`KEPT`], to
/// what that row keeps of `p/`, and, to fill a batch within its first round,
/// to `n/`.
fn every_part() -> (Vec<Part>, [usize; 2]) {
    let target = |text: &str| ResolutionTarget::new(text).expect("a target");
    // The first `kept` names `name` gives, and names no document holds past
    // them, `width` values in all.
    let listed = |name: fn(usize) -> String, width: usize, kept: usize| -> Vec<String> {
        (0..width)
            .map(|at| match at < kept {
                true => name(at),
                false => format!("x{at:03}"),
            })
            .collect()
    };
    let narrow = listed(noise_name, 2, 2);
    let wide = MERGED_SOURCE_VALUES + 1;
    let mut parts = vec![
        Part {
            shape: ReadFilter::Equal(FieldMatch::RAW),
            part: Predicate::equal_to("type", "task"),
            reach: Reach::Ordered,
        },
        Part {
            shape: ReadFilter::Member(FieldMatch::RAW),
            part: Predicate::in_any("zz_m", narrow.clone()),
            reach: Reach::Ordered,
        },
        Part {
            shape: ReadFilter::Present,
            part: Predicate::has("noted"),
            reach: Reach::Ordered,
        },
        // A path part is no document part: it bounds every arm's seek to its
        // range, so the batch walks within it and asks nothing.
        Part {
            shape: ReadFilter::PathGlob(StoredPathOrder::Sensitive),
            part: Predicate::path(format!("{FOLDER}**")),
            reach: Reach::Unasked,
        },
        Part {
            shape: ReadFilter::Tag,
            part: Predicate::tag("census"),
            reach: Reach::Ordered,
        },
        Part {
            shape: ReadFilter::Finding,
            part: Predicate::has_finding(FindingKind::Broken),
            reach: Reach::Ordered,
        },
        Part {
            shape: ReadFilter::NotEqual(FieldMatch::RAW),
            part: Predicate::not_equal_to("type", "hub"),
            reach: Reach::Unasked,
        },
        Part {
            shape: ReadFilter::Absent,
            part: Predicate::missing("status"),
            reach: Reach::Unasked,
        },
        // Not applied by a repair, so nothing drives the read it compiles.
        Part {
            shape: ReadFilter::Resolves(SuffixKey::Raw),
            part: Predicate::resolves(target("zz-repair/t/two")),
            reach: Reach::Unasked,
        },
    ];
    for (reach, kept, word) in KEPT {
        parts.extend([
            Part {
                shape: ReadFilter::Before(FieldOrder::Raw),
                part: Predicate::before("zz_n", parted_name(kept)),
                reach,
            },
            Part {
                shape: ReadFilter::After(FieldOrder::Raw),
                part: Predicate::after("zz_r", format!("r{:02}", 99 - kept)),
                reach,
            },
            Part {
                shape: ReadFilter::FullText,
                part: Predicate::matches(format!("{word}word")),
                reach,
            },
            Part {
                shape: ReadFilter::LinksTo(SuffixKey::Raw),
                part: Predicate::links_to(target(&format!("zz-repair/t/{word}"))),
                reach,
            },
            Part {
                shape: ReadFilter::Member(FieldMatch::RAW),
                part: Predicate::in_any("zz_n", listed(parted_name, wide.max(kept), kept)),
                reach,
            },
        ]);
    }
    parts.extend(
        [
            (
                ReadFilter::Before(FieldOrder::Raw),
                Predicate::before("zz_m", "nz"),
            ),
            (
                ReadFilter::After(FieldOrder::Raw),
                Predicate::after("zz_m", "n"),
            ),
            (ReadFilter::FullText, Predicate::matches("noiseword")),
            (
                ReadFilter::LinksTo(SuffixKey::Raw),
                Predicate::links_to(target("zz-repair/t/noise")),
            ),
            (
                ReadFilter::Member(FieldMatch::RAW),
                Predicate::in_any("zz_m", listed(noise_name, wide, NOISE)),
            ),
        ]
        .map(|(shape, part)| Part {
            shape,
            part,
            reach: Reach::FilledInRound,
        }),
    );
    (parts, [narrow.len(), wide])
}

/// Preview a repair over every arm and every drive of the batch read through
/// `host`, then apply some, and hand back the text of every statement each
/// ran.
fn repairs_over_every_arm(host: &attach::ServingHost, address: &VaultAddress) -> Vec<String> {
    let repairing = |mode| RepairParams::new(address.clone(), mode);
    let ran = RefCell::new(Vec::new());
    // The statements the store began since the last were taken, kept in the
    // record and handed back.
    let taken = || {
        let record = induced_failure::statements_begun();
        ran.borrow_mut().extend(record.iter().cloned());
        record
    };
    let repair = |params: RepairParams| -> (ApplyReport, Vec<String>) {
        let report = host
            .repair(params.clone())
            .unwrap_or_else(|refused| panic!("{params:?} was refused: {refused:?}"))
            .wait()
            .unwrap_or_else(|refused| panic!("{params:?} answered no report: {refused:?}"))
            .report;
        (report, taken())
    };

    let (parts, [narrow, wide]) = every_part();
    let named: BTreeSet<usize> = parts.iter().map(|part| part.shape.slot()).collect();
    // Excluding path globs are in-process schema selectors, with no wire
    // predicate. The unnarrowed advisory pass reaches that filter below;
    // batch narrowing compares only shapes a repair request can name.
    let enumerated: BTreeSet<usize> = ReadFilter::all()
        .into_iter()
        .filter(|shape| !matches!(shape, ReadFilter::ExcludePathGlob(_)))
        .map(ReadFilter::slot)
        .collect();
    assert_eq!(
        named, enumerated,
        "the census narrows by other shapes than the store enumerates"
    );
    assert!(
        narrow <= MERGED_SOURCE_VALUES && wide > MERGED_SOURCE_VALUES,
        "the census's memberships are not one each side of the widest merged from a source: \
         {narrow} and {wide} values"
    );
    // Each shape is arranged under every drive the batch read takes for it,
    // as the store classes the shape: one keeping its documents in path
    // order drives from its source, a membership wider than a source merges
    // takes every drive one keeping no path order does, and an excluding
    // part, a path part — which bounds the arms' seeks and is no document
    // part — and a resolution, which a repair does not apply, walk unasked.
    let unordered = [
        Reach::FirstCount,
        Reach::RoundsThenMatched,
        Reach::PastCeiling,
        Reach::FilledInRound,
    ];
    let drives: BTreeSet<(usize, Reach)> = ReadFilter::all()
        .into_iter()
        .filter(|shape| !matches!(shape, ReadFilter::ExcludePathGlob(_)))
        .flat_map(|shape| {
            let reaches = match shape {
                ReadFilter::PathGlob(_) | ReadFilter::Resolves(_) => vec![Reach::Unasked],
                _ if shape.excludes() => vec![Reach::Unasked],
                ReadFilter::Member(_) => [Reach::Ordered].into_iter().chain(unordered).collect(),
                _ if shape.is_path_ordered(false) => vec![Reach::Ordered],
                _ => unordered.to_vec(),
            };
            reaches.into_iter().map(move |reach| (shape.slot(), reach))
        })
        .collect();
    let arranged: BTreeSet<(usize, Reach)> = parts
        .iter()
        .map(|part| (part.shape.slot(), part.reach))
        .collect();
    assert_eq!(
        arranged, drives,
        "the census arranges each shape under other drives than the batch read takes for it"
    );
    let declared: BTreeSet<Reach> = drives.iter().map(|(_, reach)| *reach).collect();
    assert_eq!(
        declared,
        BTreeSet::from(Reach::ALL),
        "the census arranges other drives than it declares"
    );

    // Each narrowing — by each part and by none, under each selection — is
    // run as a batch of one that cuts a document, so its tail and the read
    // past it run, then continued from the cursor it minted, so a statement
    // only a continuation begins runs too.
    let selections = [
        repairing(ApplyMode::Preview),
        repairing(ApplyMode::Preview).with_rule("tasks"),
        repairing(ApplyMode::Preview)
            .with_rule("tasks")
            .with_severity(Severity::Error),
        repairing(ApplyMode::Preview).with_kinds([FindingKind::Broken]),
        repairing(ApplyMode::Preview)
            .with_kinds([FindingKind::Misplaced, FindingKind::RequiredMissing]),
        repairing(ApplyMode::Preview).with_severity(Severity::Error),
    ];
    for selection in selections {
        let unnarrowed = std::iter::once((None, Reach::Unasked));
        let narrowed = parts.iter().map(|part| (Some(part), part.reach));
        for (part, reach) in unnarrowed.chain(narrowed) {
            let narrowing = match part {
                Some(part) => selection.clone().with_predicates([part.part.clone()]),
                None => selection.clone(),
            };
            let first = narrowing.clone().with_limit(1);
            if part.is_some_and(|part| matches!(part.shape, ReadFilter::Resolves(_))) {
                let refused = host
                    .repair(first.clone())
                    .and_then(|pending| pending.wait().map(|_| ()))
                    .expect_err("a repair does not apply a resolution");
                assert_eq!(
                    *refused.code(),
                    ReasonCode::RequestUnsatisfied,
                    "{first:?} was refused otherwise: {refused:?}"
                );
                assert_eq!(
                    Reach::of(&taken()),
                    Some(reach),
                    "{first:?} drove otherwise"
                );
                continue;
            }
            let (report, record) = repair(first);
            assert_eq!(
                Reach::of(&record),
                Some(reach),
                "a batch of one under {narrowing:?} drove otherwise"
            );
            assert!(
                findings_in(planned(&report)) > 1,
                "a batch of one cut no document under {narrowing:?}: {:?}",
                provenance(planned(&report))
            );
            let next = provenance(planned(&report))
                .cursor
                .clone()
                .expect("a batch of one leaves more to repair");
            repair(narrowing.with_limit(1).with_after(*next));
        }
    }
    for params in [
        repairing(ApplyMode::Preview),
        repairing(ApplyMode::Preview)
            .with_rule("hubs")
            .with_predicates([Predicate::path(format!("{FOLDER}**"))]),
    ] {
        repair(params);
    }

    for mode in [ApplyMode::Preview, ApplyMode::Apply] {
        repair(
            repairing(mode)
                .with_predicates([Predicate::path(format!("{FOLDER}zzz-5c/**"))])
                .with_threshold(Confidence::Suggested),
        );
    }

    // A batch of one applied under each drive a part keeping no path order
    // takes, narrowed by a range: each fixes the first document its range
    // keeps and moves it under the route's folder, past `p/` and `n/`, so
    // the next stands first, holding fixes of its own, and every other
    // document stands as it was arranged. Then a batch a path part drives.
    let ranged = |reach: Reach| {
        parts
            .iter()
            .find(|part| matches!(part.shape, ReadFilter::Before(_)) && part.reach == reach)
            .map(|part| part.part.clone())
            .expect("a range arranged for each drive")
    };
    let applied_under = [
        Reach::FilledInRound,
        Reach::FirstCount,
        Reach::RoundsThenMatched,
        Reach::PastCeiling,
    ]
    .map(|reach| {
        (
            repairing(ApplyMode::Apply)
                .with_predicates([ranged(reach)])
                .with_limit(1),
            reach,
        )
    });
    let beneath =
        repairing(ApplyMode::Apply).with_predicates([Predicate::path(format!("{FOLDER}**"))]);
    for (params, reach) in applied_under.into_iter().chain([(beneath, Reach::Unasked)]) {
        let (applied, record) = repair(params.clone());
        assert!(
            matches!(applied, ApplyReport::Applied { .. }),
            "the census's apply under {params:?} landed: {applied:?}"
        );
        assert!(
            !planned(&applied).operations.is_empty(),
            "the census's apply under {params:?} wrote something"
        );
        assert_eq!(
            Reach::of(&record),
            Some(reach),
            "an apply under {params:?} drove otherwise"
        );
    }
    ran.into_inner()
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

/// Every plan in `plans` that reads a relation end to end but the one
/// [`admitted_scan`] names, builds an automatic index, or sorts into a
/// temporary b-tree [`admitted_sort`] does not admit, each spelled with its
/// statement and its plan.
fn census(plans: &[QueryPlan]) -> Vec<String> {
    plans
        .iter()
        .filter_map(|plan| {
            let scans = plan.unbounded_steps();
            let unbounded: Vec<&PlanRow> = plan
                .rows()
                .iter()
                .filter(|row| {
                    (scans.contains(row) && admitted_scan(plan, row).is_none())
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

/// The statement whose bound admits `sort` in `plan`, and why the sort is
/// bounded by what that statement reads rather than by the vault; `None` for
/// a sort no bound admits. Each admission is held to the statements that
/// carry it, recognised by the select list they open with and the seek that
/// drives them, so a sort in any other statement is found.
///
/// - **A class's heads** (`ExplainedStatement::LinkHealthHeads`): each
///   admitting count's run is read in ladder order off the admitted-key
///   index, and the runs' union — at most a head per count the key's
///   segments reach — is sorted and cut again.
/// - **A page of a write's links** (`ExplainedStatement::LinkHealthWrittenLinks`):
///   read in path order off the change-feed index, which does not say a path
///   is one document's, so the links of one path, which the changeset wrote,
///   are sorted by ordinal.
/// - **One document's findings on a page a part's source drives (NORN-390).**
///   The merged page and a validate's kind or rule page a part keeping its
///   documents in the answer's path order narrows read the part's documents
///   in a co-routine in that order, from the page's place, and seek each
///   one's findings at its path; what SQLite orders is the last two terms,
///   position and id, among one document's findings, a run no longer than
///   the document's selected findings. Admitted only where a source drives
///   the statement (`CO-ROUTINE dv`).
/// - **A page the documents a part matched drive** (NORN-393). A merged page
///   reads the documents a part matched by row id (`SEARCH dv USING INTEGER
///   PRIMARY KEY`), seeks each one's findings of each kind at its path, and
///   sorts each kind's arm into the merge's order. The batch read drives a
///   page so only once the part's count found it keeping no more documents
///   than the cap it was asked at, which is never past the ceiling, so each
///   sort holds at most the findings of that many documents past the batch's
///   place — what `docs/architecture.md:876-878` (the first count, at the
///   rows the page reads), `:899-900` ("drives the rest of the page from its
///   seek and sorts what it matched") and `:919-921` (a part keeping at most
///   the ceiling costs about what it keeps) declare.
/// - **A part's count** (`FindStatement::PartCeiling`, NORN-393). The count
///   reads the distinct documents among its part's own seek, and the
///   `DISTINCT` b-tree holds each document once, the subquery's `LIMIT`
///   stopping the read one past the cap: at most the cap and one documents,
///   never past the ceiling's — `docs/architecture.md:896-898` ("the
///   distinct documents among the rows of the part's index seek alone that
///   stops one past its cap").
/// - **A summary a document part drives.** The summary
///   (`ValidateStatement::Summary` and its rule form) reads the documents a
///   part matched by row id and seeks their findings, so its `GROUP BY` is
///   sorted: it holds the selected findings of what the part matched. It runs
///   once per drain, on a first batch, whose count costs the selected set —
///   the declared limit at `docs/architecture.md:3041-3046` ("costs the
///   findings the selection admits").
/// - **A class's head** (`FindStatement::ClassHead`, a links-to part's
///   target). Each admitting-count run is cut in ladder order through the
///   admitted index, then the bounded heads are merged and cut again. The
///   merge sorts at most one head per count and range, never the class.
fn admitted_sort(plan: &QueryPlan, sort: &PlanRow) -> Option<&'static str> {
    let admitted_runs = || {
        let runs: Vec<&PlanRow> = plan
            .rows()
            .iter()
            .filter(|row| row.searches() == Some("dl") || row.scans() == Some("dl"))
            .collect();
        !runs.is_empty()
            && runs.iter().all(|row| {
                matches!(
                    (row.index(), row.constraint()),
                    (
                        Some("documents_admitted_suffix_key"),
                        Some("(admitting_segments=? AND suffix_key>? AND suffix_key<?)")
                    ) | (
                        Some("documents_admitted_folded_suffix_key"),
                        Some(
                            "(admitting_segments=? AND folded_suffix_key>? AND folded_suffix_key<?)"
                        )
                    )
                )
            })
    };
    let searched_through = |table: &str, indexes: &[&str]| {
        plan.searches_of(table)
            .iter()
            .any(|row| row.index().is_some_and(|index| indexes.contains(&index)))
    };
    let opens_with = |select_list: &str| {
        let spelled: Vec<&str> = plan.sql().split_whitespace().collect();
        spelled.join(" ").starts_with(select_list)
    };
    let a_page = opens_with("SELECT f.id, f.kind, f.severity, f.path,");
    let sourced = plan.rows().iter().any(|row| row.detail == "CO-ROUTINE dv");
    let matched = plan
        .rows()
        .iter()
        .any(|row| row.detail == "SEARCH dv USING INTEGER PRIMARY KEY (rowid=?)");
    let a_summary = opens_with("SELECT f.kind, f.severity, COUNT(*)")
        || opens_with("SELECT fr.kind, fr.severity, COUNT(*)");
    let a_count = opens_with("SELECT count(*) <=");
    let a_class_head = opens_with("SELECT dr.id, dr.path FROM (");
    let a_health_head = opens_with("SELECT 0, j.key, dc.id, dc.path,");
    let a_suffix_head = opens_with("SELECT j.key, dc.id FROM json_each(?1) AS j");
    match sort.detail.as_str() {
        "USE TEMP B-TREE FOR ORDER BY" if (a_health_head || a_suffix_head) && admitted_runs() => {
            Some("bounded admitted-run heads")
        }
        "USE TEMP B-TREE FOR LAST TERM OF ORDER BY"
            if searched_through("documents", &["documents_change_feed"]) =>
        {
            Some("LinkHealthWrittenLinks")
        }
        "USE TEMP B-TREE FOR LAST 2 TERMS OF ORDER BY" if sourced && a_page => {
            Some("one document's findings on a page a part's source drives")
        }
        "USE TEMP B-TREE FOR ORDER BY" if matched && !sourced && a_page => {
            Some("a page the documents a part matched drive")
        }
        "USE TEMP B-TREE FOR DISTINCT" if a_count => Some("a part's count"),
        "USE TEMP B-TREE FOR GROUP BY" if matched && a_summary => {
            Some("a summary a document part drives")
        }
        "USE TEMP B-TREE FOR ORDER BY" if a_class_head && admitted_runs() => Some("a class's head"),
        _ => None,
    }
}

/// The statement whose bound admits `scan` of a whole relation in `plan`;
/// `None` for a scan nothing admits.
///
/// - **The full-text index's configuration.** Loading the index, FTS5 reads
///   every row of `documents_fts_config` by its own statement; the store sets
///   no option there, so it holds the module's version row alone, whatever
///   the vault holds — `docs/architecture.md:1193-1195`. Admitted in that
///   statement only, run by the module or nested in a match.
fn admitted_scan(plan: &QueryPlan, scan: &PlanRow) -> Option<&'static str> {
    (plan.sql() == "SELECT k, v FROM 'main'.'documents_fts_config'"
        && scan.detail == "SCAN main.documents_fts_config")
        .then_some("the full-text index's configuration")
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

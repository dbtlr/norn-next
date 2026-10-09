//! The repair batch read: the selected findings merged into one path order,
//! paged by document, and the plan and work bars its statements are judged by.
//!
//! The fixtures are the validate suite's. Where it pages one kind after
//! another, a batch pages the same findings in path order and never splits a
//! document's selected findings, so a drain of batches answers exactly the
//! findings a drain of pages does, in another order.

use std::collections::BTreeSet;

use super::{
    KIND_INDEX, KIND_RANGE_SEEK, KIND_SEEK, ROOTS, RULE_KIND_INDEX, RULE_KIND_SEEK,
    RULE_SEVERITY_INDEX, RULE_SEVERITY_SEEK, SEVERITY_INDEX, SEVERITY_SEEK, VALIDATE_SCHEMA,
    Validating, citing, declared, finding, names, plans_of, requests, rewritten, rows_of,
    rule_requests, ruling, undeclared, validating, vault,
};
use crate::common::violation;
use crate::find::failure_of;
use norn_store::{
    PageRefusal, RepairBatch, RepairSelection, StoredPathOrder, ValidatePlan, ValidateStatement,
    ValidateWork,
};
use norn_testkit::explain::{Access, QueryPlan};
use norn_wire::{
    ApplyMode, Cursor, CursorKey, Direction, FindingKind, FindingRow, Moved, PagedRows, Predicate,
    RepairParams, Severity, Sort, SortKey, ValidateParams,
};

// ---- fixtures ----

/// A repair of everything, from the first document.
fn repairing() -> RepairParams {
    RepairParams::new(vault(), ApplyMode::Preview)
}

/// The repair that selects what `selection` validates: the same predicates,
/// kinds, severity floor and rule.
pub(super) fn selecting(selection: &ValidateParams) -> RepairParams {
    let mut params = repairing()
        .with_predicates(selection.predicates.clone())
        .with_kinds(selection.kinds.clone());
    if let Some(floor) = selection.severity {
        params = params.with_severity(floor);
    }
    if let Some(rule) = &selection.rule {
        params = params.with_rule(rule.clone());
    }
    params
}

impl Validating {
    /// The batch `params` selects.
    fn batch(&self, params: &RepairParams) -> RepairBatch {
        self.snapshot()
            .repair_batch(&RepairSelection::from(params), &declared())
            .unwrap_or_else(|refusal| panic!("a batch of {params:?}: {refusal}"))
    }

    /// Why a batch of `params` is refused.
    fn refused(&self, params: &RepairParams) -> PageRefusal {
        self.snapshot()
            .repair_batch(&RepairSelection::from(params), &declared())
            .expect_err("the batch is refused")
    }

    pub(super) fn batch_plans(&self, params: &RepairParams) -> Vec<ValidatePlan> {
        self.snapshot()
            .repair_batch_plans(&RepairSelection::from(params), &declared())
            .expect("the plans of a batch")
    }
}

/// Every batch `params` selects, drained `limit` findings at a time, each
/// batch's findings in the order it answered them.
fn drained(store: &Validating, params: &RepairParams, limit: u32) -> Vec<Vec<FindingRow>> {
    let mut batches = Vec::new();
    let mut after: Option<Cursor> = None;
    loop {
        let mut request = params.clone().with_limit(limit);
        if let Some(cursor) = after.take() {
            request = request.with_after(cursor);
        }
        let batch = store.batch(&request);
        batches.push(batch.rows);
        match batch.next {
            Some(next) => after = Some(next),
            None => return batches,
        }
        assert!(batches.len() < 100, "the drain does not end");
    }
}

/// The paths a batch covers, in order, each once.
fn documents(rows: &[FindingRow]) -> Vec<&str> {
    let mut paths: Vec<&str> = rows.iter().map(|row| row.path.as_str()).collect();
    paths.dedup();
    paths
}

/// Where a path stands in the answer order, as this test computes it.
fn place(path: &str) -> (String, String) {
    (path.to_ascii_lowercase(), path.to_string())
}

/// **The fixture's findings in path order**: `a.md`'s three standing by id
/// across two kinds, then the other documents.
fn every_finding_by_path() -> Vec<(FindingKind, String, Option<String>)> {
    vec![
        finding(FindingKind::PathNamesNoDocument, "a.md", Some("v1.2")),
        finding(FindingKind::UndeclaredTag, "a.md", Some("draft")),
        finding(FindingKind::UndeclaredTag, "a.md", Some("idea")),
        finding(FindingKind::UndeclaredTag, "b.md", Some("other")),
        finding(FindingKind::BodyBytesNotUtf8, "broken.md", None),
        finding(
            FindingKind::PathNamesNoDocument,
            "notes/c.md",
            Some("glossary"),
        ),
        finding(FindingKind::FrontmatterUnreadable, "notes/d.md", None),
    ]
}

// ---- what a batch answers ----

/// **A batch answers the findings of every kind merged into one path order**,
/// not one kind after another: `a.md`'s path-names-no-document finding stands
/// ahead of its undeclared tags, and `b.md`'s undeclared tag ahead of
/// `broken.md`'s body finding, which a validate answers a kind earlier.
#[test]
fn a_batch_answers_the_kinds_merged_in_path_order() {
    let store = Validating::new("batch-merged");
    let batch = store.batch(&repairing());
    assert_eq!(names(&batch.rows), every_finding_by_path());
    assert_eq!(batch.next, None);
    assert_ne!(
        names(&batch.rows),
        names(&store.rows(&validating())),
        "a validate pages the kinds one after another"
    );
    assert!(batch.unsatisfied.is_empty());
    assert_eq!(
        batch.snapshot.schema_fingerprint, None,
        "a repair cursor is minted under no fingerprint"
    );
}

/// **A limit that falls inside a document extends the batch through the
/// document's last selected finding**, and the next batch starts at the next
/// document. `a.md` holds three findings, of two kinds: a limit of one or two
/// ends inside it, and a limit of three ends on its last finding.
#[test]
fn a_limit_inside_a_document_extends_the_batch_through_its_last_finding() {
    let store = Validating::new("batch-boundary");
    for limit in [1, 2, 3] {
        let batch = store.batch(&repairing().with_limit(limit));
        assert_eq!(
            names(&batch.rows),
            every_finding_by_path()[..3].to_vec(),
            "a batch of {limit} ends with its document"
        );
        let next = batch.next.expect("more documents remain");
        assert_eq!(next.key(), &CursorKey::repair_document("a.md"));
        let continued = store.batch(&repairing().with_limit(limit).with_after(next));
        assert_eq!(
            names(&continued.rows)[0],
            every_finding_by_path()[3],
            "the next batch of {limit} starts at the next document"
        );
    }
    let batch = store.batch(&repairing().with_limit(4));
    assert_eq!(names(&batch.rows), every_finding_by_path()[..4].to_vec());
    assert_eq!(
        batch.next.as_ref().map(Cursor::key),
        Some(&CursorKey::repair_document("b.md")),
        "a batch's cursor names the last document it covered"
    );
}

/// **A batch is cut only between documents**, and only as far past the limit
/// as its last document runs. Drained at every limit, no document appears in
/// two batches, the batches stand in path order, and each batch but the last
/// holds at least its limit of findings while the findings before its last
/// document hold fewer.
#[test]
fn a_drain_never_splits_a_document_and_extends_only_through_the_boundary_document() {
    let mut store = Validating::new("batch-whole-documents");
    for tag in ["one", "two", "three", "four"] {
        store.stand(&undeclared("notes/z.md", tag));
    }
    // Findings about links stand after a document's own, by the link's
    // ordinal, whatever their kinds: a cut inside `a.md` resumes between them.
    for (kind, ordinal, target) in [
        (FindingKind::Broken, Some(3), "three"),
        (FindingKind::Broken, None, "document"),
        (FindingKind::Broken, Some(1), "link-one"),
        (FindingKind::MissingAnchor, Some(2), "link-two"),
    ] {
        let mut finding = undeclared("a.md", target);
        finding.kind = kind;
        finding.ordinal = ordinal;
        store.stand(&finding);
    }
    let whole = store.batch(&repairing().with_limit(1000)).rows;
    let at_a: Vec<_> = whole
        .iter()
        .filter(|row| row.path.as_str() == "a.md")
        .map(|row| row.target.as_deref().expect("a target"))
        .collect();
    assert_eq!(
        at_a,
        [
            "v1.2", "draft", "idea", "document", "link-one", "link-two", "three"
        ],
        "a document's findings by position, then id, across kinds"
    );
    for limit in 1..=14 {
        let batches = drained(&store, &repairing(), limit);
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for (at, rows) in batches.iter().enumerate() {
            let paths = documents(rows);
            assert!(!paths.is_empty(), "batch {at} of {limit} is empty");
            for path in &paths {
                assert!(
                    seen.insert((*path).to_string()),
                    "{path} is in two batches of {limit}"
                );
            }
            let last = rows.last().expect("a batch holds findings").path.as_str();
            let before = rows.iter().filter(|row| row.path.as_str() != last).count();
            assert!(
                before < limit as usize,
                "batch {at} of {limit} ran past its boundary document"
            );
            if at + 1 < batches.len() {
                assert!(
                    rows.len() >= limit as usize,
                    "batch {at} of {limit} stopped short"
                );
            }
        }
        let drained_rows: Vec<FindingRow> = batches.into_iter().flatten().collect();
        assert_eq!(drained_rows, whole, "a drain of {limit}");
    }
}

/// **A cursor resumes strictly after its path**, bytewise: on a root that
/// tells spellings apart `A.md` and `a.md` fold together and stand adjacent
/// in the answer order, but are two documents. A batch ending at `A.md` is
/// followed by one beginning at `a.md`, and nothing is repeated or skipped.
#[test]
fn a_cursor_resumes_strictly_after_its_path_between_two_spellings_that_fold_together() {
    let mut store = Validating::under("batch-spellings", StoredPathOrder::Sensitive);
    store.stand(&violation("A.md"));
    let whole = store.batch(&repairing().with_limit(1000));
    assert_eq!(
        documents(&whole.rows),
        [
            "A.md",
            "a.md",
            "b.md",
            "broken.md",
            "notes/c.md",
            "notes/d.md"
        ]
    );

    let first = store.batch(&repairing().with_limit(1));
    assert_eq!(
        documents(&first.rows),
        ["A.md"],
        "`a.md` is another document"
    );
    let next = first.next.expect("more remain");
    assert_eq!(next.key(), &CursorKey::repair_document("A.md"));
    let second = store.batch(&repairing().with_limit(1).with_after(next));
    assert_eq!(
        documents(&second.rows),
        ["a.md"],
        "the batch after `A.md` begins at `a.md`"
    );
    assert_eq!(second.rows.len(), 3, "`a.md` is read whole");

    let after_lower = store.batch(&repairing().with_after(Cursor::new(
        second.snapshot.clone(),
        CursorKey::repair_document("a.md"),
    )));
    assert_eq!(
        documents(&after_lower.rows)[0],
        "b.md",
        "a cursor at `a.md` does not return `A.md`, which stands before it"
    );
    for limit in [1, 2, 3, 4] {
        let rows: Vec<FindingRow> = drained(&store, &repairing(), limit)
            .into_iter()
            .flatten()
            .collect();
        assert_eq!(rows, whole.rows, "drained {limit} at a time");
    }
}

/// **A batch ends between two spellings that fold together** rather than
/// treating them as one document: with the limit landing on `A.md`'s last
/// finding, `a.md`'s findings are not pulled into the batch.
#[test]
fn a_batch_does_not_take_the_next_spelling_for_the_same_document() {
    let mut store = Validating::under("batch-spelling-boundary", StoredPathOrder::Sensitive);
    store.stand(&violation("A.md"));
    store.stand(&undeclared("A.md", "second"));
    let batch = store.batch(&repairing().with_limit(1));
    assert_eq!(documents(&batch.rows), ["A.md"]);
    assert_eq!(batch.rows.len(), 2);
    let batch = store.batch(&repairing().with_limit(2));
    assert_eq!(documents(&batch.rows), ["A.md"]);
    assert_eq!(
        batch.next.as_ref().map(Cursor::key),
        Some(&CursorKey::repair_document("A.md"))
    );
}

/// **A drain of batches answers exactly the findings an unpaged validate
/// selects, each once**, whatever the batch limit and under every selection
/// the validate suite drains: kinds, a severity floor, path parts, document
/// parts, and a rule alone and composed. They are compared as sets, since the
/// two read in different orders, and each batch stands in path order.
#[test]
fn a_drain_of_batches_answers_the_findings_a_validate_selects_once_each() {
    for order in ROOTS {
        let store = Validating::with_rules_under("batch-drain", 0, order);
        for selection in requests().into_iter().chain(rule_requests()) {
            let mut expected: Vec<FindingRow> = store.rows(&selection.clone().with_limit(1000));
            expected.sort_by_key(|row| row.id);
            for limit in [1, 2, 3, 5] {
                let batches = drained(&store, &selecting(&selection), limit);
                let rows: Vec<FindingRow> = batches.into_iter().flatten().collect();
                let ids: BTreeSet<u64> = rows.iter().map(|row| row.id).collect();
                assert_eq!(
                    ids.len(),
                    rows.len(),
                    "{selection:?} drained {limit} at a time repeated a finding under {order:?}"
                );
                assert!(
                    rows.windows(2).all(|pair| {
                        place(pair[0].path.as_str()) <= place(pair[1].path.as_str())
                    }),
                    "{selection:?} drained {limit} at a time left path order under {order:?}"
                );
                let mut drained_rows = rows;
                drained_rows.sort_by_key(|row| row.id);
                assert_eq!(
                    drained_rows, expected,
                    "{selection:?} drained {limit} at a time under {order:?}"
                );
            }
        }
    }
}

/// **A batch answers no next cursor when nothing remains**, including when the
/// limit lands exactly on the last document's last finding, and when it lands
/// inside that document, whose tail the batch then takes.
#[test]
fn a_batch_has_no_next_cursor_when_nothing_remains() {
    let mut store = Validating::new("batch-last");
    for tag in ["one", "two", "three"] {
        store.stand(&undeclared("notes/z.md", tag));
    }
    let total = every_finding_by_path().len() + 3;
    for limit in [total, total + 1, total - 1, total - 2] {
        let batch = store.batch(&repairing().with_limit(limit as u32));
        assert_eq!(batch.rows.len(), total, "a limit of {limit}");
        assert_eq!(batch.next, None, "a limit of {limit}");
    }
    let batch = store.batch(&repairing().with_limit((total - 3) as u32));
    assert_eq!(
        batch.rows.len(),
        total - 3,
        "the limit lands between documents"
    );
    assert_eq!(
        batch.next.as_ref().map(Cursor::key),
        Some(&CursorKey::repair_document("notes/d.md"))
    );
    let batch = store.batch(&repairing().with_limit(1).with_after(Cursor::new(
        batch.snapshot.clone(),
        CursorKey::repair_document("notes/z.md"),
    )));
    assert!(
        batch.rows.is_empty(),
        "nothing stands after the last document"
    );
    assert_eq!(batch.next, None);
}

/// **A selection that matches nothing answers an empty batch and runs no
/// merged statement.** A part on a key outside the field universe admits no
/// finding and is reported, as a validate reports it.
#[test]
fn a_selection_matching_nothing_answers_an_empty_batch_and_reads_no_findings() {
    let store = Validating::new("batch-nothing");
    let params = repairing().with_predicates([Predicate::equal_to("stauts", "open")]);
    let batch = store.batch(&params);
    assert!(batch.rows.is_empty());
    assert_eq!(batch.next, None);
    assert_eq!(batch.unsatisfied.len(), 1);
    assert_eq!(batch.work.rows_read, 0);
    for plan in store.batch_plans(&params) {
        assert!(
            !matches!(
                plan.statement,
                norn_store::ReadStatement::Validate(
                    ValidateStatement::MergedPage | ValidateStatement::DocumentTail
                )
            ),
            "a merged statement ran: {plan:?}"
        );
    }
    let none = store.batch(&repairing().with_predicates([Predicate::path("nowhere/**")]));
    assert!(none.rows.is_empty());
    assert_eq!(none.next, None);
}

/// **A rule selects the findings citing it off the rule's own rows**, merged
/// into path order across kinds, and the batch carries the rule sets its rows
/// cite as a validate does.
#[test]
fn a_rule_selects_the_findings_citing_it_in_path_order_with_their_rule_sets() {
    let store = ruling("batch-rule");
    let batch = store.batch(&repairing().with_rule("tasks"));
    assert_eq!(
        names(&batch.rows),
        vec![
            finding(FindingKind::NotOneOf, "a.md", Some("status")),
            finding(FindingKind::NotOneOf, "a.md", Some("status")),
            finding(FindingKind::RequiredMissing, "b.md", Some("due")),
            finding(FindingKind::TooLong, "notes/c.md", Some("title")),
        ]
    );
    let (rows, rule_sets) = store.page_citing(&citing("tasks"));
    assert_eq!(batch.rule_sets, rule_sets);
    assert!(!batch.rule_sets.is_empty());
    let mut by_id = batch.rows.clone();
    by_id.sort_by_key(|row| row.id);
    let mut expected = rows;
    expected.sort_by_key(|row| row.id);
    assert_eq!(by_id, expected);

    let open = store.batch(&repairing().with_rule("open"));
    assert_eq!(
        documents(&open.rows),
        ["a.md", "notes/c.md"],
        "the rule `open` is cited at two documents"
    );
}

/// **Severity, kinds and a path part narrow a batch as they narrow a
/// validate**, and a document part keeps the findings of the documents it
/// matches.
#[test]
fn a_severity_kinds_and_parts_narrow_a_batch_as_they_narrow_a_validate() {
    let store = Validating::new("batch-narrow");
    let rows = |params: RepairParams| names(&store.batch(&params).rows);
    let all = every_finding_by_path();
    assert_eq!(
        rows(repairing().with_severity(Severity::Error)),
        vec![all[4].clone(), all[6].clone()]
    );
    assert_eq!(rows(repairing().with_severity(Severity::Warning)), all);
    assert_eq!(
        rows(repairing().with_kinds([FindingKind::UndeclaredTag])),
        all[1..4].to_vec()
    );
    assert_eq!(
        rows(
            repairing()
                .with_kinds([FindingKind::UndeclaredTag, FindingKind::BodyBytesNotUtf8])
                .with_severity(Severity::Error)
        ),
        vec![all[4].clone()]
    );
    assert_eq!(
        rows(repairing().with_predicates([Predicate::path("notes/**")])),
        all[5..].to_vec()
    );
    assert_eq!(
        rows(repairing().with_predicates([Predicate::equal_to("status", "open")])),
        vec![
            all[0].clone(),
            all[1].clone(),
            all[2].clone(),
            all[5].clone()
        ]
    );
}

/// **A batch judges what moved since its cursor was minted**, as a validate
/// does: a write after the cursor reports the generation, and the continuing
/// batch still resumes after the cursor's path.
#[test]
fn a_continuation_reports_what_moved_since_its_cursor() {
    let mut store = Validating::new("batch-moved");
    let first = store.batch(&repairing().with_limit(1));
    assert!(first.moved.is_empty());
    let next = first.next.expect("more remain");
    store.stand(&violation("zzz.md"));
    let continued = store.batch(&repairing().with_limit(1).with_after(next));
    assert_eq!(continued.moved, vec![Moved::Generation]);
    assert_eq!(documents(&continued.rows), ["b.md"]);
}

// ---- refusals ----

/// **A batch refuses what a validate refuses**: a limit outside the page
/// bound, a rule the declaration does not declare, and a declaration the
/// snapshot does not pin.
#[test]
fn a_batch_is_refused_as_a_validate_is() {
    let store = Validating::new("batch-refused");
    for limit in [0, u32::MAX] {
        assert!(
            matches!(
                store.refused(&repairing().with_limit(limit)),
                PageRefusal::OutOfBound { .. }
            ),
            "a limit of {limit}"
        );
    }
    assert_eq!(
        store.refused(&repairing().with_rule("nonesuch")),
        PageRefusal::UnknownRule {
            rule: "nonesuch".to_string()
        }
    );
    assert_eq!(
        store
            .snapshot()
            .repair_batch(
                &RepairSelection::from(&repairing()),
                &norn_store::ContentModel::under("another-schema"),
            )
            .expect_err("the declaration is not the pinned one"),
        PageRefusal::DeclarationNotPinned {
            declared_under: Some("another-schema".to_string()),
            pinned: Some(VALIDATE_SCHEMA.to_string()),
        }
    );
}

/// **A cursor that is not a repair's is refused on a batch, and a repair
/// cursor is refused on a validate**: a finding's, a document's and a
/// document finding's name no position among a repair's documents, and a
/// repair cursor carrying a fingerprint, which no batch mints, is not taken.
#[test]
fn a_batch_takes_no_cursor_but_its_own_and_a_validate_takes_no_repair_cursor() {
    let store = Validating::new("batch-cursors");
    let reading = store.batch(&repairing()).snapshot;
    for (key, rows) in [
        (
            CursorKey::finding(FindingKind::UndeclaredTag, "a.md", None, 1),
            PagedRows::Finding,
        ),
        (
            CursorKey::document(
                Sort::new(SortKey::path(), Direction::Ascending),
                None,
                "a.md",
            ),
            PagedRows::Document,
        ),
        (
            CursorKey::document_finding("a.md", None, FindingKind::UndeclaredTag, 1),
            PagedRows::DocumentFinding,
        ),
    ] {
        assert_eq!(
            store.refused(&repairing().with_after(Cursor::new(reading.clone(), key))),
            PageRefusal::CursorNotTaken {
                cursor: rows,
                paged: PagedRows::RepairDocument,
            }
        );
    }
    let forged = Cursor::new(
        norn_wire::Snapshot::new(
            reading.epoch.clone(),
            reading.generation,
            Some(VALIDATE_SCHEMA.to_string()),
            None,
        ),
        CursorKey::repair_document("a.md"),
    );
    assert_eq!(
        store.refused(&repairing().with_after(forged)),
        PageRefusal::CursorNotTaken {
            cursor: PagedRows::RepairDocument,
            paged: PagedRows::RepairDocument,
        }
    );
    let minted = store
        .batch(&repairing().with_limit(1))
        .next
        .expect("more remain");
    assert_eq!(
        store
            .snapshot()
            .validate(&validating().with_after(minted), &declared())
            .expect_err("a repair cursor is no validate's"),
        PageRefusal::CursorNotTaken {
            cursor: PagedRows::RepairDocument,
            paged: PagedRows::Finding,
        }
    );
    assert_eq!(
        PageRefusal::CursorNotTaken {
            cursor: PagedRows::Finding,
            paged: PagedRows::RepairDocument,
        }
        .to_string(),
        "the cursor names a position among findings, and the request pages a repair's documents"
    );
}

// ---- the plan bars ----

/// The tail of one document, from a finding's position and id on: the
/// equality on both path columns, then the row-value seek past them.
const TAIL_SEEK: &str = "(vault_schema_fingerprint=? AND kind=? AND path=? AND path=? AND \
     (position,rowid)>(?,?))";

/// That seek at one severity.
const TAIL_SEVERITY_SEEK: &str = "(vault_schema_fingerprint=? AND kind=? AND severity=? AND \
     path=? AND path=? AND (position,rowid)>(?,?))";

/// A rule's tail of one document.
const RULE_TAIL_SEEK: &str = "(vault_schema_fingerprint=? AND rule=? AND kind=? AND path=? AND \
     path=? AND (position,finding)>(?,?))";

/// A rule's tail of one document at one severity.
const RULE_TAIL_SEVERITY_SEEK: &str = "(vault_schema_fingerprint=? AND rule=? AND kind=? AND \
     severity=? AND path=? AND path=? AND (position,finding)>(?,?))";

/// The same seek bounded above by a path part's folded range.
const RULE_RANGE_SEEK: &str = "(vault_schema_fingerprint=? AND rule=? AND kind=? AND \
     (path,path,position,finding)>(?,?,?,?) AND path<?)";

/// Judge a merged statement no document part drives: `arms` seeks of `index`
/// under `constraint` on `table` (aliased `alias`), merged where more than one
/// kind is read, with nothing read end to end and nothing sorted.
fn judge_merge(
    statement: &QueryPlan,
    (table, alias): (&str, &str),
    index: &str,
    constraint: &str,
    arms: usize,
) {
    statement.assert_no_full_scan();
    statement.assert_no_temp_btree();
    let merged = statement
        .rows()
        .iter()
        .any(|row| row.detail == "MERGE (UNION ALL)");
    assert_eq!(
        merged,
        arms > 1,
        "{arms} arms: {:?}\nemitted SQL: {}",
        statement.rows(),
        statement.sql()
    );
    let seeks = rows_of(statement, alias);
    seeks.assert_searches_through(table, Access::Index(index));
    seeks.assert_search_constraint(table, constraint);
    assert_eq!(
        seeks.searches_of(table).len(),
        arms,
        "one seek per kind read: {:?}\nemitted SQL: {}",
        statement.rows(),
        statement.sql()
    );
}

/// The kinds a request names, or all, as the registry counts them.
fn kinds_in(params: &RepairParams) -> usize {
    if params.kinds.is_empty() {
        FindingKind::ALL.len()
    } else {
        params.kinds.len()
    }
}

/// One request and the seeks its merged statements carry: the table and alias
/// the arms read, the index they seek, and the constraint of a merged page
/// and of a document's tail.
struct Seeks {
    params: RepairParams,
    table: (&'static str, &'static str),
    index: &'static str,
    page: &'static str,
    tail: &'static str,
}

const FINDINGS: (&str, &str) = ("findings", "f");
const RULES: (&str, &str) = ("finding_rules", "fr");

/// The requests the bars judge, each reading a document of several findings
/// first so that a batch of one is cut inside it: no narrowing and each
/// severity floor, a rule alone and with a floor, and one kind, which is one
/// arm and no merge. A request admitting every severity names none, so the
/// severity list is never several: one severity is the narrowest floor.
fn seeks() -> Vec<Seeks> {
    let unnarrowed = |params: RepairParams, index, page, tail| Seeks {
        params,
        table: FINDINGS,
        index,
        page,
        tail,
    };
    let ruled = |params: RepairParams, index, page, tail| Seeks {
        params,
        table: RULES,
        index,
        page,
        tail,
    };
    vec![
        unnarrowed(repairing(), KIND_INDEX, KIND_SEEK, TAIL_SEEK),
        unnarrowed(
            repairing().with_severity(Severity::Error),
            SEVERITY_INDEX,
            SEVERITY_SEEK,
            TAIL_SEVERITY_SEEK,
        ),
        unnarrowed(
            repairing().with_severity(Severity::Warning),
            KIND_INDEX,
            KIND_SEEK,
            TAIL_SEEK,
        ),
        unnarrowed(
            repairing().with_kinds([FindingKind::UndeclaredTag]),
            KIND_INDEX,
            KIND_SEEK,
            TAIL_SEEK,
        ),
        ruled(
            repairing().with_rule("tasks"),
            RULE_KIND_INDEX,
            RULE_KIND_SEEK,
            RULE_TAIL_SEEK,
        ),
        ruled(
            repairing()
                .with_rule("tasks")
                .with_severity(Severity::Error),
            RULE_SEVERITY_INDEX,
            RULE_SEVERITY_SEEK,
            RULE_TAIL_SEVERITY_SEEK,
        ),
    ]
}

/// A batch of one over the rule suite's fixture reads a merged page, the tail
/// of the document it is cut inside, and a probe past it.
fn plans_of_a_cut_batch(store: &Validating, params: &RepairParams) -> Vec<ValidatePlan> {
    store.batch_plans(&params.clone().with_limit(1))
}

/// **A merged page merges one seek per kind, on either root.** Without a rule
/// each arm is a seek of `findings_fingerprint_kind_nocase` at `(fingerprint,
/// kind)` past the place the batch resumes after — or of
/// `findings_fingerprint_kind_severity_nocase` where the request admits one
/// severity — and with one each arm is the matching seek of
/// `finding_rules_fingerprint_rule_kind_nocase`, each rule row reaching its
/// finding by row id. SQLite merges the arms where more than one kind is read
/// and nothing sorts, on a first batch, on the probe past a cut document, and
/// whether a path part bounds the arms or not.
///
/// Controls: a statement rebuilt with a sort, or rebuilt without its merge,
/// fails; and with the kind index dropped the statement reads something else.
#[test]
fn a_merged_page_merges_one_seek_per_kind() {
    for order in ROOTS {
        let mut store = Validating::with_rules_under("batch-plan-page", 0, order);
        for seeks in seeks() {
            let arms = kinds_in(&seeks.params);
            let plans = plans_of_a_cut_batch(&store, &seeks.params);
            let pages = plans_of(&plans, ValidateStatement::MergedPage);
            assert_eq!(pages.len(), 2, "the first page and the probe");
            for page in &pages {
                judge_merge(page, seeks.table, seeks.index, seeks.page, arms);
            }
        }
        // A path part bounds each arm above.
        for glob in ["**/*.md", "notes/**", "*.md", "NOTES/**"] {
            let ranged = repairing().with_predicates([Predicate::path(glob)]);
            for page in plans_of(
                &plans_of_a_cut_batch(&store, &ranged),
                ValidateStatement::MergedPage,
            ) {
                judge_merge(
                    &page,
                    FINDINGS,
                    KIND_INDEX,
                    KIND_RANGE_SEEK,
                    kinds_in(&ranged),
                );
            }
        }
        let ranged = repairing()
            .with_rule("tasks")
            .with_predicates([Predicate::path("*.md")]);
        for page in plans_of(
            &plans_of_a_cut_batch(&store, &ranged),
            ValidateStatement::MergedPage,
        ) {
            judge_merge(
                &page,
                RULES,
                RULE_KIND_INDEX,
                RULE_RANGE_SEEK,
                kinds_in(&ranged),
            );
        }
        // A continuation seeks past the cursor's path the same way.
        let (_, next) = {
            let first = store.batch(&repairing().with_limit(1));
            (first.rows, first.next.expect("more remain"))
        };
        let continued = repairing().with_limit(1).with_after(next);
        for page in plans_of(
            &store.batch_plans(&continued),
            ValidateStatement::MergedPage,
        ) {
            judge_merge(
                &page,
                FINDINGS,
                KIND_INDEX,
                KIND_SEEK,
                FindingKind::ALL.len(),
            );
        }

        let page = plans_of(
            &plans_of_a_cut_batch(&store, &repairing()),
            ValidateStatement::MergedPage,
        )
        .remove(0);
        // Control: a sort over the merge.
        let sorted = rewritten(&page, |detail| {
            if detail == "MERGE (UNION ALL)" {
                "USE TEMP B-TREE FOR ORDER BY".to_string()
            } else {
                detail.to_string()
            }
        });
        failure_of("a merged page that sorts", || {
            judge_merge(
                &sorted,
                FINDINGS,
                KIND_INDEX,
                KIND_SEEK,
                FindingKind::ALL.len(),
            )
        });
        // Control: the merge taken out of a statement reading many kinds.
        let unmerged = rewritten(&page, |detail| {
            if detail == "MERGE (UNION ALL)" {
                "COMPOUND QUERY".to_string()
            } else {
                detail.to_string()
            }
        });
        failure_of("a statement that merges nothing", || {
            judge_merge(
                &unmerged,
                FINDINGS,
                KIND_INDEX,
                KIND_SEEK,
                FindingKind::ALL.len(),
            )
        });

        store.drop_index(KIND_INDEX);
        let page = plans_of(
            &plans_of_a_cut_batch(&store, &repairing()),
            ValidateStatement::MergedPage,
        )
        .remove(0);
        failure_of(&format!("{KIND_INDEX} dropped"), || {
            judge_merge(
                &page,
                FINDINGS,
                KIND_INDEX,
                KIND_SEEK,
                FindingKind::ALL.len(),
            )
        });
    }
}

/// **A document's tail merges one seek per kind at one path, on either root.**
/// Each arm pins the path by both of its columns, folded and bytewise, and
/// seeks past the finding's position and id, in the index the request's
/// merged page seeks — so it still seeks, and the arms, ordered by position
/// and id, merge with nothing sorted. It is the same for a rule's rows and
/// for a path part bounding the statement.
///
/// Controls: a tail rebuilt with a sort, or whose seek stops short of the
/// finding's position, fails; and with the kind index dropped the statement
/// reads something else.
#[test]
fn a_documents_tail_merges_one_seek_per_kind_at_one_path() {
    for order in ROOTS {
        let mut store = Validating::with_rules_under("batch-plan-tail", 0, order);
        for seeks in seeks() {
            let arms = kinds_in(&seeks.params);
            let plans = plans_of_a_cut_batch(&store, &seeks.params);
            let tails = plans_of(&plans, ValidateStatement::DocumentTail);
            assert_eq!(tails.len(), 1, "{:?}", seeks.params);
            judge_merge(&tails[0], seeks.table, seeks.index, seeks.tail, arms);
        }
        for glob in ["**/*.md", "*.md", "notes/**"] {
            let ranged = repairing().with_predicates([Predicate::path(glob)]);
            for tail in plans_of(
                &plans_of_a_cut_batch(&store, &ranged),
                ValidateStatement::DocumentTail,
            ) {
                judge_merge(&tail, FINDINGS, KIND_INDEX, TAIL_SEEK, kinds_in(&ranged));
            }
        }

        let tail = plans_of(
            &plans_of_a_cut_batch(&store, &repairing()),
            ValidateStatement::DocumentTail,
        )
        .remove(0);
        // Control: a sort over the merge.
        let sorted = rewritten(&tail, |detail| {
            if detail == "MERGE (UNION ALL)" {
                "USE TEMP B-TREE FOR ORDER BY".to_string()
            } else {
                detail.to_string()
            }
        });
        failure_of("a tail that sorts", || {
            judge_merge(
                &sorted,
                FINDINGS,
                KIND_INDEX,
                TAIL_SEEK,
                FindingKind::ALL.len(),
            )
        });
        // Control: the seek taken short of the finding's position.
        let short = rewritten(&tail, |detail| {
            detail.replace(" AND (position,rowid)>(?,?)", "")
        });
        failure_of("a tail that seeks from no position", || {
            judge_merge(
                &short,
                FINDINGS,
                KIND_INDEX,
                TAIL_SEEK,
                FindingKind::ALL.len(),
            )
        });
        // Control: the seek that pins no path.
        let unpinned = rewritten(&tail, |detail| detail.replace(" AND path=? AND path=?", ""));
        failure_of("a tail that pins no path", || {
            judge_merge(
                &unpinned,
                FINDINGS,
                KIND_INDEX,
                TAIL_SEEK,
                FindingKind::ALL.len(),
            )
        });

        store.drop_index(KIND_INDEX);
        let tail = plans_of(
            &plans_of_a_cut_batch(&store, &repairing()),
            ValidateStatement::DocumentTail,
        )
        .remove(0);
        failure_of(&format!("{KIND_INDEX} dropped"), || {
            judge_merge(
                &tail,
                FINDINGS,
                KIND_INDEX,
                TAIL_SEEK,
                FindingKind::ALL.len(),
            )
        });
    }
}

/// **A statement a document part drives is driven from the documents it
/// matched**, each reaching its findings by one seek at its path, as a kind
/// page is; the merge sorts only what the part matched.
#[test]
fn a_merged_page_a_document_part_drives_seeks_the_matched_documents_findings() {
    let store = Validating::with_rules_under("batch-driven", 0, StoredPathOrder::Sensitive);
    for params in [
        repairing().with_predicates([Predicate::tag("draft")]),
        repairing()
            .with_rule("tasks")
            .with_predicates([Predicate::equal_to("status", "open")]),
    ] {
        let plans = store.batch_plans(&params.with_limit(1));
        for page in plans_of(&plans, ValidateStatement::MergedPage) {
            page.assert_no_full_scan();
            rows_of(&page, "dv").assert_searches_through("documents", Access::RowId);
        }
    }
}

// ---- the work bar ----

/// What the batch `batches` batches into `params` costs, read at `limit`.
fn batch_work(
    store: &Validating,
    params: &RepairParams,
    limit: u32,
    batches: usize,
) -> ValidateWork {
    let mut after: Option<Cursor> = None;
    for _ in 0..batches {
        let mut request = params.clone().with_limit(limit);
        if let Some(cursor) = after.take() {
            request = request.with_after(cursor);
        }
        after = Some(
            store
                .batch(&request)
                .next
                .expect("the drain reaches the batch it is judged at"),
        );
    }
    let mut request = params.clone().with_limit(limit);
    if let Some(cursor) = after {
        request = request.with_after(cursor);
    }
    store.batch(&request).work
}

/// **A batch costs the batch, not the vault, on either root.** Beside 50 and
/// then beside 500 more documents each with a warning standing over it, a
/// batch of five — unnarrowed, by one kind, and by a rule — costs the same at
/// both sizes on its first batch and on the batches that continue it into the
/// bulk, sorts nothing and steps through no full scan. The merge reads each
/// kind's seek as far as the batch needs and no further.
///
/// Control: the kind index dropped on the larger vault, a batch reaches the
/// whole range, and the bar fails.
#[test]
fn a_batch_costs_the_batch_not_the_vault() {
    for order in ROOTS {
        let small = Validating::with_rules_under("batch-work-small", 50, order);
        let mut large = Validating::with_rules_under("batch-work-large", 500, order);
        let broad = [
            repairing(),
            repairing().with_kinds([FindingKind::UndeclaredTag]),
            repairing().with_rule("bulk"),
            repairing().with_predicates([Predicate::path("**/*.md")]),
            repairing().with_severity(Severity::Warning),
        ];
        let judge = |large: &Validating, params: &RepairParams| {
            for batches in [0, 1, 4] {
                let (at_small, at_large) = (
                    batch_work(&small, params, 5, batches),
                    batch_work(large, params, 5, batches),
                );
                assert_eq!(
                    at_small, at_large,
                    "batch {batches} of {params:?} grew with the vault under {order:?}"
                );
                assert_eq!(
                    (at_large.sorts, at_large.full_scan_steps),
                    (0, 0),
                    "batch {batches} of {params:?} sorted or scanned under {order:?}: {at_large:?}"
                );
            }
        };
        for params in &broad {
            judge(&large, params);
        }

        large.drop_index(KIND_INDEX);
        failure_of(&format!("{KIND_INDEX} dropped under {order:?}"), || {
            judge(&large, &broad[1])
        });
    }
}

/// **A batch's counters read like a validate's**: the statements it ran, the
/// rows its merged statements handed back — one past the limit where it
/// reads on, and the probe's row — and the steps those took.
#[test]
fn a_batch_counts_its_work_as_a_validate_does() {
    let store = Validating::new("batch-counts");
    let whole = store.batch(&repairing().with_limit(1000));
    assert_eq!(whole.work.rows_read, 7, "a batch that reads everything");
    assert_eq!(whole.work.sorts, 0);
    assert!(whole.work.statements >= 3, "{:?}", whole.work);
    let limited = store.batch(&repairing().with_limit(4));
    assert_eq!(
        limited.work.rows_read, 5,
        "the limit and the row past it, which stands at another document"
    );
    let inside = store.batch(&repairing().with_limit(1));
    assert_eq!(
        inside.work.rows_read,
        2 + 2 + 1,
        "the limit and the row past it, the tail of the document, then the probe's one"
    );
    assert!(inside.work.statements > limited.work.statements);
}

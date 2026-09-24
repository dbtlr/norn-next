//! The validate builder: what a page of findings and a summary answer, how a
//! part judges a finding, and the plan and work bars every statement it emits
//! is judged by.
//!
//! The fixture is four documents and the findings standing over them and over
//! one path no document row stands at, of four kinds and both severities:
//!
//! | finding | kind | severity | path | document row |
//! |---|---|---|---|---|
//! | `unreadable` | body-bytes-not-utf8 | error | `broken.md` | none |
//! | `unread` | frontmatter-unreadable | error | `notes/d.md` | no frontmatter |
//! | `glossary` | path-names-no-document | warning | `notes/c.md` | `status: open` |
//! | `dotted` | path-names-no-document | warning | `a.md` | `status: open`, `#draft` |
//! | `draft`, `idea` | undeclared-tag | warning | `a.md` | |
//! | `other` | undeclared-tag | warning | `b.md` | `status: closed` |

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::common::{
    Scratch, ambiguity, ambiguity_for_target, document, unread_block, violation, write_documents,
};
use crate::find::{failure_of, map, rows_of, string};
use norn_store::{
    DeclaredFields, FindingFacts, PageRefusal, ReadStatement, Snapshot, SnapshotReader, Store,
    TagFact, TagSource, VALIDATE_STATEMENTS, ValidatePlan, ValidateStatement, Validated,
    Validation, induced_failure,
};
use norn_testkit::explain::{Access, PlanRow, QueryPlan};
use norn_wire::{
    Cursor, CursorKey, FindingKind, FindingRow, Hint, KindTally, Predicate, ResolutionTarget,
    Severity, Unsatisfied, ValidateParams, ValidateReport, VaultAddress, VaultName,
};

// ---- fixtures ----

/// The fingerprint of the schema the fixture pins.
const VALIDATE_SCHEMA: &str = "validate-schema";

fn declared() -> DeclaredFields {
    DeclaredFields::under(VALIDATE_SCHEMA).declare("status")
}

/// A tag the vault's declared tag facet does not admit, on the document at
/// `at`.
fn undeclared(at: &str, tag: &str) -> FindingFacts {
    let mut finding = unread_block(at);
    finding.kind = FindingKind::UndeclaredTag;
    finding.severity = Severity::Warning;
    finding.target = Some(tag.to_string());
    finding.message = format!("`{tag}` is not a declared tag");
    finding.detail = None;
    finding
}

/// The fixture's documents, then its findings, recorded in the table's order
/// so their ids ascend down it.
fn seed(store: &mut Store) {
    store
        .begin_request()
        .pin_vault_schema(VALIDATE_SCHEMA.as_bytes(), VALIDATE_SCHEMA)
        .expect("pinning the fixture's schema");
    let declared = declared();
    let status = |at: &str, value: &str| {
        document(at, &format!("hash-{at}"), "a body\n")
            .with_frontmatter(Some(map(vec![("status", string(value))])), &declared)
    };
    let mut tagged = status("a.md", "open");
    tagged.tags.push(TagFact {
        name: "draft".to_string(),
        source: TagSource::Body,
        span: None,
    });
    let mut request = store.begin_request();
    write_documents(
        &mut request,
        &[
            tagged,
            status("b.md", "closed"),
            status("notes/c.md", "open"),
            document("notes/d.md", "hash-d", "a body\n"),
        ],
    );
    for finding in [
        violation("broken.md"),
        unread_block("notes/d.md"),
        ambiguity(
            "notes/c.md",
            "glossary",
            "glossary/",
            &["g/1.md", "g/2.md", "g/3.md", "g/4.md", "g/5.md"],
            7,
        ),
        ambiguity_for_target("a.md", "v1.2", &["v/v1.2.md"], 1),
        undeclared("a.md", "draft"),
        undeclared("a.md", "idea"),
        undeclared("b.md", "other"),
    ] {
        request
            .record_finding(&finding)
            .expect("recording a finding");
    }
}

/// A seeded store and the read handle its snapshots are established on.
struct Validating {
    _scratch: Scratch,
    store: Store,
    reader: Arc<SnapshotReader>,
}

impl Validating {
    fn new(label: &str) -> Self {
        Self::with_bulk(label, 0)
    }

    /// The fixture and `bulk` more documents under `bulk/`, each with the
    /// `status` `filed` and an undeclared-tag warning standing over it: enough
    /// findings that a validate whose work grows with the vault reads many
    /// times more of them.
    fn with_bulk(label: &str, bulk: usize) -> Self {
        let scratch = Scratch::new(label);
        let mut store = scratch.open();
        seed(&mut store);
        if bulk > 0 {
            let declared = declared();
            let documents: Vec<_> = (0..bulk)
                .map(|at| {
                    document(
                        &format!("bulk/{at:04}.md"),
                        &format!("hash-bulk-{at}"),
                        "a body\n",
                    )
                    .with_frontmatter(Some(map(vec![("status", string("filed"))])), &declared)
                })
                .collect();
            let mut request = store.begin_request();
            write_documents(&mut request, &documents);
            for at in 0..bulk {
                request
                    .record_finding(&undeclared(&format!("bulk/{at:04}.md"), "bulk"))
                    .expect("recording a finding");
            }
        }
        let reader = Arc::new(
            store
                .open_reader()
                .reader
                .expect("a live store mints a reader"),
        );
        Validating {
            _scratch: scratch,
            store,
            reader,
        }
    }

    fn snapshot(&self) -> Snapshot {
        self.reader
            .try_take()
            .expect("a handle nothing is reading holds its connection")
            .establish()
            .snapshot
            .expect("a snapshot")
    }

    fn validate(&self, params: &ValidateParams) -> Validated {
        self.snapshot()
            .validate(params, &declared())
            .unwrap_or_else(|refusal| panic!("a validate of {params:?}: {refusal}"))
    }

    /// The page `params` answers: its rows and its cursor.
    fn page(&self, params: &ValidateParams) -> (Vec<FindingRow>, Option<Cursor>) {
        match self.validate(params).answer {
            Validation::Findings { rows, next, .. } => (rows, next),
            Validation::Summary { .. } => panic!("a page of {params:?} answered a summary"),
        }
    }

    fn rows(&self, params: &ValidateParams) -> Vec<FindingRow> {
        self.page(params).0
    }

    fn summary(&self, params: &ValidateParams) -> Vec<KindTally> {
        match self.validate(&params.clone().summarized()).answer {
            Validation::Summary { by_kind } => by_kind,
            Validation::Findings { .. } => panic!("a summary of {params:?} answered a page"),
        }
    }

    /// Record `finding` over the seeded vault.
    fn stand(&mut self, finding: &FindingFacts) {
        self.store
            .begin_request()
            .record_finding(finding)
            .expect("recording a finding");
    }

    fn plans(&self, params: &ValidateParams) -> Vec<ValidatePlan> {
        self.snapshot()
            .validate_plans(params, &declared())
            .expect("the plans of a validate")
    }

    /// Drop `index` on the writer, which is what a negative control judges the
    /// same bar against.
    fn drop_index(&mut self, index: &str) {
        induced_failure::execute_out_of_band(&mut self.store, &format!("DROP INDEX {index}"))
            .unwrap_or_else(|problem| panic!("dropping {index}: {problem}"));
    }
}

fn vault() -> VaultAddress {
    VaultAddress::name(VaultName::new("notes").expect("a vault name"))
}

fn validating() -> ValidateParams {
    ValidateParams::new(vault())
}

/// A finding row as the fixture's table names it: its kind, path and target.
fn named(row: &FindingRow) -> (FindingKind, String, Option<String>) {
    (row.kind, row.path.as_str().to_string(), row.target.clone())
}

fn finding(
    kind: FindingKind,
    path: &str,
    target: Option<&str>,
) -> (FindingKind, String, Option<String>) {
    (kind, path.to_string(), target.map(str::to_string))
}

/// Every finding in the fixture, in `(kind, path, id)` order.
fn every_finding() -> Vec<(FindingKind, String, Option<String>)> {
    vec![
        finding(FindingKind::BodyBytesNotUtf8, "broken.md", None),
        finding(FindingKind::FrontmatterUnreadable, "notes/d.md", None),
        finding(FindingKind::PathNamesNoDocument, "a.md", Some("v1.2")),
        finding(
            FindingKind::PathNamesNoDocument,
            "notes/c.md",
            Some("glossary"),
        ),
        finding(FindingKind::UndeclaredTag, "a.md", Some("draft")),
        finding(FindingKind::UndeclaredTag, "a.md", Some("idea")),
        finding(FindingKind::UndeclaredTag, "b.md", Some("other")),
    ]
}

fn names(rows: &[FindingRow]) -> Vec<(FindingKind, String, Option<String>)> {
    rows.iter().map(named).collect()
}

// ---- what a page answers ----

/// **With no predicates every finding standing under the active fingerprint
/// answers, in `(kind, path, id)` order**: kinds in their spelled order, a
/// kind's findings by path, and two findings of one kind at one path by id.
/// A finding stands whether or not a document row stands at its path.
#[test]
fn with_no_predicates_every_finding_standing_answers_in_kind_path_id_order() {
    let validating_store = Validating::new("validate-every");
    let rows = validating_store.rows(&validating());
    assert_eq!(names(&rows), every_finding());
    assert!(rows[4].id < rows[5].id, "one kind at one path orders by id");
    assert_eq!(rows[0].severity, Severity::Error);
    assert_eq!(rows[6].severity, Severity::Warning);
    let answered = validating_store.validate(&validating());
    assert!(answered.unsatisfied.is_empty());
    assert_eq!(
        answered.snapshot.schema_fingerprint, None,
        "a validate's order is no field's, so its reading names no fingerprint"
    );
}

/// **A path part judges the finding's own path**, so a finding standing where
/// no document row does — a file that could not be read — is found by the
/// path that names it, and a glob reaches every finding under it whatever
/// stands there. **Every other part judges the document row at the finding's
/// path**, which a finding with no document row beside it never satisfies: an
/// equality, a missing key and an inequality alike leave `broken.md` out, and
/// the finding standing beside a document holding no frontmatter is the one a
/// missing key admits.
#[test]
fn a_path_part_judges_the_findings_path_and_every_other_part_its_document() {
    let validating_store = Validating::new("validate-path-rule");
    let rows = |predicates: Vec<Predicate>| {
        names(&validating_store.rows(&validating().with_predicates(predicates)))
    };
    let every = every_finding();

    assert_eq!(
        rows(vec![Predicate::path("broken.md")]),
        vec![every[0].clone()]
    );
    assert_eq!(
        rows(vec![Predicate::path("notes/**")]),
        vec![every[1].clone(), every[3].clone()]
    );
    assert_eq!(
        rows(vec![Predicate::path("*.md")]),
        vec![
            every[0].clone(),
            every[2].clone(),
            every[4].clone(),
            every[5].clone(),
            every[6].clone()
        ]
    );

    assert_eq!(
        rows(vec![Predicate::equal_to("status", "open")]),
        vec![
            every[2].clone(),
            every[3].clone(),
            every[4].clone(),
            every[5].clone()
        ]
    );
    assert_eq!(
        rows(vec![Predicate::missing("status")]),
        vec![every[1].clone()],
        "a missing key is a fact of a document, and broken.md has none"
    );
    assert_eq!(
        rows(vec![Predicate::not_equal_to("status", "open")]),
        vec![every[1].clone(), every[6].clone()],
        "an inequality is a fact of a document, and broken.md has none"
    );
    assert_eq!(
        rows(vec![Predicate::tag("draft")]),
        vec![every[2].clone(), every[4].clone(), every[5].clone()]
    );
    assert_eq!(
        rows(vec![Predicate::has_finding(FindingKind::BodyBytesNotUtf8)]),
        Vec::new(),
        "the one finding of that kind stands where no document row does"
    );
    assert_eq!(
        rows(vec![
            Predicate::path("*.md"),
            Predicate::equal_to("status", "open")
        ]),
        vec![every[2].clone(), every[4].clone(), every[5].clone()]
    );
}

/// **The kinds narrow to the kinds named, and a severity floor to the
/// severities at it or above**: an error floor admits the two errors, a
/// warning floor every finding.
#[test]
fn kinds_and_a_severity_floor_narrow_the_findings() {
    let validating_store = Validating::new("validate-kinds");
    let every = every_finding();
    assert_eq!(
        names(&validating_store.rows(&validating().with_kinds([FindingKind::UndeclaredTag]))),
        every[4..].to_vec()
    );
    assert_eq!(
        names(&validating_store.rows(&validating().with_kinds([
            FindingKind::UndeclaredTag,
            FindingKind::BodyBytesNotUtf8,
            FindingKind::UndeclaredTag,
        ]))),
        vec![
            every[0].clone(),
            every[4].clone(),
            every[5].clone(),
            every[6].clone()
        ],
        "kinds are read in their spelled order, each once, whatever the request's order"
    );
    assert_eq!(
        names(&validating_store.rows(&validating().with_severity(Severity::Error))),
        every[..2].to_vec()
    );
    assert_eq!(
        names(&validating_store.rows(&validating().with_severity(Severity::Warning))),
        every
    );
    assert_eq!(
        names(
            &validating_store.rows(
                &validating()
                    .with_severity(Severity::Error)
                    .with_kinds([FindingKind::UndeclaredTag])
            )
        ),
        Vec::new()
    );
}

/// Every finding `params` answers, drained a page of `limit` at a time.
fn drained(validating_store: &Validating, params: &ValidateParams, limit: u32) -> Vec<FindingRow> {
    let mut rows = Vec::new();
    let mut after: Option<Cursor> = None;
    loop {
        let mut page = params.clone().with_limit(limit);
        if let Some(cursor) = after.take() {
            page = page.with_after(cursor);
        }
        let (read, next) = validating_store.page(&page);
        assert!(
            read.len() <= limit as usize,
            "a page held more than its bound"
        );
        rows.extend(read);
        match next {
            Some(next) => after = Some(next),
            None => return rows,
        }
        assert!(rows.len() < 100, "the drain does not end");
    }
}

/// The requests the drain and the summary are judged over: path parts with
/// and without a literal prefix among them, since a prefix bounds the seek a
/// page's position also bounds, and a literal path naming a finding's own
/// path, with a document row there and without one.
fn requests() -> Vec<ValidateParams> {
    vec![
        validating(),
        validating().with_kinds([FindingKind::UndeclaredTag, FindingKind::PathNamesNoDocument]),
        validating().with_severity(Severity::Error),
        validating().with_predicates([Predicate::path("*.md")]),
        validating().with_predicates([Predicate::path("notes/**")]),
        validating().with_predicates([Predicate::path("notes/*.md")]),
        validating().with_predicates([Predicate::path("notes/c.md")]),
        validating().with_predicates([Predicate::path("broken.md")]),
        validating().with_predicates([Predicate::equal_to("status", "open")]),
        validating().with_predicates([Predicate::not_equal_to("status", "open")]),
    ]
}

/// **A drain a page at a time answers the findings one page does**, in the
/// same order, whatever the bound: a continuation resumes after the last
/// finding's kind, path and id, inside a kind and across kinds, among two
/// findings of one kind at one path, and under every narrowing. The page
/// stops at its last finding.
#[test]
fn a_drain_a_page_at_a_time_answers_the_findings_one_page_does() {
    let validating_store = Validating::new("validate-drain");
    for params in requests() {
        let whole = validating_store.rows(&params.clone().with_limit(1000));
        for limit in [1, 2, 3] {
            assert_eq!(
                drained(&validating_store, &params, limit),
                whole,
                "{params:?} drained {limit} at a time"
            );
        }
    }
    let (rows, next) = validating_store.page(&validating().with_limit(2));
    assert_eq!(
        next.as_ref().map(Cursor::key),
        Some(&CursorKey::finding(
            FindingKind::FrontmatterUnreadable,
            "notes/d.md",
            rows[1].id
        )),
        "a page stops at its last finding's kind, path and id"
    );
}

/// **A finding row carries the bounded head the pillar stores, the total it
/// heads, and the hint that names the `find` enumerating its class.** The
/// `glossary` finding heads five of seven candidates in the order they were
/// recorded and hints `glossary`; the `v1.2` finding is in two classes, and
/// hints the address whose probe opens both; a finding in no class carries
/// an empty head of none and no hint.
#[test]
fn a_finding_row_carries_its_bounded_head_its_total_and_its_hint() {
    let validating_store = Validating::new("validate-head");
    let rows = validating_store.rows(&validating());
    let by_target: BTreeMap<Option<String>, &FindingRow> =
        rows.iter().map(|row| (row.target.clone(), row)).collect();
    let glossary = by_target[&Some("glossary".to_string())];
    assert_eq!(
        glossary
            .head
            .candidates()
            .iter()
            .map(|candidate| candidate.path.as_str())
            .collect::<Vec<_>>(),
        vec!["g/1.md", "g/2.md", "g/3.md", "g/4.md", "g/5.md"]
    );
    assert_eq!(glossary.head.total(), 7);
    assert!(glossary.head.is_truncated());
    let target = |text: &str| ResolutionTarget::new(text).expect("a target");
    assert_eq!(glossary.hint, Some(Hint::resolves(target("glossary"))));
    let dotted = by_target[&Some("v1.2".to_string())];
    assert_eq!(dotted.hint, Some(Hint::resolves(target("v1.2"))));
    assert_eq!(dotted.head.total(), 1);

    let unreadable = &rows[0];
    assert_eq!(unreadable.path.as_str(), "broken.md");
    assert!(unreadable.head.candidates().is_empty());
    assert_eq!(unreadable.head.total(), 0);
    assert_eq!(unreadable.hint, None);
    assert!(unreadable.generation > 0);
}

// ---- the summary ----

/// **A summary tallies what a drain answers**: for every request, one tally
/// per kind and severity, in that order, each counting the findings a drain
/// of the same request answers under it. A summary is not paged, and ignores
/// the page bound.
#[test]
fn a_summary_tallies_what_a_drain_answers() {
    let validating_store = Validating::new("validate-summary");
    for params in requests() {
        let mut counted: BTreeMap<(&str, &str), u64> = BTreeMap::new();
        for row in drained(&validating_store, &params, 2) {
            *counted
                .entry((row.kind.as_str(), row.severity.as_str()))
                .or_default() += 1;
        }
        let expected: Vec<(&str, &str, u64)> = counted
            .into_iter()
            .map(|((kind, severity), count)| (kind, severity, count))
            .collect();
        let tallied: Vec<(&str, &str, u64)> = validating_store
            .summary(&params.clone().with_limit(1))
            .iter()
            .map(|tally| (tally.kind.as_str(), tally.severity.as_str(), tally.count))
            .collect();
        assert_eq!(tallied, expected, "{params:?}");
    }
    let (_, report) = validating_store
        .validate(&validating().summarized())
        .into_report();
    assert!(
        matches!(report, ValidateReport::Summary { ref by_kind, .. } if by_kind.len() == 4),
        "{report:?}"
    );
}

// ---- the parts a validate cannot apply ----

/// **A `resolves` part is answered in-band**: the findings are the ones the
/// rest of the conjunction earned, and the part is reported as not applicable
/// rather than refused, on a page and on a summary. **The other parts a
/// validate cannot apply are reported as a find reports them**, through the
/// same compilation: a malformed glob empties the answer, and a predicate key
/// outside the field universe is reported with its near keys and filters
/// nothing among documents.
#[test]
fn a_part_a_validate_cannot_apply_is_reported_as_a_find_reports_it() {
    let validating_store = Validating::new("validate-unsatisfied");
    let target = ResolutionTarget::new("glossary").expect("a target");
    let open = Predicate::equal_to("status", "open");
    let earned = validating_store.rows(&validating().with_predicates([open.clone()]));
    assert!(!earned.is_empty());

    let resolving =
        validating().with_predicates([Predicate::resolves(target.clone()), open.clone()]);
    let resolved = validating_store.validate(&resolving);
    assert_eq!(
        resolved.unsatisfied,
        vec![Unsatisfied::resolves_not_applicable(target.clone())]
    );
    assert_eq!(validating_store.rows(&resolving), earned);
    let summarized = validating_store.validate(&resolving.clone().summarized());
    assert_eq!(
        summarized.unsatisfied,
        vec![Unsatisfied::resolves_not_applicable(target)]
    );

    let malformed = validating_store
        .validate(&validating().with_predicates([Predicate::path(""), open.clone()]));
    assert!(
        matches!(
            malformed.unsatisfied.as_slice(),
            [Unsatisfied::MalformedGlob { .. }]
        ),
        "{:?}",
        malformed.unsatisfied
    );
    assert_eq!(
        malformed.answer,
        Validation::Findings {
            rows: Vec::new(),
            next: None,
            moved: Vec::new()
        }
    );
    assert_eq!(malformed.work.rows_read, 0, "no page statement ran");
    assert_eq!(
        validating_store.summary(&validating().with_predicates([Predicate::path("")])),
        Vec::new()
    );

    let unknown =
        validating_store.validate(&validating().with_predicates([Predicate::has("stauts"), open]));
    assert_eq!(
        unknown.unsatisfied,
        vec![Unsatisfied::unknown_predicate_key(
            "stauts",
            vec!["status".to_string()]
        )]
    );
    assert_eq!(
        match unknown.answer {
            Validation::Findings { rows, .. } => rows,
            Validation::Summary { .. } => Vec::new(),
        },
        earned
    );
}

/// Each finding's kind and severity, tallied in that order.
fn tallies_of(rows: &[FindingRow]) -> Vec<(FindingKind, Severity, u64)> {
    let mut counted: BTreeMap<(&str, &str), (FindingKind, Severity, u64)> = BTreeMap::new();
    for row in rows {
        counted
            .entry((row.kind.as_str(), row.severity.as_str()))
            .or_insert((row.kind, row.severity, 0))
            .2 += 1;
    }
    counted.into_values().collect()
}

fn tallied(tallies: &[KindTally]) -> Vec<(FindingKind, Severity, u64)> {
    tallies
        .iter()
        .map(|tally| (tally.kind, tally.severity, tally.count))
        .collect()
}

/// **A part on a key outside the field universe is reported and filters
/// nothing among documents, and it is still a part that judges a document**,
/// so it admits no finding standing where no document row does: on a page
/// and on a summary, an equality, an inequality, a presence, an absence and a
/// bound alike answer every finding standing on a document and none at
/// `broken.md` or `notes/gone.md`.
#[test]
fn a_part_on_an_unknown_key_admits_no_finding_without_a_document() {
    let mut validating_store = Validating::new("validate-unknown-key");
    validating_store.stand(&violation("notes/gone.md"));
    let documentless = ["broken.md", "notes/gone.md"];
    let every = validating_store.rows(&validating());
    assert!(
        documentless
            .iter()
            .all(|path| every.iter().any(|row| row.path.as_str() == *path)),
        "both documentless findings stand"
    );
    let documented: Vec<FindingRow> = every
        .into_iter()
        .filter(|row| !documentless.contains(&row.path.as_str()))
        .collect();
    assert_eq!(names(&documented), every_finding()[1..].to_vec());
    let reported = vec![Unsatisfied::unknown_predicate_key(
        "stauts",
        vec!["status".to_string()],
    )];

    for part in [
        Predicate::equal_to("stauts", "open"),
        Predicate::not_equal_to("stauts", "open"),
        Predicate::has("stauts"),
        Predicate::missing("stauts"),
        Predicate::before("stauts", "open"),
    ] {
        let params = validating().with_predicates([part.clone()]);
        let answered = validating_store.validate(&params);
        assert_eq!(answered.unsatisfied, reported, "{part:?}");
        assert_eq!(
            validating_store.rows(&params),
            documented,
            "a page of {part:?}"
        );
        let summarized = validating_store.validate(&params.clone().summarized());
        assert_eq!(summarized.unsatisfied, reported, "{part:?}");
        assert_eq!(
            tallied(&validating_store.summary(&params)),
            tallies_of(&documented),
            "a summary of {part:?}"
        );
    }
}

/// **A cursor that names no position among a validate's findings is
/// refused**: a document's.
#[test]
fn a_cursor_that_is_no_position_among_the_findings_is_refused() {
    let validating_store = Validating::new("validate-cursor");
    let reading = validating_store.validate(&validating()).snapshot;
    assert_eq!(
        validating_store
            .snapshot()
            .validate(
                &validating().with_after(Cursor::new(reading, CursorKey::document(None, "a.md"))),
                &declared()
            )
            .expect_err("the cursor is refused"),
        PageRefusal::NotAFindingCursor
    );
    assert_eq!(
        PageRefusal::NotAFindingCursor.to_string(),
        "the cursor names no position among this validate's findings"
    );
}

/// **A summary is not paged**: it refuses a cursor — even one a page of the
/// same request minted, which a page continues — and it answers the same
/// tallies whatever page bound the request names, one a page refuses
/// included.
#[test]
fn a_summary_is_not_paged_so_it_refuses_a_cursor_and_ignores_the_bound() {
    let validating_store = Validating::new("validate-summary-paging");
    let minted = validating_store
        .page(&validating().with_limit(2))
        .1
        .expect("a next page");
    assert!(
        !validating_store
            .rows(&validating().with_after(minted.clone()))
            .is_empty(),
        "a page continues the cursor"
    );
    assert_eq!(
        validating_store
            .snapshot()
            .validate(&validating().summarized().with_after(minted), &declared())
            .expect_err("a summary continues no cursor"),
        PageRefusal::SummaryNotPaged
    );
    assert_eq!(
        PageRefusal::SummaryNotPaged.to_string(),
        "a summary is not paged, so it continues no cursor"
    );

    let unbounded = validating_store.summary(&validating());
    assert!(!unbounded.is_empty());
    for limit in [0, 1, u32::MAX] {
        assert_eq!(
            validating_store.summary(&validating().with_limit(limit)),
            unbounded,
            "a summary bounded at {limit}"
        );
    }
}

// ---- the plan bars ----

/// The plan the store reported for one statement, in the harness's shape.
fn plan(emitted: &ValidatePlan) -> QueryPlan {
    QueryPlan::new(
        emitted.plan.sql.clone(),
        emitted
            .plan
            .steps
            .iter()
            .map(|step| PlanRow::new(step.id, step.parent, step.detail.clone()))
            .collect(),
    )
}

/// Every plan `plans` holds for `statement`, in the order the validate ran
/// them.
fn plans_of(plans: &[ValidatePlan], statement: ValidateStatement) -> Vec<QueryPlan> {
    let matching: Vec<QueryPlan> = plans
        .iter()
        .filter(|plan| plan.statement == ReadStatement::Validate(statement))
        .map(plan)
        .collect();
    assert!(
        !matching.is_empty(),
        "the validate runs no {statement:?}: {plans:?}"
    );
    matching
}

/// `plan` with every row's detail rewritten by `edit`: a plan a control
/// judges in place of the one SQLite reported.
fn rewritten(plan: &QueryPlan, edit: impl Fn(&str) -> String) -> QueryPlan {
    QueryPlan::new(
        plan.sql(),
        plan.rows()
            .iter()
            .map(|row| PlanRow::new(row.id, row.parent, edit(&row.detail)))
            .collect(),
    )
}

/// Which test bars each statement the builder names. Exhaustive, so a
/// statement added to [`ValidateStatement`] does not compile until its author
/// names the bar.
fn statement_barred_by(statement: ValidateStatement) -> &'static str {
    match statement {
        ValidateStatement::KindPage => "a_kind_page_seeks_its_kind_from_the_pages_position",
        ValidateStatement::Summary => "a_summary_aggregates_over_the_kind_and_severity_index",
    }
}

/// **Every statement the validate builder names carries a bar, and one bar
/// judges each**: the enumeration's slots are each claimed once, and each
/// bar is named by exactly the statements it judges.
#[test]
fn the_validate_bars_cover_every_statement_once() {
    let mut slots: Vec<usize> = ValidateStatement::all()
        .into_iter()
        .map(ValidateStatement::slot)
        .collect();
    slots.sort_unstable();
    assert_eq!(slots, (0..VALIDATE_STATEMENTS).collect::<Vec<usize>>());
    for (slot, statement) in ValidateStatement::all().into_iter().enumerate() {
        assert_eq!(statement.slot(), slot, "{statement:?} claims another slot");
    }
    let bars: std::collections::BTreeSet<&str> = ValidateStatement::all()
        .into_iter()
        .map(statement_barred_by)
        .collect();
    assert_eq!(bars.len(), VALIDATE_STATEMENTS);
}

/// Judge a kind page no document part drives: every section a seek of
/// `index` under `constraint`, handing its findings back in page order, with
/// nothing read end to end and nothing sorted.
fn judge_kind_seek(page: &QueryPlan, index: &str, constraint: &str) {
    page.assert_no_full_scan();
    page.assert_no_temp_btree();
    let rows = rows_of(page, "f");
    rows.assert_searches_through("findings", Access::Index(index));
    rows.assert_search_constraint("findings", constraint);
}

/// Judge a statement a document part drives: the matched documents by row id,
/// handed them by the part's own seek, and each one's findings by one seek at
/// its path — never a seek of a kind's findings from a position.
fn judge_driven(page: &QueryPlan) {
    page.assert_no_full_scan();
    rows_of(page, "dv").assert_searches_through("documents", Access::RowId);
    let findings = rows_of(page, "f");
    findings.assert_searches("findings");
    assert!(
        findings
            .rows()
            .iter()
            .all(|row| row.constraint().is_some_and(|seek| seek.contains("path=?"))),
        "a driven statement reached findings other than at a matched path: {:?}\n\
         emitted SQL: {}",
        page.rows(),
        page.sql()
    );
}

/// The unnarrowed kind index's seek from a page's position.
const KIND_SEEK: &str = "(vault_schema_fingerprint=? AND kind=? AND path>?)";

/// **A kind page seeks its kind from the page's position.** Each section is
/// one seek of `findings_vault_schema_fingerprint` at `(fingerprint, kind)`
/// bounded below by the page's position, on a first page and a continuation
/// alike, and its findings come off the index in `(path, id)` order, so
/// nothing sorts. **A severity floor admitting one severity seeks
/// `findings_fingerprint_kind_severity` at `(fingerprint, kind, severity)`**,
/// so a finding of another severity is never reached. **A path part bounds
/// the same seek** by its glob's range. **A document part that keeps what it
/// seeks drives the section** from the documents it matched, each reaching
/// its findings through `findings_path`.
///
/// Controls: a continuation's plan rebuilt without its position bound fails;
/// each index dropped, the section it served reads something else.
#[test]
fn a_kind_page_seeks_its_kind_from_the_pages_position() {
    let mut validating_store = Validating::new("validate-page-plan");
    let pages = |validating_store: &Validating, params: &ValidateParams| {
        plans_of(&validating_store.plans(params), ValidateStatement::KindPage)
    };
    for page in pages(&validating_store, &validating()) {
        judge_kind_seek(&page, "findings_vault_schema_fingerprint", KIND_SEEK);
    }
    let (_, next) = validating_store.page(&validating().with_limit(3));
    let continued = pages(
        &validating_store,
        &validating().with_after(next.expect("a next page")),
    );
    assert_eq!(
        continued.len(),
        2,
        "a continuation in the third kind reads that kind and the one after it"
    );
    for page in &continued {
        judge_kind_seek(page, "findings_vault_schema_fingerprint", KIND_SEEK);
    }
    for page in pages(
        &validating_store,
        &validating().with_severity(Severity::Error),
    ) {
        judge_kind_seek(
            &page,
            "findings_fingerprint_kind_severity",
            "(vault_schema_fingerprint=? AND kind=? AND severity=? AND path>?)",
        );
    }
    for page in pages(
        &validating_store,
        &validating().with_predicates([Predicate::path("notes/**")]),
    ) {
        judge_kind_seek(
            &page,
            "findings_vault_schema_fingerprint",
            "(vault_schema_fingerprint=? AND kind=? AND path>? AND path<?)",
        );
    }
    let driven = pages(
        &validating_store,
        &validating().with_predicates([Predicate::tag("draft")]),
    );
    for page in &driven {
        judge_driven(page);
    }
    for params in [
        validating().with_predicates([Predicate::equal_to("status", "open")]),
        validating()
            .with_severity(Severity::Error)
            .with_predicates([Predicate::tag("draft"), Predicate::path("*.md")]),
    ] {
        for page in pages(&validating_store, &params) {
            judge_driven(&page);
        }
        let summary = plans_of(
            &validating_store.plans(&params.summarized()),
            ValidateStatement::Summary,
        );
        judge_driven(&summary[0]);
        assert!(
            rows_of(&summary[0], "f").rows().iter().all(|row| row
                .detail
                .contains("COVERING INDEX findings_fingerprint_kind_severity")),
            "a driven summary read a finding's row: {:?}",
            summary[0].rows()
        );
    }
    // An excluding part alone drives nothing: the section seeks its kind and
    // tests each finding's document by its path.
    for page in pages(
        &validating_store,
        &validating().with_predicates([Predicate::not_equal_to("status", "open")]),
    ) {
        judge_kind_seek(&page, "findings_vault_schema_fingerprint", KIND_SEEK);
        rows_of(&page, "dv").assert_searches_through("documents", Access::Index("documents_path"));
    }

    // Control: the continuation's position taken out of the seek.
    let unbounded = rewritten(&continued[0], |detail| detail.replace(" AND path>?", ""));
    failure_of("a continuation that seeks from no position", || {
        judge_kind_seek(&unbounded, "findings_vault_schema_fingerprint", KIND_SEEK)
    });
    // Control: a driven section that seeks its kind from a position.
    let undriven = rewritten(&driven[0], |detail| {
        if detail.starts_with("SEARCH f ") {
            format!("SEARCH f USING INDEX findings_vault_schema_fingerprint {KIND_SEEK}")
        } else {
            detail.to_string()
        }
    });
    failure_of("a driven section that seeks its kind", || {
        judge_driven(&undriven)
    });
    // Control: a driven section that walks the documents.
    let walked = rewritten(&driven[0], |detail| {
        if detail.starts_with("SEARCH dv ") {
            "SCAN dv".to_string()
        } else {
            detail.to_string()
        }
    });
    failure_of("a driven section that walks the documents", || {
        judge_driven(&walked)
    });

    validating_store.drop_index("findings_fingerprint_kind_severity");
    let severity = pages(
        &validating_store,
        &validating().with_severity(Severity::Error),
    );
    failure_of("findings_fingerprint_kind_severity dropped", || {
        judge_kind_seek(
            &severity[0],
            "findings_fingerprint_kind_severity",
            "(vault_schema_fingerprint=? AND kind=? AND severity=? AND path>?)",
        )
    });
    validating_store.drop_index("findings_vault_schema_fingerprint");
    let unnarrowed = pages(&validating_store, &validating());
    failure_of("findings_vault_schema_fingerprint dropped", || {
        judge_kind_seek(
            &unnarrowed[0],
            "findings_vault_schema_fingerprint",
            KIND_SEEK,
        )
    });
}

/// The kind and severity index's seek of one `(kind, severity)` cell.
const SUMMARY_SEEK: &str = "(vault_schema_fingerprint=? AND kind=? AND severity=?)";

/// Judge a summary: a covering seek of the kind and severity index at each
/// `(kind, severity)` cell under `constraint`, grouped in the index's own
/// order, with nothing read end to end and nothing sorted.
fn judge_summary(summary: &QueryPlan, constraint: &str) {
    summary.assert_no_full_scan();
    summary.assert_no_temp_btree();
    let rows = rows_of(summary, "f");
    rows.assert_searches_through(
        "findings",
        Access::Index("findings_fingerprint_kind_severity"),
    );
    rows.assert_search_constraint("findings", constraint);
    assert!(
        rows.rows()
            .iter()
            .all(|row| row.detail.contains("COVERING INDEX")),
        "a summary read a finding's row: {:?}\nemitted SQL: {}",
        summary.rows(),
        summary.sql()
    );
}

/// **A summary aggregates over the kind and severity index.** Its tallies are
/// one seek of `findings_fingerprint_kind_severity` per `(kind, severity)`
/// cell it admits, grouped in the order the index holds, reading no finding's
/// row and sorting nothing; a path part bounds each cell's seek by its range.
/// A document part drives it as it drives a page, which the page bar judges.
///
/// Control: the index dropped, the summary reads something else.
#[test]
fn a_summary_aggregates_over_the_kind_and_severity_index() {
    let mut validating_store = Validating::new("validate-summary-plan");
    for (params, constraint) in [
        (validating(), SUMMARY_SEEK),
        (validating().with_severity(Severity::Error), SUMMARY_SEEK),
        (
            validating().with_kinds([FindingKind::UndeclaredTag]),
            SUMMARY_SEEK,
        ),
        (
            validating().with_predicates([Predicate::path("notes/**")]),
            "(vault_schema_fingerprint=? AND kind=? AND severity=? AND path>? AND path<?)",
        ),
    ] {
        let summary = plans_of(
            &validating_store.plans(&params.summarized()),
            ValidateStatement::Summary,
        );
        assert_eq!(summary.len(), 1);
        judge_summary(&summary[0], constraint);
    }
    validating_store.drop_index("findings_fingerprint_kind_severity");
    let summary = plans_of(
        &validating_store.plans(&validating().summarized()),
        ValidateStatement::Summary,
    );
    failure_of("findings_fingerprint_kind_severity dropped", || {
        judge_summary(&summary[0], SUMMARY_SEEK)
    });
}

// ---- the work bar ----

/// Judge a narrowed validate by what SQLite counted running it over two
/// vault sizes: the same work at both, and no step through a loop no
/// constraint bounds.
fn judge_narrow(small: &Validating, large: &Validating, params: &ValidateParams) {
    let (small, large) = (small.validate(params).work, large.validate(params).work);
    assert_eq!(
        small, large,
        "a narrowed validate's work grew with the vault: {params:?}"
    );
    assert_eq!(
        large.full_scan_steps, 0,
        "a narrowed validate stepped through a full scan: {large:?} for {params:?}"
    );
}

/// **A narrowing part narrows a validate's work to the findings it matches.**
/// Over the fixture and 50, then 500, more documents each with an
/// undeclared-tag warning standing over it, a validate narrowed to the
/// fixture's own findings — by kind, by severity, by a path part, by two path
/// parts whose ranges overlap only at the paths both admit, by a tag and by a
/// field's value on the finding's document — runs the same statements and the
/// same VM steps at both sizes, as a page and as a summary, and steps through
/// no full scan. **A warning floor admits every severity**, so it reads
/// exactly what a validate with no floor reads.
///
/// Control: `findings_fingerprint_kind_severity` dropped on the larger vault,
/// a severity floor reaches the warnings it does not admit, and the bar
/// fails.
#[test]
fn a_narrowing_part_narrows_a_validates_work_to_the_findings_it_matches() {
    let small = Validating::with_bulk("validate-work-small", 50);
    let mut large = Validating::with_bulk("validate-work-large", 500);
    let narrowing = [
        validating().with_kinds([FindingKind::PathNamesNoDocument]),
        validating().with_severity(Severity::Error),
        validating().with_predicates([Predicate::path("notes/**")]),
        // `b*.md` ranges over `bulk/`, and `broken.md` does not.
        validating().with_predicates([Predicate::path("b*.md"), Predicate::path("broken.md")]),
        validating().with_predicates([Predicate::tag("draft")]),
        validating().with_predicates([Predicate::equal_to("status", "open")]),
    ];
    for params in &narrowing {
        judge_narrow(&small, &large, params);
        judge_narrow(&small, &large, &params.clone().summarized());
    }
    for whole in [validating(), validating().summarized()] {
        assert_eq!(
            large
                .validate(&whole.clone().with_severity(Severity::Warning))
                .work,
            large.validate(&whole).work,
            "a warning floor read other than no floor does: {whole:?}"
        );
    }
    let whole = validating();
    assert!(
        large.validate(&whole.clone().summarized()).work.vm_steps
            > small.validate(&whole.summarized()).work.vm_steps * 4,
        "an unnarrowed summary's work did not grow with the findings it tallies"
    );

    large.drop_index("findings_fingerprint_kind_severity");
    failure_of("findings_fingerprint_kind_severity dropped", || {
        judge_narrow(&small, &large, &validating().with_severity(Severity::Error))
    });
}

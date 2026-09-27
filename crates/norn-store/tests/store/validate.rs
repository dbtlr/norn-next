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
    DOCUMENT_PAYLOAD, Scratch, assert_covers_every_driving_shape, document, driving_parts,
    narrowable, path_names_no_document_for_target, path_names_no_document_in_class, reads_of,
    unread_block, violation, write_documents,
};
use crate::find::{failure_of, map, rows_of, string};
use norn_store::{
    ContentModel, FindingFacts, PageRefusal, ReadStatement, Snapshot, SnapshotReader, Store,
    StoredPathOrder, TagFact, TagSource, VALIDATE_STATEMENTS, ValidatePlan, ValidateStatement,
    ValidateWork, Validated, Validation, induced_failure,
};
use norn_testkit::explain::{Access, PlanRow, QueryPlan};
use norn_wire::{
    Cursor, CursorKey, Direction, FindingKind, FindingRow, Hint, KindTally, PagedRows, Pattern,
    Predicate, ResolutionTarget, Severity, Sort, SortKey, Unsatisfied, ValidateParams,
    ValidateReport, VaultAddress, VaultName,
};

// ---- fixtures ----

/// The fingerprint of the schema the fixture pins.
const VALIDATE_SCHEMA: &str = "validate-schema";

fn declared() -> ContentModel {
    ContentModel::under(VALIDATE_SCHEMA).declare("status")
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
        path_names_no_document_in_class(
            "notes/c.md",
            "glossary",
            "glossary/",
            &["g/1.md", "g/2.md", "g/3.md", "g/4.md", "g/5.md"],
            7,
        ),
        path_names_no_document_for_target("a.md", "v1.2", &["v/v1.2.md"], 1),
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
        Self::with_bulk_under(label, bulk, StoredPathOrder::Sensitive)
    }

    /// The fixture in a store over a root proven to have `order`'s case
    /// behaviour, which every snapshot of it reads under.
    fn under(label: &str, order: StoredPathOrder) -> Self {
        Self::with_bulk_under(label, 0, order)
    }

    /// [`Validating::with_bulk`] in a store over a root proven to have
    /// `order`'s case behaviour.
    fn with_bulk_under(label: &str, bulk: usize, order: StoredPathOrder) -> Self {
        let scratch = Scratch::new(label);
        let mut store = scratch.open_under(order);
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

    /// Stand `finding` over the seeded vault. The suite orders findings about
    /// links and of link-health kinds beside a caller's, and those are the
    /// store's own to file, so every finding stands through the fenced door,
    /// which writes each as the store files one.
    fn stand(&mut self, finding: &FindingFacts) {
        induced_failure::record_finding_out_of_band(&mut self.store, finding)
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

/// Every finding in the fixture, in `(kind, path, position, id)` order. Each
/// is about its document, so two at one path share a position and stand by
/// id.
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
/// answers, in `(kind, path, position, id)` order**: kinds in the byte order
/// of their codes, a kind's findings by path, and two findings of one kind at
/// one path, both about the document and so at one position, by id.
/// A finding stands whether or not a document row stands at its path.
#[test]
fn with_no_predicates_every_finding_standing_answers_in_kind_path_position_id_order() {
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

/// **A path part judges the finding's path under the root's order.** On a
/// root that folds ASCII case, `BROKEN.MD` finds the finding standing at
/// `broken.md` where no document row does, and `NOTES/**` every finding under
/// `notes/`, as a page and as a summary; on a root that tells spellings apart
/// the same globs find nothing, and the globs spelled as the paths are find
/// the same findings on either root.
#[test]
fn a_path_part_judges_the_findings_path_under_the_roots_order() {
    let sensitive = Validating::under("validate-path-case", StoredPathOrder::Sensitive);
    let folding = Validating::under(
        "validate-path-case-folded",
        StoredPathOrder::AsciiCaseInsensitive,
    );
    let every = every_finding();
    let narrowed = |glob: &str| validating().with_predicates([Predicate::path(glob)]);
    for (upper, lower, expected) in [
        ("BROKEN.MD", "broken.md", vec![every[0].clone()]),
        (
            "NOTES/**",
            "notes/**",
            vec![every[1].clone(), every[3].clone()],
        ),
    ] {
        for validating_store in [&sensitive, &folding] {
            assert_eq!(names(&validating_store.rows(&narrowed(lower))), expected);
        }
        assert_eq!(names(&folding.rows(&narrowed(upper))), expected, "{upper}");
        assert_eq!(
            folding.summary(&narrowed(upper)),
            sensitive.summary(&narrowed(lower)),
            "{upper}"
        );
        assert!(sensitive.rows(&narrowed(upper)).is_empty(), "{upper}");
        assert!(sensitive.summary(&narrowed(upper)).is_empty(), "{upper}");
    }
}

/// **A path part ranges over its glob's folded prefix where the root tells
/// spellings apart.** Findings at `notes/Z.md` and `notes/Za.md` stand in the
/// answer's path order, where `notes/z` folds above `notes/[`, so `notes/Z*`
/// admits both, as a page, drained a finding at a time, and as a summary. A
/// range bounded bytewise, `["notes/Z", "notes/[")`, holds no path the folded
/// order reaches.
#[test]
fn a_path_part_ranges_over_its_folded_prefix_where_the_root_tells_spellings_apart() {
    let mut validating_store =
        Validating::under("validate-sensitive-range", StoredPathOrder::Sensitive);
    for at in ["notes/Z.md", "notes/Za.md"] {
        validating_store.stand(&violation(at));
    }
    let expected = vec![
        finding(FindingKind::BodyBytesNotUtf8, "notes/Z.md", None),
        finding(FindingKind::BodyBytesNotUtf8, "notes/Za.md", None),
    ];
    let narrowed = validating().with_predicates([Predicate::path("notes/Z*")]);
    assert_eq!(names(&validating_store.rows(&narrowed)), expected);
    assert_eq!(names(&drained(&validating_store, &narrowed, 1)), expected);
    assert_eq!(
        tallied(&validating_store.summary(&narrowed)),
        vec![(FindingKind::BodyBytesNotUtf8, Severity::Error, 2)]
    );
}

/// **A document part matches a finding to the document at its exact path.**
/// On a root that tells spellings apart, `a.md` holds `status: open` and
/// `A.md` holds `status: closed`, and a finding stands at `A.md`. The two
/// paths fold together, and the equality admits `a.md` alone, so a page and a
/// summary narrowed by it answer the findings on `a.md` and none on `A.md`.
#[test]
fn a_document_part_matches_a_finding_to_the_document_at_its_exact_path() {
    let mut validating_store =
        Validating::under("validate-exact-document", StoredPathOrder::Sensitive);
    let closed = document("A.md", "hash-A", "a body\n")
        .with_frontmatter(Some(map(vec![("status", string("closed"))])), &declared());
    let mut request = validating_store.store.begin_request();
    write_documents(&mut request, &[closed]);
    request
        .record_finding(&violation("A.md"))
        .expect("recording a finding");
    let every = every_finding();
    let expected = vec![
        every[2].clone(),
        every[3].clone(),
        every[4].clone(),
        every[5].clone(),
    ];
    let narrowed = validating().with_predicates([Predicate::equal_to("status", "open")]);
    let rows = validating_store.rows(&narrowed);
    assert_eq!(names(&rows), expected);
    assert_eq!(
        tallied(&validating_store.summary(&narrowed)),
        tallies_of(&rows)
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
        "kinds are read in the byte order of their codes, each once, whatever the request's \
         order"
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
/// finding's kind, path, ordinal and id, inside a kind and across kinds, among two
/// findings of one kind at one path, and under every narrowing. The page
/// stops at its last finding, **and reads one finding past its bound** to
/// learn a next page exists: a page of two with more behind it reads three.
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
            None,
            rows[1].id
        )),
        "a page stops at its last finding's kind, path, ordinal and id"
    );
    let read = validating_store
        .validate(&validating().with_limit(2))
        .work
        .rows_read;
    assert_eq!(
        read, 3,
        "a page of two with a next page reads one past its bound"
    );
}

/// **A validate's drain resumes between two findings at one path**, on
/// either root. Five `link/broken` findings stand at `a.md`, filed about link
/// 3, the document, link 1, the document again and link 2. The two about the
/// document stand first, by id, and then links 1, 2 and 3. Drained one, two
/// and three at a time, the validate answers each once in that order: a
/// cursor resumes past its finding's ordinal as well as its path and id, from
/// a finding about the document and from one about a link, and it carries
/// that ordinal.
#[test]
fn a_validate_drain_resumes_between_two_findings_of_one_path() {
    for order in ROOTS {
        let mut validating_store = Validating::under("validate-one-path-drain", order);
        for (ordinal, target) in [
            (Some(3), "three"),
            (None, "document"),
            (Some(1), "one"),
            (None, "again"),
            (Some(2), "two"),
        ] {
            let mut finding = undeclared("a.md", target);
            finding.kind = FindingKind::Broken;
            finding.ordinal = ordinal;
            validating_store.stand(&finding);
        }
        let broken = validating().with_kinds([FindingKind::Broken]);
        let expected: Vec<(FindingKind, String, Option<String>)> =
            ["document", "again", "one", "two", "three"]
                .into_iter()
                .map(|target| finding(FindingKind::Broken, "a.md", Some(target)))
                .collect();
        assert_eq!(
            names(&validating_store.rows(&broken)),
            expected,
            "one page under {order:?}"
        );
        for limit in [1, 2, 3] {
            assert_eq!(
                names(&drained(&validating_store, &broken, limit)),
                expected,
                "drained {limit} at a time under {order:?}"
            );
        }
        let (rows, next) = validating_store.page(&broken.clone().with_limit(3));
        assert_eq!(
            next.as_ref().map(Cursor::key),
            Some(&CursorKey::finding(
                FindingKind::Broken,
                "a.md",
                Some(1),
                rows[2].id
            )),
            "a page stops at its last finding's ordinal under {order:?}"
        );
    }
}

/// A finding row's place in a page, computed here rather than read off the
/// store: its kind's code, its path with ASCII case folded, its path bytewise,
/// and its id. The position between the path and the id is left out, since
/// every finding it places is about its document.
fn folded_place(row: &FindingRow) -> (&'static str, String, String, u64) {
    (
        row.kind.as_str(),
        row.path.as_str().to_ascii_lowercase(),
        row.path.as_str().to_string(),
        row.id,
    )
}

/// **On either root, a validate answers every finding in folded `(kind, path,
/// id)` order, and a drain answers each once.** The findings stand at paths
/// whose folded and bytewise orders differ: `Z` sorts below `[`, `_` and `` `
/// `` bytewise and above them folded, and `Notes/` and `NOTES/` sort with
/// `notes/` folded. `notes/B.md` and `notes/b.md` fold together, so the
/// bytewise path breaks their tie ahead of the id, which is recorded the other
/// way round; `NOTES/b.md` and `notes/b.MD` fold with them too. The
/// unnarrowed validate and each path part — `NOTES/**`, `notes/**`, and the
/// literal `notes/b.md`, whose range opens at the least path folding to it,
/// `NOTES/b.md` bytewise — whole and drained one, two and three findings at a
/// time, answer in the order this test computes from each row's kind, folded
/// path, path and id, with no finding skipped and none repeated. A part
/// admits the paths its glob matches under the root's own fold: all four
/// spellings of `notes/b.md` where the root folds, and the one written where
/// it tells spellings apart.
#[test]
fn a_drain_answers_each_finding_once_in_folded_order_on_either_root() {
    for order in [
        StoredPathOrder::Sensitive,
        StoredPathOrder::AsciiCaseInsensitive,
    ] {
        drain_in_folded_order(order);
    }
}

/// [`a_drain_answers_each_finding_once_in_folded_order_on_either_root`] on a
/// root with `order`'s case behaviour.
fn drain_in_folded_order(order: StoredPathOrder) {
    let mut validating_store = Validating::under("validate-folded-drain", order);
    let mixed = [
        "Notes/A.md",
        "notes/_x.md",
        "notes/[y.md",
        "notes/`b.md",
        "notes/b.md",
        "notes/B.md",
        "NOTES/b.md",
        "notes/b.MD",
        "notes/Z.md",
    ];
    for at in mixed {
        validating_store.stand(&violation(at));
    }
    validating_store.stand(&undeclared("notes/Z.md", "one"));
    validating_store.stand(&undeclared("notes/Z.md", "two"));
    let whole = validating_store.rows(&validating().with_limit(1000));
    assert_eq!(whole.len(), every_finding().len() + mixed.len() + 2);
    let mut expected = whole.clone();
    expected.sort_by_key(folded_place);
    let violations: Vec<&str> = expected
        .iter()
        .filter(|row| row.kind == FindingKind::BodyBytesNotUtf8)
        .map(|row| row.path.as_str())
        .collect();
    assert_eq!(
        violations,
        [
            "broken.md",
            "notes/[y.md",
            "notes/_x.md",
            "notes/`b.md",
            "Notes/A.md",
            "NOTES/b.md",
            "notes/B.md",
            "notes/b.MD",
            "notes/b.md",
            "notes/Z.md"
        ],
        "the order this test computes is the folded one"
    );
    let mut judged = vec![(validating(), expected.clone())];
    for glob in ["NOTES/**", "notes/**", "notes/b.md"] {
        let pattern = Pattern::parse(glob).expect("a glob");
        let admitted: Vec<FindingRow> = expected
            .iter()
            .filter(|row| pattern.matches(row.path.as_str(), order.glob_case()))
            .cloned()
            .collect();
        assert!(!admitted.is_empty(), "{glob} admits a finding");
        judged.push((
            validating().with_predicates([Predicate::path(glob)]),
            admitted,
        ));
    }
    let literal = judged.last().expect("the literal part").1.len();
    assert_eq!(
        literal,
        match order {
            StoredPathOrder::Sensitive => 1,
            StoredPathOrder::AsciiCaseInsensitive => 4,
        },
        "`notes/b.md` admits its spellings under {order:?}"
    );
    for (params, expected) in judged {
        assert_eq!(
            validating_store.rows(&params.clone().with_limit(1000)),
            expected,
            "{params:?} in one page under {order:?}"
        );
        for limit in [1, 2, 3] {
            let rows = drained(&validating_store, &params, limit);
            let mut ids: Vec<u64> = rows.iter().map(|row| row.id).collect();
            ids.sort_unstable();
            ids.dedup();
            assert_eq!(
                ids.len(),
                rows.len(),
                "a drain of {limit} repeated a finding: {params:?} under {order:?}"
            );
            assert_eq!(
                names(&rows),
                names(&expected),
                "{params:?} drained {limit} at a time under {order:?}"
            );
            assert_eq!(
                rows, expected,
                "{params:?} drained {limit} at a time under {order:?}"
            );
        }
    }
}

/// **A finding row carries the bounded head the pillar stores, the total it
/// heads, and the hint that names the `find` enumerating its class.** The
/// fixture's two resolution findings stand under a kind that is not
/// `link/ambiguous`, and each is recorded again as an ambiguous link's
/// finding at `notes/d.md`. The `glossary` finding heads five of seven
/// candidates in the order they were recorded; the ambiguous one hints
/// `glossary`, and the other kind hints nothing. The ambiguous `v1.2`
/// finding is in two classes, and hints the address whose probe opens both.
/// A finding in no class carries an empty head of none and no hint.
#[test]
fn a_finding_row_carries_its_bounded_head_its_total_and_its_hint() {
    let mut validating_store = Validating::new("validate-head");
    for (target, mut finding) in [
        (
            "glossary",
            path_names_no_document_in_class(
                "notes/d.md",
                "glossary",
                "glossary/",
                &["g/1.md", "g/2.md", "g/3.md", "g/4.md", "g/5.md"],
                7,
            ),
        ),
        (
            "v1.2",
            path_names_no_document_for_target("notes/d.md", "v1.2", &["v/v1.2.md", "v/v1.md"], 2),
        ),
    ] {
        finding.kind = FindingKind::Ambiguous;
        finding.target = Some(target.to_string());
        validating_store.stand(&finding);
    }
    let rows = validating_store.rows(&validating());
    let by_target: BTreeMap<(&str, Option<String>), &FindingRow> = rows
        .iter()
        .map(|row| ((row.kind.as_str(), row.target.clone()), row))
        .collect();
    let target = |text: &str| ResolutionTarget::new(text).expect("a target");
    let read = |kind: FindingKind, text: &str| by_target[&(kind.as_str(), Some(text.to_string()))];
    for kind in [FindingKind::Ambiguous, FindingKind::PathNamesNoDocument] {
        let glossary = read(kind, "glossary");
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
    }
    assert_eq!(
        read(FindingKind::Ambiguous, "glossary").hint,
        Some(Hint::resolves(target("glossary")))
    );
    assert_eq!(
        read(FindingKind::PathNamesNoDocument, "glossary").hint,
        None
    );
    let dotted = read(FindingKind::Ambiguous, "v1.2");
    assert_eq!(dotted.hint, Some(Hint::resolves(target("v1.2"))));
    assert_eq!(dotted.head.total(), 2);
    assert_eq!(read(FindingKind::PathNamesNoDocument, "v1.2").hint, None);

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
    let (_, _, report) = validating_store
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
/// refused**: a document's, and a document finding's, which a get's page of
/// one document's findings mints in an order that is not a validate's.
#[test]
fn a_cursor_that_is_no_position_among_the_findings_is_refused() {
    let validating_store = Validating::new("validate-cursor");
    let reading = validating_store.validate(&validating()).snapshot;
    for (key, rows) in [
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
            validating_store
                .snapshot()
                .validate(
                    &validating().with_after(Cursor::new(reading.clone(), key)),
                    &declared()
                )
                .expect_err("the cursor is refused"),
            PageRefusal::CursorNotTaken {
                cursor: rows,
                paged: PagedRows::Finding,
            }
        );
    }
    assert_eq!(
        PageRefusal::CursorNotTaken {
            cursor: PagedRows::Document,
            paged: PagedRows::Finding,
        }
        .to_string(),
        "the cursor names a position among documents, and the request pages findings"
    );
}

/// **A finding's cursor whose id or ordinal is past what the store counts is
/// refused** as no position among the findings.
#[test]
fn a_finding_cursor_past_what_the_store_counts_is_refused() {
    let validating_store = Validating::new("validate-cursor-past");
    let reading = validating_store.validate(&validating()).snapshot;
    let past = u64::try_from(i64::MAX).expect("a positive bound") + 1;
    for key in [
        CursorKey::finding(FindingKind::UndeclaredTag, "a.md", None, past),
        CursorKey::finding(FindingKind::UndeclaredTag, "a.md", Some(past), 1),
    ] {
        assert_eq!(
            validating_store
                .snapshot()
                .validate(
                    &validating().with_after(Cursor::new(reading.clone(), key.clone())),
                    &declared()
                )
                .expect_err("the cursor is refused"),
            PageRefusal::CursorNotTaken {
                cursor: PagedRows::Finding,
                paged: PagedRows::Finding,
            },
            "{key:?}"
        );
    }
}

/// **A finding's cursor carrying a schema fingerprint is refused as not
/// taken**: no page of findings mints one, so it names no position among
/// them, even under the fingerprint the snapshot pins.
#[test]
fn a_finding_cursor_carrying_a_fingerprint_is_not_taken() {
    let validating_store = Validating::new("validate-cursor-fingerprint");
    let reading = validating_store.validate(&validating()).snapshot;
    let forged = Cursor::new(
        norn_wire::Snapshot::new(
            reading.epoch.clone(),
            reading.generation,
            Some(VALIDATE_SCHEMA.to_string()),
            None,
        ),
        CursorKey::finding(FindingKind::UndeclaredTag, "a.md", None, 1),
    );
    assert_eq!(
        validating_store
            .snapshot()
            .validate(&validating().with_after(forged), &declared())
            .expect_err("the cursor is refused"),
        PageRefusal::CursorNotTaken {
            cursor: PagedRows::Finding,
            paged: PagedRows::Finding,
        }
    );
}

/// **A declaration read from another schema than the snapshot pins is
/// refused**, as every read refuses it, by a page and a summary alike: one
/// read from another schema, and the declaration of a store with none, each
/// named against the schema the snapshot pins.
#[test]
fn a_declaration_the_snapshot_does_not_pin_is_refused() {
    let validating_store = Validating::new("validate-declaration");
    for (declared, declared_under) in [
        (
            ContentModel::under("another-schema").declare("status"),
            Some("another-schema".to_string()),
        ),
        (ContentModel::none(), None),
    ] {
        for params in [validating(), validating().summarized()] {
            assert_eq!(
                validating_store
                    .snapshot()
                    .validate(&params, &declared)
                    .expect_err("the declaration is not the pinned one"),
                PageRefusal::DeclarationNotPinned {
                    declared_under: declared_under.clone(),
                    pinned: Some(VALIDATE_SCHEMA.to_string()),
                },
                "{params:?}"
            );
        }
    }
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

/// The index a page seeks a kind's findings through, in the answer's path
/// order: the path folded, then bytewise.
const KIND_INDEX: &str = "findings_fingerprint_kind_nocase";

/// That index's seek from a page's position: the folded path, the path
/// bytewise, the finding's position among its path's findings, then the id.
const KIND_SEEK: &str =
    "(vault_schema_fingerprint=? AND kind=? AND (path,path,position,rowid)>(?,?,?,?))";

/// The same seek bounded above by a path part's folded range.
const KIND_RANGE_SEEK: &str =
    "(vault_schema_fingerprint=? AND kind=? AND (path,path,position,rowid)>(?,?,?,?) AND path<?)";

/// The index a page admitting one severity seeks its kind's findings through,
/// and a summary its `(kind, severity)` cells.
const SEVERITY_INDEX: &str = "findings_fingerprint_kind_severity_nocase";

/// That index's seek of one kind at one severity from a page's position.
const SEVERITY_SEEK: &str = "(vault_schema_fingerprint=? AND kind=? AND severity=? AND \
     (path,path,position,rowid)>(?,?,?,?))";

/// Both roots, each bar judged on a store over each.
const ROOTS: [StoredPathOrder; 2] = [
    StoredPathOrder::Sensitive,
    StoredPathOrder::AsciiCaseInsensitive,
];

/// **A kind page seeks its kind from the page's position, on either root.**
/// Each section is one seek of `findings_fingerprint_kind_nocase` at
/// `(fingerprint, kind)` bounded below by the page's position — the folded
/// path, the path bytewise, the finding's position among its path's findings,
/// then the id — on a first page and a continuation alike, and its findings
/// come off the index in `(path COLLATE NOCASE, path, position, id)` order, so
/// nothing sorts. **A path part bounds the same seek** by its
/// glob's folded range, whether its glob matches bytes or folds, and in
/// either case. **A severity floor admitting one severity seeks
/// `findings_fingerprint_kind_severity_nocase` at `(fingerprint, kind,
/// severity)`** the same way, so a finding of another severity is never
/// reached. **A document part that keeps what it seeks drives the section**
/// from the documents it matched, each reaching its findings by one seek at
/// its path.
///
/// Controls: a continuation's plan rebuilt without its position bound fails,
/// and so does one whose seek stops at the path, short of the finding's
/// position; a driven section rebuilt to seek its kind, or to walk the
/// documents, fails; each index dropped, the sections it served read something
/// else.
#[test]
fn a_kind_page_seeks_its_kind_from_the_pages_position() {
    for order in ROOTS {
        judge_kind_pages(Validating::under("validate-page-plan", order));
    }
}

/// [`a_kind_page_seeks_its_kind_from_the_pages_position`] on one store.
fn judge_kind_pages(mut validating_store: Validating) {
    let pages = |validating_store: &Validating, params: &ValidateParams| {
        plans_of(&validating_store.plans(params), ValidateStatement::KindPage)
    };
    let continuing = |validating_store: &Validating, params: &ValidateParams| {
        let (_, next) = validating_store.page(&params.clone().with_limit(1));
        params.clone().with_after(next.expect("a next page"))
    };
    for page in pages(&validating_store, &validating()) {
        judge_kind_seek(&page, KIND_INDEX, KIND_SEEK);
    }
    let (first, next) = validating_store.page(&validating().with_limit(3));
    let continued = pages(
        &validating_store,
        &validating().with_after(next.expect("a next page")),
    );
    let mut registry: Vec<&str> = FindingKind::ALL.iter().map(|kind| kind.as_str()).collect();
    registry.sort_unstable();
    let resumed_in = first.last().expect("a first page holds rows").kind.as_str();
    let from = registry
        .iter()
        .position(|kind| *kind == resumed_in)
        .expect("the resumed kind is in the registry");
    assert_eq!(
        continued.len(),
        registry.len() - from,
        "a continuation reads the kind it resumes in and every kind after it in the \
         registry's order"
    );
    for page in &continued {
        judge_kind_seek(page, KIND_INDEX, KIND_SEEK);
    }
    let severity = validating().with_severity(Severity::Error);
    for params in [severity.clone(), continuing(&validating_store, &severity)] {
        for page in pages(&validating_store, &params) {
            judge_kind_seek(&page, SEVERITY_INDEX, SEVERITY_SEEK);
        }
    }
    for page in pages(
        &validating_store,
        &severity
            .clone()
            .with_predicates([Predicate::path("NOTES/**")]),
    ) {
        judge_kind_seek(
            &page,
            SEVERITY_INDEX,
            "(vault_schema_fingerprint=? AND kind=? AND severity=? AND \
             (path,path,position,rowid)>(?,?,?,?) AND path<?)",
        );
    }
    for glob in ["notes/**", "*.md"] {
        let ranged = validating().with_predicates([Predicate::path(glob)]);
        for params in [ranged.clone(), continuing(&validating_store, &ranged)] {
            for page in pages(&validating_store, &params) {
                judge_kind_seek(&page, KIND_INDEX, KIND_RANGE_SEEK);
            }
        }
    }
    for glob in ["NOTES/**", "*.MD"] {
        for page in pages(
            &validating_store,
            &validating().with_predicates([Predicate::path(glob)]),
        ) {
            judge_kind_seek(&page, KIND_INDEX, KIND_RANGE_SEEK);
        }
    }
    for page in pages(
        &validating_store,
        &validating().with_predicates([Predicate::path("*.MD"), Predicate::path("B*")]),
    ) {
        judge_kind_seek(&page, KIND_INDEX, KIND_RANGE_SEEK);
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
        validating()
            .with_severity(Severity::Error)
            .with_predicates([Predicate::tag("draft"), Predicate::path("*.MD")]),
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
                .contains(&format!("COVERING INDEX {SEVERITY_INDEX}"))),
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
        judge_kind_seek(&page, KIND_INDEX, KIND_SEEK);
        rows_of(&page, "dv").assert_searches_through("documents", Access::Index("documents_path"));
    }

    // Control: the continuation's position taken out of the seek.
    let unbounded = rewritten(&continued[0], |detail| {
        detail.replace(" AND (path,path,position,rowid)>(?,?,?,?)", "")
    });
    failure_of("a continuation that seeks from no position", || {
        judge_kind_seek(&unbounded, KIND_INDEX, KIND_SEEK)
    });
    // Control: the continuation's seek stopped at the path.
    let short = rewritten(&continued[0], |detail| {
        detail.replace("(path,path,position,rowid)>(?,?,?,?)", "(path,path)>(?,?)")
    });
    failure_of("a continuation that seeks to its path alone", || {
        judge_kind_seek(&short, KIND_INDEX, KIND_SEEK)
    });
    // Control: a driven section that seeks its kind from a position.
    let undriven = rewritten(&driven[0], |detail| {
        if detail.starts_with("SEARCH f ") {
            format!("SEARCH f USING INDEX {KIND_INDEX} {KIND_SEEK}")
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

    validating_store.drop_index(SEVERITY_INDEX);
    let page = pages(&validating_store, &severity);
    failure_of(&format!("{SEVERITY_INDEX} dropped"), || {
        judge_kind_seek(&page[0], SEVERITY_INDEX, SEVERITY_SEEK)
    });
    validating_store.drop_index(KIND_INDEX);
    for (params, constraint) in [
        (validating(), KIND_SEEK),
        (
            validating().with_predicates([Predicate::path("NOTES/**")]),
            KIND_RANGE_SEEK,
        ),
    ] {
        let page = pages(&validating_store, &params);
        failure_of(&format!("{KIND_INDEX} dropped, {params:?}"), || {
            judge_kind_seek(&page[0], KIND_INDEX, constraint)
        });
    }
}

/// The page `params` continues to after `pages` pages of `limit`, and what
/// that page cost.
fn paged_work(
    validating_store: &Validating,
    params: &ValidateParams,
    limit: u32,
    pages: usize,
) -> ValidateWork {
    let mut after: Option<Cursor> = None;
    for _ in 0..pages {
        let mut page = params.clone().with_limit(limit);
        if let Some(cursor) = after.take() {
            page = page.with_after(cursor);
        }
        after = Some(
            validating_store
                .page(&page)
                .1
                .expect("the drain reaches the page it is judged at"),
        );
    }
    let mut page = params.clone().with_limit(limit);
    if let Some(cursor) = after {
        page = page.with_after(cursor);
    }
    validating_store.validate(&page).work
}

/// **A page of a broad range costs the page, not the range, on either
/// root.** Over the fixture and 50, then 500, more documents each with a
/// warning standing over it, a page of five undeclared-tag findings —
/// unnarrowed, and narrowed by `**/*.md` and `**`, which match every finding,
/// and where the root folds by `**/*.MD`, which matches every finding there —
/// costs the same at both sizes on its first page and on the pages that
/// continue it into the bulk, sorts nothing and steps through no full scan.
///
/// Control: the kind index dropped on the larger vault, a page of the range
/// reaches the whole range, and the bar fails.
#[test]
fn a_page_of_a_broad_range_costs_the_page_not_the_range() {
    for order in ROOTS {
        let small = Validating::with_bulk_under("validate-page-small", 50, order);
        let mut large = Validating::with_bulk_under("validate-page-large", 500, order);
        let tags = || validating().with_kinds([FindingKind::UndeclaredTag]);
        let mut broad = vec![
            tags(),
            tags().with_predicates([Predicate::path("**/*.md")]),
            tags().with_predicates([Predicate::path("**")]),
        ];
        if order == StoredPathOrder::AsciiCaseInsensitive {
            broad.push(tags().with_predicates([Predicate::path("**/*.MD")]));
        }
        let judge = |large: &Validating, params: &ValidateParams| {
            for pages in [0, 1, 4] {
                let (at_small, at_large) = (
                    paged_work(&small, params, 5, pages),
                    paged_work(large, params, 5, pages),
                );
                assert_eq!(
                    at_small, at_large,
                    "page {pages} of {params:?} grew with the vault under {order:?}"
                );
                assert_eq!(
                    (at_large.sorts, at_large.full_scan_steps),
                    (0, 0),
                    "page {pages} of {params:?} sorted or scanned under {order:?}: {at_large:?}"
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

/// **A continuation among the findings at one path costs its page, on either
/// root.** 500 undeclared-tag warnings stand at `same.md`, so the answer
/// order holds them as one run of equal paths, broken by id. Paged five at a
/// time, a page ten, fifty and ninety pages into the run does the work the
/// page one page in does. A seek that stopped at the path would reread the
/// run's earlier findings on every page.
#[test]
fn a_continuation_among_the_findings_at_one_path_costs_its_page() {
    for order in ROOTS {
        let mut validating_store = Validating::under("validate-one-path-run", order);
        for at in 0..500 {
            validating_store.stand(&undeclared("same.md", &format!("tag-{at:03}")));
        }
        let tags = validating().with_kinds([FindingKind::UndeclaredTag]);
        let work: Vec<ValidateWork> = [1, 10, 50, 90]
            .into_iter()
            .map(|pages| paged_work(&validating_store, &tags, 5, pages))
            .collect();
        assert!(
            work.iter().all(|at| *at == work[0]),
            "pages 1, 10, 50 and 90 cost {:?} VM steps under {order:?}: {work:?}",
            work.iter().map(|at| at.vm_steps).collect::<Vec<u64>>()
        );
    }
}

/// The kind and severity index's seek of one `(kind, severity)` cell.
const SUMMARY_SEEK: &str = "(vault_schema_fingerprint=? AND kind=? AND severity=?)";

/// The same cell's seek over a path part's folded range.
const SUMMARY_RANGE_SEEK: &str =
    "(vault_schema_fingerprint=? AND kind=? AND severity=? AND path>? AND path<?)";

/// Judge a summary: a covering seek of the kind and severity index at each
/// `(kind, severity)` cell under `constraint`, grouped in the index's own
/// order, with nothing read end to end and nothing sorted.
fn judge_summary(summary: &QueryPlan, constraint: &str) {
    summary.assert_no_full_scan();
    summary.assert_no_temp_btree();
    let rows = rows_of(summary, "f");
    rows.assert_searches_through("findings", Access::Index(SEVERITY_INDEX));
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

/// **A summary aggregates over the kind and severity index, on either
/// root.** Its tallies are one seek of
/// `findings_fingerprint_kind_severity_nocase` per `(kind, severity)` cell it
/// admits, grouped in the order the index holds, reading no finding's row and
/// sorting nothing; a path part bounds each cell's seek by its glob's folded
/// range. A document part drives it as it drives a page, which the page bar
/// judges.
///
/// Control: the index dropped, the summary reads something else.
#[test]
fn a_summary_aggregates_over_the_kind_and_severity_index() {
    for order in ROOTS {
        let mut validating_store = Validating::under("validate-summary-plan", order);
        for (params, constraint) in [
            (validating(), SUMMARY_SEEK),
            (validating().with_severity(Severity::Error), SUMMARY_SEEK),
            (
                validating().with_kinds([FindingKind::UndeclaredTag]),
                SUMMARY_SEEK,
            ),
            (
                validating().with_predicates([Predicate::path("notes/**")]),
                SUMMARY_RANGE_SEEK,
            ),
            (
                validating().with_predicates([Predicate::path("*.MD")]),
                SUMMARY_RANGE_SEEK,
            ),
            (
                validating()
                    .with_severity(Severity::Error)
                    .with_predicates([Predicate::path("NOTES/**")]),
                SUMMARY_RANGE_SEEK,
            ),
        ] {
            let summary = plans_of(
                &validating_store.plans(&params.summarized()),
                ValidateStatement::Summary,
            );
            assert_eq!(summary.len(), 1);
            judge_summary(&summary[0], constraint);
        }
        validating_store.drop_index(SEVERITY_INDEX);
        for (params, constraint) in [
            (validating(), SUMMARY_SEEK),
            (
                validating().with_predicates([Predicate::path("NOTES/**")]),
                SUMMARY_RANGE_SEEK,
            ),
        ] {
            let summary = plans_of(
                &validating_store.plans(&params.summarized()),
                ValidateStatement::Summary,
            );
            failure_of(&format!("{SEVERITY_INDEX} dropped under {order:?}"), || {
                judge_summary(&summary[0], constraint)
            });
        }
    }
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

/// **A narrowing part narrows a validate's work to the findings it matches,
/// on either root.** Over the fixture and 50, then 500, more documents each
/// with an undeclared-tag warning standing over it, a validate narrowed to the
/// fixture's own findings — by kind, by severity, by a path part in either
/// case, by two path parts whose ranges overlap only at the paths both admit,
/// by a tag and by a field's value on the finding's document — runs the same
/// statements and the same VM steps at both sizes, as a page and as a
/// summary, and steps through no full scan. A path part's range is its glob's
/// folded range on either root, so where the root tells spellings apart an
/// upper-case glob reads the lower-case findings in its range and admits
/// none. **A warning floor admits every severity**, so it reads exactly what a
/// validate with no floor reads.
///
/// Controls: each index dropped on the larger vault, the narrowing it served
/// reaches findings it does not admit, and the bar fails.
#[test]
fn a_narrowing_part_narrows_a_validates_work_to_the_findings_it_matches() {
    for order in ROOTS {
        let small = Validating::with_bulk_under("validate-work-small", 50, order);
        let mut large = Validating::with_bulk_under("validate-work-large", 500, order);
        let narrowing = [
            validating().with_kinds([FindingKind::PathNamesNoDocument]),
            validating().with_severity(Severity::Error),
            validating().with_predicates([Predicate::path("notes/**")]),
            validating().with_predicates([Predicate::path("NOTES/**")]),
            // `b*.md` ranges over `bulk/`, and `broken.md` does not.
            validating().with_predicates([Predicate::path("b*.md"), Predicate::path("broken.md")]),
            validating().with_predicates([Predicate::path("B*.MD"), Predicate::path("BROKEN.md")]),
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

        large.drop_index(SEVERITY_INDEX);
        failure_of(&format!("{SEVERITY_INDEX} dropped under {order:?}"), || {
            judge_narrow(&small, &large, &validating().with_severity(Severity::Error))
        });
        failure_of(
            &format!("{SEVERITY_INDEX} dropped under {order:?}, a summary"),
            || judge_narrow(&small, &large, &narrowing[3].clone().summarized()),
        );
        large.drop_index(KIND_INDEX);
        failure_of(&format!("{KIND_INDEX} dropped under {order:?}"), || {
            judge_narrow(&small, &large, &narrowing[3])
        });
    }
}

/// **Every part that keeps what it seeks narrows a validate.** Over the
/// fixture, [`narrowable`] with a violation standing over it, and 50, then
/// 500, bulk documents each with a warning standing over it, a validate
/// narrowed by one part of each shape that keeps what it seeks narrows every
/// page and summary statement by that shape, runs the same statements and
/// the same VM steps at both sizes, as a page and as a summary, and steps
/// through no full scan.
#[test]
fn every_driving_part_narrows_a_validates_work_to_the_findings_it_admits() {
    let table = driving_parts();
    assert_covers_every_driving_shape(&table);
    let with_narrowable = |label: &str, bulk: usize, order: StoredPathOrder| {
        let mut validating_store = Validating::with_bulk_under(label, bulk, order);
        let mut request = validating_store.store.begin_request();
        write_documents(&mut request, &[narrowable()]);
        request
            .record_finding(&violation("narrowed.md"))
            .expect("recording a finding");
        validating_store
    };
    for order in ROOTS {
        let small = with_narrowable("validate-driving-small", 50, order);
        let large = with_narrowable("validate-driving-large", 500, order);
        for (shape, part) in &table {
            let narrowed = validating().with_predicates([part.clone()]);
            for params in [narrowed.clone(), narrowed.summarized()] {
                for plan in small.plans(&params) {
                    if matches!(plan.statement, ReadStatement::Validate(_)) {
                        assert!(
                            plan.filters
                                .iter()
                                .any(|filter| filter.slot() == shape.slot()),
                            "a {shape:?} part did not narrow {:?}: {:?}",
                            plan.statement,
                            plan.filters
                        );
                    }
                }
                let (at_small, at_large) =
                    (small.validate(&params).work, large.validate(&params).work);
                assert_eq!(
                    at_small, at_large,
                    "a validate narrowed by a {shape:?} part grew with the vault under \
                     {order:?}: {params:?}"
                );
                assert_eq!(
                    at_large.full_scan_steps, 0,
                    "a validate narrowed by a {shape:?} part stepped through a full scan under \
                     {order:?}: {at_large:?}"
                );
            }
        }
    }
}

// ---- the payload bar ----

/// **No statement a validate runs reads a document's payload.** Every
/// statement a page of findings and a summary emit — the kind page, the
/// summary, the probes the conjunction's compilation runs and the reads of
/// each finding row's head and classes — under every narrowing the drain
/// reads, a continuation, and a document part driving each, reads none of
/// [`crate::common::DOCUMENT_PAYLOAD`] as SQLite's authorizer reports the
/// columns it reads: so what a page or a summary costs never includes the
/// body bytes of the documents its findings stand over.
#[test]
fn no_statement_a_validate_runs_reads_a_documents_payload() {
    let validating_store = Validating::new("validate-payload");
    let mut narrowings = requests();
    narrowings.push(validating().with_predicates([Predicate::tag("draft")]));
    narrowings.push(
        validating()
            .with_severity(Severity::Error)
            .with_predicates([Predicate::tag("draft"), Predicate::path("*.md")]),
    );
    let (_, next) = validating_store.page(&validating().with_limit(3));
    let mut shapes = vec![validating().with_after(next.expect("a next page"))];
    for params in narrowings {
        shapes.push(params.clone().summarized());
        shapes.push(params);
    }
    let mut reached: Vec<ReadStatement> = Vec::new();
    for params in &shapes {
        for emitted in validating_store.plans(params) {
            reached.push(emitted.statement);
            reads_of(&emitted.plan).assert_reads_none_of(DOCUMENT_PAYLOAD);
        }
    }
    for statement in ValidateStatement::all() {
        assert!(
            reached.contains(&ReadStatement::Validate(statement)),
            "the payload bar never reached {statement:?}: {reached:?}"
        );
    }
}

/// **A validate's work reads out whole, each count under its own name**, for
/// the reason a find's does.
#[test]
fn a_validates_work_reads_out_every_count_by_name() {
    let work = norn_store::ValidateWork {
        statements: 1,
        rows_read: 2,
        full_scan_steps: 3,
        sorts: 4,
        vm_steps: 5,
    };
    assert_eq!(
        work.readings().collect::<Vec<_>>(),
        vec![
            ("validate_statements", 1),
            ("validate_rows_read", 2),
            ("validate_full_scan_steps", 3),
            ("validate_sorts", 4),
            ("validate_vm_steps", 5),
        ]
    );
}

//! The find builder: what a page answers, and the plan bars every statement and
//! filter it emits is judged by.
//!
//! Every plan here is one [`Snapshot::find_plans`] took of a statement
//! [`Snapshot::find`] ran for the same request, continuation and all: the text
//! and values the find recorded as it prepared them, explained on the
//! snapshot's own read-only connection. So a bar judges the SQL a page actually
//! read, and a statement is barred by a request that runs it. Each bar runs its
//! negative control in the same case: the index the bar names is dropped, or
//! the plan is rebuilt without the row the bar is about, and the bar is shown
//! to fail.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use crate::common::{Scratch, ambiguity, document, violation, write_documents};
use norn_store::{
    BlockFact, DEFAULT_PAGE, DeclaredFields, FIND_STATEMENTS, FieldOrder, FindPlan, FindStatement,
    Found, FrontmatterValue, HeadingFact, IN_VALUES_CEILING, MAX_PAGE, NESTED_ROW_CEILING, Nested,
    PageDirection, PageRefusal, READ_FILTERS, ReadBound, ReadFilter, Snapshot, SnapshotReader,
    Span, Store, StoreError, StoredPathOrder, SuffixKey, TagFact, TagSource, TypedOrder,
    induced_failure,
};
use norn_testkit::explain::{Access, PlanRow, QueryPlan};
use norn_wire::{
    Column, Cursor, CursorKey, Direction, FindParams, FindingKind, Pattern, Predicate,
    ResolutionTarget, Sort, SortKey, Unsatisfied, VaultAddress, VaultName,
};

// ---- fixtures ----

pub(crate) fn request() -> FindParams {
    FindParams::new(VaultAddress::name(
        VaultName::new("notes").expect("a vault name"),
    ))
}

pub(crate) fn sorted(key: SortKey, direction: Direction) -> FindParams {
    request().with_sort(Sort::new(key, direction))
}

/// An order that reads a raw value as an integer: `"10"` stands before `"9"` as
/// text and after it as a number, so the two orders part on the fixture.
pub(crate) fn integer_order() -> TypedOrder {
    TypedOrder::new(|raw| {
        raw.parse::<i64>()
            .ok()
            .map(|number| format!("{:020}", i128::from(number) - i128::from(i64::MIN)))
    })
}

/// The fingerprint of the schema the fixture pins, which its declarations are
/// read from.
pub(crate) const SEED_SCHEMA: &str = "seed-schema";

/// `status` declared as text, and `count` declared with a typed order, under
/// the fixture's schema.
pub(crate) fn declared() -> DeclaredFields {
    DeclaredFields::under(SEED_SCHEMA)
        .declare("status")
        .declare_typed("count", integer_order())
}

/// `status` and `count` both declared as text, under the fixture's schema.
pub(crate) fn declared_raw() -> DeclaredFields {
    DeclaredFields::under(SEED_SCHEMA)
        .declare("status")
        .declare("count")
}

pub(crate) fn map(entries: Vec<(&str, FrontmatterValue)>) -> FrontmatterValue {
    FrontmatterValue::Map(
        entries
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect(),
    )
}

pub(crate) fn string(text: &str) -> FrontmatterValue {
    FrontmatterValue::String(text.to_string())
}

pub(crate) fn integers(values: &[i64]) -> FrontmatterValue {
    FrontmatterValue::Sequence(values.iter().copied().map(FrontmatterValue::Int).collect())
}

/// Five documents whose fields part the two orders, whose paths part the two
/// path orders, and one of each fact a filter reads, written under the
/// fixture's schema, which the store pins first:
///
/// | path | status | count | also |
/// |---|---|---|---|
/// | `notes/a.md` | `open` | `3` | the tag `draft`, the term `interloper` |
/// | `notes/B.md` | `closed` | `[10, 9]` | |
/// | `notes/c.md` | `open` | `nine` | |
/// | `other/glossary.md` | | | a finding |
/// | `other/v1.2.md` | `done` | `[]` | |
pub(crate) fn seed(store: &mut Store) {
    store
        .begin_request()
        .pin_vault_schema(SEED_SCHEMA.as_bytes(), SEED_SCHEMA)
        .expect("pinning the fixture's schema");
    let declared = declared();
    let mut tagged = document("notes/a.md", "hash-a", "the interloper walked in\n")
        .with_frontmatter(
            Some(map(vec![
                ("status", string("open")),
                ("count", FrontmatterValue::Int(3)),
            ])),
            &declared,
        );
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
            document("notes/B.md", "hash-b", "a body\n").with_frontmatter(
                Some(map(vec![
                    ("status", string("closed")),
                    ("count", integers(&[10, 9])),
                ])),
                &declared,
            ),
            document("notes/c.md", "hash-c", "a body\n").with_frontmatter(
                Some(map(vec![
                    ("status", string("open")),
                    ("count", string("nine")),
                ])),
                &declared,
            ),
            document("other/glossary.md", "hash-g", "a body\n"),
            document("other/v1.2.md", "hash-v", "a body\n").with_frontmatter(
                Some(map(vec![
                    ("status", string("done")),
                    ("count", integers(&[])),
                ])),
                &declared,
            ),
        ],
    );
    request
        .record_finding(&violation("other/glossary.md"))
        .expect("recording a finding");
}

/// A seeded store and the read handle its snapshots are established on.
pub(crate) struct Seeded {
    _scratch: Scratch,
    pub(crate) store: Store,
    reader: Arc<SnapshotReader>,
}

impl Seeded {
    pub(crate) fn new(label: &str) -> Self {
        Self::with_bulk(label, 0)
    }

    /// The fixture and `bulk` more documents under `bulk/`, each carrying a
    /// `count` and the tag `bulk`, every other one a `status` too: enough rows
    /// in every table a page reads that a step through any of them end to end
    /// is counted many times over.
    fn with_bulk(label: &str, bulk: usize) -> Self {
        Self::with_bulk_under(label, bulk, StoredPathOrder::Sensitive)
    }

    /// The fixture in a store over a root proven to have `order`'s case
    /// behaviour, which every snapshot of it reads under.
    fn under(label: &str, order: StoredPathOrder) -> Self {
        Self::with_bulk_under(label, 0, order)
    }

    fn with_bulk_under(label: &str, bulk: usize, order: StoredPathOrder) -> Self {
        let scratch = Scratch::new(label);
        let mut store = Store::open(scratch.database(), order).expect("opening a store");
        seed(&mut store);
        let declared = declared();
        let documents: Vec<_> = (0..bulk)
            .map(|at| {
                let mut fields = vec![("count", FrontmatterValue::Int(at as i64))];
                if at % 2 == 0 {
                    fields.push(("status", string("filed")));
                }
                let mut facts = document(
                    &format!("bulk/{at:03}.md"),
                    &format!("hash-bulk-{at}"),
                    "a body\n",
                )
                .with_frontmatter(Some(map(fields)), &declared);
                facts.tags.push(TagFact {
                    name: "bulk".to_string(),
                    source: TagSource::Body,
                    span: None,
                });
                facts
            })
            .collect();
        if !documents.is_empty() {
            write_documents(&mut store.begin_request(), &documents);
        }
        let reader = Arc::new(
            store
                .open_reader()
                .reader
                .expect("a live store mints a reader"),
        );
        Seeded {
            _scratch: scratch,
            store,
            reader,
        }
    }

    /// A snapshot of the store, under the order its rows were derived under.
    pub(crate) fn snapshot(&self) -> Snapshot {
        self.reader
            .try_take()
            .expect("a handle nothing is reading holds its connection")
            .establish()
            .snapshot
            .expect("a snapshot")
    }

    /// One find of `params` on a fresh snapshot, under the fixture's
    /// declaration.
    fn page(&self, params: &FindParams) -> Found {
        self.snapshot()
            .find(params, &declared())
            .unwrap_or_else(|refusal| panic!("a find of {params:?}: {refusal}"))
    }

    fn paths(&self, params: &FindParams) -> Vec<String> {
        row_paths(&self.page(params))
    }

    pub(crate) fn plans(&self, params: &FindParams) -> Vec<FindPlan> {
        self.plans_under(params, &declared())
    }

    fn plans_under(&self, params: &FindParams, declared: &DeclaredFields) -> Vec<FindPlan> {
        self.snapshot()
            .find_plans(params, declared)
            .expect("the plans of a request")
    }

    /// `params` continuing from a row ordered by `sort` at `path`, under the
    /// fixture's declaration.
    fn resumed(&self, params: &FindParams, sort: Option<&str>, path: &str) -> FindParams {
        self.resumed_under(params, &declared(), sort, path)
    }

    /// `params` continuing from a row ordered by `sort` at `path`, with a
    /// cursor minted in the order `params` reads under `declared`: the reading
    /// a first page of it answers from, positioned there.
    fn resumed_under(
        &self,
        params: &FindParams,
        declared: &DeclaredFields,
        sort: Option<&str>,
        path: &str,
    ) -> FindParams {
        let reading = self
            .snapshot()
            .find(params, declared)
            .expect("a first page")
            .snapshot;
        params.clone().with_after(Cursor::new(
            reading,
            CursorKey::document(sort.map(str::to_string), path),
        ))
    }

    /// Drop `index` on the writer, which is what a negative control judges the
    /// same bar against. A snapshot established after it reads the new schema.
    fn drop_index(&mut self, index: &str) {
        induced_failure::execute_out_of_band(&mut self.store, &format!("DROP INDEX {index}"))
            .unwrap_or_else(|problem| panic!("dropping {index}: {problem}"));
    }
}

/// The paths a page's rows stand at, in its order.
fn row_paths(found: &Found) -> Vec<String> {
    found
        .rows
        .iter()
        .map(|row| row.path.as_str().to_string())
        .collect()
}

/// The plan the store reported for one statement, in the harness's shape.
fn plan(emitted: &FindPlan) -> QueryPlan {
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

/// The one plan `plans` holds for `statement`.
fn plan_of(plans: &[FindPlan], statement: FindStatement) -> QueryPlan {
    let matching: Vec<&FindPlan> = plans
        .iter()
        .filter(|plan| plan.statement == statement)
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "the request runs {statement:?} {} times: {plans:?}",
        matching.len()
    );
    plan(matching[0])
}

/// The rows of `plan` that read the relation a statement calls `alias`, as a
/// plan of their own over the same statement.
///
/// A filter reaches its table under an alias of its own, and the page it
/// narrows may reach the same table for another reason: a path filter's
/// subquery and the page itself both search `documents`. A bar about the
/// filter is judged on the filter's rows alone, and the alias is what binds
/// them.
pub(crate) fn rows_of(plan: &QueryPlan, alias: &str) -> QueryPlan {
    QueryPlan::new(
        plan.sql(),
        plan.rows()
            .iter()
            .filter(|row| row.searches() == Some(alias) || row.scans() == Some(alias))
            .cloned()
            .collect(),
    )
}

/// The panic `bar` raises, which a negative control requires it to raise. The
/// message is printed, so a run shows what the bar says when it fails.
pub(crate) fn failure_of(control: &str, bar: impl FnOnce()) -> String {
    let failure = catch_unwind(AssertUnwindSafe(bar)).expect_err(&format!(
        "the bar held under its negative control: {control}"
    ));
    let message = failure
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| failure.downcast_ref::<&str>().map(|text| text.to_string()))
        .unwrap_or_default();
    eprintln!("negative control `{control}` fails the bar: {message}");
    message
}

// ---- the census ----

/// Which test bars each statement the builder names. Exhaustive, so a statement
/// added to [`FindStatement`] does not compile until its author names the bar.
fn statement_barred_by(statement: FindStatement) -> &'static str {
    match statement {
        FindStatement::ActiveFingerprint | FindStatement::PathPage(_) => {
            "a_path_page_seeks_the_case_insensitive_index_in_either_direction"
        }
        FindStatement::FieldValuePage(..) | FindStatement::FieldMissingPage(..) => {
            "a_field_sort_seeks_its_marker_rows_and_pages_its_missing_section_by_path"
        }
        FindStatement::KnownKey | FindStatement::FieldUniverse => {
            "a_known_key_and_the_field_universe_read_the_presence_rows_alone"
        }
        FindStatement::BareDirectory => "a_bare_directory_probe_is_two_seeks_of_the_path_index",
        FindStatement::MatchProbe => {
            "a_match_probe_reads_the_full_text_index_through_its_selection"
        }
        FindStatement::HydrateDocuments
        | FindStatement::NestedHead(_)
        | FindStatement::NestedTotal(_)
        | FindStatement::FindingHead
        | FindStatement::FindingTotal
        | FindStatement::FindingCandidates
        | FindStatement::FindingClasses => {
            "hydration_reads_the_page_rows_by_id_and_each_collection_by_its_ordinal_index"
        }
    }
}

/// Which test bars each filter the builder names. Exhaustive for the reason
/// [`statement_barred_by`] is.
fn filter_barred_by(filter: ReadFilter) -> &'static str {
    match filter {
        ReadFilter::Equal(_)
        | ReadFilter::NotEqual(_)
        | ReadFilter::Member(_)
        | ReadFilter::Present
        | ReadFilter::Absent
        | ReadFilter::Before(_)
        | ReadFilter::After(_)
        | ReadFilter::FullText
        | ReadFilter::PathGlob
        | ReadFilter::Resolves(_)
        | ReadFilter::Tag
        | ReadFilter::Finding => "every_filter_seeks_the_index_its_values_are_bounds_for",
    }
}

/// Every form a filter slot takes: a filter that compares values compares
/// under either order, and each order is a form its bar has to probe.
/// Exhaustive, so a filter added to [`ReadFilter`] names its forms here.
fn forms_of(filter: ReadFilter) -> Vec<ReadFilter> {
    let orders = [FieldOrder::Raw, FieldOrder::Typed];
    match filter {
        ReadFilter::Equal(_) => orders.map(ReadFilter::Equal).to_vec(),
        ReadFilter::NotEqual(_) => orders.map(ReadFilter::NotEqual).to_vec(),
        ReadFilter::Member(_) => orders.map(ReadFilter::Member).to_vec(),
        ReadFilter::Before(_) => orders.map(ReadFilter::Before).to_vec(),
        ReadFilter::After(_) => orders.map(ReadFilter::After).to_vec(),
        ReadFilter::Resolves(_) => [SuffixKey::Raw, SuffixKey::Folded]
            .map(ReadFilter::Resolves)
            .to_vec(),
        ReadFilter::Present
        | ReadFilter::Absent
        | ReadFilter::FullText
        | ReadFilter::PathGlob
        | ReadFilter::Tag
        | ReadFilter::Finding => vec![filter],
    }
}

/// **Every statement and every filter the builder names carries a bar**, and
/// one bar judges each. The path-page bar judges the statements [`PAGE_BARS`]
/// names and the fingerprint read; the field-sort bar judges both sections of
/// every [`FIELD_BARS`] entry; the key-probe bar judges [`KEY_PROBES`]; the
/// bare-directory bar and the match-probe bar their one probe each; the
/// hydration bar [`hydration_statements`]; the filter bar judges [`filter_bars`]. A bar
/// ranges over the order and the direction a statement carries, and those are
/// not slots, so each bar's statements are read as the set of slots it
/// reaches: across the bars, those sets cover the enumeration exactly once,
/// and the filter bar's table holds each filter slot exactly once, probing each
/// of the slot's [`forms_of`] — both orders of a filter that compares values —
/// and no form of another slot. A statement
/// dropped from the list a bar iterates leaves a slot empty here, and one two
/// bars claim fills a slot twice.
#[test]
fn the_find_bars_cover_every_statement_and_filter_once() {
    let per_bar: [Vec<FindStatement>; 6] = [
        std::iter::once(FindStatement::ActiveFingerprint)
            .chain(PAGE_BARS.iter().map(|(statement, ..)| *statement))
            .collect(),
        FIELD_BARS
            .iter()
            .flat_map(|bar| [bar.valued, bar.missing])
            .collect(),
        KEY_PROBES.to_vec(),
        vec![FindStatement::BareDirectory],
        vec![FindStatement::MatchProbe],
        hydration_statements(),
    ];
    let mut slots: Vec<usize> = Vec::new();
    for statements in &per_bar {
        let mut reached: Vec<usize> = statements
            .iter()
            .map(|statement| statement.slot())
            .collect();
        reached.sort_unstable();
        reached.dedup();
        slots.extend(reached);
    }
    slots.sort_unstable();
    assert_eq!(
        slots,
        (0..FIND_STATEMENTS).collect::<Vec<usize>>(),
        "the find bars do not judge every statement slot exactly once: {per_bar:?}"
    );
    let judged: Vec<FindStatement> = per_bar.into_iter().flatten().collect();
    for (slot, statement) in FindStatement::all().into_iter().enumerate() {
        assert_eq!(statement.slot(), slot, "{statement:?} claims another slot");
    }

    let filters: Vec<ReadFilter> = filter_bars().iter().map(|bar| bar.shape).collect();
    let mut filter_slots: Vec<usize> = filters.iter().map(|filter| filter.slot()).collect();
    filter_slots.sort_unstable();
    assert_eq!(
        filter_slots,
        (0..READ_FILTERS).collect::<Vec<usize>>(),
        "the filter bar does not judge every filter slot exactly once: {filters:?}"
    );
    for (slot, filter) in ReadFilter::all().into_iter().enumerate() {
        assert_eq!(filter.slot(), slot, "{filter:?} claims another slot");
    }
    for bar in filter_bars() {
        let mut probed: Vec<ReadFilter> = Vec::new();
        for (_, form, _) in &bar.probes {
            if !probed.contains(form) {
                probed.push(*form);
            }
        }
        let forms = forms_of(bar.shape);
        assert!(
            probed.len() == forms.len() && forms.iter().all(|form| probed.contains(form)),
            "the bar for {:?} probes {probed:?}, not its forms {forms:?}",
            bar.shape
        );
    }

    let bars: std::collections::BTreeSet<&str> = judged
        .into_iter()
        .map(statement_barred_by)
        .chain(filters.into_iter().map(filter_barred_by))
        .collect();
    assert_eq!(
        bars,
        [
            "a_bare_directory_probe_is_two_seeks_of_the_path_index",
            "a_field_sort_seeks_its_marker_rows_and_pages_its_missing_section_by_path",
            "a_known_key_and_the_field_universe_read_the_presence_rows_alone",
            "a_match_probe_reads_the_full_text_index_through_its_selection",
            "a_path_page_seeks_the_case_insensitive_index_in_either_direction",
            "every_filter_seeks_the_index_its_values_are_bounds_for",
            "hydration_reads_the_page_rows_by_id_and_each_collection_by_its_ordinal_index",
        ]
        .into_iter()
        .collect()
    );
}

// ---- the page bars ----

/// The path pages, each with the constraint its search opens on
/// `documents_path_nocase`.
const PAGE_BARS: &[(FindStatement, Direction, &str)] = &[
    (
        FindStatement::PathPage(PageDirection::Ascending),
        Direction::Ascending,
        "(path>?)",
    ),
    (
        FindStatement::PathPage(PageDirection::Descending),
        Direction::Descending,
        "(path<?)",
    ),
];

/// **A path page is a seek of the case-insensitive path index, in either
/// direction and from a continuation alike.** The order the page states — the
/// path folded by ASCII case, the bytewise path breaking the tie — is the order
/// `documents_path_nocase` holds, so the rows come off the index in page order
/// and nothing sorts. A continuation is explained with its position bound, and
/// the position is the search's bound: `(path>?)` ascending, `(path<?)`
/// descending.
///
/// The fingerprint read a finding filter binds is the snapshot's one point read,
/// a primary-key seek of `meta`.
///
/// Controls: the case-insensitive index dropped, the page reads another index
/// and sorts; a continuation plan rebuilt without its bound constraint fails the
/// constraint bar.
#[test]
fn a_path_page_seeks_the_case_insensitive_index_in_either_direction() {
    let mut seeded = Seeded::new("find-path-page");
    let resumed = |seeded: &Seeded, params: FindParams| seeded.resumed(&params, None, "notes/B.md");
    let judge = |page: &QueryPlan, constraint: &str| {
        page.assert_no_full_scan();
        page.assert_searches_through("documents", Access::Index("documents_path_nocase"));
        page.assert_search_constraint("documents", constraint);
        page.assert_no_temp_btree();
    };
    for (statement, direction, constraint) in PAGE_BARS {
        let params = sorted(SortKey::path(), *direction);
        for params in [params.clone(), resumed(&seeded, params)] {
            judge(&plan_of(&seeded.plans(&params), *statement), constraint);
        }
    }
    // A request that names no order pages by path, ascending.
    judge(
        &plan_of(
            &seeded.plans(&request()),
            FindStatement::PathPage(PageDirection::Ascending),
        ),
        "(path>?)",
    );

    let fingerprint = plan_of(
        &seeded.plans(
            &request().with_predicates([Predicate::has_finding(FindingKind::BodyBytesNotUtf8)]),
        ),
        FindStatement::ActiveFingerprint,
    );
    fingerprint.assert_searches_through("meta", Access::PrimaryKey);
    fingerprint.assert_search_constraint("meta", "(key=?)");

    // Control: the continuation's bound taken out of the search it opens.
    let continued = plan_of(
        &seeded.plans(&resumed(
            &seeded,
            sorted(SortKey::path(), Direction::Ascending),
        )),
        FindStatement::PathPage(PageDirection::Ascending),
    );
    let unbounded = QueryPlan::new(
        continued.sql(),
        continued
            .rows()
            .iter()
            .map(|row| PlanRow::new(row.id, row.parent, row.detail.replace(" (path>?)", "")))
            .collect(),
    );
    failure_of("a continuation that seeks from no bound", || {
        judge(&unbounded, "(path>?)")
    });

    // Control: the index the order is held by, gone.
    seeded.drop_index("documents_path_nocase");
    for (statement, direction, constraint) in PAGE_BARS {
        let plans = seeded.plans(&sorted(SortKey::path(), *direction));
        failure_of("documents_path_nocase dropped", || {
            judge(&plan_of(&plans, *statement), constraint)
        });
    }
}

/// One field order's two sections, and the marker index its valued section
/// seeks.
struct FieldBar {
    key: &'static str,
    valued: FindStatement,
    missing: FindStatement,
    direction: Direction,
    marker_index: &'static str,
    valued_constraint: &'static str,
    missing_constraint: &'static str,
}

/// Each field order in each direction: raw over `status`, which is declared
/// without a type, and typed over `count`, which carries one.
const FIELD_BARS: &[FieldBar] = &[
    FieldBar {
        key: "status",
        valued: FindStatement::FieldValuePage(FieldOrder::Raw, PageDirection::Ascending),
        missing: FindStatement::FieldMissingPage(FieldOrder::Raw, PageDirection::Ascending),
        direction: Direction::Ascending,
        marker_index: "document_fields_least_raw",
        valued_constraint: "(key=? AND (raw,path)>(?,?))",
        missing_constraint: "(path>?)",
    },
    FieldBar {
        key: "status",
        valued: FindStatement::FieldValuePage(FieldOrder::Raw, PageDirection::Descending),
        missing: FindStatement::FieldMissingPage(FieldOrder::Raw, PageDirection::Descending),
        direction: Direction::Descending,
        marker_index: "document_fields_least_raw",
        valued_constraint: "(key=? AND (raw,path)<(?,?))",
        missing_constraint: "(path<?)",
    },
    FieldBar {
        key: "count",
        valued: FindStatement::FieldValuePage(FieldOrder::Typed, PageDirection::Ascending),
        missing: FindStatement::FieldMissingPage(FieldOrder::Typed, PageDirection::Ascending),
        direction: Direction::Ascending,
        marker_index: "document_fields_least_typed",
        valued_constraint: "(key=? AND (typed,path)>(?,?))",
        missing_constraint: "(path>?)",
    },
    FieldBar {
        key: "count",
        valued: FindStatement::FieldValuePage(FieldOrder::Typed, PageDirection::Descending),
        missing: FindStatement::FieldMissingPage(FieldOrder::Typed, PageDirection::Descending),
        direction: Direction::Descending,
        marker_index: "document_fields_least_typed",
        valued_constraint: "(key=? AND (typed,path)<(?,?))",
        missing_constraint: "(path<?)",
    },
];

/// Judge a field sort's valued section: one seek of the marker index from the
/// page's position, rows in page order.
fn judge_valued(page: &QueryPlan, bar: &FieldBar) {
    page.assert_no_full_scan();
    page.assert_searches_through("document_fields", Access::Index(bar.marker_index));
    page.assert_search_constraint("document_fields", bar.valued_constraint);
    page.assert_no_temp_btree();
}

/// Judge a field sort's missing section: path order off `documents_path` from
/// the page's position, and one primary-key seek per document for its marker.
fn judge_missing(page: &QueryPlan, bar: &FieldBar) {
    page.assert_no_full_scan();
    page.assert_searches_through("documents", Access::Index("documents_path"));
    page.assert_search_constraint("documents", bar.missing_constraint);
    page.assert_searches_through("document_fields", Access::PrimaryKey);
    page.assert_search_constraint("document_fields", "(document=? AND key=?)");
    page.assert_no_temp_btree();
}

/// **A field sort seeks its marker rows, and pages its missing section by
/// path.** The valued section reads the order's marker index — one row per
/// document, its least value — from the page's `(value, path)` position, so the
/// page reads its own rows and none ahead of it, and nothing sorts. The missing
/// section walks `documents_path` from the page's path and seeks each
/// document's marker by primary key. Both are judged on a first page and on a
/// continuation into each section, which is where a keyset position is bound.
///
/// Controls: each marker index dropped, the valued section reads something
/// else; a continuation's plan rebuilt without its `(value, path)` bound fails
/// the constraint bar.
#[test]
fn a_field_sort_seeks_its_marker_rows_and_pages_its_missing_section_by_path() {
    let mut seeded = Seeded::new("find-field-sort");
    for bar in FIELD_BARS {
        let params = sorted(SortKey::field(bar.key), bar.direction);
        let in_valued = seeded.resumed(&params, Some("m"), "notes/a.md");
        let in_missing = seeded.resumed(&params, None, "notes/a.md");
        let first = seeded.plans(&params);
        judge_valued(&plan_of(&first, bar.valued), bar);
        judge_missing(&plan_of(&first, bar.missing), bar);
        judge_valued(&plan_of(&seeded.plans(&in_valued), bar.valued), bar);
        judge_missing(&plan_of(&seeded.plans(&in_missing), bar.missing), bar);

        // Control: the continuation's `(value, path)` bound taken out.
        let resumed = plan_of(&seeded.plans(&in_valued), bar.valued);
        let bound = bar
            .valued_constraint
            .strip_prefix("(key=? AND ")
            .and_then(|rest| rest.strip_suffix(')'))
            .expect("a valued constraint leads with the key");
        let unbounded = QueryPlan::new(
            resumed.sql(),
            resumed
                .rows()
                .iter()
                .map(|row| {
                    PlanRow::new(
                        row.id,
                        row.parent,
                        row.detail.replace(&format!(" AND {bound}"), ""),
                    )
                })
                .collect(),
        );
        failure_of("a continuation that seeks from the key alone", || {
            judge_valued(&unbounded, bar)
        });
    }

    for index in ["document_fields_least_raw", "document_fields_least_typed"] {
        seeded.drop_index(index);
        for bar in FIELD_BARS.iter().filter(|bar| bar.marker_index == index) {
            let plans = seeded.plans(&sorted(SortKey::field(bar.key), bar.direction));
            failure_of(&format!("{index} dropped"), || {
                judge_valued(&plan_of(&plans, bar.valued), bar)
            });
        }
    }
}

// ---- the probe bars ----

/// The statements that ask whether a key is known and what the field universe
/// holds.
const KEY_PROBES: [FindStatement; 2] = [FindStatement::KnownKey, FindStatement::FieldUniverse];

/// **Whether a key is known, and what the field universe holds, are read off
/// the presence rows alone.** Whether a key is known is one existence seek of
/// `document_fields_presence` at the key. The universe is a walk of the same
/// index that seeks past each key to the next — `(key>?)` at every step — so
/// it reads one entry per distinct key, never the rows that carry them, and
/// never the table. A declared key asks neither.
///
/// Controls: the presence index dropped, both statements read something else.
#[test]
fn a_known_key_and_the_field_universe_read_the_presence_rows_alone() {
    let mut seeded = Seeded::new("find-key-probes");
    // `seen` is declared nowhere and no document carries it, so the request
    // asks whether it is known and, finding it is not, walks the universe.
    let params = request().with_predicates([Predicate::has("seen")]);
    let judge_known = |plans: &[FindPlan]| {
        let known = plan_of(plans, FindStatement::KnownKey);
        known.assert_no_full_scan();
        known.assert_searches_through("document_fields", Access::Index("document_fields_presence"));
        known.assert_search_constraint("document_fields", "(key=?)");
    };
    let judge_universe = |plans: &[FindPlan]| {
        let universe = plan_of(plans, FindStatement::FieldUniverse);
        universe.assert_no_full_scan_of("document_fields");
        universe.assert_no_temp_btree();
        universe
            .assert_searches_through("document_fields", Access::Index("document_fields_presence"));
        universe.assert_search_constraint("document_fields", "(key>?)");
        assert_eq!(
            universe.searches_of("document_fields").len(),
            2,
            "the universe reaches the presence rows other than by its first and its next \
             seek: {:?}",
            universe.rows()
        );
    };
    let plans = seeded.plans(&params);
    judge_known(&plans);
    judge_universe(&plans);

    let declared_only = seeded.plans(
        &request()
            .with_predicates([Predicate::has("status")])
            .with_sort(Sort::new(SortKey::field("count"), Direction::Ascending)),
    );
    assert!(
        declared_only
            .iter()
            .all(|plan| !KEY_PROBES.contains(&plan.statement)),
        "a declared key asked the snapshot whether it is known: {declared_only:?}"
    );

    seeded.drop_index("document_fields_presence");
    let plans = seeded.plans(&params);
    failure_of("document_fields_presence dropped, the key probe", || {
        judge_known(&plans)
    });
    failure_of("document_fields_presence dropped, the universe", || {
        judge_universe(&plans)
    });
}

/// **A bare-directory probe is two seeks of the path index**: one at the path,
/// which finds no document there, and one of the range beneath it, which finds
/// one that stands under it. Each is judged on the rows its own subquery reads.
///
/// Controls: `documents_path` dropped, neither seek is one.
#[test]
fn a_bare_directory_probe_is_two_seeks_of_the_path_index() {
    let mut seeded = Seeded::new("find-bare-directory");
    let params = request().with_predicates([Predicate::path("notes")]);
    let judge = |plan: &QueryPlan| {
        plan.assert_no_full_scan();
        let at = rows_of(plan, "da");
        at.assert_searches_through("documents", Access::Index("documents_path"));
        at.assert_search_constraint("documents", "(path=?)");
        let under = rows_of(plan, "du");
        under.assert_searches_through("documents", Access::Index("documents_path"));
        under.assert_search_constraint("documents", "(path>? AND path<?)");
    };
    judge(&plan_of(
        &seeded.plans(&params),
        FindStatement::BareDirectory,
    ));
    // A glob with a wildcard is no directory, and asks nothing.
    assert!(
        seeded
            .plans(&request().with_predicates([Predicate::path("notes/*")]))
            .iter()
            .all(|plan| plan.statement != FindStatement::BareDirectory)
    );

    seeded.drop_index("documents_path");
    let plan = plan_of(&seeded.plans(&params), FindStatement::BareDirectory);
    failure_of("documents_path dropped", || judge(&plan));
}

/// **A match probe is one read of the full-text index through its `MATCH`
/// selection**, which is the step at which the engine parses the query: the
/// probe reads no relation end to end, and asks the index nothing but the
/// query. Only a match part probes; a request without one asks nothing.
///
/// Control: the plan rebuilt with its `MATCH` selection taken out, the read
/// the engine would make without parsing the query, fails the bar.
#[test]
fn a_match_probe_reads_the_full_text_index_through_its_selection() {
    let seeded = Seeded::new("find-match-probe");
    let judge = |plan: &QueryPlan| {
        plan.assert_no_full_scan();
        assert!(
            plan.rows().iter().any(|row| matches!(
                row.scan_target(),
                Some(norn_testkit::explain::ScanTarget::VirtualTable { specification, .. })
                    if specification.starts_with('M')
            )),
            "the match probe does not read `documents_fts` through its MATCH selection: {:?}\n\
             emitted SQL: {}",
            plan.rows(),
            plan.sql()
        );
    };
    let params = request().with_predicates([Predicate::matches("interloper")]);
    let probe = plan_of(&seeded.plans(&params), FindStatement::MatchProbe);
    judge(&probe);
    assert!(
        seeded
            .plans(&request().with_predicates([Predicate::tag("draft")]))
            .iter()
            .all(|plan| plan.statement != FindStatement::MatchProbe)
    );

    let unselected = QueryPlan::new(
        probe.sql(),
        probe
            .rows()
            .iter()
            .map(|row| PlanRow::new(row.id, row.parent, row.detail.replace("0:M1", "0:")))
            .collect(),
    );
    failure_of("a match probe with no MATCH selection", || {
        judge(&unselected)
    });
}

/// Every hydration statement: the document rows, each collection's head and
/// total, and the findings column's head and total and the candidate heads
/// and classes of the findings it read.
fn hydration_statements() -> Vec<FindStatement> {
    std::iter::once(FindStatement::HydrateDocuments)
        .chain(Nested::ALL.into_iter().flat_map(|nested| {
            [
                FindStatement::NestedHead(nested),
                FindStatement::NestedTotal(nested),
            ]
        }))
        .chain([
            FindStatement::FindingHead,
            FindStatement::FindingTotal,
            FindStatement::FindingCandidates,
            FindStatement::FindingClasses,
        ])
        .collect()
}

/// The index a collection's head and total seek.
fn ordinal_index(nested: Nested) -> String {
    format!("{}_document_ordinal", nested.table())
}

/// **A page's rows are read by row id, and each collection by its ordinal
/// index.** The document rows are primary-key seeks of the page's ids. A
/// collection's head seeks `(document, ordinal)` for each id with the ceiling
/// as the ordinal's bound, so a document's rows past it are never reached; its
/// total counts the same index, and never the table. A total is counted only
/// for a document whose head the ceiling filled, so the page holds one whose
/// every collection fills it, and each total runs. **The findings column is
/// read by path**: its head seeks `findings_path` at each page path and the
/// active fingerprint, stopping at the ceiling, and reaches each finding it
/// kept by row id; its total counts the same index; and the candidate heads
/// and classes of the findings it read are primary-key seeks of their ids.
///
/// Controls: the document plan rebuilt with its row-id seek as a scan; each
/// ordinal index dropped, its head and total read something else;
/// `findings_path` dropped, the findings head and total read something else;
/// the candidate and class reads rebuilt as scans.
#[test]
fn hydration_reads_the_page_rows_by_id_and_each_collection_by_its_ordinal_index() {
    let mut seeded = Seeded::new("find-hydration");
    let mut full = document("long/full.md", "hash-full", "a body\n");
    full.tags = (0..NESTED_ROW_CEILING)
        .map(|index| TagFact {
            name: format!("t{index:03}"),
            source: TagSource::Frontmatter,
            span: None,
        })
        .collect();
    full.headings = (0..NESTED_ROW_CEILING)
        .map(|index| HeadingFact {
            level: 2,
            text: format!("heading {index}"),
            slug: format!("heading-{index}"),
            span: Span {
                line: index as u64 + 1,
                column: 1,
                byte_offset: 0,
            },
            body_offset: 0,
            inside_container: false,
        })
        .collect();
    full.blocks = (0..NESTED_ROW_CEILING)
        .map(|index| BlockFact {
            block_id: format!("b{index}"),
            span: None,
        })
        .collect();
    let mut writing = seeded.store.begin_request();
    write_documents(&mut writing, &[full]);
    for _ in 0..NESTED_ROW_CEILING {
        writing
            .record_finding(&ambiguity(
                "long/full.md",
                "glossary",
                "glossary/",
                &["other/glossary.md"],
                1,
            ))
            .expect("recording a finding");
    }
    let params = request().with_columns([
        Column::fields(),
        Column::body(),
        Column::tags(),
        Column::headings(),
        Column::blocks(),
        Column::findings(),
    ]);
    let judge_documents = |plan: &QueryPlan| {
        plan.assert_no_full_scan();
        plan.assert_searches_through("documents", Access::RowId);
        plan.assert_search_constraint("documents", "(rowid=?)");
    };
    let judge_head = |plans: &[FindPlan], nested: Nested| {
        let index = ordinal_index(nested);
        let head = plan_of(plans, FindStatement::NestedHead(nested));
        head.assert_no_full_scan();
        head.assert_searches_through(nested.table(), Access::Index(&index));
        head.assert_search_constraint(nested.table(), "(document=? AND ordinal<?)");
    };
    let judge_total = |plans: &[FindPlan], nested: Nested| {
        let index = ordinal_index(nested);
        let total = plan_of(plans, FindStatement::NestedTotal(nested));
        total.assert_no_full_scan();
        total.assert_searches_through(nested.table(), Access::Index(&index));
        total.assert_search_constraint(nested.table(), "(document=?)");
    };
    let judge_finding_head = |plans: &[FindPlan]| {
        let head = plan_of(plans, FindStatement::FindingHead);
        head.assert_no_full_scan();
        let seek = rows_of(&head, "h");
        seek.assert_searches_through("findings", Access::Index("findings_path"));
        seek.assert_search_constraint("findings", "(path=? AND vault_schema_fingerprint=?)");
        assert!(
            head.rows()
                .iter()
                .all(|row| !row.detail.contains("TEMP B-TREE")),
            "a findings head sorted: {:?}\nemitted SQL: {}",
            head.rows(),
            head.sql()
        );
        rows_of(&head, "f").assert_searches_through("findings", Access::RowId);
    };
    let judge_finding_total = |plans: &[FindPlan]| {
        let total = plan_of(plans, FindStatement::FindingTotal);
        total.assert_no_full_scan();
        total.assert_searches_through("findings", Access::Index("findings_path"));
        total.assert_search_constraint("findings", "(path=? AND vault_schema_fingerprint=?)");
    };
    let judge_by_finding = |plan: &QueryPlan, table: &str| {
        plan.assert_no_full_scan();
        plan.assert_searches_through(table, Access::PrimaryKey);
        plan.assert_search_constraint(table, "(finding=?)");
    };
    let plans = seeded.plans(&params);
    let documents = plan_of(&plans, FindStatement::HydrateDocuments);
    judge_documents(&documents);
    for nested in Nested::ALL {
        judge_head(&plans, nested);
        judge_total(&plans, nested);
    }
    judge_finding_head(&plans);
    judge_finding_total(&plans);
    let candidates = plan_of(&plans, FindStatement::FindingCandidates);
    judge_by_finding(&candidates, "finding_candidates");
    let classes = plan_of(&plans, FindStatement::FindingClasses);
    judge_by_finding(&classes, "finding_classes");

    // Control: the candidate and class reads, each rebuilt as a scan.
    for (plan, alias, table) in [
        (&candidates, "c", "finding_candidates"),
        (&classes, "k", "finding_classes"),
    ] {
        let scanned = QueryPlan::new(
            plan.sql(),
            plan.rows()
                .iter()
                .map(|row| {
                    let detail = if row.detail.starts_with(&format!("SEARCH {alias} ")) {
                        format!("SCAN {alias}")
                    } else {
                        row.detail.clone()
                    };
                    PlanRow::new(row.id, row.parent, detail)
                })
                .collect(),
        );
        failure_of(&format!("a {table} read that scans"), || {
            judge_by_finding(&scanned, table)
        });
    }

    // Control: the row-id seek taken out of the document plan.
    let scanned = QueryPlan::new(
        documents.sql(),
        documents
            .rows()
            .iter()
            .map(|row| {
                PlanRow::new(
                    row.id,
                    row.parent,
                    row.detail
                        .replace("SEARCH d USING INTEGER PRIMARY KEY (rowid=?)", "SCAN d"),
                )
            })
            .collect(),
    );
    failure_of("a document hydration that scans", || {
        judge_documents(&scanned)
    });

    // Control: the findings path index, dropped.
    seeded.drop_index("findings_path");
    let plans = seeded.plans(&params);
    failure_of("findings_path dropped, the findings head", || {
        judge_finding_head(&plans)
    });
    failure_of("findings_path dropped, the findings total", || {
        judge_finding_total(&plans)
    });

    // Control: each ordinal index, dropped.
    for nested in Nested::ALL {
        seeded.drop_index(&ordinal_index(nested));
        let plans = seeded.plans(&params);
        failure_of(
            &format!("{} dropped, the head", ordinal_index(nested)),
            || judge_head(&plans, nested),
        );
        failure_of(
            &format!("{} dropped, the total", ordinal_index(nested)),
            || judge_total(&plans, nested),
        );
    }
}

// ---- the filter bar ----

/// What one filter's rows are judged by.
enum Seek {
    /// The filter's alias searches `table` through `access`, every search of it
    /// carrying `constraint`. `dropped` is the index the control drops.
    Index {
        alias: &'static str,
        table: &'static str,
        access: Access<'static>,
        constraint: &'static str,
        dropped: &'static str,
    },
    /// The filter reads the full-text index through its own `MATCH` selection.
    FullText,
}

/// One filter slot, and the parts of a request that spell it.
struct FilterBar {
    shape: ReadFilter,
    probes: Vec<(Predicate, ReadFilter, Seek)>,
}

/// A seek of `document_fields` under `alias`.
fn field_seek(alias: &'static str, index: &'static str, constraint: &'static str) -> Seek {
    Seek::Index {
        alias,
        table: "document_fields",
        access: Access::Index(index),
        constraint,
        dropped: index,
    }
}

/// Every filter slot, once. A filter that compares values is spelled under both
/// orders: `status` is declared without a type and `count` with one.
fn filter_bars() -> Vec<FilterBar> {
    let target = |text: &str| ResolutionTarget::new(text).expect("a target");
    vec![
        FilterBar {
            shape: ReadFilter::Equal(FieldOrder::Raw),
            probes: vec![
                (
                    Predicate::equal_to("status", "open"),
                    ReadFilter::Equal(FieldOrder::Raw),
                    field_seek("fv", "document_fields_raw", "(key=? AND raw=?)"),
                ),
                (
                    Predicate::equal_to("count", "9"),
                    ReadFilter::Equal(FieldOrder::Typed),
                    field_seek("fv", "document_fields_typed", "(key=? AND typed=?)"),
                ),
            ],
        },
        FilterBar {
            shape: ReadFilter::NotEqual(FieldOrder::Raw),
            probes: vec![
                (
                    Predicate::not_equal_to("status", "open"),
                    ReadFilter::NotEqual(FieldOrder::Raw),
                    field_seek("fv", "document_fields_raw", "(key=? AND raw=?)"),
                ),
                (
                    Predicate::not_equal_to("count", "9"),
                    ReadFilter::NotEqual(FieldOrder::Typed),
                    field_seek("fv", "document_fields_typed", "(key=? AND typed=?)"),
                ),
            ],
        },
        FilterBar {
            shape: ReadFilter::Member(FieldOrder::Raw),
            probes: vec![
                (
                    Predicate::in_any("status", ["open".to_string(), "done".to_string()]),
                    ReadFilter::Member(FieldOrder::Raw),
                    field_seek("fv", "document_fields_raw", "(key=? AND raw=?)"),
                ),
                (
                    Predicate::in_any("count", ["3".to_string(), "9".to_string()]),
                    ReadFilter::Member(FieldOrder::Typed),
                    field_seek("fv", "document_fields_typed", "(key=? AND typed=?)"),
                ),
            ],
        },
        FilterBar {
            shape: ReadFilter::Present,
            probes: vec![(
                Predicate::has("status"),
                ReadFilter::Present,
                field_seek("fp", "document_fields_presence", "(key=?)"),
            )],
        },
        FilterBar {
            shape: ReadFilter::Absent,
            probes: vec![(
                Predicate::missing("status"),
                ReadFilter::Absent,
                field_seek("fp", "document_fields_presence", "(key=?)"),
            )],
        },
        FilterBar {
            shape: ReadFilter::Before(FieldOrder::Raw),
            probes: vec![
                (
                    Predicate::before("status", "open"),
                    ReadFilter::Before(FieldOrder::Raw),
                    field_seek("fb", "document_fields_raw", "(key=? AND raw<?)"),
                ),
                (
                    Predicate::before("count", "5"),
                    ReadFilter::Before(FieldOrder::Typed),
                    field_seek("fb", "document_fields_typed", "(key=? AND typed<?)"),
                ),
            ],
        },
        FilterBar {
            shape: ReadFilter::After(FieldOrder::Raw),
            probes: vec![
                (
                    Predicate::after("status", "done"),
                    ReadFilter::After(FieldOrder::Raw),
                    field_seek("fb", "document_fields_raw", "(key=? AND raw>?)"),
                ),
                (
                    Predicate::after("count", "5"),
                    ReadFilter::After(FieldOrder::Typed),
                    field_seek("fb", "document_fields_typed", "(key=? AND typed>?)"),
                ),
            ],
        },
        FilterBar {
            shape: ReadFilter::FullText,
            probes: vec![(
                Predicate::matches("interloper"),
                ReadFilter::FullText,
                Seek::FullText,
            )],
        },
        FilterBar {
            shape: ReadFilter::PathGlob,
            probes: vec![(
                Predicate::path("notes/*.md"),
                ReadFilter::PathGlob,
                Seek::Index {
                    alias: "dg",
                    table: "documents",
                    access: Access::Index("documents_path"),
                    constraint: "(path>? AND path<?)",
                    dropped: "documents_path",
                },
            )],
        },
        FilterBar {
            shape: ReadFilter::Resolves(SuffixKey::Raw),
            probes: [target("glossary"), target("v1.2#Heading")]
                .into_iter()
                .flat_map(|target| {
                    [
                        (
                            Predicate::resolves(target.clone()),
                            ReadFilter::Resolves(SuffixKey::Raw),
                            Seek::Index {
                                alias: "dr",
                                table: "documents",
                                access: Access::Index("documents_suffix_key"),
                                constraint: "(suffix_key>? AND suffix_key<?)",
                                dropped: "documents_suffix_key",
                            },
                        ),
                        (
                            Predicate::resolves(target),
                            ReadFilter::Resolves(SuffixKey::Folded),
                            Seek::Index {
                                alias: "dr",
                                table: "documents",
                                access: Access::Index("documents_folded_suffix_key"),
                                constraint: "(folded_suffix_key>? AND folded_suffix_key<?)",
                                dropped: "documents_folded_suffix_key",
                            },
                        ),
                    ]
                })
                .collect(),
        },
        FilterBar {
            shape: ReadFilter::Tag,
            probes: vec![(
                Predicate::tag("draft"),
                ReadFilter::Tag,
                Seek::Index {
                    alias: "tg",
                    table: "document_tags",
                    access: Access::Index("document_tags_name"),
                    constraint: "(name=?)",
                    dropped: "document_tags_name",
                },
            )],
        },
        FilterBar {
            shape: ReadFilter::Finding,
            probes: vec![(
                Predicate::has_finding(FindingKind::BodyBytesNotUtf8),
                ReadFilter::Finding,
                Seek::Index {
                    alias: "fg",
                    table: "findings",
                    access: Access::Index("findings_fingerprint_kind_severity"),
                    constraint: "(vault_schema_fingerprint=? AND kind=?)",
                    dropped: "findings_fingerprint_kind_severity",
                },
            )],
        },
    ]
}

/// The case behaviour of the root a filter form is compiled on: the folded
/// resolution form is what a root that folds ASCII case compiles, and every
/// other form is the same on either root.
fn root_of(shape: ReadFilter) -> StoredPathOrder {
    match shape {
        ReadFilter::Resolves(SuffixKey::Folded) => StoredPathOrder::AsciiCaseInsensitive,
        _ => StoredPathOrder::Sensitive,
    }
}

/// Judge one filter's rows in a page's plan.
fn judge_filter(page: &QueryPlan, seek: &Seek) {
    page.assert_no_full_scan();
    match seek {
        Seek::Index {
            alias,
            table,
            access,
            constraint,
            ..
        } => {
            let rows = rows_of(page, alias);
            rows.assert_searches_through(table, *access);
            rows.assert_search_constraint(table, constraint);
        }
        Seek::FullText => {
            let rows = rows_of(page, "documents_fts");
            assert!(
                rows.rows().iter().any(|row| matches!(
                    row.scan_target(),
                    Some(norn_testkit::explain::ScanTarget::VirtualTable { specification, .. })
                        if specification.starts_with('M')
                )),
                "the full-text filter does not read `documents_fts` through its MATCH \
                 selection: {:?}\nemitted SQL: {}",
                page.rows(),
                page.sql()
            );
        }
    }
}

/// **Every filter is a membership test one index seek answers.** Each part of
/// a conjunction is judged on the rows its own subquery reads, bound by the
/// alias the filter reads its table under: equality, inequality and membership
/// seek `(key, raw)`, or `(key, typed)` on a key with a typed order; presence and absence the presence rows by key; a bound
/// the order's value column from the key; the full-text part the index's own
/// `MATCH` selection; the path part the glob's literal-prefix range on
/// `documents_path`; the resolution part each suffix range the target opens;
/// the tag part `(name)`; and the finding part `(kind, fingerprint)`. No step
/// of any of these pages reads a relation end to end.
///
/// Each filter is judged on the path page, and on the valued section of a field
/// sort, which reach the page's document through a row id and through the
/// field rows respectively.
///
/// Controls: the index each filter seeks is dropped, and the same bar fails;
/// the full-text filter, whose index cannot be dropped, is judged on a plan
/// rebuilt with its `MATCH` selection taken out.
#[test]
fn every_filter_seeks_the_index_its_values_are_bounds_for() {
    // One fixture per root: a filter form is compiled on the root its case
    // behaviour belongs to, and a snapshot reads under its store's order.
    let mut roots = [
        Seeded::under("find-filters", StoredPathOrder::Sensitive),
        Seeded::under("find-filters-folded", StoredPathOrder::AsciiCaseInsensitive),
    ];
    let on = |shape: ReadFilter| match root_of(shape) {
        StoredPathOrder::Sensitive => 0,
        StoredPathOrder::AsciiCaseInsensitive => 1,
    };
    let bars = filter_bars();
    for bar in &bars {
        for (part, shape, seek) in &bar.probes {
            for (params, statement) in [
                (request(), FindStatement::PathPage(PageDirection::Ascending)),
                (
                    sorted(SortKey::field("status"), Direction::Ascending),
                    FindStatement::FieldValuePage(FieldOrder::Raw, PageDirection::Ascending),
                ),
            ] {
                let plans = roots[on(*shape)].plans(&params.with_predicates([part.clone()]));
                let page = plans
                    .iter()
                    .find(|plan| plan.statement == statement)
                    .expect("the page statement");
                assert_eq!(
                    page.filters,
                    vec![*shape],
                    "{part:?} compiled to another filter"
                );
                judge_filter(&plan(page), seek);
            }
        }
    }

    // Control: the full-text selection taken out of the plan.
    let plans = roots[0].plans(&request().with_predicates([Predicate::matches("interloper")]));
    let page = plan_of(&plans, FindStatement::PathPage(PageDirection::Ascending));
    let unselected = QueryPlan::new(
        page.sql(),
        page.rows()
            .iter()
            .map(|row| PlanRow::new(row.id, row.parent, row.detail.replace("0:M1", "0:")))
            .collect(),
    );
    failure_of("a full-text read with no MATCH selection", || {
        judge_filter(&unselected, &Seek::FullText)
    });

    // Control: every index a filter seeks, dropped, each after its own filters
    // were judged with it standing.
    let mut dropped: Vec<&str> = Vec::new();
    for bar in &bars {
        for (part, shape, seek) in &bar.probes {
            let Seek::Index { dropped: index, .. } = seek else {
                continue;
            };
            if !dropped.contains(index) {
                for root in &mut roots {
                    root.drop_index(index);
                }
                dropped.push(index);
            }
            let plans = roots[on(*shape)].plans(&request().with_predicates([part.clone()]));
            let page = plan_of(&plans, FindStatement::PathPage(PageDirection::Ascending));
            failure_of(&format!("{index} dropped under {part:?}"), || {
                judge_filter(&page, seek)
            });
        }
    }
}

// ---- the work bar ----

/// Whether `statement` is a page section rather than a probe or a hydration.
fn is_page(statement: FindStatement) -> bool {
    matches!(
        statement,
        FindStatement::PathPage(_)
            | FindStatement::FieldValuePage(..)
            | FindStatement::FieldMissingPage(..)
    )
}

/// Every order a page can be read in: the path in either direction, and each
/// field order's key in either direction.
fn page_orders() -> Vec<FindParams> {
    let mut orders = vec![
        sorted(SortKey::path(), Direction::Ascending),
        sorted(SortKey::path(), Direction::Descending),
    ];
    orders.extend(
        FIELD_BARS
            .iter()
            .map(|bar| sorted(SortKey::field(bar.key), bar.direction)),
    );
    orders
}

/// Judge a page with no filter by what SQLite counted running it: no step
/// through a loop no constraint bounds, and no sort.
fn judge_unfiltered_work(seeded: &Seeded, params: &FindParams) {
    let work = seeded.page(params).work;
    assert_eq!(
        (work.page_full_scan_steps, work.page_sorts),
        (0, 0),
        "a page with no filter scanned or sorted: {work:?} for {params:?}"
    );
}

/// Judge a filtered page by what SQLite counted running it: no step through a
/// loop no constraint bounds, and at most one sort per page statement it ran.
fn judge_filtered_work(seeded: &Seeded, params: &FindParams) {
    let work = seeded.page(params).work;
    let sections = seeded
        .plans(params)
        .iter()
        .filter(|plan| is_page(plan.statement))
        .count() as u64;
    assert_eq!(
        work.page_full_scan_steps, 0,
        "a filtered page stepped through a full scan: {work:?} for {params:?}"
    );
    assert!(
        work.page_sorts <= sections,
        "a filtered page sorted more than once per page statement ({sections} ran): \
         {work:?} for {params:?}"
    );
}

/// **A page's work is judged by what SQLite counted running it, not by the
/// plan's words.** Over a vault large enough that a read of any relation end
/// to end is counted many times over, every page shape — the path page in
/// either direction, each field order's valued and missing sections in either
/// direction, first pages and continuations alike — steps through no full scan
/// and sorts nothing. Every filter shape, in every form the filter bar spells
/// it, applied to a page in each of those orders, steps through no full scan
/// either, and sorts at most once per page statement.
///
/// Controls: `documents_path_nocase` dropped, the path page with no filter
/// reads another order and the unfiltered bar fails; `document_tags_name`
/// dropped, the tag part reads its table end to end and the filtered bar
/// fails.
#[test]
fn a_page_steps_through_no_full_scan_and_sorts_only_where_a_filter_narrows_it() {
    let mut seeded = Seeded::with_bulk("find-page-work", 64);
    for order in page_orders() {
        judge_unfiltered_work(&seeded, &order);
        for bar in filter_bars() {
            for (part, _, _) in &bar.probes {
                judge_filtered_work(&seeded, &order.clone().with_predicates([part.clone()]));
            }
        }
    }
    for (_, direction, _) in PAGE_BARS {
        let params = sorted(SortKey::path(), *direction);
        judge_unfiltered_work(&seeded, &seeded.resumed(&params, None, "notes/B.md"));
    }
    for bar in FIELD_BARS {
        let params = sorted(SortKey::field(bar.key), bar.direction);
        judge_unfiltered_work(&seeded, &seeded.resumed(&params, Some("m"), "notes/a.md"));
        judge_unfiltered_work(&seeded, &seeded.resumed(&params, None, "notes/a.md"));
    }

    // Control: the index the path order is held by, gone.
    seeded.drop_index("documents_path_nocase");
    for (_, direction, _) in PAGE_BARS {
        let params = sorted(SortKey::path(), *direction);
        failure_of("documents_path_nocase dropped", || {
            judge_unfiltered_work(&seeded, &params)
        });
    }

    // Control: the index the tag part seeks, gone.
    seeded.drop_index("document_tags_name");
    let tagged = request().with_predicates([Predicate::tag("bulk")]);
    failure_of("document_tags_name dropped", || {
        judge_filtered_work(&seeded, &tagged)
    });
}

/// The VM steps the first page of `count` in `direction`, one row long,
/// took over the fixture and `bulk` more documents carrying `count`.
fn first_page_vm_steps(bulk: usize, direction: Direction) -> u64 {
    let seeded = Seeded::with_bulk(&format!("find-missing-walk-{bulk}"), bulk);
    seeded
        .page(&sorted(SortKey::field("count"), direction).with_limit(1))
        .work
        .page_vm_steps
}

/// **A field sort's missing section costs a walk of the documents that carry
/// the key, where it stands first.** A document missing the sort field orders
/// as `NULL` does: first ascending, last descending. So an ascending first
/// page reads the missing section first, and that section walks the path
/// index testing each document's marker, passing every document that carries
/// the key to reach the few that do not; a descending first page reads the
/// valued section first, a seek of the marker index that stops at the page's
/// bound. Over the fixture and 50, then 500, more documents carrying `count`,
/// the ascending one-row first page's VM steps grow with the vault, and the
/// descending one's do not.
#[test]
fn an_ascending_field_page_walks_the_documents_carrying_its_key_before_its_first_row() {
    let ascending = [50, 500].map(|bulk| first_page_vm_steps(bulk, Direction::Ascending));
    let descending = [50, 500].map(|bulk| first_page_vm_steps(bulk, Direction::Descending));
    assert!(
        ascending[1] > ascending[0] * 5,
        "the ascending first page's VM steps did not grow with the documents carrying its key: \
         {ascending:?}. This pins the documented cost of ordering missing as NULL, not a defect"
    );
    assert_eq!(
        descending[0], descending[1],
        "the descending first page's VM steps grew with the vault: {descending:?}. The valued \
         section is a seek of the marker index that stops at the page's bound"
    );
}

/// The alias a page statement reads its own rows under: `f` for a field
/// sort's valued section, which reads the marker rows, and `d` otherwise.
fn page_alias(statement: FindStatement) -> &'static str {
    match statement {
        FindStatement::FieldValuePage(..) => "f",
        _ => "d",
    }
}

/// Judge a page statement narrowed by a filter that keeps what it seeks: its
/// own rows are reached one by one, by the document id each match the filter's
/// seek handed it carries — the row id, or the document and key of its marker
/// row — so the page reads nothing of its order index and sorts the matches,
/// once.
fn judge_driven_by_filter(page: &QueryPlan, alias: &str) {
    page.assert_no_full_scan_of("documents");
    let own = rows_of(page, alias);
    assert!(
        !own.rows().is_empty()
            && own.rows().iter().all(|row| matches!(
                row.constraint(),
                Some("(rowid=?)" | "(document=? AND key=?)")
            )),
        "the page does not reach its rows by the keys its filter's seek handed it: {:?}\n\
         emitted SQL: {}",
        page.rows(),
        page.sql()
    );
    let sorts = page
        .rows()
        .iter()
        .filter(|row| row.detail.contains("TEMP B-TREE"))
        .count();
    assert!(
        sorts <= 1,
        "the page sorts more than once: {:?}\nemitted SQL: {}",
        page.rows(),
        page.sql()
    );
}

/// **A page with no filter is a seek of its order index; a page with a filter
/// drives from the filter's seek and sorts the matched set.** Every page
/// statement of every order, with no filter, builds no temporary B-tree. With
/// a filter that keeps what it seeks — every filter shape but inequality and
/// absence, in every form the filter bar spells it — every page statement
/// reaches its own rows by the key each match carries and never reads
/// `documents` end to end, so what it sorts is the filter's seek output and
/// its cost is the match count. A page narrowed only by inequality or absence
/// has no seek of what it keeps, and reads its order index as a page with no
/// filter does: no temporary B-tree.
///
/// Controls: a page with no filter, which seeks its order index, fails the
/// driven bar; `documents_path_nocase` dropped, the path page with no filter
/// sorts and fails the unfiltered bar.
#[test]
fn a_filtered_page_drives_from_its_filter_and_an_unfiltered_page_seeks_its_order() {
    let mut seeded = Seeded::new("find-page-driver");
    for order in page_orders() {
        for plan in seeded.plans(&order) {
            if is_page(plan.statement) {
                self::plan(&plan).assert_no_temp_btree();
            }
        }
        for bar in filter_bars() {
            for (part, shape, _) in &bar.probes {
                let params = order.clone().with_predicates([part.clone()]);
                for page in seeded.plans(&params) {
                    if !is_page(page.statement) {
                        continue;
                    }
                    if shape.excludes() {
                        self::plan(&page).assert_no_temp_btree();
                    } else {
                        judge_driven_by_filter(&self::plan(&page), page_alias(page.statement));
                    }
                }
            }
        }
    }

    // Control: a page no filter narrows seeks its order index.
    for order in page_orders() {
        for page in seeded.plans(&order) {
            if is_page(page.statement) {
                failure_of("a page with no filter", || {
                    judge_driven_by_filter(&self::plan(&page), page_alias(page.statement))
                });
            }
        }
    }

    // Control: the index the path order is held by, gone.
    seeded.drop_index("documents_path_nocase");
    let page = plan_of(
        &seeded.plans(&request()),
        FindStatement::PathPage(PageDirection::Ascending),
    );
    failure_of("documents_path_nocase dropped", || {
        page.assert_no_temp_btree()
    });
}

// ---- what a page answers ----

/// **A document whose sort field holds a set appears once, at its least value,
/// under either order and in either direction.** `notes/B.md` holds `count:
/// [10, 9]`: its least raw value is `"10"` and its least number is nine, and
/// those are the one value it is ordered by under each order.
///
/// The marker rows are the only rows the valued section touches: its plan reads
/// the order's marker index, whose partial predicate holds one row per document
/// and key, and nothing else of `document_fields`. `notes/c.md`'s `nine` reads
/// as no number, so the typed order has no value for it and it stands in the
/// missing section, beside the documents with no `count` at all.
#[test]
fn a_set_valued_sort_field_orders_a_document_once_at_its_least_value() {
    let seeded = Seeded::new("find-set-valued");
    let integer = |number: i64| integer_order().sort_key(&number.to_string());
    // A first page runs in the typed order where the declaration gives the key
    // one, and in the raw order where it does not.
    let read = |direction: Direction, declared: &DeclaredFields, order: FieldOrder| {
        let page = seeded
            .snapshot()
            .find(&sorted(SortKey::field("count"), direction), declared)
            .expect("a page");
        // A page in a typed order is read under the pinned schema, and one in
        // the raw order under none.
        assert_eq!(
            page.snapshot.schema_fingerprint.as_deref(),
            (order == FieldOrder::Typed).then_some(SEED_SCHEMA)
        );
        let keys = keyed(&seeded, &SortKey::field("count"), direction, declared);
        assert_eq!(
            keys.iter()
                .map(|(path, _)| path.clone())
                .collect::<Vec<_>>(),
            row_paths(&page)
        );
        keys
    };
    let row = |path: &str, sort: Option<String>| (path.to_string(), sort);
    let then = |mut first: Vec<(String, Option<String>)>, second: Vec<(String, Option<String>)>| {
        first.extend(second);
        first
    };
    let reversed = |rows: &Vec<(String, Option<String>)>| rows.iter().rev().cloned().collect();

    let missing_typed = vec![
        row("notes/c.md", None),
        row("other/glossary.md", None),
        row("other/v1.2.md", None),
    ];
    let valued_typed = vec![row("notes/a.md", integer(3)), row("notes/B.md", integer(9))];
    let typed = declared();
    assert_eq!(
        read(Direction::Ascending, &typed, FieldOrder::Typed),
        then(missing_typed.clone(), valued_typed.clone())
    );
    assert_eq!(
        read(Direction::Descending, &typed, FieldOrder::Typed),
        then(reversed(&valued_typed), reversed(&missing_typed))
    );

    let missing_raw = vec![row("other/glossary.md", None), row("other/v1.2.md", None)];
    let valued_raw = vec![
        row("notes/B.md", Some("10".to_string())),
        row("notes/a.md", Some("3".to_string())),
        row("notes/c.md", Some("nine".to_string())),
    ];
    let raw = declared_raw();
    assert_eq!(
        read(Direction::Ascending, &raw, FieldOrder::Raw),
        then(missing_raw.clone(), valued_raw.clone())
    );
    assert_eq!(
        read(Direction::Descending, &raw, FieldOrder::Raw),
        then(reversed(&valued_raw), reversed(&missing_raw))
    );

    for (order, index, declared) in [
        (FieldOrder::Raw, "document_fields_least_raw", &raw),
        (FieldOrder::Typed, "document_fields_least_typed", &typed),
    ] {
        for (direction, page_direction) in [
            (Direction::Ascending, PageDirection::Ascending),
            (Direction::Descending, PageDirection::Descending),
        ] {
            let resumed = seeded.resumed_under(
                &sorted(SortKey::field("count"), direction),
                declared,
                Some("0"),
                "notes/a.md",
            );
            let plans = seeded.plans_under(&resumed, declared);
            let valued = plan_of(&plans, FindStatement::FieldValuePage(order, page_direction));
            let touched = valued.searches_of("document_fields");
            assert!(
                !touched.is_empty()
                    && touched
                        .iter()
                        .all(|row| row.access() == Some(Access::Index(index))),
                "the valued section reads `document_fields` other than through {index}: {:?}",
                valued.rows()
            );
            valued.assert_no_full_scan();
        }
    }
}

/// Each row a request sorted by `key` answers under `declared`, as the key a
/// page stopping at it names in its cursor: its path, and the value it was
/// ordered by.
///
/// A page of `n` rows names its `n`th row's key. The last row of all ends no
/// page that has a row after it, so its key is named by the first page of one
/// row in the other direction, which starts at it.
fn keyed(
    seeded: &Seeded,
    key: &SortKey,
    direction: Direction,
    declared: &DeclaredFields,
) -> Vec<(String, Option<String>)> {
    let page = |direction: Direction, limit: usize| {
        let limit = u32::try_from(limit).expect("a page bound");
        seeded
            .snapshot()
            .find(&sorted(key.clone(), direction).with_limit(limit), declared)
            .expect("a page")
    };
    let named = |found: Found| match found.next.as_ref().map(Cursor::key) {
        Some(CursorKey::Document { sort, path, .. }) => (path.clone(), sort.clone()),
        other => panic!("a page with a row after it names a document: {other:?}"),
    };
    let count = page(direction, MAX_PAGE).rows.len();
    let reverse = match direction {
        Direction::Ascending => Direction::Descending,
        _ => Direction::Ascending,
    };
    (1..count)
        .map(|limit| named(page(direction, limit)))
        .chain(std::iter::once(named(page(reverse, 1))))
        .collect()
}

/// Every path a request's pages hold, drained a page of `limit` at a time,
/// each page continuing the cursor the one before it minted.
fn drained(seeded: &Seeded, params: &FindParams, limit: u32) -> Vec<String> {
    let first = params.clone().with_limit(limit);
    let mut request = first.clone();
    let mut paths = Vec::new();
    for _ in 0..32 {
        let page = seeded.page(&request);
        assert!(page.rows.len() <= limit as usize);
        paths.extend(row_paths(&page));
        let Some(next) = page.next else {
            return paths;
        };
        request = first.clone().with_after(next);
    }
    panic!("the pages did not end: {paths:?}");
}

/// **A continuation resumes exactly where its page stopped.** Drained a row at
/// a time and two rows at a time, every order yields the same rows as one page
/// holding them all — across a field sort's section boundary in either
/// direction, and through the case-insensitive path order, where `notes/B.md`
/// stands between `notes/a.md` and `notes/c.md`.
#[test]
fn a_continuation_resumes_exactly_where_its_page_stopped() {
    let seeded = Seeded::new("find-continuation");
    for (key, direction) in [
        (SortKey::path(), Direction::Ascending),
        (SortKey::path(), Direction::Descending),
        (SortKey::field("status"), Direction::Ascending),
        (SortKey::field("status"), Direction::Descending),
        (SortKey::field("count"), Direction::Ascending),
        (SortKey::field("count"), Direction::Descending),
    ] {
        let params = sorted(key.clone(), direction);
        let whole = drained(&seeded, &params, 100);
        assert_eq!(whole.len(), 5, "{key:?} {direction:?}: {whole:?}");
        for limit in [1, 2] {
            assert_eq!(
                drained(&seeded, &params, limit),
                whole,
                "{key:?} {direction:?} drained {limit} at a time"
            );
        }
    }
    let paths = drained(&seeded, &sorted(SortKey::path(), Direction::Ascending), 1);
    assert_eq!(
        paths,
        [
            "notes/a.md",
            "notes/B.md",
            "notes/c.md",
            "other/glossary.md",
            "other/v1.2.md"
        ]
    );
}

/// **Each filter answers the documents its part names.**
#[test]
fn each_filter_answers_the_documents_its_part_names() {
    let seeded = Seeded::new("find-filter-rows");
    let with = |part: Predicate| seeded.paths(&request().with_predicates([part]));
    let target = |text: &str| ResolutionTarget::new(text).expect("a target");
    assert_eq!(
        with(Predicate::equal_to("status", "open")),
        ["notes/a.md", "notes/c.md"]
    );
    assert_eq!(
        with(Predicate::not_equal_to("status", "open")),
        ["notes/B.md", "other/glossary.md", "other/v1.2.md"]
    );
    assert_eq!(
        with(Predicate::in_any(
            "status",
            ["done".to_string(), "closed".to_string()]
        )),
        ["notes/B.md", "other/v1.2.md"]
    );
    assert_eq!(
        with(Predicate::has("count")),
        ["notes/a.md", "notes/B.md", "notes/c.md", "other/v1.2.md"]
    );
    assert_eq!(with(Predicate::missing("count")), ["other/glossary.md"]);
    // Raw text: "closed" and "done" sort before "o".
    assert_eq!(
        with(Predicate::before("status", "o")),
        ["notes/B.md", "other/v1.2.md"]
    );
    // Typed: nine and ten are after five; "nine" is no number and three is not.
    assert_eq!(with(Predicate::after("count", "5")), ["notes/B.md"]);
    assert_eq!(with(Predicate::before("count", "5")), ["notes/a.md"]);
    // Typed equality is the typed order's: `09` is nine, which `notes/B.md`
    // holds; `nine` is no number, so no number equals it and every number
    // differs from it.
    assert_eq!(with(Predicate::equal_to("count", "09")), ["notes/B.md"]);
    assert_eq!(
        with(Predicate::not_equal_to("count", "03")),
        [
            "notes/B.md",
            "notes/c.md",
            "other/glossary.md",
            "other/v1.2.md"
        ]
    );
    assert_eq!(
        with(Predicate::in_any(
            "count",
            ["03".to_string(), "010".to_string()]
        )),
        ["notes/a.md", "notes/B.md"]
    );
    assert_eq!(with(Predicate::matches("interloper")), ["notes/a.md"]);
    assert_eq!(
        with(Predicate::path("notes/*.md")),
        ["notes/a.md", "notes/B.md", "notes/c.md"]
    );
    assert_eq!(with(Predicate::path("**/v1.?.md")), ["other/v1.2.md"]);
    assert_eq!(
        with(Predicate::resolves(target("glossary#Design"))),
        ["other/glossary.md"]
    );
    // Both reductions: `v1.2` is the stem `v1.2` and the stem `v1` with `.2`
    // taken as an extension.
    assert_eq!(
        with(Predicate::resolves(target("other/v1.2"))),
        ["other/v1.2.md"]
    );
    assert_eq!(with(Predicate::tag("draft")), ["notes/a.md"]);
    assert_eq!(
        with(Predicate::has_finding(FindingKind::BodyBytesNotUtf8)),
        ["other/glossary.md"]
    );
    assert_eq!(
        with(Predicate::has_finding(FindingKind::FrontmatterUnreadable)),
        Vec::<String>::new()
    );
    // A conjunction is every part at once.
    assert_eq!(
        seeded.paths(&request().with_predicates([
            Predicate::has("count"),
            Predicate::path("notes/**"),
            Predicate::not_equal_to("status", "closed"),
        ])),
        ["notes/a.md", "notes/c.md"]
    );
}

/// **A part no document can satisfy is reported, and the page is empty.** A
/// resolution target that is not a suffix address names no document, and
/// neither does a glob that does not parse.
#[test]
fn a_part_no_document_can_satisfy_is_reported_rather_than_read_as_an_empty_vault() {
    let seeded = Seeded::new("find-unsatisfiable");
    let target = ResolutionTarget::new("../escape").expect("a target");
    let page =
        seeded.page(&request().with_predicates([Predicate::resolves(target), Predicate::path("")]));
    assert!(page.rows.is_empty());
    assert_eq!(
        page.unsatisfied,
        vec![
            Unsatisfied::impossible_path("../escape"),
            Unsatisfied::malformed_glob("", "a pattern cannot be empty"),
        ]
    );
}

/// **A match part whose query the full-text engine cannot parse is reported
/// with the engine's words, and the page is empty**, as a malformed glob's is:
/// a part with no meaning narrows the answer to nothing, so the page is never
/// broader than the request. A dangling operator and an unterminated phrase
/// are each a query the engine refuses to read; the page they stand in holds
/// no row and no cursor, beside another part or alone, and the probe that read
/// the query is the one statement a page runs for it. A query the engine reads
/// is applied: it narrows the page and reports nothing.
#[test]
fn a_malformed_full_text_query_is_reported_and_empties_the_page() {
    let seeded = Seeded::new("find-malformed-query");
    // A bound below the vault's size, so a page answered without the part
    // would carry a cursor, and a tag part that alone holds a row.
    let bounded = || request().with_limit(1);
    assert!(seeded.page(&bounded()).next.is_some());
    assert_eq!(
        seeded.paths(&bounded().with_predicates([Predicate::tag("draft")])),
        ["notes/a.md"]
    );
    for query in ["interloper AND", "\"interloper"] {
        let page = seeded.page(&bounded().with_predicates([Predicate::matches(query)]));
        assert!(page.rows.is_empty(), "{query:?}: {:?}", row_paths(&page));
        assert!(page.next.is_none(), "{query:?} minted a cursor");
        let [
            Unsatisfied::MalformedQuery {
                query: named,
                problem,
                ..
            },
        ] = page.unsatisfied.as_slice()
        else {
            panic!(
                "{query:?} was not reported as malformed: {:?}",
                page.unsatisfied
            );
        };
        assert_eq!(named, query);
        assert!(!problem.is_empty(), "{query:?} carries no problem");

        let beside = seeded
            .page(&bounded().with_predicates([Predicate::matches(query), Predicate::tag("draft")]));
        assert!(
            beside.rows.is_empty(),
            "{query:?} beside a tag part: {:?}",
            row_paths(&beside)
        );
        assert!(beside.next.is_none(), "{query:?} beside a tag part");
        assert_eq!(beside.unsatisfied, page.unsatisfied);

        let plans = seeded.plans(&bounded().with_predicates([Predicate::matches(query)]));
        plan_of(&plans, FindStatement::MatchProbe);
        assert!(
            plans
                .iter()
                .all(|plan| !matches!(plan.statement, FindStatement::PathPage(_))),
            "an empty page still ran a page statement: {plans:?}"
        );
    }
    let page = seeded.page(&request().with_predicates([Predicate::matches("interloper")]));
    assert_eq!(row_paths(&page), ["notes/a.md"]);
    assert!(page.unsatisfied.is_empty(), "{:?}", page.unsatisfied);
}

/// **A full-text index the store cannot read is the store's fault, not a
/// malformed query.** With `documents_fts` dropped out of band, the probe that
/// asks whether a match part's query parses does not prepare, and the find is
/// refused as the store's — never answered as a page reporting the query.
#[test]
fn a_full_text_index_that_does_not_prepare_refuses_the_find() {
    let mut seeded = Seeded::new("find-match-probe-unprepared");
    induced_failure::execute_out_of_band(&mut seeded.store, "DROP TABLE documents_fts")
        .unwrap_or_else(|problem| panic!("dropping documents_fts: {problem}"));
    let answered = seeded.snapshot().find(
        &request().with_predicates([Predicate::matches("interloper")]),
        &declared(),
    );
    let Err(PageRefusal::Store(StoreError::Sql { operation, message })) = answered else {
        panic!(
            "a find over a missing full-text index was not refused as the store's: {answered:?}"
        );
    };
    assert_eq!(operation, "asking whether a full-text query parses");
    assert!(message.contains("documents_fts"), "{message}");
}

/// **A part the store keeps no index of, or a value that names no place in
/// its key's order, is refused.** `links_to` filters by a link's target, which
/// the store keeps no index of, and the refusal names that fact.
#[test]
fn a_part_the_store_cannot_answer_is_refused_by_name() {
    let seeded = Seeded::new("find-refusals");
    let refusal = seeded
        .snapshot()
        .find(
            &request().with_predicates([Predicate::links_to(
                ResolutionTarget::new("glossary").expect("a target"),
            )]),
            &declared(),
        )
        .expect_err("a links_to part is refused");
    assert_eq!(
        refusal,
        PageRefusal::NotIndexed {
            fact: "a link's target",
        }
    );
    assert_eq!(
        refusal.to_string(),
        "the store keeps no index of a link's target"
    );
    let refusal = seeded
        .snapshot()
        .find(
            &request().with_predicates([Predicate::before("count", "many")]),
            &declared(),
        )
        .expect_err("a bound that reads as no number is refused");
    assert_eq!(
        refusal,
        PageRefusal::UnreadableBound {
            key: "count".to_string(),
            value: "many".to_string(),
        }
    );
    for part in [
        Predicate::equal_to("count", "many"),
        Predicate::not_equal_to("count", "many"),
        Predicate::in_any("count", ["3".to_string(), "many".to_string()]),
    ] {
        let refusal = seeded
            .snapshot()
            .find(&request().with_predicates([part.clone()]), &declared())
            .expect_err("a compared value that reads as no number is refused");
        assert_eq!(
            refusal,
            PageRefusal::UnreadableBound {
                key: "count".to_string(),
                value: "many".to_string(),
            },
            "{part:?}"
        );
    }
}

/// **A count a request names outside the range the store holds it to is
/// refused, never clamped or read as an empty part.** A page bound of none or
/// of more than [`MAX_PAGE`] rows, a membership part naming no value, and one
/// naming more than [`IN_VALUES_CEILING`] are each refused with the fact that
/// makes them unanswerable; the bounds themselves are answered. The refusal
/// stands before anything is compiled, so a membership part on a key no
/// document carries is refused the same way, and the plans a request would
/// run are refused where the request is.
#[test]
fn a_count_outside_its_bound_is_refused_rather_than_clamped() {
    let seeded = Seeded::new("find-out-of-bound");
    let refused = |params: &FindParams| {
        let rows = seeded
            .snapshot()
            .find(params, &declared())
            .expect_err("a count outside its bound is refused");
        let plans = seeded
            .snapshot()
            .find_plans(params, &declared())
            .expect_err("a count outside its bound is refused");
        assert_eq!(rows, plans, "{params:?}");
        rows
    };
    for limit in [0, MAX_PAGE as u32 + 1, u32::MAX] {
        assert_eq!(
            refused(&request().with_limit(limit)),
            PageRefusal::OutOfBound {
                bound: ReadBound::PageRows,
                given: limit as usize,
            },
            "a page bound of {limit}"
        );
    }
    let answered = seeded
        .snapshot()
        .find(&request().with_limit(MAX_PAGE as u32), &declared())
        .expect("a page of the most rows a page holds");
    assert_eq!(answered.rows.len(), 5);

    for key in ["status", "nothing"] {
        assert_eq!(
            refused(&request().with_predicates([Predicate::in_any(key, Vec::new())])),
            PageRefusal::EmptyMembership {
                key: key.to_string()
            }
        );
    }
    let values = |count: usize| (0..count).map(|value| format!("value-{value}"));
    assert_eq!(
        refused(
            &request()
                .with_predicates([Predicate::in_any("status", values(IN_VALUES_CEILING + 1))])
        ),
        PageRefusal::OutOfBound {
            bound: ReadBound::MembershipValues,
            given: IN_VALUES_CEILING + 1,
        }
    );
    let most = seeded.page(&request().with_predicates([Predicate::in_any(
        "status",
        values(IN_VALUES_CEILING - 1).chain(["open".to_string()]),
    )]));
    assert_eq!(row_paths(&most), ["notes/a.md", "notes/c.md"]);
}

/// **A page holds the bound it names, the default where it names none, and at
/// most [`MAX_PAGE`]** — a bound outside that is refused, which
/// `a_count_outside_its_bound_is_refused_rather_than_clamped` holds; **each
/// statement it runs is counted on its snapshot.** A
/// path page is one statement beside the fingerprint read. A field sort whose
/// first section does not fill the page reads the second, and one that fills
/// it stops there.
#[test]
fn a_page_holds_its_bound_and_counts_each_statement_it_runs() {
    let seeded = Seeded::new("find-bound");
    const { assert!(DEFAULT_PAGE >= 5 && DEFAULT_PAGE <= MAX_PAGE) };
    let count = |params: &FindParams| {
        let snapshot = seeded.snapshot();
        let before = snapshot.counters().statements_executed();
        let page = snapshot.find(params, &declared()).expect("a page");
        (
            page.rows.len(),
            page.next.is_some(),
            snapshot.counters().statements_executed() - before,
        )
    };
    // Every page reads the active fingerprint once, which its declaration is
    // judged against, and then its sections.
    assert_eq!(count(&request()), (5, false, 2));
    assert_eq!(count(&request().with_limit(2)), (2, true, 2));
    assert_eq!(count(&request().with_limit(1)), (1, true, 2));
    assert_eq!(count(&request().with_limit(MAX_PAGE as u32)), (5, false, 2));
    let by_status = sorted(SortKey::field("status"), Direction::Ascending);
    assert_eq!(count(&by_status), (5, false, 3));
    // Ascending, the missing section's one row does not fill a page of one and
    // the row after it: the valued section is read for that row.
    assert_eq!(count(&by_status.with_limit(1)), (1, true, 3));
    // Descending, the valued section fills it.
    assert_eq!(
        count(&sorted(SortKey::field("status"), Direction::Descending).with_limit(1)),
        (1, true, 2)
    );
    // A finding part binds that same reading, and costs no further statement.
    assert_eq!(
        count(&request().with_predicates([Predicate::has_finding(FindingKind::BodyBytesNotUtf8)])),
        (1, false, 2)
    );
}

// ---- the glob a statement runs ----

/// A deterministic stream of choices, so the corpus is the same on every run.
struct Choices(u64);

impl Choices {
    /// A choice below `bound`, from an xorshift64 stream.
    fn below(&mut self, bound: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % bound as u64) as usize
    }

    fn pick<'a>(&mut self, from: &[&'a str]) -> &'a str {
        from[self.below(from.len())]
    }

    /// `/`-joined picks from `from`, one to three of them.
    fn joined(&mut self, from: &[&str]) -> String {
        let depth = 1 + self.below(3);
        (0..depth)
            .map(|_| self.pick(from))
            .collect::<Vec<&str>>()
            .join("/")
    }
}

/// **The glob a statement runs agrees with the in-process matcher.** A corpus
/// of generated paths is written, and every generated pattern is asked of it
/// through a find: the page is exactly the paths [`Pattern::matches`] accepts.
/// The patterns mix literal prefixes, `?`, `*`, whole-segment `**` in every
/// position and non-ASCII characters, so the range each one seeks and the
/// function run over it are both on trial — a range narrower than the pattern
/// drops a path the matcher keeps.
#[test]
fn the_glob_a_statement_runs_agrees_with_the_in_process_matcher() {
    const SEGMENTS: &[&str] = &["a", "b", "ab", "ba", "é", "a.md", "ab.md", "B.md", "é.md"];
    const PARTS: &[&str] = &[
        "a", "b", "ab", "é", "*", "?", "**", "a*", "*b", "?.md", "*.md", "a?", "é*", "*.*", "a**",
        "?b*",
    ];
    let mut choices = Choices(0x9E37_79B9_7F4A_7C15);
    let mut paths: Vec<String> = Vec::new();
    while paths.len() < 64 {
        let path = choices.joined(SEGMENTS);
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    let mut patterns: Vec<String> = Vec::new();
    while patterns.len() < 64 {
        let pattern = choices.joined(PARTS);
        if !patterns.contains(&pattern) {
            patterns.push(pattern);
        }
    }

    let scratch = Scratch::new("find-glob-corpus");
    let mut store = scratch.open();
    let mut writes = store.begin_request();
    write_documents(
        &mut writes,
        &paths
            .iter()
            .enumerate()
            .map(|(index, path)| document(path, &format!("hash-{index}"), "a body\n"))
            .collect::<Vec<_>>(),
    );
    let reader = Arc::new(store.open_reader().reader.expect("a reader"));
    let snapshot = reader
        .try_take()
        .expect("a free handle")
        .establish()
        .snapshot
        .expect("a snapshot");

    let mut pairs = 0;
    let mut matched = 0;
    for source in &patterns {
        let pattern = Pattern::parse(source).expect("a generated pattern parses");
        let mut expected: Vec<&str> = paths
            .iter()
            .map(String::as_str)
            .filter(|path| pattern.matches(path, norn_wire::CaseFold::Exact))
            .collect();
        expected.sort_unstable();
        let page = snapshot
            .find(
                &request()
                    .with_predicates([Predicate::path(source.clone())])
                    .with_limit(MAX_PAGE as u32),
                &DeclaredFields::none(),
            )
            .expect("a page");
        let mut answered = row_paths(&page);
        answered.sort_unstable();
        assert_eq!(
            answered, expected,
            "the statement and the matcher part on `{source}`"
        );
        pairs += paths.len();
        matched += expected.len();
    }
    assert!(pairs >= 4000, "the corpus holds {pairs} pairs");
    assert!(
        matched > 0 && matched < pairs / 2,
        "the corpus matched {matched} of {pairs} pairs, which says little either way"
    );
}

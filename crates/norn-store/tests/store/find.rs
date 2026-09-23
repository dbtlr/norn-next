//! The find builder: what a page answers, and the plan bars every statement and
//! filter it emits is judged by.
//!
//! Every plan here is one [`Snapshot::find_plans`] took of the statements
//! [`Snapshot::find_keys`] runs for the same request, on the snapshot's own
//! read-only connection, so a bar judges the SQL a page actually reads. Each
//! bar runs its negative control in the same case: the index the bar names is
//! dropped, or the plan is rebuilt without the row the bar is about, and the
//! bar is shown to fail.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use crate::common::{Scratch, document, violation, write_documents};
use norn_store::{
    DEFAULT_PAGE, DeclaredFields, FIND_FILTERS, FIND_STATEMENTS, FieldOrder, FindFilter, FindPlan,
    FindPosition, FindRefusal, FindStatement, FrontmatterValue, KeyPage, MAX_PAGE, Nested,
    PageDirection, Resume, Snapshot, SnapshotReader, Store, TagFact, TagSource, TypedOrder,
    induced_failure,
};
use norn_testkit::explain::{Access, PlanRow, QueryPlan};
use norn_wire::{
    Column, Direction, FindParams, FindingKind, Pattern, Predicate, ResolutionTarget, Sort,
    SortKey, Unsatisfied, VaultAddress, VaultName,
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
        let scratch = Scratch::new(label);
        let mut store = scratch.open();
        seed(&mut store);
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

    pub(crate) fn snapshot(&self) -> Snapshot {
        self.reader
            .try_take()
            .expect("a handle nothing is reading holds its connection")
            .establish()
            .snapshot
            .expect("a snapshot")
    }

    fn page(&self, params: &FindParams, resume: Option<&Resume>) -> KeyPage {
        self.snapshot()
            .find_keys(params, &declared(), resume)
            .expect("a page")
    }

    fn paths(&self, params: &FindParams) -> Vec<String> {
        self.page(params, None)
            .keys
            .iter()
            .map(|key| key.path().to_string())
            .collect()
    }

    pub(crate) fn plans(&self, params: &FindParams, resume: Option<&Resume>) -> Vec<FindPlan> {
        self.plans_under(params, &declared(), resume)
    }

    fn plans_under(
        &self,
        params: &FindParams,
        declared: &DeclaredFields,
        resume: Option<&Resume>,
    ) -> Vec<FindPlan> {
        self.snapshot()
            .find_plans(params, declared, resume)
            .expect("the plans of a request")
    }

    /// Drop `index` on the writer, which is what a negative control judges the
    /// same bar against. A snapshot established after it reads the new schema.
    fn drop_index(&mut self, index: &str) {
        induced_failure::execute_out_of_band(&mut self.store, &format!("DROP INDEX {index}"))
            .unwrap_or_else(|problem| panic!("dropping {index}: {problem}"));
    }
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
fn rows_of(plan: &QueryPlan, alias: &str) -> QueryPlan {
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
fn failure_of(control: &str, bar: impl FnOnce()) -> String {
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
        | FindStatement::NestedTotal(_) => {
            "hydration_reads_the_page_rows_by_id_and_each_collection_by_its_ordinal_index"
        }
    }
}

/// Which test bars each filter the builder names. Exhaustive for the reason
/// [`statement_barred_by`] is.
fn filter_barred_by(filter: FindFilter) -> &'static str {
    match filter {
        FindFilter::Equal(_)
        | FindFilter::NotEqual(_)
        | FindFilter::Member(_)
        | FindFilter::Present
        | FindFilter::Absent
        | FindFilter::Before(_)
        | FindFilter::After(_)
        | FindFilter::FullText
        | FindFilter::PathGlob
        | FindFilter::Resolves
        | FindFilter::Tag
        | FindFilter::Finding => "every_filter_seeks_the_index_its_values_are_bounds_for",
    }
}

/// Every form a filter slot takes: a filter that compares values compares
/// under either order, and each order is a form its bar has to probe.
/// Exhaustive, so a filter added to [`FindFilter`] names its forms here.
fn forms_of(filter: FindFilter) -> Vec<FindFilter> {
    let orders = [FieldOrder::Raw, FieldOrder::Typed];
    match filter {
        FindFilter::Equal(_) => orders.map(FindFilter::Equal).to_vec(),
        FindFilter::NotEqual(_) => orders.map(FindFilter::NotEqual).to_vec(),
        FindFilter::Member(_) => orders.map(FindFilter::Member).to_vec(),
        FindFilter::Before(_) => orders.map(FindFilter::Before).to_vec(),
        FindFilter::After(_) => orders.map(FindFilter::After).to_vec(),
        FindFilter::Present
        | FindFilter::Absent
        | FindFilter::FullText
        | FindFilter::PathGlob
        | FindFilter::Resolves
        | FindFilter::Tag
        | FindFilter::Finding => vec![filter],
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

    let filters: Vec<FindFilter> = filter_bars().iter().map(|bar| bar.shape).collect();
    let mut filter_slots: Vec<usize> = filters.iter().map(|filter| filter.slot()).collect();
    filter_slots.sort_unstable();
    assert_eq!(
        filter_slots,
        (0..FIND_FILTERS).collect::<Vec<usize>>(),
        "the filter bar does not judge every filter slot exactly once: {filters:?}"
    );
    for (slot, filter) in FindFilter::all().into_iter().enumerate() {
        assert_eq!(filter.slot(), slot, "{filter:?} claims another slot");
    }
    for bar in filter_bars() {
        let mut probed: Vec<FindFilter> = Vec::new();
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
    let continuation = Resume {
        at: FindPosition {
            sort: None,
            path: "notes/B.md".to_string(),
        },
    };
    let judge = |page: &QueryPlan, constraint: &str| {
        page.assert_no_full_scan();
        page.assert_searches_through("documents", Access::Index("documents_path_nocase"));
        page.assert_search_constraint("documents", constraint);
        page.assert_no_temp_btree();
    };
    for (statement, direction, constraint) in PAGE_BARS {
        for resume in [None, Some(&continuation)] {
            let plans = seeded.plans(&sorted(SortKey::path(), *direction), resume);
            judge(&plan_of(&plans, *statement), constraint);
        }
    }
    // A request that names no order pages by path, ascending.
    judge(
        &plan_of(
            &seeded.plans(&request(), None),
            FindStatement::PathPage(PageDirection::Ascending),
        ),
        "(path>?)",
    );

    let fingerprint = plan_of(
        &seeded.plans(
            &request().with_predicates([Predicate::has_finding(FindingKind::BodyBytesNotUtf8)]),
            None,
        ),
        FindStatement::ActiveFingerprint,
    );
    fingerprint.assert_searches_through("meta", Access::PrimaryKey);
    fingerprint.assert_search_constraint("meta", "(key=?)");

    // Control: the continuation's bound taken out of the search it opens.
    let resumed = plan_of(
        &seeded.plans(
            &sorted(SortKey::path(), Direction::Ascending),
            Some(&continuation),
        ),
        FindStatement::PathPage(PageDirection::Ascending),
    );
    let unbounded = QueryPlan::new(
        resumed.sql(),
        resumed
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
        let plans = seeded.plans(&sorted(SortKey::path(), *direction), None);
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
        let in_valued = Resume {
            at: FindPosition {
                sort: Some("m".to_string()),
                path: "notes/a.md".to_string(),
            },
        };
        let in_missing = Resume {
            at: FindPosition {
                sort: None,
                path: "notes/a.md".to_string(),
            },
        };
        let first = seeded.plans(&params, None);
        judge_valued(&plan_of(&first, bar.valued), bar);
        judge_missing(&plan_of(&first, bar.missing), bar);
        judge_valued(
            &plan_of(&seeded.plans(&params, Some(&in_valued)), bar.valued),
            bar,
        );
        judge_missing(
            &plan_of(&seeded.plans(&params, Some(&in_missing)), bar.missing),
            bar,
        );

        // Control: the continuation's `(value, path)` bound taken out.
        let resumed = plan_of(&seeded.plans(&params, Some(&in_valued)), bar.valued);
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
            let plans = seeded.plans(&sorted(SortKey::field(bar.key), bar.direction), None);
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
    let plans = seeded.plans(&params, None);
    judge_known(&plans);
    judge_universe(&plans);

    let declared_only = seeded.plans(
        &request()
            .with_predicates([Predicate::has("status")])
            .with_sort(Sort::new(SortKey::field("count"), Direction::Ascending)),
        None,
    );
    assert!(
        declared_only
            .iter()
            .all(|plan| !KEY_PROBES.contains(&plan.statement)),
        "a declared key asked the snapshot whether it is known: {declared_only:?}"
    );

    seeded.drop_index("document_fields_presence");
    let plans = seeded.plans(&params, None);
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
        &seeded.plans(&params, None),
        FindStatement::BareDirectory,
    ));
    // A glob with a wildcard is no directory, and asks nothing.
    assert!(
        seeded
            .plans(
                &request().with_predicates([Predicate::path("notes/*")]),
                None
            )
            .iter()
            .all(|plan| plan.statement != FindStatement::BareDirectory)
    );

    seeded.drop_index("documents_path");
    let plan = plan_of(&seeded.plans(&params, None), FindStatement::BareDirectory);
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
    let probe = plan_of(&seeded.plans(&params, None), FindStatement::MatchProbe);
    judge(&probe);
    assert!(
        seeded
            .plans(&request().with_predicates([Predicate::tag("draft")]), None)
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

/// Every hydration statement: the document rows, and each collection's head
/// and total.
fn hydration_statements() -> Vec<FindStatement> {
    std::iter::once(FindStatement::HydrateDocuments)
        .chain(Nested::ALL.into_iter().flat_map(|nested| {
            [
                FindStatement::NestedHead(nested),
                FindStatement::NestedTotal(nested),
            ]
        }))
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
/// total counts the same index, and never the table.
///
/// Controls: the document plan rebuilt with its row-id seek as a scan; each
/// ordinal index dropped, its head and total read something else.
#[test]
fn hydration_reads_the_page_rows_by_id_and_each_collection_by_its_ordinal_index() {
    let mut seeded = Seeded::new("find-hydration");
    let params = request().with_columns([
        Column::fields(),
        Column::body(),
        Column::tags(),
        Column::headings(),
        Column::blocks(),
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
    let plans = seeded.plans(&params, None);
    let documents = plan_of(&plans, FindStatement::HydrateDocuments);
    judge_documents(&documents);
    for nested in Nested::ALL {
        judge_head(&plans, nested);
        judge_total(&plans, nested);
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

    // Control: each ordinal index, dropped.
    for nested in Nested::ALL {
        seeded.drop_index(&ordinal_index(nested));
        let plans = seeded.plans(&params, None);
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
    shape: FindFilter,
    probes: Vec<(Predicate, FindFilter, Seek)>,
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
            shape: FindFilter::Equal(FieldOrder::Raw),
            probes: vec![
                (
                    Predicate::equal_to("status", "open"),
                    FindFilter::Equal(FieldOrder::Raw),
                    field_seek("fv", "document_fields_raw", "(key=? AND raw=?)"),
                ),
                (
                    Predicate::equal_to("count", "9"),
                    FindFilter::Equal(FieldOrder::Typed),
                    field_seek("fv", "document_fields_typed", "(key=? AND typed=?)"),
                ),
            ],
        },
        FilterBar {
            shape: FindFilter::NotEqual(FieldOrder::Raw),
            probes: vec![
                (
                    Predicate::not_equal_to("status", "open"),
                    FindFilter::NotEqual(FieldOrder::Raw),
                    field_seek("fv", "document_fields_raw", "(key=? AND raw=?)"),
                ),
                (
                    Predicate::not_equal_to("count", "9"),
                    FindFilter::NotEqual(FieldOrder::Typed),
                    field_seek("fv", "document_fields_typed", "(key=? AND typed=?)"),
                ),
            ],
        },
        FilterBar {
            shape: FindFilter::Member(FieldOrder::Raw),
            probes: vec![
                (
                    Predicate::in_any("status", ["open".to_string(), "done".to_string()]),
                    FindFilter::Member(FieldOrder::Raw),
                    field_seek("fv", "document_fields_raw", "(key=? AND raw=?)"),
                ),
                (
                    Predicate::in_any("count", ["3".to_string(), "9".to_string()]),
                    FindFilter::Member(FieldOrder::Typed),
                    field_seek("fv", "document_fields_typed", "(key=? AND typed=?)"),
                ),
            ],
        },
        FilterBar {
            shape: FindFilter::Present,
            probes: vec![(
                Predicate::has("status"),
                FindFilter::Present,
                field_seek("fp", "document_fields_presence", "(key=?)"),
            )],
        },
        FilterBar {
            shape: FindFilter::Absent,
            probes: vec![(
                Predicate::missing("status"),
                FindFilter::Absent,
                field_seek("fp", "document_fields_presence", "(key=?)"),
            )],
        },
        FilterBar {
            shape: FindFilter::Before(FieldOrder::Raw),
            probes: vec![
                (
                    Predicate::before("status", "open"),
                    FindFilter::Before(FieldOrder::Raw),
                    field_seek("fb", "document_fields_raw", "(key=? AND raw<?)"),
                ),
                (
                    Predicate::before("count", "5"),
                    FindFilter::Before(FieldOrder::Typed),
                    field_seek("fb", "document_fields_typed", "(key=? AND typed<?)"),
                ),
            ],
        },
        FilterBar {
            shape: FindFilter::After(FieldOrder::Raw),
            probes: vec![
                (
                    Predicate::after("status", "done"),
                    FindFilter::After(FieldOrder::Raw),
                    field_seek("fb", "document_fields_raw", "(key=? AND raw>?)"),
                ),
                (
                    Predicate::after("count", "5"),
                    FindFilter::After(FieldOrder::Typed),
                    field_seek("fb", "document_fields_typed", "(key=? AND typed>?)"),
                ),
            ],
        },
        FilterBar {
            shape: FindFilter::FullText,
            probes: vec![(
                Predicate::matches("interloper"),
                FindFilter::FullText,
                Seek::FullText,
            )],
        },
        FilterBar {
            shape: FindFilter::PathGlob,
            probes: vec![(
                Predicate::path("notes/*.md"),
                FindFilter::PathGlob,
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
            shape: FindFilter::Resolves,
            probes: [target("glossary"), target("v1.2#Heading")]
                .into_iter()
                .map(|target| {
                    (
                        Predicate::resolves(target),
                        FindFilter::Resolves,
                        Seek::Index {
                            alias: "dr",
                            table: "documents",
                            access: Access::Index("documents_suffix_key"),
                            constraint: "(suffix_key>? AND suffix_key<?)",
                            dropped: "documents_suffix_key",
                        },
                    )
                })
                .collect(),
        },
        FilterBar {
            shape: FindFilter::Tag,
            probes: vec![(
                Predicate::tag("draft"),
                FindFilter::Tag,
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
            shape: FindFilter::Finding,
            probes: vec![(
                Predicate::has_finding(FindingKind::BodyBytesNotUtf8),
                FindFilter::Finding,
                Seek::Index {
                    alias: "fg",
                    table: "findings",
                    access: Access::Index("findings_vault_schema_fingerprint"),
                    constraint: "(vault_schema_fingerprint=? AND kind=?)",
                    dropped: "findings_vault_schema_fingerprint",
                },
            )],
        },
    ]
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
    let mut seeded = Seeded::new("find-filters");
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
                let plans = seeded.plans(&params.with_predicates([part.clone()]), None);
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
    let plans = seeded.plans(
        &request().with_predicates([Predicate::matches("interloper")]),
        None,
    );
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
        for (part, _, seek) in &bar.probes {
            let Seek::Index { dropped: index, .. } = seek else {
                continue;
            };
            if !dropped.contains(index) {
                seeded.drop_index(index);
                dropped.push(index);
            }
            let plans = seeded.plans(&request().with_predicates([part.clone()]), None);
            let page = plan_of(&plans, FindStatement::PathPage(PageDirection::Ascending));
            failure_of(&format!("{index} dropped under {part:?}"), || {
                judge_filter(&page, seek)
            });
        }
    }
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
            .find_keys(&sorted(SortKey::field("count"), direction), declared, None)
            .expect("a page");
        assert_eq!(page.order, Some(order));
        page.keys
            .iter()
            .map(|key| (key.path().to_string(), key.sort().map(str::to_string)))
            .collect::<Vec<_>>()
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
            let resume = Resume {
                at: FindPosition {
                    sort: Some("0".to_string()),
                    path: "notes/a.md".to_string(),
                },
            };
            let plans = seeded.plans_under(
                &sorted(SortKey::field("count"), direction),
                declared,
                Some(&resume),
            );
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

/// Every path a request's pages hold, drained a page of `limit` at a time.
fn drained(seeded: &Seeded, params: &FindParams, limit: u32) -> Vec<(String, Option<String>)> {
    let params = params.clone().with_limit(limit);
    let mut rows = Vec::new();
    let mut resume: Option<Resume> = None;
    for _ in 0..32 {
        let page = seeded.page(&params, resume.as_ref());
        assert!(page.keys.len() <= limit as usize);
        rows.extend(
            page.keys
                .iter()
                .map(|key| (key.path().to_string(), key.sort().map(str::to_string))),
        );
        let Some(next) = page.next else {
            return rows;
        };
        resume = Some(Resume { at: next });
    }
    panic!("the pages did not end: {rows:?}");
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
    let paths: Vec<String> = drained(&seeded, &sorted(SortKey::path(), Direction::Ascending), 1)
        .into_iter()
        .map(|(path, _)| path)
        .collect();
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
        with(Predicate::in_any("status", Vec::new())),
        Vec::<String>::new()
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
    let page = seeded.page(
        &request().with_predicates([Predicate::resolves(target), Predicate::path("")]),
        None,
    );
    assert!(page.keys.is_empty());
    assert_eq!(
        page.unsatisfied,
        vec![
            Unsatisfied::impossible_path("../escape"),
            Unsatisfied::malformed_glob("", "a pattern cannot be empty"),
        ]
    );
}

/// **A match part whose query the full-text engine cannot parse is reported
/// with the engine's words, and the page is answered without it.** A dangling
/// operator and an unterminated phrase are each a query the engine refuses to
/// read; the page they stand in holds every document the rest of the request
/// names, and the probe that read the query is the statement the plans list.
/// A query the engine reads is applied: it narrows the page and reports
/// nothing.
#[test]
fn a_malformed_full_text_query_is_reported_and_the_page_answered_without_it() {
    let seeded = Seeded::new("find-malformed-query");
    let everything = seeded.paths(&request());
    let tagged = seeded.paths(&request().with_predicates([Predicate::tag("draft")]));
    assert!(everything.len() > tagged.len(), "{everything:?}");
    for query in ["interloper AND", "\"interloper"] {
        let page = seeded.page(
            &request().with_predicates([Predicate::matches(query)]),
            None,
        );
        let paths: Vec<&str> = page.keys.iter().map(|key| key.path()).collect();
        assert_eq!(paths, everything, "{query:?} filtered the page");
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

        let beside = seeded.page(
            &request().with_predicates([Predicate::matches(query), Predicate::tag("draft")]),
            None,
        );
        let paths: Vec<&str> = beside.keys.iter().map(|key| key.path()).collect();
        assert_eq!(paths, tagged, "{query:?} beside a tag part");
        assert_eq!(beside.unsatisfied.len(), 1, "{:?}", beside.unsatisfied);

        let plans = seeded.plans(
            &request().with_predicates([Predicate::matches(query)]),
            None,
        );
        plan_of(&plans, FindStatement::MatchProbe);
        assert!(
            !plan_of(&plans, FindStatement::PathPage(PageDirection::Ascending))
                .sql()
                .contains("documents_fts"),
            "the page statement still spells the malformed part"
        );
    }
    let page = seeded.page(
        &request().with_predicates([Predicate::matches("interloper")]),
        None,
    );
    let paths: Vec<&str> = page.keys.iter().map(|key| key.path()).collect();
    assert_eq!(paths, ["notes/a.md"]);
    assert!(page.unsatisfied.is_empty(), "{:?}", page.unsatisfied);
}

/// **A part the store keeps no index of, or a value that names no place in
/// its key's order, is refused.** `links_to` filters by a link's target, which
/// the store keeps no index of, and the refusal names that fact.
#[test]
fn a_part_the_store_cannot_answer_is_refused_by_name() {
    let seeded = Seeded::new("find-refusals");
    let refusal = seeded
        .snapshot()
        .find_keys(
            &request().with_predicates([Predicate::links_to(
                ResolutionTarget::new("glossary").expect("a target"),
            )]),
            &declared(),
            None,
        )
        .expect_err("a links_to part is refused");
    assert_eq!(
        refusal,
        FindRefusal::NotIndexed {
            fact: "a link's target",
        }
    );
    assert_eq!(
        refusal.to_string(),
        "the store keeps no index of a link's target"
    );
    let refusal = seeded
        .snapshot()
        .find_keys(
            &request().with_predicates([Predicate::before("count", "many")]),
            &declared(),
            None,
        )
        .expect_err("a bound that reads as no number is refused");
    assert_eq!(
        refusal,
        FindRefusal::UnreadableBound {
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
            .find_keys(
                &request().with_predicates([part.clone()]),
                &declared(),
                None,
            )
            .expect_err("a compared value that reads as no number is refused");
        assert_eq!(
            refusal,
            FindRefusal::UnreadableBound {
                key: "count".to_string(),
                value: "many".to_string(),
            },
            "{part:?}"
        );
    }
}

/// **A page holds the bound it names, the default where it names none, and at
/// most [`MAX_PAGE`]; each statement it runs is counted on its snapshot.** A
/// path page is one statement beside the fingerprint read. A field sort whose
/// first section does not fill the page reads the second, and one that fills
/// it stops there.
#[test]
fn a_page_holds_its_bound_and_counts_each_statement_it_runs() {
    let seeded = Seeded::new("find-bound");
    const { assert!(DEFAULT_PAGE >= 5 && DEFAULT_PAGE <= MAX_PAGE) };
    let count = |params: &FindParams| {
        let mut snapshot = seeded.snapshot();
        let before = snapshot.counters().statements_executed();
        let page = snapshot
            .find_keys(params, &declared(), None)
            .expect("a page");
        (
            page.keys.len(),
            page.next.is_some(),
            snapshot.counters().statements_executed() - before,
        )
    };
    // Every page reads the active fingerprint once, which its declaration is
    // judged against, and then its sections.
    assert_eq!(count(&request()), (5, false, 2));
    assert_eq!(count(&request().with_limit(2)), (2, true, 2));
    assert_eq!(count(&request().with_limit(0)), (1, true, 2));
    assert_eq!(count(&request().with_limit(u32::MAX)), (5, false, 2));
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
    let mut snapshot = reader
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
            .filter(|path| pattern.matches(path))
            .collect();
        expected.sort_unstable();
        let page = snapshot
            .find_keys(
                &request()
                    .with_predicates([Predicate::path(source.clone())])
                    .with_limit(MAX_PAGE as u32),
                &DeclaredFields::none(),
                None,
            )
            .expect("a page");
        let mut answered: Vec<&str> = page.keys.iter().map(|key| key.path()).collect();
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

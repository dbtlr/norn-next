//! The describe builder: what a page of facets answers, how it pages across
//! kinds, and the plan and work bars its one statement is judged by.
//!
//! The fixture declares one of every declared shape under its schema, and
//! writes four documents whose frontmatter holds keys in every container:
//!
//! | path | `aliases` | `meta` | `status` |
//! |---|---|---|---|
//! | `a.md` | `foo` | | `draft` |
//! | `b.md` | `[a, b]` | | `live` |
//! | `c.md` | | `{x: 1}` | `live` |
//! | `d.md` | no frontmatter | | |
//!
//! `status` is declared and observed, `aliases` and `meta` are observed and
//! undeclared, and `due` and `reviewer` are declared and carried by nothing.

use std::sync::Arc;

use crate::common::{Scratch, document, write_documents};
use crate::find::{failure_of, map, rows_of, string};
use norn_store::{
    ContentModel, DESCRIBE_STATEMENTS, DescribePlan, DescribeStatement, Described,
    FieldDeclaration, FrontmatterValue, PageRefusal, ReadStatement, Snapshot, SnapshotReader,
    Store, TypedOrder, induced_failure,
};
use norn_testkit::explain::{Access, PlanRow, QueryPlan};
use norn_wire::{
    ContainerKind, Cursor, CursorKey, DescribeParams, Facet, FacetKind, FieldType, PathRuleKind,
    Pattern, TagStance, VaultAddress, VaultName,
};

// ---- fixtures ----

/// The fingerprint of the schema the fixture pins.
const DESCRIBE_SCHEMA: &str = "describe-schema";

/// One declaration of every declared shape, under the fixture's schema.
fn declared() -> ContentModel {
    ContentModel::under(DESCRIBE_SCHEMA)
        .declare_field(
            "status",
            FieldDeclaration::text()
                .required()
                .one_of(["draft", "live"]),
        )
        // An ISO day sorts as its own text, which is the order this date reads
        // into.
        .declare_field(
            "due",
            FieldDeclaration::date(TypedOrder::new(|raw| Some(raw.to_string()))),
        )
        .declare("reviewer")
        .declare_tag("project")
        .declare_tag("area")
        .declare_tag_pattern("person/**")
        .declare_undeclared_tags(TagStance::Report)
        .declare_folder("journal", Some("One per day".to_string()))
        .declare_folder("archive", None)
        .declare_ambiguity_ignore(Pattern::parse("archive/**").expect("a glob"))
}

fn sequence(items: &[&str]) -> FrontmatterValue {
    FrontmatterValue::Sequence(items.iter().map(|item| string(item)).collect())
}

/// The fixture's four documents, under `declared`.
fn documents(declared: &ContentModel) -> Vec<norn_store::DocumentFacts> {
    let with = |at: &str, entries: Vec<(&str, FrontmatterValue)>| {
        document(at, &format!("hash-{at}"), "a body\n")
            .with_frontmatter(Some(map(entries)), declared)
    };
    vec![
        with(
            "a.md",
            vec![("aliases", string("foo")), ("status", string("draft"))],
        ),
        with(
            "b.md",
            vec![
                ("aliases", sequence(&["a", "b"])),
                ("status", string("live")),
            ],
        ),
        with(
            "c.md",
            vec![
                ("meta", map(vec![("x", FrontmatterValue::Int(1))])),
                ("status", string("live")),
            ],
        ),
        document("d.md", "hash-d.md", "a body\n"),
    ]
}

/// What more a vault holds beyond the fixture, for the work bar.
#[derive(Clone, Copy)]
struct Bulk {
    /// How many more documents, under `bulk/`, each carrying `aliases` and
    /// `status`.
    documents: usize,
    /// How many bytes each one's body holds.
    body: usize,
    /// How many of them also carry a key of their own, each sorting after
    /// every fixture key.
    own_keys: usize,
}

/// A seeded store and the read handle its snapshots are established on.
struct Describing {
    _scratch: Scratch,
    store: Store,
    reader: Arc<SnapshotReader>,
    declared: ContentModel,
}

impl Describing {
    fn new(label: &str) -> Self {
        Self::with_bulk(
            label,
            Bulk {
                documents: 0,
                body: 0,
                own_keys: 0,
            },
        )
    }

    /// The fixture, pinned under its schema, and `bulk` beside it.
    fn with_bulk(label: &str, bulk: Bulk) -> Self {
        let scratch = Scratch::new(label);
        let mut store = scratch.open();
        store
            .begin_request()
            .pin_vault_schema(DESCRIBE_SCHEMA.as_bytes(), DESCRIBE_SCHEMA)
            .expect("pinning the fixture's schema");
        let declared = declared();
        let mut written = documents(&declared);
        let body = "word ".repeat(bulk.body / 5 + 1);
        written.extend((0..bulk.documents).map(|at| {
            let own = format!("x{at:04}");
            let mut entries = vec![("aliases", sequence(&["x"])), ("status", string("filed"))];
            if at < bulk.own_keys {
                entries.push((own.as_str(), string("own")));
            }
            document(
                &format!("bulk/{at:04}.md"),
                &format!("hash-bulk-{at}"),
                &body,
            )
            .with_frontmatter(Some(map(entries)), &declared)
        }));
        write_documents(&mut store.begin_request(), &written);
        Self::reading(scratch, store, declared)
    }

    /// The fixture's documents in a store that pins no schema, read under the
    /// declaration of a store with none.
    fn unpinned(label: &str) -> Self {
        let scratch = Scratch::new(label);
        let mut store = scratch.open();
        let declared = ContentModel::none();
        write_documents(&mut store.begin_request(), &documents(&declared));
        Self::reading(scratch, store, declared)
    }

    fn reading(scratch: Scratch, store: Store, declared: ContentModel) -> Self {
        let reader = Arc::new(
            store
                .open_reader()
                .reader
                .expect("a live store mints a reader"),
        );
        Describing {
            _scratch: scratch,
            store,
            reader,
            declared,
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

    fn describe(&self, params: &DescribeParams) -> Described {
        self.snapshot()
            .describe(params, &self.declared)
            .unwrap_or_else(|refusal| panic!("a describe of {params:?}: {refusal}"))
    }

    fn facets(&self, params: &DescribeParams) -> Vec<Facet> {
        self.describe(params).facets
    }

    fn plans(&self, params: &DescribeParams) -> Vec<DescribePlan> {
        self.snapshot()
            .describe_plans(params, &self.declared)
            .expect("the plans of a describe")
    }

    /// Drop `index` on the writer, which is what a negative control judges the
    /// same bar against.
    fn drop_index(&mut self, index: &str) {
        induced_failure::execute_out_of_band(&mut self.store, &format!("DROP INDEX {index}"))
            .unwrap_or_else(|problem| panic!("dropping {index}: {problem}"));
    }
}

fn describing() -> DescribeParams {
    DescribeParams::new(VaultAddress::name(
        VaultName::new("notes").expect("a vault name"),
    ))
}

fn observed_only() -> DescribeParams {
    describing().with_facets([FacetKind::ObservedField])
}

/// Every facet the fixture answers, in `(kind, key)` order.
fn every_facet() -> Vec<Facet> {
    vec![
        Facet::declared_field("due", FieldType::Date, false, None),
        Facet::declared_field("reviewer", FieldType::Text, false, None),
        Facet::declared_field(
            "status",
            FieldType::Text,
            true,
            Some(vec!["draft".to_string(), "live".to_string()]),
        ),
        Facet::observed_field("aliases", [ContainerKind::Scalar, ContainerKind::Sequence]),
        Facet::observed_field("meta", [ContainerKind::Map]),
        Facet::observed_field("status", [ContainerKind::Scalar]),
        Facet::declared_tag("area"),
        Facet::declared_tag("project"),
        Facet::folder("archive", None),
        Facet::folder("journal", Some("One per day".to_string())),
        Facet::path_rule(PathRuleKind::AmbiguityIgnore, "archive/**"),
        Facet::tag_pattern("person/**"),
        Facet::undeclared_tags(TagStance::Report),
    ]
}

/// The facets of `every_facet` whose kind is among `kinds`.
fn of_kinds(kinds: &[FacetKind]) -> Vec<Facet> {
    every_facet()
        .into_iter()
        .filter(|facet| kinds.contains(&facet.kind()))
        .collect()
}

// ---- what a page answers ----

/// **With no kinds named every facet answers, in `(kind, key)` order**: the
/// kinds in the order the vocabulary declares them, and within a kind by the
/// byte order of the text that keys it. The declared facets are the pinned
/// declaration's; the observed fields are the keys the documents carry, one
/// facet per key. The reading names no fingerprint, because the order is no
/// schema's.
#[test]
fn every_facet_answers_in_kind_then_key_order() {
    let describing_store = Describing::new("describe-every");
    let answered = describing_store.describe(&describing());
    assert_eq!(answered.facets, every_facet());
    assert_eq!(answered.next, None);
    assert!(answered.moved.is_empty());
    assert_eq!(
        answered.snapshot.schema_fingerprint, None,
        "a describe's order is no schema's, so its reading names no fingerprint"
    );
    let report = answered.into_report();
    assert_eq!(report.rows, every_facet());
}

/// **An observed field lists every container its documents hold the key
/// in**, each once, in the vocabulary's order, whatever order the documents
/// were written in: `aliases` held bare in one document and as a sequence in
/// another is one facet held in both, `meta` held as a map is one held in a
/// map. **A key is observed whether or not it is declared, and declared
/// whether or not it is observed**: `aliases` and `meta` are observed and
/// declared nowhere, `due` and `reviewer` declared and carried by nothing,
/// and `status` both.
#[test]
fn an_observed_field_carries_every_container_its_documents_hold_it_in() {
    let describing_store = Describing::new("describe-containers");
    let observed = describing_store.facets(&observed_only());
    assert_eq!(
        observed,
        vec![
            Facet::observed_field("aliases", [ContainerKind::Scalar, ContainerKind::Sequence]),
            Facet::observed_field("meta", [ContainerKind::Map]),
            Facet::observed_field("status", [ContainerKind::Scalar]),
        ]
    );
    let declared: Vec<String> = describing_store
        .facets(&describing().with_facets([FacetKind::DeclaredField]))
        .iter()
        .map(key_of)
        .collect();
    assert_eq!(declared, ["due", "reviewer", "status"]);
}

/// **Naming kinds answers those kinds alone, in the vocabulary's order**,
/// whatever order the request names them in and however often.
#[test]
fn naming_kinds_answers_those_kinds_alone_in_the_vocabularys_order() {
    let describing_store = Describing::new("describe-kinds");
    for kinds in [
        vec![FacetKind::ObservedField],
        vec![FacetKind::UndeclaredTags, FacetKind::DeclaredField],
        vec![
            FacetKind::Folder,
            FacetKind::TagPattern,
            FacetKind::Folder,
            FacetKind::DeclaredTag,
        ],
        vec![FacetKind::PathRule],
    ] {
        assert_eq!(
            describing_store.facets(&describing().with_facets(kinds.clone())),
            of_kinds(&kinds),
            "{kinds:?}"
        );
    }
}

/// The key a facet's cursor names.
fn key_of(facet: &Facet) -> String {
    match facet.cursor_key() {
        CursorKey::Facet { key, .. } => key,
        other => panic!("a facet's cursor key is a facet's: {other:?}"),
    }
}

/// Every facet `params` answers, drained a page of `limit` at a time.
fn drained(describing_store: &Describing, params: &DescribeParams, limit: u32) -> Vec<Facet> {
    let mut facets = Vec::new();
    let mut after: Option<Cursor> = None;
    loop {
        let mut page = params.clone().with_limit(limit);
        if let Some(cursor) = after.take() {
            page = page.with_after(cursor);
        }
        let answered = describing_store.describe(&page);
        assert!(
            answered.facets.len() <= limit as usize,
            "a page held more than its bound"
        );
        facets.extend(answered.facets);
        match answered.next {
            Some(next) => after = Some(next),
            None => return facets,
        }
        assert!(facets.len() < 100, "the drain does not end");
    }
}

/// **A drain a page at a time answers the facets one page does**, in the same
/// order, whatever the bound: a continuation resumes after the last facet's
/// kind and key, inside a kind and across kinds — the observed section's
/// statement and the declared sections alike — and under every selection of
/// kinds. A page stops at its last facet.
#[test]
fn a_drain_a_page_at_a_time_answers_the_facets_one_page_does() {
    let describing_store = Describing::new("describe-drain");
    for params in [
        describing(),
        observed_only(),
        describing().with_facets([FacetKind::ObservedField, FacetKind::DeclaredTag]),
        describing().with_facets([FacetKind::UndeclaredTags, FacetKind::DeclaredField]),
    ] {
        let whole = describing_store.facets(&params.clone().with_limit(1000));
        for limit in [1, 2, 3] {
            assert_eq!(
                drained(&describing_store, &params, limit),
                whole,
                "{params:?} drained {limit} at a time"
            );
        }
    }
    let page = describing_store.describe(&describing().with_limit(4));
    assert_eq!(
        page.next.as_ref().map(Cursor::key),
        Some(&CursorKey::facet(FacetKind::ObservedField, "aliases")),
        "a page stops at its last facet's kind and key"
    );
}

/// **A cursor is a position in the one `(kind, key)` order, whichever kinds
/// the request that minted it named**: a cursor minted at the first declared
/// tag of a request naming tags alone continues a request naming every kind
/// from the next tag on, and a request naming only kinds before it from
/// nowhere.
#[test]
fn a_cursor_continues_from_its_position_whichever_kinds_minted_it() {
    let describing_store = Describing::new("describe-cursor-position");
    let minted = describing_store
        .describe(
            &describing()
                .with_facets([FacetKind::DeclaredTag])
                .with_limit(1),
        )
        .next
        .expect("a next page");
    assert_eq!(
        minted.key(),
        &CursorKey::facet(FacetKind::DeclaredTag, "area")
    );
    let every = every_facet();
    let at = every
        .iter()
        .position(|facet| facet == &Facet::declared_tag("area"))
        .expect("the tag is in the fixture");
    assert_eq!(
        describing_store.facets(&describing().with_after(minted.clone())),
        every[at + 1..].to_vec()
    );
    assert_eq!(
        describing_store.facets(
            &describing()
                .with_facets([FacetKind::DeclaredField, FacetKind::ObservedField])
                .with_after(minted)
        ),
        Vec::new()
    );
}

/// **A cursor that names no position among facets is refused**: a
/// document's, and a finding's.
#[test]
fn a_cursor_that_is_no_facets_position_is_refused() {
    let describing_store = Describing::new("describe-cursor-refused");
    let reading = describing_store.describe(&describing()).snapshot;
    for key in [
        CursorKey::document(None, "a.md"),
        CursorKey::tally([Some("draft".to_string())]),
    ] {
        assert_eq!(
            describing_store
                .snapshot()
                .describe(
                    &describing().with_after(Cursor::new(reading.clone(), key)),
                    &describing_store.declared
                )
                .expect_err("the cursor is refused"),
            PageRefusal::NotAFacetCursor
        );
    }
    assert_eq!(
        PageRefusal::NotAFacetCursor.to_string(),
        "the cursor names no position among a describe's facets"
    );
}

/// **A declaration read from another schema than the snapshot pins is
/// refused**, as every read refuses it, and **a page bound outside its range
/// is refused rather than clamped**.
#[test]
fn a_declaration_not_pinned_and_a_bound_outside_its_range_are_refused() {
    let describing_store = Describing::new("describe-refused");
    assert_eq!(
        describing_store
            .snapshot()
            .describe(
                &describing(),
                &ContentModel::under("another-schema").declare("status")
            )
            .expect_err("the declaration is not the pinned one"),
        PageRefusal::DeclarationNotPinned {
            declared_under: Some("another-schema".to_string()),
            pinned: Some(DESCRIBE_SCHEMA.to_string()),
        }
    );
    assert!(matches!(
        describing_store
            .snapshot()
            .describe(&describing().with_limit(0), &describing_store.declared),
        Err(PageRefusal::OutOfBound { given: 0, .. })
    ));
}

/// **With no schema pinned describe answers the observed fields alone**: no
/// schema declares anything, so there is no declared field, tag, pattern,
/// folder, path rule or stance, and the keys the documents carry are the
/// whole field universe.
#[test]
fn with_no_schema_pinned_describe_answers_the_observed_fields_alone() {
    let describing_store = Describing::unpinned("describe-unpinned");
    assert_eq!(
        describing_store.facets(&describing()),
        of_kinds(&[FacetKind::ObservedField])
    );
    assert_eq!(
        describing_store.facets(
            &describing().with_facets([FacetKind::DeclaredField, FacetKind::UndeclaredTags])
        ),
        Vec::new()
    );
}

// ---- the plan bars ----

/// The plan the store reported for one statement, in the harness's shape.
fn plan(emitted: &DescribePlan) -> QueryPlan {
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

/// Every plan `plans` holds for `statement`, in the order the describe ran
/// them.
fn plans_of(plans: &[DescribePlan], statement: DescribeStatement) -> Vec<QueryPlan> {
    plans
        .iter()
        .filter(|plan| plan.statement == ReadStatement::Describe(statement))
        .map(plan)
        .collect()
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
/// statement added to [`DescribeStatement`] does not compile until its author
/// names the bar.
fn statement_barred_by(statement: DescribeStatement) -> &'static str {
    match statement {
        DescribeStatement::ObservedFields => {
            "an_observed_field_page_walks_the_presence_index_key_by_key"
        }
    }
}

/// **Every statement the describe builder names carries a bar, and one bar
/// judges each**: the enumeration's slots are each claimed once, and each bar
/// is named by exactly the statements it judges.
#[test]
fn the_describe_bars_cover_every_statement_once() {
    let mut slots: Vec<usize> = DescribeStatement::all()
        .into_iter()
        .map(DescribeStatement::slot)
        .collect();
    slots.sort_unstable();
    assert_eq!(slots, (0..DESCRIBE_STATEMENTS).collect::<Vec<usize>>());
    for (slot, statement) in DescribeStatement::all().into_iter().enumerate() {
        assert_eq!(statement.slot(), slot, "{statement:?} claims another slot");
    }
    let bars: std::collections::BTreeSet<&str> = DescribeStatement::all()
        .into_iter()
        .map(statement_barred_by)
        .collect();
    assert_eq!(bars.len(), DESCRIBE_STATEMENTS);
}

/// The container probe's seek of one key's presence rows in one container.
const CONTAINER_SEEK: &str = "(key=? AND container=?)";

/// Judge an observed-field page: every read of `document_fields` a search of
/// the presence index, reading no row behind it — the walk's first and next
/// seeks past a key, `(key>?)`, and one seek per container at the key it
/// reached — with the table never read end to end and nothing sorted.
fn judge_observed(page: &QueryPlan) {
    page.assert_no_full_scan_of("document_fields");
    page.assert_no_temp_btree();
    page.assert_searches_through("document_fields", Access::Index("document_fields_presence"));
    let searches = page.searches_of("document_fields");
    assert!(
        searches.iter().all(|row| row
            .detail
            .contains("COVERING INDEX document_fields_presence")),
        "an observed-field page read a field row: {searches:?}\nemitted SQL: {}",
        page.sql()
    );
    let walked = rows_of(page, "o");
    walked.assert_search_constraint("document_fields", "(key>?)");
    assert_eq!(
        walked.rows().len(),
        2,
        "the walk reaches the presence rows other than by its first and its next seek: {:?}",
        page.rows()
    );
    let probed = rows_of(page, "c");
    probed.assert_search_constraint("document_fields", CONTAINER_SEEK);
    assert_eq!(
        probed.rows().len(),
        3,
        "a key's containers are other than one seek per container: {:?}",
        page.rows()
    );
}

/// **An observed-field page walks the presence index key by key.** Its one
/// statement seeks `document_fields_presence` for the least key after the
/// page's position, then for the least key after each key it reached — so it
/// reads one index entry per distinct key rather than one per document that
/// carries it — and asks, per key it reached, one covering seek of `(key,
/// container)` per container whether any document holds the key there. On a
/// first page and a continuation alike it reads no field row, never reads the
/// table end to end, and sorts nothing. **A declared facet is a read of the
/// pinned declaration**, so a page of declared kinds runs no describe
/// statement at all.
///
/// Controls: a plan whose container probe seeks the key alone fails; one that
/// reads the rows behind the index fails; the presence index dropped, the
/// statement reads something else.
#[test]
fn an_observed_field_page_walks_the_presence_index_key_by_key() {
    let mut describing_store = Describing::new("describe-plan");
    let first = plans_of(
        &describing_store.plans(&observed_only().with_limit(2)),
        DescribeStatement::ObservedFields,
    );
    assert_eq!(first.len(), 1, "a page runs its observed statement once");
    judge_observed(&first[0]);
    let next = describing_store
        .describe(&observed_only().with_limit(1))
        .next
        .expect("a next page");
    let continued = plans_of(
        &describing_store.plans(&describing().with_after(next)),
        DescribeStatement::ObservedFields,
    );
    assert_eq!(continued.len(), 1);
    judge_observed(&continued[0]);

    let declared_kinds: Vec<FacetKind> = FacetKind::ALL
        .into_iter()
        .filter(|kind| *kind != FacetKind::ObservedField)
        .collect();
    let declared_only = describing_store.plans(&describing().with_facets(declared_kinds));
    assert!(
        declared_only
            .iter()
            .all(|plan| !matches!(plan.statement, ReadStatement::Describe(_))),
        "a page of declared facets ran a describe statement: {declared_only:?}"
    );

    let key_alone = rewritten(&first[0], |detail| {
        detail.replace(CONTAINER_SEEK, "(key=?)")
    });
    failure_of("a container probe that seeks the key alone", || {
        judge_observed(&key_alone)
    });
    let uncovered = rewritten(&first[0], |detail| {
        detail.replace("COVERING INDEX", "INDEX")
    });
    failure_of("a page that reads the rows behind the index", || {
        judge_observed(&uncovered)
    });
    describing_store.drop_index("document_fields_presence");
    let dropped = plans_of(
        &describing_store.plans(&observed_only().with_limit(2)),
        DescribeStatement::ObservedFields,
    );
    failure_of("document_fields_presence dropped", || {
        judge_observed(&dropped[0])
    });
}

// ---- the work bar ----

/// **An observed-field page's work follows the distinct keys it pages, not
/// the documents that carry them.** Over the fixture beside 50 more documents
/// of 64-byte bodies, and beside 1000 more of 16 KiB bodies of which 200 each
/// carry a key of their own, a page of two observed fields runs the same
/// statements and the same VM steps, reads the same facets, steps through no
/// full scan and sorts nothing: neither the documents carrying a key, nor
/// their bodies, nor the keys past the page are read. A page of three reads
/// more than a page of one, because it reaches more keys.
///
/// Control: `document_fields_presence` dropped on the larger vault, the page
/// reads the rows the documents carry, and the bar fails.
#[test]
fn an_observed_field_pages_work_follows_the_keys_it_pages() {
    let small = Describing::with_bulk(
        "describe-work-small",
        Bulk {
            documents: 50,
            body: 64,
            own_keys: 0,
        },
    );
    let mut large = Describing::with_bulk(
        "describe-work-large",
        Bulk {
            documents: 1000,
            body: 16 * 1024,
            own_keys: 200,
        },
    );
    let judge = |small: &Describing, large: &Describing| {
        let page = observed_only().with_limit(2);
        let (small, large) = (small.describe(&page).work, large.describe(&page).work);
        assert_eq!(
            small, large,
            "an observed-field page's work grew with the vault"
        );
        assert_eq!(large.full_scan_steps, 0, "{large:?}");
        assert_eq!(large.sorts, 0, "{large:?}");
    };
    judge(&small, &large);
    assert!(
        small.describe(&observed_only().with_limit(3)).work.vm_steps
            > small.describe(&observed_only().with_limit(1)).work.vm_steps,
        "a page reaching more keys did no more work"
    );

    large.drop_index("document_fields_presence");
    failure_of("document_fields_presence dropped", || judge(&small, &large));
}

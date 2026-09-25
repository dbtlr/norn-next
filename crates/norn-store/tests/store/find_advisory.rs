//! The advisory a read answers beside its rows: a comparison that decided the
//! answer's order, grouping or membership compared a date stating an offset
//! against one stating none. A find carries the most cases; a count, a
//! validate and a search raise it through the same machinery, and each is
//! held to raising it over a mixed vault and not over a uniform one.
//!
//! Every case derives its documents under a declaration whose `due` key carries
//! a dated order, written in the two spellings the order parts: `2026-03-04Z`
//! states an offset and `2026-03-04` states none, and the two are one place in
//! the order. The advisory speaks for every value the key holds in the
//! snapshot, not for the page, so the cases hold it raised where every row the
//! page carries writes one spelling.

use std::sync::Arc;

use crate::common::{Scratch, dated_order, document, write_documents};
use crate::find::{failure_of, map, plan_of, request, string};
use norn_store::{
    ContentModel, Counted, FieldDeclaration, FindPlan, FindStatement, Found, FrontmatterValue,
    LexicalQuery, Searched, Snapshot, SnapshotReader, Store, TypedOrder, Validated,
    induced_failure,
};
use norn_testkit::explain::Access;
use norn_wire::{
    AnswerAdvisory, ComparedBy, CountParams, Direction, FindParams, GroupKey, Predicate, Sort,
    SortKey, ValidateParams, VaultAddress, VaultName,
};

/// The statements the offset-spelling bar judges.
pub(crate) const OFFSET_PROBES: [FindStatement; 1] = [FindStatement::OffsetSpellings];

const SCHEMA: &str = "advisory-schema";

/// An order reading a value as an integer, which spells no offset.
fn integer_order() -> TypedOrder {
    TypedOrder::new(|raw| raw.parse::<i64>().ok().map(|n| format!("{n:020}")))
}

/// `due` dated, `count` an integer, `note` declared as text.
fn declared() -> ContentModel {
    ContentModel::under(SCHEMA)
        .declare_field("due", FieldDeclaration::date(dated_order()))
        .declare_field("count", FieldDeclaration::number(integer_order()))
        .declare("note")
}

/// The same keys with `due` declared as text: the same bytes, no dated order.
fn declared_as_text() -> ContentModel {
    ContentModel::under(SCHEMA)
        .declare("due")
        .declare_field("count", FieldDeclaration::number(integer_order()))
        .declare("note")
}

/// A store holding one document per `(path, due)`, each also carrying a
/// `count` and a `note` whose text is a stated date, derived under `declared`.
struct Vault {
    _scratch: Scratch,
    store: Store,
    reader: Arc<SnapshotReader>,
}

impl Vault {
    fn new(label: &str, dues: &[(&str, &str)], declared: &ContentModel) -> Self {
        let scratch = Scratch::new(label);
        let mut store = scratch.open();
        store
            .begin_request()
            .pin_vault_schema(SCHEMA.as_bytes(), SCHEMA)
            .expect("pinning the schema");
        let documents: Vec<_> = dues
            .iter()
            .enumerate()
            .map(|(at, (path, due))| {
                document(path, &format!("hash-{at}"), "a body\n").with_frontmatter(
                    Some(map(vec![
                        ("due", string(due)),
                        ("count", FrontmatterValue::Int(at as i64)),
                        ("note", string("2026-03-04Z")),
                    ])),
                    declared,
                )
            })
            .collect();
        write_documents(&mut store.begin_request(), &documents);
        let reader = Arc::new(store.open_reader().reader.expect("a reader"));
        Vault {
            _scratch: scratch,
            store,
            reader,
        }
    }

    /// Stated dates, one unstated date, and a value that is no date.
    fn mixed(label: &str, declared: &ContentModel) -> Self {
        Self::new(
            label,
            &[
                ("notes/a.md", "2026-03-04Z"),
                ("notes/b.md", "2026-03-05"),
                ("notes/c.md", "2026-03-06Z"),
                ("notes/d.md", "soon"),
            ],
            declared,
        )
    }

    /// Stated dates alone, and a value that is no date.
    fn stated(label: &str) -> Self {
        Self::new(
            label,
            &[
                ("notes/a.md", "2026-03-04Z"),
                ("notes/b.md", "2026-03-05Z"),
                ("notes/c.md", "2026-03-06Z"),
                ("notes/d.md", "soon"),
            ],
            &declared(),
        )
    }

    /// Unstated dates alone, and a value that is no date.
    fn unstated(label: &str) -> Self {
        Self::new(
            label,
            &[
                ("notes/a.md", "2026-03-04"),
                ("notes/b.md", "2026-03-05"),
                ("notes/c.md", "2026-03-06"),
                ("notes/d.md", "soon"),
            ],
            &declared(),
        )
    }

    fn snapshot(&self) -> Snapshot {
        self.reader
            .try_take()
            .expect("an idle reader")
            .establish()
            .snapshot
            .expect("a snapshot")
    }

    fn find_under(&self, params: &FindParams, declared: &ContentModel) -> Found {
        self.snapshot()
            .find(params, declared)
            .unwrap_or_else(|refusal| panic!("a find of {params:?}: {refusal}"))
    }

    fn find(&self, params: &FindParams) -> Found {
        self.find_under(params, &declared())
    }

    fn count(&self, params: &CountParams) -> Counted {
        self.snapshot()
            .count(params, &declared())
            .unwrap_or_else(|refusal| panic!("a count of {params:?}: {refusal}"))
    }

    fn validate(&self, params: &ValidateParams) -> Validated {
        self.snapshot()
            .validate(params, &declared())
            .unwrap_or_else(|refusal| panic!("a validate of {params:?}: {refusal}"))
    }

    fn search(&self, query: &LexicalQuery) -> Searched {
        self.snapshot()
            .search(query, &declared())
            .unwrap_or_else(|refusal| panic!("a search of {query:?}: {refusal}"))
    }

    fn plans(&self, params: &FindParams) -> Vec<FindPlan> {
        self.snapshot()
            .find_plans(params, &declared())
            .expect("the plans of a request")
    }
}

fn address() -> VaultAddress {
    VaultAddress::name(VaultName::new("notes").expect("a vault name"))
}

fn by_due(direction: Direction) -> FindParams {
    request().with_sort(Sort::new(SortKey::field("due"), direction))
}

fn paths(found: &Found) -> Vec<&str> {
    found.rows.iter().map(|row| row.path.as_str()).collect()
}

fn sort_advised() -> Vec<AnswerAdvisory> {
    vec![AnswerAdvisory::mixed_offset("due", ComparedBy::Sort)]
}

fn predicate_advised() -> Vec<AnswerAdvisory> {
    vec![AnswerAdvisory::mixed_offset("due", ComparedBy::Predicate)]
}

fn group_advised() -> Vec<AnswerAdvisory> {
    vec![AnswerAdvisory::mixed_offset("due", ComparedBy::Group)]
}

/// A stated bound over `due`, which compares across spellings in the mixed
/// vault and against one spelling in the stated one.
fn stated_bound() -> Predicate {
    Predicate::after("due", "2026-03-05Z")
}

/// **A date order over both spellings is advised, whatever the page holds.**
/// The order places every row among every date the key holds, so a first page
/// holding one stated date alone is advised as the whole order is, both ways
/// and on a continuation. The advisory is not an unsatisfied part: the order
/// was applied as asked. The same order over a vault writing one spelling is
/// not advised.
#[test]
fn a_date_order_over_both_spellings_is_advised() {
    let vault = Vault::mixed("advisory-order", &declared());
    for direction in [Direction::Ascending, Direction::Descending] {
        let found = vault.find(&by_due(direction));
        assert_eq!(found.advisories, sort_advised(), "{direction:?}");
        assert!(found.unsatisfied.is_empty(), "{:?}", found.unsatisfied);
    }

    let first = vault.find(&by_due(Direction::Descending).with_limit(1));
    assert_eq!(
        paths(&first),
        ["notes/c.md"],
        "the page holds a stated date alone"
    );
    assert_eq!(first.advisories, sort_advised());
    let next = first.next.expect("a next page");
    let continued = vault.find(&by_due(Direction::Descending).with_limit(1).with_after(next));
    assert_eq!(continued.advisories, sort_advised());

    let uniform = Vault::stated("advisory-order-uniform");
    for direction in [Direction::Ascending, Direction::Descending] {
        assert_eq!(
            uniform.find(&by_due(direction)).advisories,
            [],
            "one spelling was advised {direction:?}"
        );
    }
}

/// **A predicate comparing its value across spellings is advised, whether or
/// not a row it compared reaches the page.** An `after` bound stating an
/// offset keeps only stated dates here, yet it decided that the unstated
/// `2026-03-05` falls outside it by reading that date at offset zero, so the
/// answer is advised. A bound, an equality, an inequality and a membership
/// each compare their value against every date the key holds. Where the
/// value's spelling is the only one the key holds, nothing is advised; where
/// it is not, the part is advised though the key's own dates agree: an
/// unstated value against stated dates, and a stated value against unstated
/// ones.
#[test]
fn a_date_predicate_comparing_across_spellings_is_advised() {
    let vault = Vault::mixed("advisory-predicate", &declared());
    let after = vault.find(&request().with_predicates([Predicate::after("due", "2026-03-05Z")]));
    assert_eq!(paths(&after), ["notes/c.md"]);
    assert_eq!(after.advisories, predicate_advised());
    for part in [
        Predicate::before("due", "2026-03-05Z"),
        Predicate::after("due", "2026-03-04"),
        Predicate::equal_to("due", "2026-03-05Z"),
        Predicate::not_equal_to("due", "2026-03-04Z"),
        Predicate::in_any("due", ["2026-03-04Z".to_string()]),
    ] {
        assert_eq!(
            vault
                .find(&request().with_predicates([part.clone()]))
                .advisories,
            predicate_advised(),
            "{part:?}"
        );
    }

    let uniform = Vault::stated("advisory-predicate-uniform");
    for part in [
        Predicate::after("due", "2026-03-04Z"),
        Predicate::equal_to("due", "2026-03-05Z"),
        Predicate::in_any(
            "due",
            ["2026-03-04Z".to_string(), "2026-03-06Z".to_string()],
        ),
    ] {
        assert_eq!(
            uniform
                .find(&request().with_predicates([part.clone()]))
                .advisories,
            [],
            "{part:?} compared one spelling and was advised"
        );
    }
    for part in [
        Predicate::after("due", "2026-03-04"),
        Predicate::in_any("due", ["2026-03-04Z".to_string(), "2026-03-06".to_string()]),
    ] {
        assert_eq!(
            uniform
                .find(&request().with_predicates([part.clone()]))
                .advisories,
            predicate_advised(),
            "{part:?} compared an unstated value against stated dates"
        );
    }

    let unstated = Vault::unstated("advisory-predicate-unstated");
    for part in [
        Predicate::after("due", "2026-03-04"),
        Predicate::equal_to("due", "2026-03-05"),
        Predicate::in_any("due", ["2026-03-04".to_string(), "2026-03-06".to_string()]),
    ] {
        assert_eq!(
            unstated
                .find(&request().with_predicates([part.clone()]))
                .advisories,
            [],
            "{part:?} compared one spelling and was advised"
        );
    }
    for part in [
        Predicate::after("due", "2026-03-04Z"),
        Predicate::equal_to("due", "2026-03-05Z"),
        Predicate::in_any("due", ["2026-03-06Z".to_string()]),
    ] {
        assert_eq!(
            unstated
                .find(&request().with_predicates([part.clone()]))
                .advisories,
            predicate_advised(),
            "{part:?} compared a stated value against unstated dates"
        );
    }
}

/// **An answer is advised once per key and place, the order before the
/// predicates.** Two predicates over the key are one comparison of its dates
/// to advise about, and a sort over it is another.
#[test]
fn an_answer_is_advised_once_per_key_and_place() {
    let vault = Vault::mixed("advisory-places", &declared());
    let found = vault.find(&by_due(Direction::Ascending).with_predicates([
        Predicate::after("due", "2026-03-01Z"),
        Predicate::before("due", "2026-03-09Z"),
    ]));
    assert_eq!(
        found.advisories,
        [
            AnswerAdvisory::mixed_offset("due", ComparedBy::Sort),
            AnswerAdvisory::mixed_offset("due", ComparedBy::Predicate),
        ]
    );
}

/// **A key without a dated order is never advised.** An integer order and a
/// key declared as text compare no date, whatever the text says — `note`
/// writes a stated date everywhere and is compared against an unstated one —
/// and the same bytes under a declaration giving `due` no dated order are not
/// advised either.
#[test]
fn a_key_without_a_dated_order_is_never_advised() {
    let vault = Vault::mixed("advisory-undated", &declared());
    for params in [
        request().with_sort(Sort::new(SortKey::field("count"), Direction::Ascending)),
        request().with_predicates([Predicate::after("count", "1")]),
        request().with_sort(Sort::new(SortKey::field("note"), Direction::Ascending)),
        request().with_predicates([Predicate::after("note", "2026-03-04")]),
    ] {
        assert_eq!(vault.find(&params).advisories, [], "{params:?}");
    }

    let as_text = Vault::mixed("advisory-as-text", &declared_as_text());
    for params in [
        by_due(Direction::Ascending),
        request().with_predicates([Predicate::after("due", "2026-03-05Z")]),
    ] {
        assert_eq!(
            as_text.find_under(&params, &declared_as_text()).advisories,
            [],
            "{params:?}"
        );
    }
}

/// **The advisory costs one probe per dated key a request compares.** A sort
/// and two predicates over `due` ask the snapshot once; a request comparing no
/// dated key asks nothing; and a request with a part that matches nothing
/// compared no date, so it asks nothing and is advised of nothing.
#[test]
fn the_advisory_costs_one_probe_per_dated_key() {
    let vault = Vault::mixed("advisory-cost", &declared());
    let probes = |params: &FindParams| {
        vault
            .plans(params)
            .iter()
            .filter(|plan| plan.statement == FindStatement::OffsetSpellings)
            .count()
    };
    assert_eq!(
        probes(&by_due(Direction::Ascending).with_predicates([
            Predicate::after("due", "2026-03-01Z"),
            Predicate::equal_to("due", "2026-03-05"),
        ])),
        1
    );
    assert_eq!(
        probes(&request().with_sort(Sort::new(SortKey::field("count"), Direction::Ascending))),
        0
    );
    let nothing = by_due(Direction::Ascending).with_predicates([Predicate::path("../escape")]);
    assert_eq!(probes(&nothing), 0);
    let found = vault.find(&nothing);
    assert!(found.rows.is_empty() && !found.unsatisfied.is_empty());
    assert_eq!(found.advisories, []);
}

/// **Whether a date key holds each spelling is two seeks of the offset
/// index**, one at each spelling, and never a read of the table.
///
/// Control: `document_fields_offset` dropped, neither is a seek of it.
#[test]
fn a_date_keys_offset_spellings_are_two_seeks_of_the_offset_index() {
    let vault = Vault::mixed("advisory-plan", &declared());
    let params = by_due(Direction::Ascending);
    let judge = |plans: &[FindPlan]| {
        let probe = plan_of(plans, FindStatement::OffsetSpellings);
        probe.assert_no_full_scan();
        probe.assert_searches_through("document_fields", Access::Index("document_fields_offset"));
        probe.assert_search_constraint("document_fields", "(key=? AND offset_stated=?)");
        assert_eq!(
            probe.searches_of("document_fields").len(),
            2,
            "the probe reaches the offset rows other than by one seek per spelling: {:?}",
            probe.rows()
        );
    };
    judge(&vault.plans(&params));

    let mut vault = vault;
    induced_failure::execute_out_of_band(&mut vault.store, "DROP INDEX document_fields_offset")
        .expect("dropping the offset index");
    let plans = vault.plans(&params);
    failure_of("document_fields_offset dropped", || judge(&plans));
}

/// **A count grouping by a dated key over both spellings is advised, as a
/// grouping.** A grouping makes one tally of the dates reading as one instant
/// and orders the tallies, so it compares the key's dates as an order does;
/// its conjunction's parts are advised as a find's are, after the grouping. A
/// grouping by an undated key, and a count over one spelling, are not
/// advised.
#[test]
fn a_count_grouping_or_comparing_dates_across_spellings_is_advised() {
    let vault = Vault::mixed("advisory-count", &declared());
    let by_due = CountParams::new(address()).with_by([GroupKey::field("due")]);
    assert_eq!(vault.count(&by_due).advisories, group_advised());
    assert_eq!(
        vault
            .count(&CountParams::new(address()).with_predicates([stated_bound()]))
            .advisories,
        predicate_advised()
    );
    let both = vault.count(&by_due.clone().with_predicates([stated_bound()]));
    assert_eq!(
        both.advisories,
        [
            AnswerAdvisory::mixed_offset("due", ComparedBy::Group),
            AnswerAdvisory::mixed_offset("due", ComparedBy::Predicate),
        ]
    );
    assert!(both.unsatisfied.is_empty(), "{:?}", both.unsatisfied);
    assert_eq!(
        vault
            .count(&CountParams::new(address()).with_by([GroupKey::field("count")]))
            .advisories,
        []
    );

    let uniform = Vault::stated("advisory-count-uniform");
    assert_eq!(
        uniform
            .count(&by_due.with_predicates([stated_bound()]))
            .advisories,
        []
    );
}

/// **A validate whose conjunction compares dates across spellings is
/// advised**, whatever findings stand: the part decided which documents'
/// findings it admits. The same part over one spelling is not advised.
#[test]
fn a_validate_comparing_dates_across_spellings_is_advised() {
    let params = ValidateParams::new(address()).with_predicates([stated_bound()]);
    let vault = Vault::mixed("advisory-validate", &declared());
    assert_eq!(vault.validate(&params).advisories, predicate_advised());
    assert_eq!(
        vault.validate(&params.clone().summarized()).advisories,
        predicate_advised()
    );

    let uniform = Vault::stated("advisory-validate-uniform");
    assert_eq!(uniform.validate(&params).advisories, []);
}

/// **A search whose conjunction compares dates across spellings is
/// advised.** The same part over one spelling is not, and a query holding no
/// word runs no page, so its conjunction compared no date.
#[test]
fn a_search_comparing_dates_across_spellings_is_advised() {
    let query = LexicalQuery::new("body").with_predicates([stated_bound()]);
    let vault = Vault::mixed("advisory-search", &declared());
    let searched = vault.search(&query);
    assert_eq!(searched.advisories, predicate_advised());
    assert!(!searched.hits.is_empty(), "the query matches every body");
    assert_eq!(
        vault
            .search(&LexicalQuery::new("  ").with_predicates([stated_bound()]))
            .advisories,
        []
    );

    let uniform = Vault::stated("advisory-search-uniform");
    assert_eq!(uniform.search(&query).advisories, []);
}

//! The count builder: what a page of tallies answers, how it agrees with a
//! find, and the plan and work bars every statement it emits is judged by.
//!
//! The fixture is seven documents whose fields hold every shape a grouping
//! meets: a sequence, a sequence repeating a value and holding a null, an
//! empty sequence, a map, a missing key, a typed key spelled two ways for one
//! value and once in a way that reads as no value of its type, and tags.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::common::{DOCUMENT_PAYLOAD, Scratch, document, reads_of, write_documents};
use crate::find::{failure_of, map, rows_of, string};
use norn_store::{
    COUNT_STATEMENTS, ContentModel, CountPlan, CountStatement, Counted, FieldDeclaration,
    FieldOrder, FindStatement, Found, FrontmatterValue, GroupMember, PageRefusal, ReadStatement,
    Snapshot, SnapshotReader, Store, TagFact, TagSource, TypedOrder, induced_failure,
};
use norn_testkit::explain::{Access, PlanRow, QueryPlan};
use norn_wire::{
    Column, CountParams, Cursor, CursorKey, Direction, FindParams, GroupKey, Predicate,
    ResolutionTarget, Sort, SortKey, Tally, Unsatisfied, VaultAddress, VaultName,
};

// ---- fixtures ----

/// The fingerprint of the schema the fixture pins.
const COUNT_SCHEMA: &str = "count-schema";

/// An order that reads a raw value as a decimal number: `9`, `09` and `9.0`
/// are one value under it, and `10` stands after `9`.
fn decimal_order() -> TypedOrder {
    TypedOrder::new(|raw| {
        let number: f64 = raw.parse().ok()?;
        if !number.is_finite() {
            return None;
        }
        // Zero has one place whatever its sign.
        let bits = (number + 0.0).to_bits();
        let ordered = if bits >> 63 == 1 {
            !bits
        } else {
            bits | (1 << 63)
        };
        Some(format!("{ordered:016x}"))
    })
}

/// `status` and `aliases` declared as text, `n` declared as a decimal.
fn declared() -> ContentModel {
    ContentModel::under(COUNT_SCHEMA)
        .declare("status")
        .declare("aliases")
        .declare_field("n", FieldDeclaration::number(decimal_order()))
}

fn texts(values: &[&str]) -> FrontmatterValue {
    FrontmatterValue::Sequence(values.iter().map(|value| string(value)).collect())
}

fn tagged(mut facts: norn_store::DocumentFacts, tags: &[&str]) -> norn_store::DocumentFacts {
    facts.tags = tags
        .iter()
        .map(|name| TagFact {
            name: (*name).to_string(),
            source: TagSource::Frontmatter,
            span: None,
        })
        .collect();
    facts
}

/// Seven documents, written under the fixture's schema:
///
/// | path | status | aliases | n | tags |
/// |---|---|---|---|---|
/// | `a.md` | `open` | `[x, y]` | `9` | `draft`, `idea` |
/// | `b.md` | `open` | `[y]` | `9.0` | `draft` |
/// | `c.md` | `closed` | `[]` | `10` | |
/// | `d.md` | | `{k: v}` | `[3, nine]` | `idea` |
/// | `e.md` | `closed` | | `nine` | |
/// | `f.md` | `open` | `[x, x, null]` | `[9, 09]` | `draft`, `draft` |
/// | `g.md` | no frontmatter | | | |
fn seed(store: &mut Store) {
    store
        .begin_request()
        .pin_vault_schema(COUNT_SCHEMA.as_bytes(), COUNT_SCHEMA)
        .expect("pinning the fixture's schema");
    let declared = declared();
    let fields = |at: &str, entries: Vec<(&str, FrontmatterValue)>| {
        document(at, &format!("hash-{at}"), "a body\n")
            .with_frontmatter(Some(map(entries)), &declared)
    };
    write_documents(
        &mut store.begin_request(),
        &[
            tagged(
                fields(
                    "a.md",
                    vec![
                        ("status", string("open")),
                        ("aliases", texts(&["x", "y"])),
                        ("n", string("9")),
                    ],
                ),
                &["draft", "idea"],
            ),
            tagged(
                fields(
                    "b.md",
                    vec![
                        ("status", string("open")),
                        ("aliases", texts(&["y"])),
                        ("n", string("9.0")),
                    ],
                ),
                &["draft"],
            ),
            fields(
                "c.md",
                vec![
                    ("status", string("closed")),
                    ("aliases", texts(&[])),
                    ("n", string("10")),
                ],
            ),
            tagged(
                fields(
                    "d.md",
                    vec![
                        ("aliases", map(vec![("k", string("v"))])),
                        ("n", texts(&["3", "nine"])),
                    ],
                ),
                &["idea"],
            ),
            fields(
                "e.md",
                vec![("status", string("closed")), ("n", string("nine"))],
            ),
            tagged(
                fields(
                    "f.md",
                    vec![
                        ("status", string("open")),
                        (
                            "aliases",
                            FrontmatterValue::Sequence(vec![
                                string("x"),
                                string("x"),
                                FrontmatterValue::Null,
                            ]),
                        ),
                        ("n", texts(&["9", "09"])),
                    ],
                ),
                &["draft", "draft"],
            ),
            document("g.md", "hash-g.md", "a body\n"),
        ],
    );
}

/// Three documents beside the fixture holding the empty text, which is the
/// value that sorts first:
///
/// | path | status | aliases |
/// |---|---|---|
/// | `h.md` | | `[x, x, ""]` |
/// | `i.md` | `""` | `[""]` |
/// | `j.md` | `""` | `[x]` |
fn blanks() -> Vec<norn_store::DocumentFacts> {
    let fields = |at: &str, entries: Vec<(&str, FrontmatterValue)>| {
        document(at, &format!("hash-{at}"), "a body\n")
            .with_frontmatter(Some(map(entries)), &declared())
    };
    vec![
        fields("h.md", vec![("aliases", texts(&["x", "x", ""]))]),
        fields(
            "i.md",
            vec![("status", string("")), ("aliases", texts(&[""]))],
        ),
        fields(
            "j.md",
            vec![("status", string("")), ("aliases", texts(&["x"]))],
        ),
    ]
}

/// A seeded store and the read handle its snapshots are established on.
pub(crate) struct Counting {
    _scratch: Scratch,
    store: Store,
    reader: Arc<SnapshotReader>,
}

impl Counting {
    fn new(label: &str) -> Self {
        Self::with_bulk(label, 0)
    }

    /// The fixture and `bulk` more documents under `bulk/`, each with the
    /// `status` `filed`, its own `n` and alias, and the tag `bulk`: enough
    /// rows that a count whose work grows with the vault reads many times
    /// more of them.
    fn with_bulk(label: &str, bulk: usize) -> Self {
        Self::with_bulk_bodies(label, bulk, "a body\n")
    }

    /// [`Counting::with_bulk`], each bulk document's body `body`.
    fn with_bulk_bodies(label: &str, bulk: usize, body: &str) -> Self {
        let documents: Vec<_> = (0..bulk)
            .map(|at| {
                tagged(
                    document(
                        &format!("bulk/{at:04}.md"),
                        &format!("hash-bulk-{at}"),
                        body,
                    )
                    .with_frontmatter(
                        Some(map(vec![
                            ("status", string("filed")),
                            ("n", string(&at.to_string())),
                            ("aliases", texts(&[&format!("b{at}")])),
                        ])),
                        &declared(),
                    ),
                    &["bulk"],
                )
            })
            .collect();
        Self::with_documents(label, documents)
    }

    /// The fixture and `documents` beside it.
    fn with_documents(label: &str, documents: Vec<norn_store::DocumentFacts>) -> Self {
        let scratch = Scratch::new(label);
        let mut store = scratch.open();
        seed(&mut store);
        if !documents.is_empty() {
            write_documents(&mut store.begin_request(), &documents);
        }
        let reader = Arc::new(
            store
                .open_reader()
                .reader
                .expect("a live store mints a reader"),
        );
        Counting {
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

    fn count(&self, params: &CountParams) -> Counted {
        self.snapshot()
            .count(params, &declared())
            .unwrap_or_else(|refusal| panic!("a count of {params:?}: {refusal}"))
    }

    fn find(&self, params: &FindParams) -> Found {
        self.snapshot()
            .find(params, &declared())
            .unwrap_or_else(|refusal| panic!("a find of {params:?}: {refusal}"))
    }
}

fn vault() -> VaultAddress {
    VaultAddress::name(VaultName::new("notes").expect("a vault name"))
}

fn counting(by: Vec<GroupKey>) -> CountParams {
    CountParams::new(vault()).with_by(by)
}

fn field(key: &str) -> GroupKey {
    GroupKey::field(key)
}

/// A tally as the fixture's tables spell it: `None` for `null`.
fn tally(group: &[Option<&str>], count: u64) -> Tally {
    Tally::new(group.iter().map(|member| member.map(str::to_string)), count)
}

// ---- what a tally counts ----

/// **A set-valued field groups a document once per element**, and a key
/// holding no scalar value groups with a missing key under `null`. `aliases`
/// holds `[x, y]` on `a.md`, so it is counted under both; `f.md` repeats `x`
/// and holds a null, and is counted once, under `x`; the empty sequence on
/// `c.md`, the map on `d.md` and the missing key on `e.md` and `g.md` are the
/// `null` group. The tallies sum past the seven documents.
#[test]
fn a_set_valued_field_counts_a_document_once_in_each_group_it_holds() {
    let counting_store = Counting::new("count-set-valued");
    let counted = counting_store.count(&counting(vec![field("aliases")]));
    assert_eq!(
        counted.tallies,
        vec![
            tally(&[None], 4),
            tally(&[Some("x")], 2),
            tally(&[Some("y")], 2),
        ]
    );
    assert_eq!(counted.next, None);
    assert!(counted.unsatisfied.is_empty());
}

/// **A typed key groups by typed equality**, each group labelled by the least
/// raw spelling among its members and ordered by the typed order: `9`, `9.0`
/// and `09` are one decimal, labelled `09`; `10` stands after it though its
/// text sorts first; `nine` reads as no decimal, so `e.md`, which holds
/// nothing else, groups under `null` with `g.md`, and `d.md` groups under `3`
/// alone.
#[test]
fn a_typed_key_groups_by_typed_equality_under_its_least_spelling() {
    let counting_store = Counting::new("count-typed");
    let counted = counting_store.count(&counting(vec![field("n")]));
    assert_eq!(
        counted.tallies,
        vec![
            tally(&[None], 2),
            tally(&[Some("3")], 1),
            tally(&[Some("09")], 3),
            tally(&[Some("10")], 1),
        ]
    );
    assert!(
        counted.snapshot.schema_fingerprint.is_some(),
        "a count grouped by a typed key answers under the pinned schema"
    );
    // A label is the least spelling among the tally's own documents: `a.md`
    // alone stands in `(9, idea)`, and spells its value `9`.
    let crossed = counting_store.count(&counting(vec![field("n"), GroupKey::tag()]));
    assert!(
        crossed
            .tallies
            .contains(&tally(&[Some("09"), Some("draft")], 3))
            && crossed
                .tallies
                .contains(&tally(&[Some("9"), Some("idea")], 1)),
        "{:?}",
        crossed.tallies
    );
}

/// **A tag grouping is one group per tag, and several keys group by their
/// cross product**, ordered by the tuple in the request's key order with
/// `null` first in each member.
#[test]
fn tags_group_one_per_tag_and_several_keys_group_by_their_cross_product() {
    let counting_store = Counting::new("count-cross");
    assert_eq!(
        counting_store
            .count(&counting(vec![GroupKey::tag()]))
            .tallies,
        vec![
            tally(&[None], 3),
            tally(&[Some("draft")], 3),
            tally(&[Some("idea")], 2),
        ]
    );
    assert_eq!(
        counting_store
            .count(&counting(vec![field("aliases"), GroupKey::tag()]))
            .tallies,
        vec![
            tally(&[None, None], 3),
            tally(&[None, Some("idea")], 1),
            tally(&[Some("x"), Some("draft")], 2),
            tally(&[Some("x"), Some("idea")], 1),
            tally(&[Some("y"), Some("draft")], 2),
            tally(&[Some("y"), Some("idea")], 1),
        ]
    );
}

/// **A request that groups by nothing answers one tally over the whole
/// match**, and a filter narrows it.
#[test]
fn an_empty_grouping_answers_one_tally_over_the_whole_match() {
    let counting_store = Counting::new("count-ungrouped");
    assert_eq!(
        counting_store.count(&counting(Vec::new())).tallies,
        vec![tally(&[], 7)]
    );
    assert_eq!(
        counting_store
            .count(&counting(Vec::new()).with_predicates([Predicate::equal_to("status", "open")]))
            .tallies,
        vec![tally(&[], 3)]
    );
}

/// **A document stands in a group once however many of its values fall
/// there**, in a tally whose leading member is `null` as in any other:
/// `h.md` carries no `status` and holds `x` twice under `aliases`, and is
/// counted once in `(null, x)`.
#[test]
fn a_document_repeating_a_value_stands_once_in_a_null_led_tally() {
    let counting_store = Counting::with_documents("count-null-repeat", blanks());
    let counted = counting_store.count(&counting(vec![field("status"), field("aliases")]));
    assert_eq!(
        counted.tallies,
        vec![
            tally(&[None, None], 2),
            tally(&[None, Some("")], 1),
            tally(&[None, Some("x")], 1),
            tally(&[Some(""), Some("")], 1),
            tally(&[Some(""), Some("x")], 1),
            tally(&[Some("closed"), None], 2),
            tally(&[Some("open"), Some("x")], 2),
            tally(&[Some("open"), Some("y")], 2),
        ]
    );
}

/// **A grouped count answers no group holding no document.** Every `draft`
/// document holds a value under `n`, so grouped by `n` they answer the one
/// group they stand in, and no `null` tally of zero.
#[test]
fn a_grouped_count_answers_no_group_of_zero_documents() {
    let counting_store = Counting::new("count-no-empty-group");
    assert_eq!(
        counting_store
            .count(&counting(vec![field("n")]).with_predicates([Predicate::tag("draft")]))
            .tallies,
        vec![tally(&[Some("09")], 3)]
    );
}

// ---- agreement with find ----

/// The paths a find of `predicates` answers, drained in one page.
fn found_paths(counting_store: &Counting, predicates: Vec<Predicate>) -> BTreeSet<String> {
    let found = counting_store.find(
        &FindParams::new(vault())
            .with_predicates(predicates)
            .with_limit(1000),
    );
    assert_eq!(found.next, None, "the fixture fits one page");
    found
        .rows
        .iter()
        .map(|row| row.path.as_str().to_string())
        .collect()
}

/// The find part that holds a document in group `label` of `key`.
fn member_part(key: &GroupKey, label: &str) -> Predicate {
    match key {
        GroupKey::Field { key, .. } => Predicate::equal_to(key.clone(), label),
        GroupKey::Tag { .. } => Predicate::tag(label),
        _ => panic!("a group key this suite does not know: {key:?}"),
    }
}

/// **Every tally agrees with a find.** For each tally a count returns, the
/// documents a find with the count's predicates and one part per member —
/// `eq key:<label>` for a field, `tag:<label>` for a tag — holds number exactly
/// the tally's count.
///
/// A `null` member is no single find part: it holds the documents that carry
/// no scalar value for the key, which is a missing key, a map, an empty
/// sequence or, under a typed key, no value that reads as the type. The
/// equivalent this states is the complement: the documents the find of the
/// other members holds, less every document a find of this key's labels
/// holds. The labels are every label the count returned for the key, which is
/// every value the matched documents carry under it, since a document carrying
/// a value is counted in that value's group.
#[test]
fn every_tally_agrees_with_a_find_of_its_members() {
    let counting_store = Counting::new("count-agreement");
    let groupings = [
        vec![field("aliases")],
        vec![field("n")],
        vec![GroupKey::tag()],
        vec![field("status")],
        vec![field("aliases"), GroupKey::tag()],
        vec![field("n"), field("aliases"), GroupKey::tag()],
        vec![GroupKey::tag(), field("n")],
    ];
    let conjunctions = [
        Vec::new(),
        vec![Predicate::equal_to("status", "open")],
        vec![Predicate::tag("draft")],
        vec![Predicate::missing("status")],
        vec![Predicate::not_equal_to("status", "closed")],
    ];
    for predicates in &conjunctions {
        for by in &groupings {
            let counted = counting_store.count(
                &counting(by.clone())
                    .with_predicates(predicates.clone())
                    .with_limit(1000),
            );
            assert_eq!(counted.next, None, "the fixture fits one page");
            assert!(
                !counted.tallies.is_empty(),
                "every conjunction here matches some document: {by:?} under {predicates:?}"
            );
            let mut labels: BTreeMap<usize, BTreeSet<String>> = BTreeMap::new();
            for tally in &counted.tallies {
                for (at, member) in tally.group.iter().enumerate() {
                    if let Some(label) = member {
                        labels.entry(at).or_default().insert(label.clone());
                    }
                }
            }
            for tally in &counted.tallies {
                let mut parts = predicates.clone();
                for (key, member) in by.iter().zip(&tally.group) {
                    if let Some(label) = member {
                        parts.push(member_part(key, label));
                    }
                }
                let mut held = found_paths(&counting_store, parts.clone());
                for (at, (key, member)) in by.iter().zip(&tally.group).enumerate() {
                    if member.is_some() {
                        continue;
                    }
                    for label in labels.get(&at).into_iter().flatten() {
                        let mut valued = parts.clone();
                        valued.push(member_part(key, label));
                        for path in found_paths(&counting_store, valued) {
                            held.remove(&path);
                        }
                    }
                }
                assert_eq!(
                    held.len() as u64,
                    tally.count,
                    "{tally:?} of {by:?} under {predicates:?} disagrees with a find, which \
                     holds {held:?}"
                );
            }
        }
    }
}

// ---- the page ----

/// Every tally of `params`, drained a page of `limit` at a time.
fn drained(counting_store: &Counting, params: &CountParams, limit: u32) -> Vec<Tally> {
    let mut tallies = Vec::new();
    let mut after: Option<Cursor> = None;
    loop {
        let mut page = params.clone().with_limit(limit);
        if let Some(cursor) = after.take() {
            page = page.with_after(cursor);
        }
        let counted = counting_store.count(&page);
        assert!(
            counted.tallies.len() <= limit as usize,
            "a page held more than its bound"
        );
        tallies.extend(counted.tallies);
        match counted.next {
            Some(next) => after = Some(next),
            None => return tallies,
        }
        assert!(tallies.len() < 100, "the drain does not end");
    }
}

/// **A drain a page at a time answers the tallies one page does**, in the same
/// order, whatever the bound: a continuation resumes after the last tally's
/// tuple, inside the `null`-lead section and out of it, under a typed member
/// and a raw one, filtered and not.
#[test]
fn a_drain_a_page_at_a_time_answers_the_tallies_one_page_does() {
    let counting_store = Counting::new("count-drain");
    let groupings = [
        vec![field("aliases")],
        vec![field("n")],
        vec![GroupKey::tag()],
        vec![field("aliases"), GroupKey::tag()],
        vec![field("n"), GroupKey::tag(), field("aliases")],
    ];
    for predicates in [Vec::new(), vec![Predicate::tag("draft")]] {
        for by in &groupings {
            let params = counting(by.clone()).with_predicates(predicates.clone());
            let whole = counting_store
                .count(&params.clone().with_limit(1000))
                .tallies;
            for limit in [1, 2, 3] {
                assert_eq!(
                    drained(&counting_store, &params, limit),
                    whole,
                    "{by:?} under {predicates:?} drained {limit} at a time"
                );
            }
        }
    }
    let first = counting_store.count(&counting(vec![field("n")]).with_limit(2));
    assert_eq!(
        first.next.as_ref().map(Cursor::key),
        Some(&CursorKey::tally([Some("3".to_string())])),
        "a page stops at its last tally's labels"
    );
}

/// **`null` and the empty text are two places in a member's order**, `null`
/// first: a drain a page at a time over members holding the empty text beside
/// members holding no value answers the tallies one page does, wherever the
/// bound falls between them.
#[test]
fn a_drain_resumes_between_null_and_the_empty_text() {
    let counting_store = Counting::with_documents("count-drain-blank", blanks());
    for by in [
        vec![field("status")],
        vec![field("status"), field("aliases")],
    ] {
        let params = counting(by.clone());
        let whole = counting_store
            .count(&params.clone().with_limit(1000))
            .tallies;
        assert!(
            whole.iter().any(|tally| tally.group[0].is_none())
                && whole
                    .iter()
                    .any(|tally| tally.group[0].as_deref() == Some("")),
            "the grouping holds both `null` and the empty text: {whole:?}"
        );
        for limit in [1, 2, 3] {
            assert_eq!(
                drained(&counting_store, &params, limit),
                whole,
                "{by:?} drained {limit} at a time"
            );
        }
    }
}

/// **A count grouped by nothing has one tally, which a continuation has
/// passed**: continued after it, the page is empty and the last, whether the
/// match holds documents or a part the count cannot apply emptied it.
#[test]
fn an_ungrouped_count_continued_after_its_one_tally_answers_an_empty_page() {
    let counting_store = Counting::new("count-ungrouped-resumed");
    for predicates in [Vec::new(), vec![Predicate::path("")]] {
        let params = counting(Vec::new()).with_predicates(predicates.clone());
        let counted = counting_store.count(&counting_store.resumed(&params, &[]));
        assert_eq!(counted.tallies, Vec::new(), "under {predicates:?}");
        assert_eq!(counted.next, None, "under {predicates:?}");
    }
}

// ---- the parts a count cannot apply ----

/// **A `resolves` part is answered in-band**: the tallies are the ones the
/// rest of the conjunction earned, and the part is reported as not applicable
/// rather than refused. **The other parts a count cannot apply are reported
/// as a find reports them**, through the same compilation: a malformed glob
/// empties the page, and a predicate key outside the field universe is
/// reported with its near keys and filters nothing.
#[test]
fn a_part_a_count_cannot_apply_is_reported_as_a_find_reports_it() {
    let counting_store = Counting::new("count-unsatisfied");
    let target = ResolutionTarget::new("a").expect("a target");
    let open = Predicate::equal_to("status", "open");
    let earned = counting_store
        .count(&counting(vec![field("aliases")]).with_predicates([open.clone()]))
        .tallies;
    assert!(!earned.is_empty());

    let resolved = counting_store.count(
        &counting(vec![field("aliases")])
            .with_predicates([Predicate::resolves(target.clone()), open.clone()]),
    );
    assert_eq!(resolved.tallies, earned);
    assert_eq!(
        resolved.unsatisfied,
        vec![Unsatisfied::resolves_not_applicable(target)]
    );

    let malformed = counting_store.count(
        &counting(vec![field("aliases")]).with_predicates([Predicate::path(""), open.clone()]),
    );
    assert_eq!(malformed.tallies, Vec::new());
    assert!(
        matches!(
            malformed.unsatisfied.as_slice(),
            [Unsatisfied::MalformedGlob { .. }]
        ),
        "{:?}",
        malformed.unsatisfied
    );

    // An ungrouped count is one tally over the whole match however the match
    // emptied — a part that cannot be applied, or a filter matching nothing —
    // and a grouped count over an empty match answers no tallies either way.
    let nowhere = Predicate::equal_to("status", "nowhere");
    let ungrouped_unapplied =
        counting_store.count(&counting(Vec::new()).with_predicates([Predicate::path("")]));
    assert_eq!(ungrouped_unapplied.tallies, vec![tally(&[], 0)]);
    assert_eq!(ungrouped_unapplied.next, None);
    assert_eq!(
        ungrouped_unapplied.work.tallies_read, 0,
        "no tally statement ran to hand the empty match's tally back"
    );
    assert!(
        matches!(
            ungrouped_unapplied.unsatisfied.as_slice(),
            [Unsatisfied::MalformedGlob { .. }]
        ),
        "{:?}",
        ungrouped_unapplied.unsatisfied
    );
    let ungrouped_unmatched =
        counting_store.count(&counting(Vec::new()).with_predicates([nowhere.clone()]));
    assert_eq!(ungrouped_unmatched.tallies, vec![tally(&[], 0)]);
    assert!(ungrouped_unmatched.unsatisfied.is_empty());
    let grouped_unmatched =
        counting_store.count(&counting(vec![field("aliases")]).with_predicates([nowhere]));
    assert_eq!(grouped_unmatched.tallies, Vec::new());
    assert!(grouped_unmatched.unsatisfied.is_empty());

    let unknown = counting_store
        .count(&counting(vec![field("aliases")]).with_predicates([Predicate::has("stauts"), open]));
    assert_eq!(unknown.tallies, earned);
    assert_eq!(
        unknown.unsatisfied,
        vec![Unsatisfied::unknown_predicate_key(
            "stauts",
            vec!["status".to_string()]
        )]
    );
}

/// **A `by` key outside the field universe still answers**: no document
/// carries it, so every one of the fixture's seven documents groups under
/// `null`, and the key is reported with its near key. **A known key carries
/// no such report.**
#[test]
fn a_by_key_outside_the_field_universe_answers_null_and_is_reported() {
    let counting_store = Counting::new("count-unknown-group");

    let unknown = counting_store.count(&counting(vec![field("staus")]));
    assert_eq!(unknown.tallies, vec![tally(&[None], 7)]);
    assert_eq!(
        unknown.unsatisfied,
        vec![Unsatisfied::unknown_group_key(
            "staus",
            vec!["status".to_string()]
        )]
    );

    let known = counting_store.count(&counting(vec![field("status")]));
    assert!(known.unsatisfied.is_empty());
}

// ---- the cursor ----

/// **A cursor that names no position among the request's tallies is
/// refused**: a document's, a tally of another width, and one whose typed
/// member does not read as its key's type. **A cursor minted in a typed
/// grouping is refused under another schema**, and a raw grouping's cursor
/// continued in a typed grouping is refused as minted in another order.
#[test]
fn a_cursor_that_is_no_position_among_the_requests_tallies_is_refused() {
    let mut counting_store = Counting::new("count-cursor");
    let typed = counting(vec![field("n")]);
    let reading = counting_store.count(&typed).snapshot;
    let refused = |counting_store: &Counting, params: &CountParams| {
        counting_store
            .snapshot()
            .count(params, &declared())
            .expect_err("the cursor is refused")
    };
    for key in [
        CursorKey::document(
            Sort::new(SortKey::path(), Direction::Ascending),
            None,
            "a.md",
        ),
        CursorKey::tally([None, None]),
        CursorKey::tally([Some("nine".to_string())]),
    ] {
        assert_eq!(
            refused(
                &counting_store,
                &typed
                    .clone()
                    .with_after(Cursor::new(reading.clone(), key.clone()))
            ),
            PageRefusal::NotATallyCursor,
            "{key:?}"
        );
    }

    let raw_reading = counting_store
        .count(&counting(vec![field("aliases")]))
        .snapshot;
    assert!(
        matches!(
            refused(
                &counting_store,
                &typed.clone().with_after(Cursor::new(
                    raw_reading,
                    CursorKey::tally([Some("3".to_string())])
                ))
            ),
            PageRefusal::OrderChanged(_)
        ),
        "a raw grouping's cursor continued in a typed grouping"
    );

    let cursor = Cursor::new(reading, CursorKey::tally([Some("3".to_string())]));
    counting_store
        .store
        .begin_request()
        .pin_vault_schema(b"another-schema", "another-schema")
        .expect("pinning another schema");
    let refusal = counting_store
        .snapshot()
        .count(
            &typed.clone().with_after(cursor),
            &ContentModel::under("another-schema")
                .declare_field("n", FieldDeclaration::number(decimal_order())),
        )
        .expect_err("a typed cursor under another schema");
    assert!(
        matches!(refusal, PageRefusal::OrderChanged(_)),
        "{refusal:?}"
    );
}

/// **A declaration read from another schema than the snapshot pins is
/// refused**, as every read refuses it, grouped or not: one read from another
/// schema, and the declaration of a store with none, each named against the
/// schema the snapshot pins.
#[test]
fn a_declaration_the_snapshot_does_not_pin_is_refused() {
    let counting_store = Counting::new("count-declaration");
    for (declared, declared_under) in [
        (
            ContentModel::under("another-schema").declare("status"),
            Some("another-schema".to_string()),
        ),
        (ContentModel::none(), None),
    ] {
        for params in [counting(vec![field("status")]), counting(Vec::new())] {
            assert_eq!(
                counting_store
                    .snapshot()
                    .count(&params, &declared)
                    .expect_err("the declaration is not the pinned one"),
                PageRefusal::DeclarationNotPinned {
                    declared_under: declared_under.clone(),
                    pinned: Some(COUNT_SCHEMA.to_string()),
                },
                "{params:?}"
            );
        }
    }
}

// ---- the plan bars ----

impl Counting {
    fn plans(&self, params: &CountParams) -> Vec<CountPlan> {
        self.snapshot()
            .count_plans(params, &declared())
            .expect("the plans of a count")
    }

    /// `params` continuing after the tally `group`, with a cursor minted in
    /// the reading a first page of it answers from.
    fn resumed(&self, params: &CountParams, group: &[Option<&str>]) -> CountParams {
        let reading = self.count(params).snapshot;
        params.clone().with_after(Cursor::new(
            reading,
            CursorKey::tally(group.iter().map(|member| member.map(str::to_string))),
        ))
    }

    /// Drop `index` on the writer, which is what a negative control judges the
    /// same bar against.
    fn drop_index(&mut self, index: &str) {
        induced_failure::execute_out_of_band(&mut self.store, &format!("DROP INDEX {index}"))
            .unwrap_or_else(|problem| panic!("dropping {index}: {problem}"));
    }
}

/// The plan the store reported for one statement, in the harness's shape.
fn plan(emitted: &CountPlan) -> QueryPlan {
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
fn plan_of(plans: &[CountPlan], statement: CountStatement) -> QueryPlan {
    let matching: Vec<&CountPlan> = plans
        .iter()
        .filter(|plan| plan.statement == ReadStatement::Count(statement))
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "the count runs {statement:?} {} times: {plans:?}",
        matching.len()
    );
    plan(matching[0])
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
/// statement added to [`CountStatement`] does not compile until its author
/// names the bar.
fn statement_barred_by(statement: CountStatement) -> &'static str {
    match statement {
        CountStatement::Total => {
            "an_ungrouped_count_counts_the_documents_or_reaches_them_by_its_filters_seek"
        }
        CountStatement::NullLead(_) => {
            "a_null_lead_section_probes_each_document_for_its_leading_key_by_the_document"
        }
        CountStatement::ValuedLead(_) => {
            "a_valued_section_seeks_its_leading_index_from_the_pages_position"
        }
    }
}

/// **Every statement the count builder names carries a bar, and one bar
/// judges each**: across the bars, the statements each judges cover the
/// enumeration's slots exactly once, and the two section bars each probe every
/// member a leading key is read from.
#[test]
fn the_count_bars_cover_every_statement_once() {
    let per_bar: [Vec<CountStatement>; 3] = [
        vec![CountStatement::Total],
        LEAD_BARS
            .iter()
            .map(|bar| CountStatement::NullLead(bar.member))
            .collect(),
        LEAD_BARS
            .iter()
            .map(|bar| CountStatement::ValuedLead(bar.member))
            .collect(),
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
    assert_eq!(slots, (0..COUNT_STATEMENTS).collect::<Vec<usize>>());
    for (slot, statement) in CountStatement::all().into_iter().enumerate() {
        assert_eq!(statement.slot(), slot, "{statement:?} claims another slot");
    }
    let probed: Vec<GroupMember> = LEAD_BARS.iter().map(|bar| bar.member).collect();
    assert_eq!(probed, GroupMember::ALL.to_vec());
    let bars: BTreeSet<&str> = per_bar
        .into_iter()
        .flatten()
        .map(statement_barred_by)
        .collect();
    assert_eq!(
        bars,
        [
            "a_null_lead_section_probes_each_document_for_its_leading_key_by_the_document",
            "a_valued_section_seeks_its_leading_index_from_the_pages_position",
            "an_ungrouped_count_counts_the_documents_or_reaches_them_by_its_filters_seek",
        ]
        .into_iter()
        .collect()
    );
}

/// **An ungrouped count with no filter is the documents table's own count**,
/// which SQLite answers from the b-tree through its narrowest index and never
/// the table's rows, and **one with a filter reaches the documents the
/// filter's seek hands it by row id**.
///
/// Controls: the unfiltered plan rebuilt as a read of the table's own rows,
/// and the filtered plan rebuilt with its row-id seek as a scan, each fail.
#[test]
fn an_ungrouped_count_counts_the_documents_or_reaches_them_by_its_filters_seek() {
    let counting_store = Counting::new("count-total-plan");
    let judge_whole = |plan: &QueryPlan| {
        plan.assert_no_table_scan();
        assert!(
            plan.rows().len() == 1
                && plan.rows()[0].scans().map(|name| plan.table_of(name)) == Some("documents")
                && plan.rows()[0].index().is_some(),
            "an unfiltered total is not one count of `documents` through an index: {:?}\n\
             emitted SQL: {}",
            plan.rows(),
            plan.sql()
        );
    };
    let judge_filtered = |plan: &QueryPlan| {
        plan.assert_no_full_scan();
        rows_of(plan, "d").assert_searches_through("documents", Access::RowId);
        rows_of(plan, "d").assert_search_constraint("documents", "(rowid=?)");
    };
    let whole = plan_of(
        &counting_store.plans(&counting(Vec::new())),
        CountStatement::Total,
    );
    judge_whole(&whole);
    let filtered = plan_of(
        &counting_store.plans(&counting(Vec::new()).with_predicates([Predicate::tag("draft")])),
        CountStatement::Total,
    );
    judge_filtered(&filtered);

    let unindexed = rewritten(&whole, |detail| {
        detail.split(" USING ").next().unwrap_or(detail).to_string()
    });
    failure_of("an unfiltered total that reads the table's rows", || {
        judge_whole(&unindexed)
    });
    let scanned = rewritten(&filtered, |detail| {
        detail.replace("SEARCH d USING INTEGER PRIMARY KEY (rowid=?)", "SCAN d")
    });
    failure_of("a filtered total that scans the documents", || {
        judge_filtered(&scanned)
    });
}

/// One member a grouping's leading key is read from, and the index its valued
/// section seeks.
struct LeadBar {
    member: GroupMember,
    key: fn() -> GroupKey,
    /// A label of the member's first valued group, which a continuation
    /// resumes after.
    label: &'static str,
    index: &'static str,
    constraint: &'static str,
}

/// Each member a leading key is read from: raw over `status`, which is
/// declared as text; typed over `n`, which is declared a decimal; and the tag.
const LEAD_BARS: &[LeadBar] = &[
    LeadBar {
        member: GroupMember::Field(FieldOrder::Raw),
        key: || GroupKey::field("status"),
        label: "closed",
        index: "document_fields_raw",
        constraint: "(key=? AND raw>?)",
    },
    LeadBar {
        member: GroupMember::Field(FieldOrder::Typed),
        key: || GroupKey::field("n"),
        label: "3",
        index: "document_fields_typed",
        constraint: "(key=? AND typed>?)",
    },
    LeadBar {
        member: GroupMember::Tag,
        key: GroupKey::tag,
        label: "draft",
        index: "document_tags_name",
        constraint: "(name>?)",
    },
];

/// Judge the rows a member reached by its document reads under `alias`: a
/// field's rows by the primary key's `(document, key)` prefix, a tag's by its
/// `(document, ordinal)` index.
fn judge_by_document(plan: &QueryPlan, alias: &str, member: GroupMember) {
    let rows = rows_of(plan, alias);
    match member {
        GroupMember::Field(_) => {
            rows.assert_searches_through("document_fields", Access::PrimaryKey);
            rows.assert_search_constraint("document_fields", "(document=? AND key=?)");
        }
        GroupMember::Tag => {
            rows.assert_searches_through(
                "document_tags",
                Access::Index("document_tags_document_ordinal"),
            );
            rows.assert_search_constraint("document_tags", "(document=?)");
        }
    }
}

/// Whether `plan` sorts to group or order: a temporary B-tree for the
/// `GROUP BY` or the `ORDER BY`. A count's distinct-document tally keeps a
/// temporary B-tree per group of its own, which is not a sort of the page.
fn sorts_the_page(plan: &QueryPlan) -> bool {
    plan.rows().iter().any(|row| {
        row.detail.contains("TEMP B-TREE FOR GROUP BY")
            || row.detail.contains("TEMP B-TREE FOR ORDER BY")
    })
}

/// Judge a valued section no filter drives: a seek of the leading member's
/// value index from the page's position, and — grouped by one key — the
/// groups streamed off it in order, with no sort.
fn judge_valued(plan: &QueryPlan, bar: &LeadBar, single: bool) {
    plan.assert_no_full_scan();
    let lead = rows_of(plan, "g1");
    lead.assert_searches_through(bar.member.table(), Access::Index(bar.index));
    lead.assert_search_constraint(bar.member.table(), bar.constraint);
    assert!(
        !single || !sorts_the_page(plan),
        "a count grouped by one key sorted its groups: {:?}\nemitted SQL: {}",
        plan.rows(),
        plan.sql()
    );
}

/// Judge a valued section a filter drives: the matched documents by row id,
/// each reaching its leading values by the document, never through the
/// leading member's value index.
fn judge_valued_driven(plan: &QueryPlan, bar: &LeadBar) {
    plan.assert_no_full_scan();
    rows_of(plan, "d").assert_searches_through("documents", Access::RowId);
    judge_by_document(plan, "g1", bar.member);
    assert!(
        rows_of(plan, "g1")
            .rows()
            .iter()
            .all(|row| row.index() != Some(bar.index)),
        "a driven section reads its leading value index: {:?}\nemitted SQL: {}",
        plan.rows(),
        plan.sql()
    );
}

/// **A valued section seeks its leading member's index from the page's
/// position.** With no filter, the leading member's rows are one seek of its
/// value index — `(key, raw)`, `(key, typed)`, or the tag's `(name)` — bound
/// below by the page's position, on a first page and a continuation alike;
/// grouped by one key the groups stream off that index in order and nothing
/// sorts. Every trailing member is reached by the document. With a filter
/// that keeps what it seeks, the matched documents drive the section and reach
/// their leading values by the document.
///
/// Controls: a continuation's plan rebuilt without its position bound fails;
/// each value index dropped, the section reads something else; the tag's
/// document index dropped, a trailing tag is reached some other way; a driven
/// plan rebuilt to read its leading value index fails.
#[test]
fn a_valued_section_seeks_its_leading_index_from_the_pages_position() {
    let mut counting_store = Counting::new("count-valued-plan");
    for bar in LEAD_BARS {
        let statement = CountStatement::ValuedLead(bar.member);
        let alone = counting(vec![(bar.key)()]);
        let resumed = counting_store.resumed(&alone, &[Some(bar.label)]);
        judge_valued(
            &plan_of(&counting_store.plans(&alone), statement),
            bar,
            true,
        );
        let continued = plan_of(&counting_store.plans(&resumed), statement);
        judge_valued(&continued, bar, true);

        let crossed = counting(vec![(bar.key)(), GroupKey::tag(), field("aliases")]);
        for params in [
            crossed.clone(),
            counting_store.resumed(&crossed, &[Some(bar.label), None, None]),
        ] {
            let plan = plan_of(&counting_store.plans(&params), statement);
            judge_valued(&plan, bar, false);
            judge_by_document(&plan, "g2", GroupMember::Tag);
            judge_by_document(&plan, "g3", GroupMember::Field(FieldOrder::Raw));
        }

        let driven = plan_of(
            &counting_store.plans(&alone.clone().with_predicates([Predicate::tag("draft")])),
            statement,
        );
        judge_valued_driven(&driven, bar);

        // Control: the continuation's position taken out of the seek.
        let bound = bar
            .constraint
            .trim_start_matches('(')
            .trim_end_matches(')')
            .rsplit(" AND ")
            .next()
            .expect("a constraint names its bound");
        let unbounded = rewritten(&continued, |detail| {
            detail
                .replace(&format!(" AND {bound}"), "")
                .replace(&format!(" ({bound})"), "")
        });
        failure_of("a continuation that seeks from no position", || {
            judge_valued(&unbounded, bar, true)
        });
        // Control: a driven section that reads its leading value index.
        let undriven = rewritten(&driven, |detail| {
            if detail.starts_with("SEARCH g1 ") {
                format!("SEARCH g1 USING INDEX {} {}", bar.index, bar.constraint)
            } else {
                detail.to_string()
            }
        });
        failure_of("a driven section that reads its value index", || {
            judge_valued_driven(&undriven, bar)
        });
    }

    for bar in LEAD_BARS {
        counting_store.drop_index(bar.index);
        let plans = counting_store.plans(&counting(vec![(bar.key)()]));
        failure_of(&format!("{} dropped", bar.index), || {
            judge_valued(
                &plan_of(&plans, CountStatement::ValuedLead(bar.member)),
                bar,
                true,
            )
        });
    }
    counting_store.drop_index("document_tags_document_ordinal");
    let plans = counting_store.plans(&counting(vec![field("status"), GroupKey::tag()]));
    failure_of("document_tags_document_ordinal dropped", || {
        judge_by_document(
            &plan_of(
                &plans,
                CountStatement::ValuedLead(GroupMember::Field(FieldOrder::Raw)),
            ),
            "g2",
            GroupMember::Tag,
        )
    });
}

/// Judge a `null`-lead section: each document probed for a value under the
/// leading key by the document, and no read of the leading key's rows end to
/// end. With no filter the section walks the documents — the one read of a
/// relation end to end it makes — and with one it reaches the matched
/// documents by row id and reads nothing end to end.
fn judge_null(plan: &QueryPlan, member: GroupMember, driven: bool) {
    plan.assert_no_full_scan_of("document_fields");
    plan.assert_no_full_scan_of("document_tags");
    judge_by_document(plan, "m", member);
    if driven {
        plan.assert_no_full_scan();
        rows_of(plan, "d").assert_searches_through("documents", Access::RowId);
    } else {
        assert!(
            plan.unbounded_steps()
                .iter()
                .all(|row| row.scans() == Some("d") && row.index().is_some()),
            "an unfiltered null section reads more than the documents end to end: {:?}\n\
             emitted SQL: {}",
            plan.rows(),
            plan.sql()
        );
    }
}

/// **A `null`-lead section probes each document for its leading key by the
/// document.** Whether a document holds a value under the leading key is one
/// seek of that document's rows — the primary key's `(document, key)` prefix
/// for a field, the `(document, ordinal)` index for a tag — so no document
/// reads the leading key's rows, and nothing reads them end to end. Unfiltered,
/// the section walks the documents through an index, which is the price of
/// finding the documents holding no value; a filter that keeps what it seeks
/// drives it, and then nothing is read end to end. Trailing members are
/// reached by the document.
///
/// Controls: a field probe rebuilt to read the leading key's value index —
/// every row under the key, for every document — fails; the tag's document
/// index dropped, the tag probe reads something else; a driven plan rebuilt
/// to walk the documents fails.
#[test]
fn a_null_lead_section_probes_each_document_for_its_leading_key_by_the_document() {
    let mut counting_store = Counting::new("count-null-plan");
    for bar in LEAD_BARS {
        let statement = CountStatement::NullLead(bar.member);
        let alone = counting(vec![(bar.key)()]);
        judge_null(
            &plan_of(&counting_store.plans(&alone), statement),
            bar.member,
            false,
        );
        let crossed = counting(vec![(bar.key)(), GroupKey::tag(), field("aliases")]);
        for params in [
            crossed.clone(),
            counting_store.resumed(&crossed, &[None, None, Some("x")]),
        ] {
            let plan = plan_of(&counting_store.plans(&params), statement);
            judge_null(&plan, bar.member, false);
            judge_by_document(&plan, "g2", GroupMember::Tag);
            judge_by_document(&plan, "g3", GroupMember::Field(FieldOrder::Raw));
        }
        let driven = plan_of(
            &counting_store.plans(&alone.clone().with_predicates([Predicate::tag("draft")])),
            statement,
        );
        judge_null(&driven, bar.member, true);

        // Control: a driven section that walks the documents.
        let walked = rewritten(&driven, |detail| {
            detail.replace(
                "SEARCH d USING INTEGER PRIMARY KEY (rowid=?)",
                "SCAN d USING COVERING INDEX documents_suffix_key",
            )
        });
        failure_of("a driven null section that walks the documents", || {
            judge_null(&walked, bar.member, true)
        });
        if let GroupMember::Field(_) = bar.member {
            // Control: the probe through the leading key's value index.
            let unkeyed = rewritten(
                &plan_of(&counting_store.plans(&alone), statement),
                |detail| {
                    if detail.starts_with("SEARCH m ") {
                        format!(
                            "SEARCH m USING COVERING INDEX {} {}",
                            bar.index, bar.constraint
                        )
                    } else {
                        detail.to_string()
                    }
                },
            );
            failure_of("a probe that reads the key's value index", || {
                judge_null(&unkeyed, bar.member, false)
            });
        }
    }

    counting_store.drop_index("document_tags_document_ordinal");
    let plans = counting_store.plans(&counting(vec![GroupKey::tag()]));
    failure_of("document_tags_document_ordinal dropped", || {
        judge_null(
            &plan_of(&plans, CountStatement::NullLead(GroupMember::Tag)),
            GroupMember::Tag,
            false,
        )
    });
}

// ---- the work bar ----

/// The groupings the work bar reads: a raw key, a typed key, the tag, three
/// keys crossed, and none.
fn work_groupings() -> [Vec<GroupKey>; 5] {
    [
        vec![field("status")],
        vec![field("n")],
        vec![GroupKey::tag()],
        vec![field("n"), GroupKey::tag(), field("aliases")],
        Vec::new(),
    ]
}

/// Judge a narrowed count by what SQLite counted running it over two vault
/// sizes: the same work at both, and no step through a loop no constraint
/// bounds.
fn judge_narrow(small: &Counting, large: &Counting, params: &CountParams) {
    let (small, large) = (small.count(params).work, large.count(params).work);
    assert_eq!(
        small, large,
        "a narrowed count's work grew with the vault: {params:?}"
    );
    assert_eq!(
        large.full_scan_steps, 0,
        "a narrowed count stepped through a full scan: {large:?} for {params:?}"
    );
}

/// **A narrowing part narrows a count's work to the documents it matches.**
/// Over the fixture and 50, then 500, more documents, a count narrowed to
/// the fixture's own documents — by a tag, by a field's value, and by a tag
/// beside an inequality — runs the same statements and the same VM steps at
/// both sizes, under every grouping, and steps through no full scan: every
/// statement is driven from the seek of a part that keeps what it seeks,
/// whatever excluding part stands beside it.
///
/// **An unfiltered grouped count is linear in the vault, and says so.** Its
/// `null`-lead section walks every document once, so its full-scan steps grow
/// by exactly one per document added; its valued section reads the leading
/// key's rows from the page's position, so its VM steps grow with them too.
/// An unfiltered ungrouped count is the b-tree's own count of `documents`,
/// which steps no row, so its readings stand still while the pages it counts
/// grow.
///
/// Control: `document_tags_name` dropped on the larger vault, the tag part
/// reads its table end to end, and the narrowed bar fails.
#[test]
fn a_narrowing_part_narrows_a_counts_work_to_the_documents_it_matches() {
    let small = Counting::with_bulk("count-work-small", 50);
    let mut large = Counting::with_bulk("count-work-large", 500);
    let narrowing = [
        vec![Predicate::tag("draft")],
        vec![Predicate::equal_to("status", "open")],
        vec![
            Predicate::tag("draft"),
            Predicate::not_equal_to("status", "closed"),
        ],
    ];
    for by in work_groupings() {
        for predicates in &narrowing {
            judge_narrow(
                &small,
                &large,
                &counting(by.clone()).with_predicates(predicates.clone()),
            );
        }
        let whole = counting(by.clone());
        let (at_small, at_large) = (small.count(&whole).work, large.count(&whole).work);
        if by.is_empty() {
            assert_eq!(
                (at_small.full_scan_steps, at_small.vm_steps),
                (at_large.full_scan_steps, at_large.vm_steps),
                "an unfiltered ungrouped count stepped rows"
            );
        } else {
            assert_eq!(
                at_large.full_scan_steps - at_small.full_scan_steps,
                450,
                "an unfiltered count's null section did not walk each added document once: \
                 {at_small:?} then {at_large:?} for {by:?}"
            );
            assert!(
                at_large.vm_steps > at_small.vm_steps * 4,
                "an unfiltered count's VM steps did not grow with the vault: {at_small:?} then \
                 {at_large:?} for {by:?}"
            );
        }
    }

    large.drop_index("document_tags_name");
    for by in work_groupings() {
        let tagged = counting(by).with_predicates([Predicate::tag("draft")]);
        failure_of("document_tags_name dropped", || {
            judge_narrow(&small, &large, &tagged)
        });
    }
}

/// Whether `statement` is one a count runs: a tally statement, or a probe the
/// conjunction's compilation runs, or the fingerprint read. A page of document
/// rows and a hydration are neither.
fn a_count_runs(statement: ReadStatement) -> bool {
    matches!(
        statement,
        ReadStatement::Count(_)
            | ReadStatement::Find(
                FindStatement::ActiveFingerprint
                    | FindStatement::KnownKey
                    | FindStatement::FieldUniverse
                    | FindStatement::BareDirectory
                    | FindStatement::MatchProbe
            )
    )
}

/// **A count's work does not follow the body bytes of the documents it
/// counts.** Over the fixture beside 200 more documents of 64-byte bodies, and
/// beside the same 200 of 16 KiB bodies — the same documents, fields and tags,
/// so the same groups — every grouping the work bar reads, unfiltered and
/// narrowed by a tag and by a field's value, runs the same statements, reads
/// the same tallies and takes the same steps at both. **No count hydrates a
/// document**: every statement it runs is a tally statement, a probe its
/// conjunction's compilation runs, or the fingerprint read, never a page of
/// document rows nor a hydration.
///
/// What a tally costs follows the index entries it reads — the documents it
/// counts, and the leading key's rows a valued section pages — rather than the
/// groups alone, which the narrowing bar reads; this pair holds those fixed
/// and varies the bodies alone. The counters count a statement's steps and
/// not the bytes a step reads, so a statement reading a body by its row id
/// steps the same over both: that no statement reads one is
/// [`no_statement_a_count_runs_reads_a_documents_payload`]'s to hold.
#[test]
fn a_counts_work_does_not_follow_the_body_bytes_it_counts() {
    let short = Counting::with_bulk_bodies("count-bodies-short", 200, &"b".repeat(64));
    let long = Counting::with_bulk_bodies("count-bodies-long", 200, &"b".repeat(16 * 1024));
    let narrowing = [
        Vec::new(),
        vec![Predicate::tag("bulk")],
        vec![Predicate::equal_to("status", "filed")],
    ];
    for by in work_groupings() {
        for predicates in &narrowing {
            let params = counting(by.clone()).with_predicates(predicates.clone());
            let (at_short, at_long) = (short.count(&params), long.count(&params));
            assert_eq!(at_short.tallies, at_long.tallies, "{params:?}");
            assert_eq!(
                at_short.work, at_long.work,
                "a count's work followed the bodies: {params:?}"
            );
            let plans = long.plans(&params);
            assert!(
                plans.iter().all(|plan| a_count_runs(plan.statement)),
                "a count ran a statement that reads document rows: {plans:?}"
            );
        }
    }
}

// ---- the payload bar ----

/// The conjunctions the payload bar compiles: none, and one of each part a
/// count applies — the probes a compilation runs among them: a key the
/// declaration names, a key it does not, a path naming no wildcard, and a
/// full-text query.
fn payload_narrowing() -> Vec<Vec<Predicate>> {
    vec![
        Vec::new(),
        vec![Predicate::tag("draft")],
        vec![Predicate::equal_to("status", "open")],
        vec![
            Predicate::tag("draft"),
            Predicate::not_equal_to("status", "closed"),
        ],
        vec![Predicate::has("status")],
        vec![Predicate::missing("status")],
        vec![Predicate::before("n", "10")],
        vec![Predicate::equal_to("unnamed", "x")],
        vec![Predicate::path("notes")],
        vec![Predicate::path("*.md")],
        vec![Predicate::matches("body")],
    ]
}

/// **No statement a count runs reads a document's payload.** Every statement
/// a count emits — each tally statement, and each probe its conjunction's
/// compilation runs — over every grouping the work bar reads under every part
/// a count applies, and every continuation the plan bars resume, reads none
/// of [`crate::common::DOCUMENT_PAYLOAD`] as SQLite's authorizer reports the
/// columns it reads: so what a count costs never includes the body bytes of
/// the documents it counts, whatever its plan and its step counts say.
///
/// Control: a find projecting the body and the fields, on the same snapshot,
/// hydrates rows reading the body and the frontmatter, and the bar fails over
/// it.
#[test]
fn no_statement_a_count_runs_reads_a_documents_payload() {
    let counting_store = Counting::new("count-payload");
    let mut shapes: Vec<CountParams> = Vec::new();
    for by in work_groupings() {
        for predicates in payload_narrowing() {
            shapes.push(counting(by.clone()).with_predicates(predicates));
        }
    }
    for bar in LEAD_BARS {
        let alone = counting(vec![(bar.key)()]);
        shapes.push(counting_store.resumed(&alone, &[Some(bar.label)]));
        let crossed = counting(vec![(bar.key)(), GroupKey::tag(), field("aliases")]);
        shapes.push(counting_store.resumed(&crossed, &[Some(bar.label), None, None]));
        shapes.push(counting_store.resumed(&crossed, &[None, None, Some("x")]));
    }
    let mut reached: Vec<ReadStatement> = Vec::new();
    for params in &shapes {
        for emitted in counting_store.plans(params) {
            reached.push(emitted.statement);
            reads_of(&emitted.plan).assert_reads_none_of(DOCUMENT_PAYLOAD);
        }
    }
    for statement in CountStatement::all() {
        assert!(
            reached.contains(&ReadStatement::Count(statement)),
            "the payload bar never reached {statement:?}: {reached:?}"
        );
    }
    for probe in [
        FindStatement::KnownKey,
        FindStatement::FieldUniverse,
        FindStatement::BareDirectory,
        FindStatement::MatchProbe,
    ] {
        assert!(
            reached.contains(&ReadStatement::Find(probe)),
            "the payload bar never reached {probe:?}: {reached:?}"
        );
    }

    let hydrated = counting_store
        .snapshot()
        .find_plans(
            &FindParams::new(vault()).with_columns([Column::fields(), Column::body()]),
            &declared(),
        )
        .expect("the plans of a find");
    let rows = hydrated
        .iter()
        .find(|emitted| emitted.statement == FindStatement::HydrateDocuments)
        .expect("a find hydrates its rows");
    let rows = reads_of(&rows.plan);
    assert!(
        rows.reads("documents", "body") && rows.reads("documents", "frontmatter"),
        "a hydration projecting the body and the fields read neither: {rows:?}"
    );
    failure_of("a hydration of the body and the fields", || {
        rows.assert_reads_none_of(DOCUMENT_PAYLOAD)
    });
}

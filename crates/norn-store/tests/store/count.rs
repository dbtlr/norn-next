//! The count builder: what a page of tallies answers, how it agrees with a
//! find, and the plan and work bars every statement it emits is judged by.
//!
//! The fixture is seven documents whose fields hold every shape a grouping
//! meets: a sequence, a sequence repeating a value and holding a null, an
//! empty sequence, a map, a missing key, a typed key spelled two ways for one
//! value and once in a way that reads as no value of its type, and tags.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::common::{Scratch, document, write_documents};
use crate::find::{map, string};
use norn_store::{
    Counted, DeclaredFields, FindRefusal, Found, FrontmatterValue, Snapshot, SnapshotReader, Store,
    TagFact, TagSource, TypedOrder,
};
use norn_wire::{
    CountParams, Cursor, CursorKey, FindParams, GroupKey, Predicate, ResolutionTarget, Tally,
    Unsatisfied, VaultAddress, VaultName,
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
fn declared() -> DeclaredFields {
    DeclaredFields::under(COUNT_SCHEMA)
        .declare("status")
        .declare("aliases")
        .declare_typed("n", decimal_order())
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

/// A seeded store and the read handle its snapshots are established on.
pub(crate) struct Counting {
    _scratch: Scratch,
    store: Store,
    reader: Arc<SnapshotReader>,
}

impl Counting {
    fn new(label: &str) -> Self {
        let scratch = Scratch::new(label);
        let mut store = scratch.open();
        seed(&mut store);
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
        CursorKey::document(None, "a.md"),
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
            FindRefusal::NotATallyCursor,
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
            FindRefusal::OrderChanged(_)
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
            &DeclaredFields::under("another-schema").declare_typed("n", decimal_order()),
        )
        .expect_err("a typed cursor under another schema");
    assert!(
        matches!(refusal, FindRefusal::OrderChanged(_)),
        "{refusal:?}"
    );
}

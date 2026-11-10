//! The death prune ADR 0052 rules: a death is kept only while lane 2 has not
//! read it, up to one unread death per live document, or while the
//! move-following reads return it for a link that is broken now.

use std::collections::BTreeSet;

use norn_store::{
    Change, ContentModel, DEATH_PRUNE_LEAST_ROWS, DEATH_PRUNE_STATEMENTS,
    DEATH_PRUNE_UNIT_REMOVALS, DEATH_PRUNE_UNIT_ROWS, DeathPrunePass, DeathPrunePhase,
    DeathPruneStatement, DeathPruneUnit, DocumentFacts, IncrementProvenance, LinkFamily,
    MovedTombstone, Provenance, ReadThrough, Snapshot, Store, StoredPathOrder,
};
use norn_testkit::explain::{Access, PlanRow, QueryPlan};

use crate::common::{Scratch, path, pinned_declaration};
use crate::health::derived;

// ---- fixtures ----

/// Apply one changeset under the pinned declaration, and hand back its
/// generation.
fn apply(store: &mut Store, changes: Vec<Change>) -> i64 {
    let mut request = store.begin_request();
    let declared = pinned_declaration(&request);
    request
        .apply_increment(IncrementProvenance::Derived, changes, &[], &declared)
        .expect("applying a changeset")
        .generation
        .expect("a changeset that names an entry takes a generation")
}

/// The document at `at` holding `body`, with the links the text layer reads.
fn holder(at: &str, body: &str) -> Change {
    Change::Upsert(derived(at, body))
}

/// A document at `at` with nothing in it.
fn plain(at: &str) -> Change {
    Change::Upsert(DocumentFacts::new(
        path(at),
        format!("hash-{at}"),
        "body\n",
        5,
    ))
}

/// A document's death learned from the watcher.
fn death(at: &str) -> Change {
    Change::Death {
        path: path(at),
        provenance: Provenance::WatcherRemoval,
    }
}

/// Record the death of a document that stood at `at`, so the death carries
/// the hash move-following matches a holder by.
fn died(store: &mut Store, at: &str) -> i64 {
    apply(store, vec![plain(at)]);
    apply(store, vec![death(at)])
}

fn snapshot(store: &Store) -> Snapshot {
    std::sync::Arc::new(store.open_reader().reader.expect("a reader"))
        .try_take()
        .expect("an idle reader")
        .establish()
        .expect("a snapshot")
}

/// Every death the store holds, by path.
fn deaths(store: &mut Store) -> BTreeSet<String> {
    let request = store.begin_request();
    let mut found = BTreeSet::new();
    let mut after = None;
    loop {
        let page = request
            .stored_tombstones_after(after.as_ref(), 64)
            .expect("a page of deaths");
        let Some(last) = page.last() else { break };
        after = Some(last.path.clone());
        found.extend(page.iter().map(|death| death.path.as_str().to_string()));
    }
    found
}

/// Run units of `pass` until it finishes, each reading at most `bound` rows,
/// every one handed `read_through`.
fn run_pass(
    store: &mut Store,
    pass: &mut DeathPrunePass,
    read_through: Option<ReadThrough>,
    bound: usize,
) -> Vec<DeathPruneUnit> {
    let mut units = Vec::new();
    loop {
        let unit = store
            .begin_request()
            .death_prune_unit(pass, read_through, bound)
            .expect("a death-prune unit");
        let finished = unit.finished;
        units.push(unit);
        if finished {
            return units;
        }
        assert!(units.len() < 10_000, "the pass never finished");
    }
}

/// One whole pass at the default bound.
fn prune(store: &mut Store, read_through: Option<ReadThrough>) -> Vec<DeathPruneUnit> {
    run_pass(
        store,
        &mut DeathPrunePass::new(),
        read_through,
        DEATH_PRUNE_UNIT_ROWS,
    )
}

/// Each link holding a current `link/broken` finding, as the holder's path
/// and the link's family, protocol and target.
type BrokenLink = (String, norn_wire::LinkFamily, Option<String>, String);

fn broken_links(store: &mut Store) -> Vec<BrokenLink> {
    let mut request = store.begin_request();
    let mut found = Vec::new();
    let mut after = None;
    loop {
        let page = request
            .stored_findings_after(after, 64)
            .expect("a page of findings");
        let Some((last, _)) = page.last() else { break };
        after = Some(*last);
        for (_, finding) in page {
            if finding.kind != "link/broken" {
                continue;
            }
            let ordinal = finding.ordinal.expect("a link finding names its link") as usize;
            let facts = request
                .stored_facts(&finding.path)
                .expect("reading the holder")
                .expect("the holder stands");
            let link = &facts.links[ordinal].fact;
            let family = match link.family {
                LinkFamily::Wikilink => norn_wire::LinkFamily::Wikilink,
                LinkFamily::Markdown => norn_wire::LinkFamily::Markdown,
            };
            found.push((
                finding.path.as_str().to_string(),
                family,
                link.protocol.clone(),
                link.target.clone(),
            ));
        }
    }
    found
}

/// What every move-following read returns for each of `links`.
fn followed(store: &Store, links: &[BrokenLink]) -> Vec<(BrokenLink, Vec<MovedTombstone>)> {
    let read = snapshot(store);
    links
        .iter()
        .map(|link| {
            let (holder, family, protocol, target) = link;
            let deaths = read
                .followed_tombstones(*family, protocol.as_deref(), target, &path(holder))
                .expect("the move-following reads");
            (link.clone(), deaths)
        })
        .collect()
}

fn names(paths: &[&str]) -> BTreeSet<String> {
    paths.iter().map(|at| at.to_string()).collect()
}

// ---- which deaths a link keeps ----

/// **A broken link keeps the two deaths its read returns, and no other.** A
/// third death named the same is one repair cannot reach, so it goes; a
/// healthy or an ambiguous link keeps nothing, since move-following starts
/// only from a broken link; and no link keeps a quarantine death, which
/// move-following never reads. With no reader of the death feed standing,
/// condition 1 keeps nothing, so what stands is exactly what the links keep.
#[test]
fn a_broken_link_keeps_the_two_deaths_its_read_returns_and_nothing_else_does() {
    let scratch = Scratch::new("prune-links");
    let mut store = scratch.open();
    for at in ["a/target.md", "b/target.md", "c/target.md"] {
        died(&mut store, at);
    }
    died(&mut store, "gone/alive.md");
    died(&mut store, "r/amb.md");
    died(&mut store, "notes/moved.md");
    apply(
        &mut store,
        vec![Change::Death {
            path: path("q/held.md"),
            provenance: Provenance::Quarantine,
        }],
    );
    apply(
        &mut store,
        vec![
            holder("notes/one.md", "[[target]]\n"),
            holder("notes/two.md", "[[alive]]\n"),
            holder("notes/three.md", "[[amb]]\n"),
            holder("notes/four.md", "[[held]]\n"),
            holder("notes/five.md", "[gone](./moved.md)\n"),
            plain("alive.md"),
            plain("p/amb.md"),
            plain("s/amb.md"),
        ],
    );
    let broken = broken_links(&mut store);
    assert_eq!(
        broken
            .iter()
            .map(|link| link.0.as_str())
            .collect::<Vec<_>>(),
        ["notes/five.md", "notes/four.md", "notes/one.md"],
        "the fixture's broken links"
    );
    let before = followed(&store, &broken);
    let kept_by_target: BTreeSet<String> = before
        .iter()
        .flat_map(|(_, deaths)| deaths)
        .map(|death| death.path.as_str().to_string())
        .collect();
    assert_eq!(
        kept_by_target,
        names(&["a/target.md", "b/target.md", "notes/moved.md"])
    );

    let units = prune(&mut store, None);

    assert_eq!(
        deaths(&mut store),
        names(&["a/target.md", "b/target.md", "notes/moved.md"]),
        "a third death, one a healthy, ambiguous or quarantine-reading link names, stood"
    );
    assert_eq!(followed(&store, &broken), before);
    let removed: u64 = units.iter().map(|unit| unit.work.deaths_removed).sum();
    let kept: u64 = units.iter().map(|unit| unit.work.deaths_kept_by_link).sum();
    assert_eq!((removed, kept), (4, 3));
}

/// **Move-following answers the same before and after every unit.** Over
/// broken links whose names collide with older and newer deaths, by suffix
/// and by exact path, every move-following read for every broken link is
/// compared before and after each unit, with changesets breaking, healing and
/// refiling links and recording deaths between the units.
#[test]
fn move_following_answers_the_same_before_and_after_every_unit() {
    for seed in 1..=6u64 {
        let scratch = Scratch::new("prune-property");
        let mut store = scratch.open();
        let mut random = SplitMix(seed);
        let stems = ["target", "glossary", "index", "moved", "x"];
        let folders = ["a", "b", "c", "notes"];
        let pick = |random: &mut SplitMix, from: &[&'static str]| -> &'static str {
            from[random.below(from.len() as u64) as usize]
        };
        let change = |store: &mut Store, random: &mut SplitMix| {
            let stem = pick(random, &stems);
            let folder = pick(random, &folders);
            match random.below(5) {
                0 | 1 => {
                    died(store, &format!("{folder}/{stem}.md"));
                }
                2 => {
                    let body = if random.below(2) == 0 {
                        format!("[[{stem}]]\n")
                    } else {
                        format!("[l](../{folder}/{stem}.md) [m](./{stem}.md)\n")
                    };
                    let at = format!("notes/h{}.md", random.below(4));
                    apply(store, vec![holder(&at, &body)]);
                }
                3 => {
                    apply(store, vec![plain(&format!("{folder}/{stem}.md"))]);
                }
                _ => {
                    apply(store, vec![death(&format!("{folder}/{stem}.md"))]);
                }
            }
        };
        for _ in 0..40 {
            change(&mut store, &mut random);
        }
        let read_through = match random.below(3) {
            0 => None,
            1 => Some(ReadThrough::Nothing),
            _ => {
                let generation = store.begin_request().write_generation().unwrap();
                Some(ReadThrough::Through(generation / 2))
            }
        };
        let mut pass = DeathPrunePass::new();
        let mut units = 0;
        loop {
            let broken = broken_links(&mut store);
            let before = followed(&store, &broken);
            let unit = store
                .begin_request()
                .death_prune_unit(&mut pass, read_through, DEATH_PRUNE_LEAST_ROWS + 3)
                .expect("a unit");
            assert_eq!(
                followed(&store, &broken),
                before,
                "seed {seed}: a unit changed what move-following reads for a broken link"
            );
            if unit.finished {
                break;
            }
            units += 1;
            assert!(units < 10_000, "seed {seed}: the pass never finished");
            if random.below(4) == 0 {
                change(&mut store, &mut random);
            }
        }
        store.verify_integrity().expect("a sound store");
    }
}

/// A SplitMix64 stream: deterministic, so a failing seed replays.
struct SplitMix(u64);

impl SplitMix {
    fn below(&mut self, bound: u64) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        (z ^ (z >> 31)) % bound
    }
}

// ---- lane 2 and the live-document bound ----

/// Three live documents, and six deaths in six changesets; the generations.
fn six_deaths_over_three_documents(store: &mut Store) -> Vec<i64> {
    apply(
        store,
        vec![plain("live/1.md"), plain("live/2.md"), plain("live/3.md")],
    );
    (1..=6)
        .map(|at| died(store, &format!("dead/{at}.md")))
        .collect()
}

/// **Unread deaths stand up to the live-document count, and past it the oldest
/// go first.** With three live documents the walk passes the three newest
/// deaths and the fourth newest is the cutoff: it and the older unread deaths
/// go, the floor rises to the cutoff's generation, and the three newest stand.
#[test]
fn unread_deaths_stand_up_to_the_live_document_count_and_the_oldest_go_first() {
    let scratch = Scratch::new("prune-bound");
    let mut store = scratch.open();
    let generations = six_deaths_over_three_documents(&mut store);

    let mut pass = DeathPrunePass::new();
    let units = run_pass(
        &mut store,
        &mut pass,
        Some(ReadThrough::Through(generations[1])),
        DEATH_PRUNE_UNIT_ROWS,
    );

    assert_eq!(
        deaths(&mut store),
        names(&["dead/4.md", "dead/5.md", "dead/6.md"])
    );
    assert_eq!(
        pass.cutoff().map(|cutoff| cutoff.path().as_str()),
        Some("dead/3.md")
    );
    let raised: Vec<i64> = units.iter().filter_map(|unit| unit.floor_raised).collect();
    assert_eq!(raised, [generations[2]]);
    assert_eq!(
        store.begin_request().death_floor().unwrap(),
        Some(generations[2])
    );
    assert!(pass.kept_for_lane_two_alone());
    assert_eq!(
        pass.lowest_read_through(),
        Some(ReadThrough::Through(generations[1]))
    );
}

/// **Within the bound, every death above the read-through generation stands,
/// and every one at or below it goes.** The walk reaches a read death before
/// it passes the live-document count, so the pass has no cutoff.
#[test]
fn within_the_bound_every_unread_death_stands() {
    let scratch = Scratch::new("prune-within");
    let mut store = scratch.open();
    let generations = six_deaths_over_three_documents(&mut store);

    let mut pass = DeathPrunePass::new();
    run_pass(
        &mut store,
        &mut pass,
        Some(ReadThrough::Through(generations[3])),
        DEATH_PRUNE_UNIT_ROWS,
    );
    assert_eq!(pass.cutoff(), None);
    assert_eq!(deaths(&mut store), names(&["dead/5.md", "dead/6.md"]));
    assert_eq!(
        store.begin_request().death_floor().unwrap(),
        Some(generations[3])
    );
}

/// **With no reader standing, every death no link keeps goes**; with readers
/// that have read nothing, the bound alone decides.
#[test]
fn no_reader_keeps_nothing_and_readers_that_read_nothing_keep_the_bound() {
    let scratch = Scratch::new("prune-no-reader");
    let mut store = scratch.open();
    let generations = six_deaths_over_three_documents(&mut store);
    prune(&mut store, Some(ReadThrough::Nothing));
    assert_eq!(
        deaths(&mut store),
        names(&["dead/4.md", "dead/5.md", "dead/6.md"])
    );
    assert_eq!(
        store.begin_request().death_floor().unwrap(),
        Some(generations[2])
    );
    let units = prune(&mut store, None);
    assert!(deaths(&mut store).is_empty());
    assert_eq!(
        store.begin_request().death_floor().unwrap(),
        Some(generations[5])
    );
    // Condition 1 keeps nothing, so the pass reads no walk rows and never
    // counts the live documents.
    assert_eq!(
        units
            .iter()
            .map(|unit| unit.work.deaths_walked)
            .sum::<u64>(),
        0
    );
    assert!(units.iter().all(|unit| {
        !matches!(
            unit.ran,
            DeathPrunePhase::Counting | DeathPrunePhase::Walking
        ) && unit.work.live_documents.is_none()
    }));
}

/// **A pass begun with no reader that is later handed one keeps every death
/// above that read-through for the pass**: it has no cutoff, so it removes
/// less than the bound allows and never more.
#[test]
fn a_pass_that_skipped_the_walk_and_is_later_handed_a_reader_keeps_what_it_has_not_read() {
    let scratch = Scratch::new("prune-skipped-walk");
    let mut store = scratch.open();
    let generations = six_deaths_over_three_documents(&mut store);

    let mut pass = DeathPrunePass::new();
    // Units until the marking ends and the pass reaches removing, handed none.
    while pass.phase() != DeathPrunePhase::Removing {
        store
            .begin_request()
            .death_prune_unit(&mut pass, None, DEATH_PRUNE_UNIT_ROWS)
            .expect("a death-prune unit");
    }
    assert_eq!(pass.cutoff(), None);
    while !pass.is_finished() {
        store
            .begin_request()
            .death_prune_unit(
                &mut pass,
                Some(ReadThrough::Through(generations[1])),
                DEATH_PRUNE_UNIT_ROWS,
            )
            .expect("a death-prune unit");
    }
    assert_eq!(
        deaths(&mut store),
        names(&["dead/3.md", "dead/4.md", "dead/5.md", "dead/6.md"])
    );
    assert!(pass.kept_for_lane_two_alone());
}

/// **Every unread death counts toward the bound, one a broken link keeps
/// included.** Two of the three newest deaths are the ones a broken link
/// reads, so condition 1 keeps one death of its own and the cutoff does not
/// move past them.
#[test]
fn a_death_a_broken_link_keeps_still_counts_toward_the_bound() {
    let scratch = Scratch::new("prune-counted");
    let mut store = scratch.open();
    apply(
        &mut store,
        vec![
            plain("live/1.md"),
            plain("live/2.md"),
            holder("live/3.md", "[[kept]]\n"),
        ],
    );
    for at in ["dead/1.md", "dead/2.md", "dead/3.md", "dead/4.md"] {
        died(&mut store, at);
    }
    died(&mut store, "x/kept.md");
    died(&mut store, "y/kept.md");

    let mut pass = DeathPrunePass::new();
    let units = run_pass(
        &mut store,
        &mut pass,
        Some(ReadThrough::Nothing),
        DEATH_PRUNE_UNIT_ROWS,
    );
    assert_eq!(
        pass.cutoff().map(|cutoff| cutoff.path().as_str()),
        Some("dead/3.md")
    );
    assert_eq!(
        deaths(&mut store),
        names(&["dead/4.md", "x/kept.md", "y/kept.md"])
    );
    let kept_for_lane_two: u64 = units
        .iter()
        .map(|unit| unit.work.deaths_kept_for_lane_two)
        .sum();
    assert_eq!(kept_for_lane_two, 1);
}

/// **No live document keeps no unread death**: the cutoff is the newest
/// death, and the floor rises to the generation of the last deletion.
#[test]
fn with_no_live_documents_every_unread_death_goes() {
    let scratch = Scratch::new("prune-empty");
    let mut store = scratch.open();
    died(&mut store, "only.md");
    let last = died(&mut store, "last.md");
    prune(&mut store, Some(ReadThrough::Nothing));
    assert!(deaths(&mut store).is_empty());
    assert_eq!(store.begin_request().death_floor().unwrap(), Some(last));
}

// ---- the death floor ----

/// **The floor is absent until the first prune removes a death, only rises,
/// and is read with the store reading every snapshot carries.** A death a
/// broken link kept through the first pass, removed by a later one once its
/// link heals, leaves the floor where the first pass raised it.
#[test]
fn the_floor_is_absent_until_a_removal_and_only_rises() {
    let scratch = Scratch::new("prune-floor");
    let mut store = scratch.open();
    apply(&mut store, vec![holder("h.md", "[[kept]]\n")]);
    died(&mut store, "x/kept.md");
    let newest = died(&mut store, "y/other.md");
    assert_eq!(store.begin_request().death_floor().unwrap(), None);
    assert_eq!(snapshot(&store).reading().death_floor(), None);

    // Nothing to remove: no floor.
    prune(&mut store, Some(ReadThrough::Nothing));
    assert_eq!(store.begin_request().death_floor().unwrap(), None);

    prune(&mut store, None);
    assert_eq!(deaths(&mut store), names(&["x/kept.md"]));
    assert_eq!(snapshot(&store).reading().death_floor(), Some(newest));

    // The link heals; its death goes, and the floor stays.
    apply(&mut store, vec![plain("kept.md")]);
    let units = prune(&mut store, None);
    assert!(deaths(&mut store).is_empty());
    assert!(units.iter().all(|unit| unit.floor_raised.is_none()));
    assert_eq!(store.begin_request().death_floor().unwrap(), Some(newest));
}

/// **The unit that removes a death raises the floor in its own transaction**:
/// the reading it hands back, read before it commits, already carries both
/// the removal and the floor, and a unit takes no write generation.
#[test]
fn a_removing_unit_raises_the_floor_and_takes_no_generation() {
    let scratch = Scratch::new("prune-floor-transaction");
    let mut store = scratch.open();
    let generation = died(&mut store, "gone.md");
    let units = prune(&mut store, None);
    let removing: Vec<&DeathPruneUnit> = units
        .iter()
        .filter(|unit| unit.work.deaths_removed > 0)
        .collect();
    assert_eq!(removing.len(), 1);
    assert_eq!(removing[0].floor_raised, Some(generation));
    assert_eq!(removing[0].reading.death_floor(), Some(generation));
    for unit in &units {
        assert_eq!(unit.reading.write_generation(), generation);
        assert_eq!(unit.reading.epoch(), store.epoch());
    }
    assert_eq!(
        store.begin_request().write_generation().unwrap(),
        generation
    );
}

/// **A feed page reads the floor in its own snapshot.** A floor raised while
/// a page is open is not the page's, and the next page reads it.
#[test]
fn a_feed_page_reports_the_floor_its_own_snapshot_reads() {
    let scratch = Scratch::new("prune-feed-floor");
    let mut store = scratch.open();
    let generation = died(&mut store, "gone.md");
    let mut source = store.feed_source();
    {
        let mut page = source.page().expect("a page");
        assert_eq!(page.death_floor().unwrap(), None);
        prune(&mut store, None);
        assert_eq!(
            page.death_floor().unwrap(),
            None,
            "the page read a floor raised after its snapshot"
        );
        let reading = page.store_reading().unwrap();
        assert_eq!(
            (reading.write_generation(), reading.death_floor()),
            (generation, None)
        );
        assert_eq!(page.changed_tombstones_after(None, 8).unwrap().len(), 1);
    }
    let mut page = source.page().expect("the next page");
    assert_eq!(page.death_floor().unwrap(), Some(generation));
    assert_eq!(
        page.store_reading().unwrap().death_floor(),
        Some(generation)
    );
    assert!(page.changed_tombstones_after(None, 8).unwrap().is_empty());
    drop(page);
    assert_eq!(store.feed_read().death_floor().unwrap(), Some(generation));
}

/// A store created again from zero carries a new epoch and no floor.
#[test]
fn a_store_created_from_zero_holds_no_floor() {
    let scratch = Scratch::new("prune-rebuild");
    let mut store = scratch.open();
    died(&mut store, "gone.md");
    prune(&mut store, None);
    assert!(store.begin_request().death_floor().unwrap().is_some());
    let epoch = store.epoch().to_string();
    let mut rebuilt = store
        .discard_and_reopen(StoredPathOrder::Sensitive)
        .expect("rebuilding the store from zero");
    assert_ne!(rebuilt.epoch(), epoch);
    assert_eq!(rebuilt.begin_request().death_floor().unwrap(), None);
}

// ---- a pass across units ----

/// **A pass continued across many small units reaches what one large unit
/// reaches, reading each finding and death once.** Every unit stays within
/// its row bound save the live-document count, which runs alone.
#[test]
fn a_pass_across_small_units_reaches_what_one_large_unit_does() {
    let build = |label: &str| {
        let scratch = Scratch::new(label);
        let mut store = scratch.open();
        apply(
            &mut store,
            (0..6)
                .map(|at| holder(&format!("h/{at}.md"), &format!("[[t{at}]] [[u{at}]]\n")))
                .chain((0..10).map(|at| plain(&format!("live/{at}.md"))))
                .collect(),
        );
        for at in 0..6 {
            for folder in ["a", "b", "c"] {
                died(&mut store, &format!("{folder}/t{at}.md"));
            }
        }
        for at in 0..10 {
            died(&mut store, &format!("plain/{at}.md"));
        }
        (scratch, store)
    };
    let (_large_scratch, mut large) = build("prune-large");
    let (_small_scratch, mut small) = build("prune-small");
    let read_through = Some(ReadThrough::Nothing);

    let mut large_pass = DeathPrunePass::new();
    run_pass(
        &mut large,
        &mut large_pass,
        read_through,
        DEATH_PRUNE_UNIT_ROWS,
    );
    let mut small_pass = DeathPrunePass::new();
    let units = run_pass(
        &mut small,
        &mut small_pass,
        read_through,
        DEATH_PRUNE_LEAST_ROWS,
    );

    assert_eq!(deaths(&mut small), deaths(&mut large));
    assert_eq!(
        small_pass.cutoff().map(|cutoff| cutoff.path().clone()),
        large_pass.cutoff().map(|cutoff| cutoff.path().clone())
    );
    for unit in &units {
        if unit.ran != DeathPrunePhase::Counting {
            assert!(
                unit.work.rows_read <= DEATH_PRUNE_LEAST_ROWS as u64,
                "a {:?} unit read {} rows",
                unit.ran,
                unit.work.rows_read
            );
        }
    }
    let phases: BTreeSet<String> = units.iter().map(|unit| format!("{:?}", unit.ran)).collect();
    assert_eq!(
        phases,
        ["Clearing", "Counting", "Marking", "Removing", "Walking"]
            .map(String::from)
            .into()
    );
    for phase in [
        DeathPrunePhase::Marking,
        DeathPrunePhase::Walking,
        DeathPrunePhase::Removing,
    ] {
        assert!(
            units.iter().filter(|unit| unit.ran == phase).count() > 1,
            "{phase:?} never continued across units"
        );
    }
    let sum = |field: fn(&DeathPruneUnit) -> u64| units.iter().map(field).sum::<u64>();
    assert_eq!(
        sum(|unit| unit.work.findings_marked),
        12,
        "a finding was marked twice"
    );
    assert_eq!(sum(|unit| unit.work.broken_links_marked), 12);
    assert_eq!(sum(|unit| unit.work.deaths_marked), 12);
    assert_eq!(sum(|unit| unit.work.catch_up_findings), 0);
    assert_eq!(
        sum(|unit| unit.work.deaths_examined),
        28,
        "a death was judged twice"
    );
    assert_eq!(
        sum(|unit| unit.work.deaths_examined),
        sum(|unit| unit.work.deaths_removed)
            + sum(|unit| unit.work.deaths_kept_by_link)
            + sum(|unit| unit.work.deaths_kept_for_lane_two)
    );
    assert_eq!(sum(|unit| unit.work.kept_rows_cleared), 12);
    assert_eq!(
        units
            .iter()
            .filter(|unit| unit.ran == DeathPrunePhase::Counting)
            .count(),
        1
    );
    let counted = units
        .iter()
        .find(|unit| unit.ran == DeathPrunePhase::Counting)
        .unwrap();
    assert_eq!(counted.work.live_documents, Some(16));
    // The walk passes the sixteen newest deaths and meets the cutoff.
    assert_eq!(sum(|unit| unit.work.deaths_walked), 17);
    assert_eq!(small_pass.kept_rows(), 0);
}

/// **A removing unit deletes at most [`DEATH_PRUNE_UNIT_REMOVALS`] deaths**,
/// however far its row bound reaches, and the next unit continues from the
/// last death it judged. A death a broken link keeps spends none of that
/// bound.
#[test]
fn a_removing_unit_deletes_at_most_its_removal_bound() {
    let scratch = Scratch::new("prune-removal-bound");
    let mut store = scratch.open();
    apply(&mut store, vec![holder("h.md", "[[kept]]\n")]);
    let total = 2 * DEATH_PRUNE_UNIT_REMOVALS + 3;
    let at = |n: usize| format!("dead/{n:04}.md");
    apply(&mut store, (0..total).map(|n| plain(&at(n))).collect());
    apply(&mut store, (0..total).map(|n| death(&at(n))).collect());
    died(&mut store, "x/kept.md");
    // The row bound alone would let one unit remove every death here.
    const { assert!(2 * DEATH_PRUNE_UNIT_REMOVALS < DEATH_PRUNE_UNIT_ROWS) };

    let units = prune(&mut store, None);

    assert_eq!(deaths(&mut store), names(&["x/kept.md"]));
    let removing: Vec<&DeathPruneUnit> = units
        .iter()
        .filter(|unit| unit.ran == DeathPrunePhase::Removing)
        .collect();
    for unit in &removing {
        assert!(
            unit.work.deaths_removed <= DEATH_PRUNE_UNIT_REMOVALS as u64,
            "a removing unit deleted {} deaths",
            unit.work.deaths_removed
        );
    }
    let removed: Vec<u64> = removing
        .iter()
        .map(|unit| unit.work.deaths_removed)
        .collect();
    let full = DEATH_PRUNE_UNIT_REMOVALS as u64;
    assert_eq!(removed, vec![full, full, 3]);
    assert_eq!(
        removing
            .iter()
            .map(|unit| unit.work.deaths_examined)
            .sum::<u64>(),
        total as u64 + 1,
        "a death was judged twice or never"
    );
    assert_eq!(removing.last().unwrap().work.deaths_kept_by_link, 1);
}

/// **A removing unit first marks the broken findings filed since the
/// marking cursor**, so a link that broke after marking keeps its deaths.
#[test]
fn a_link_broken_after_marking_keeps_its_deaths() {
    let scratch = Scratch::new("prune-catch-up");
    let mut store = scratch.open();
    died(&mut store, "old/late.md");
    died(&mut store, "plain.md");
    let mut pass = DeathPrunePass::new();
    while pass.phase() != DeathPrunePhase::Removing {
        store
            .begin_request()
            .death_prune_unit(&mut pass, None, DEATH_PRUNE_UNIT_ROWS)
            .unwrap();
    }
    apply(&mut store, vec![holder("h.md", "[[late]]\n")]);
    let units = run_pass(&mut store, &mut pass, None, DEATH_PRUNE_UNIT_ROWS);
    assert_eq!(deaths(&mut store), names(&["old/late.md"]));
    assert_eq!(units[0].work.catch_up_findings, 1);
    assert_eq!(units[0].work.deaths_marked, 1);
}

/// **A unit whose catch-up fills its bound deletes nothing**, and the next
/// unit continues the catch-up.
#[test]
fn a_catch_up_that_fills_the_bound_deletes_nothing() {
    let scratch = Scratch::new("prune-catch-up-bound");
    let mut store = scratch.open();
    died(&mut store, "plain.md");
    let mut pass = DeathPrunePass::new();
    while pass.phase() != DeathPrunePhase::Removing {
        store
            .begin_request()
            .death_prune_unit(&mut pass, None, DEATH_PRUNE_UNIT_ROWS)
            .unwrap();
    }
    apply(&mut store, vec![holder("h.md", "[[l1]] [[l2]] [[l3]]\n")]);
    let unit = store
        .begin_request()
        .death_prune_unit(&mut pass, None, DEATH_PRUNE_LEAST_ROWS)
        .unwrap();
    assert_eq!(unit.work.catch_up_findings, 1);
    assert_eq!(unit.work.deaths_examined, 0);
    assert_eq!(deaths(&mut store), names(&["plain.md"]));
    run_pass(&mut store, &mut pass, None, DEATH_PRUNE_LEAST_ROWS);
    assert!(deaths(&mut store).is_empty());
}

/// **A pass that finds another fingerprint pinned clears its kept set and
/// starts over**, under the new fingerprint's findings.
#[test]
fn a_fingerprint_change_restarts_the_pass() {
    let scratch = Scratch::new("prune-fingerprint");
    let mut store = scratch.open();
    store
        .begin_request()
        .pin_vault_schema(b"first", "first")
        .unwrap();
    apply(
        &mut store,
        vec![holder("h1.md", "[[one]]\n"), holder("h2.md", "[[two]]\n")],
    );
    died(&mut store, "x/one.md");
    died(&mut store, "x/two.md");
    died(&mut store, "plain.md");
    let mut pass = DeathPrunePass::new();
    let first = store
        .begin_request()
        .death_prune_unit(&mut pass, None, DEATH_PRUNE_LEAST_ROWS)
        .unwrap();
    assert_eq!(first.ran, DeathPrunePhase::Marking);
    assert_eq!(pass.kept_rows(), 1);

    store
        .begin_request()
        .pin_vault_schema(b"second", "second")
        .unwrap();
    store
        .begin_request()
        .judge_link_health_once(&ContentModel::under("second"), &mut || {})
        .unwrap();
    let restart = store
        .begin_request()
        .death_prune_unit(&mut pass, None, DEATH_PRUNE_UNIT_ROWS)
        .unwrap();
    assert!(restart.restarted);
    assert_eq!(restart.ran, DeathPrunePhase::Clearing);
    assert_eq!(restart.work.kept_rows_cleared, 1);
    run_pass(&mut store, &mut pass, None, DEATH_PRUNE_UNIT_ROWS);
    assert_eq!(deaths(&mut store), names(&["x/one.md", "x/two.md"]));
}

/// **A pass never deletes against a kept set it did not fill.** A pass handed
/// to another store finds its token missing there and starts over, and a pass
/// that ends clears its kept set, so the next pass begins with none.
#[test]
fn a_pass_whose_kept_set_is_gone_starts_over() {
    let first_scratch = Scratch::new("prune-token-first");
    let mut first = first_scratch.open();
    let second_scratch = Scratch::new("prune-token-second");
    let mut second = second_scratch.open();
    for store in [&mut first, &mut second] {
        apply(store, vec![holder("h.md", "[[kept]]\n")]);
        died(store, "x/kept.md");
        died(store, "plain.md");
    }
    let mut pass = DeathPrunePass::new();
    while pass.phase() != DeathPrunePhase::Removing {
        first
            .begin_request()
            .death_prune_unit(&mut pass, None, DEATH_PRUNE_UNIT_ROWS)
            .unwrap();
    }
    let moved = second
        .begin_request()
        .death_prune_unit(&mut pass, None, DEATH_PRUNE_UNIT_ROWS)
        .unwrap();
    assert!(moved.restarted);
    assert_eq!(moved.ran, DeathPrunePhase::Marking);
    run_pass(&mut second, &mut pass, None, DEATH_PRUNE_UNIT_ROWS);
    assert_eq!(deaths(&mut second), names(&["x/kept.md"]));

    // The first store holds the abandoned pass's kept set; the next pass
    // there clears it before it begins.
    let mut next = DeathPrunePass::new();
    let units = run_pass(&mut first, &mut next, None, DEATH_PRUNE_UNIT_ROWS);
    assert_eq!(units[0].ran, DeathPrunePhase::Clearing);
    assert_eq!(units[0].work.kept_rows_cleared, 1);
    assert_eq!(deaths(&mut first), names(&["x/kept.md"]));
    assert_eq!(next.kept_rows(), 0);
    assert!(next.is_finished());

    // A unit run on a finished pass begins the next one.
    let again = first
        .begin_request()
        .death_prune_unit(&mut next, None, DEATH_PRUNE_UNIT_ROWS)
        .unwrap();
    assert_eq!(again.ran, DeathPrunePhase::Marking);
    assert!(!again.restarted);
}

/// **A unit is refused below the least row bound, while link health is
/// deferred, and during a from-zero build.**
#[test]
fn a_unit_is_refused_below_its_least_bound() {
    let scratch = Scratch::new("prune-refused");
    let mut store = scratch.open();
    let refused = store.begin_request().death_prune_unit(
        &mut DeathPrunePass::new(),
        None,
        DEATH_PRUNE_LEAST_ROWS - 1,
    );
    assert!(
        matches!(refused, Err(norn_store::StoreError::Bound { .. })),
        "{refused:?}"
    );

    let scratch = Scratch::new("prune-refused-building");
    let mut building = scratch.open();
    building.defer_link_health().expect("deferring link health");
    let refused = building.begin_request().death_prune_unit(
        &mut DeathPrunePass::new(),
        None,
        DEATH_PRUNE_UNIT_ROWS,
    );
    assert!(
        matches!(refused, Err(norn_store::StoreError::Lifecycle { .. })),
        "a unit ran while link health was deferred: {refused:?}"
    );
    building.begin_steps().expect("beginning a from-zero build");
    let refused = building.begin_request().death_prune_unit(
        &mut DeathPrunePass::new(),
        None,
        DEATH_PRUNE_UNIT_ROWS,
    );
    assert!(
        matches!(refused, Err(norn_store::StoreError::Lifecycle { .. })),
        "a unit ran during a from-zero build: {refused:?}"
    );
}

// ---- the due marks ----

/// **The marks move with exactly what makes a pass due.** A death moves the
/// newest death, a broken finding filed moves the newest broken finding, one
/// discarded with none filed lowers the count, and a write that does neither
/// moves only the write generation; a pass moves none of them.
#[test]
fn the_due_marks_move_with_deaths_and_broken_findings_alone() {
    let scratch = Scratch::new("prune-marks");
    let mut store = scratch.open();
    let empty = store.begin_request().death_prune_marks().expect("marks");
    assert_eq!(empty.newest_death, None);
    assert_eq!(empty.newest_broken, None);
    assert_eq!(empty.broken_findings, 0);

    let broke = apply(&mut store, vec![holder("h.md", "[[gone]]\n")]);
    let marks = store.begin_request().death_prune_marks().expect("marks");
    let filed = marks
        .newest_broken
        .expect("the broken finding's generation");
    assert!(filed >= broke && filed <= marks.write_generation);
    assert_eq!(marks.broken_findings, 1);
    assert_eq!(marks.newest_death, None);

    let died_at = died(&mut store, "x/other.md");
    let marks = store.begin_request().death_prune_marks().expect("marks");
    assert_eq!(marks.newest_death, Some(died_at));
    assert_eq!(marks.newest_broken, Some(filed), "a death broke no link");
    assert_eq!(marks.broken_findings, 1);

    let edit = apply(&mut store, vec![plain("unrelated.md")]);
    let edited = store.begin_request().death_prune_marks().expect("marks");
    assert_eq!(edited.write_generation, edit);
    assert_eq!(
        (
            edited.newest_death,
            edited.newest_broken,
            edited.broken_findings
        ),
        (
            marks.newest_death,
            marks.newest_broken,
            marks.broken_findings
        ),
        "a write that neither records a death nor moves a broken finding moves no mark"
    );

    prune(&mut store, None);
    let pruned = store.begin_request().death_prune_marks().expect("marks");
    assert_eq!(
        pruned.write_generation, edited.write_generation,
        "a pass takes no generation"
    );

    apply(&mut store, vec![holder("h.md", "nothing linked\n")]);
    let healed = store.begin_request().death_prune_marks().expect("marks");
    assert_eq!(healed.broken_findings, 0, "the discard lowers the count");
    assert_eq!(healed.newest_broken, None);
}

// ---- plan bars ----

fn plan(emitted: norn_store::EmittedPlan) -> QueryPlan {
    QueryPlan::new(
        emitted.sql,
        emitted
            .steps
            .into_iter()
            .map(|step| PlanRow::new(step.id, step.parent, step.detail))
            .collect(),
    )
}

/// Every named shape has a slot and a bar below.
#[test]
fn the_death_prune_bars_cover_every_statement() {
    let mut slots: Vec<usize> = DeathPruneStatement::all()
        .into_iter()
        .map(DeathPruneStatement::slot)
        .collect();
    slots.sort_unstable();
    assert_eq!(slots, (0..DEATH_PRUNE_STATEMENTS).collect::<Vec<_>>());
}

/// **Every phase continues by an exclusive keyset seek, and none sorts.**
/// Marking seeks `findings_broken_filed` past its cursor under the pinned
/// fingerprint and reaches each finding's link by key; the walk and the
/// removal seek `tombstones_change_feed` past theirs, the walk descending,
/// and the removal probes the kept set by its key. Each write seeks the row
/// it writes. The live-document count is the one statement that steps a
/// whole index, and it reads an index rather than the table.
#[test]
fn every_death_prune_statement_seeks_its_index_and_sorts_nothing() {
    let scratch = Scratch::new("prune-plans");
    let mut store = scratch.open();
    apply(&mut store, vec![holder("h.md", "[[gone]]\n")]);
    died(&mut store, "x/gone.md");
    let request = store.begin_request();
    for statement in DeathPruneStatement::all() {
        let plan = plan(request.death_prune_plan(statement).expect("a plan"));
        plan.assert_no_temp_btree();
        match statement {
            // The clear walks the kept set from its first key and stops at
            // its limit: every row it passes is one it deletes, since the
            // rows before them were deleted by the clears before it.
            DeathPruneStatement::ClearKept => {
                for row in plan.rows() {
                    assert!(
                        row.detail == "SCAN temp.death_prune_kept"
                            || row
                                .detail
                                .starts_with("SEARCH temp.death_prune_kept USING PRIMARY KEY")
                            || !row.detail.starts_with("S"),
                        "the clear reads more than the kept set's head: {:?}",
                        plan.rows()
                    );
                }
                assert!(plan.sql().contains("ORDER BY path LIMIT ?1"));
                continue;
            }
            DeathPruneStatement::LiveDocuments => {
                assert!(
                    plan.rows()
                        .iter()
                        .all(|row| row.detail.contains("COVERING INDEX")),
                    "the count reads the table: {:?}",
                    plan.rows()
                );
                continue;
            }
            DeathPruneStatement::RaiseFloor => {
                // An upsert of `meta`'s primary key reports no search.
                plan.assert_no_full_scan();
                continue;
            }
            _ => {}
        }
        plan.assert_no_full_scan();
        plan.assert_no_table_scan();
        match statement {
            DeathPruneStatement::BrokenFiled => {
                plan.assert_searches_through("findings", Access::Index("findings_broken_filed"));
                plan.assert_search_constraint(
                    "findings",
                    "(vault_schema_fingerprint=? AND (generation,path,position)>(?,?,?))",
                );
                plan.assert_searches_through("documents", Access::Index("documents_path"));
                plan.assert_searches_through("links", Access::Index("links_document_ordinal"));
            }
            DeathPruneStatement::WalkDeaths => {
                plan.assert_searches_through("tombstones", Access::Index("tombstones_change_feed"));
                plan.assert_search_constraint("tombstones", "((generation,path)<(?,?))");
            }
            DeathPruneStatement::RemovalPage => {
                plan.assert_searches_through("tombstones", Access::Index("tombstones_change_feed"));
                plan.assert_search_constraint(
                    "tombstones",
                    "((generation,path)>(?,?) AND generation<?)",
                );
                plan.assert_searches_through("death_prune_kept", Access::PrimaryKey);
            }
            DeathPruneStatement::RemoveDeath => {
                plan.assert_searches_through("tombstones", Access::Index("tombstones_path"));
            }
            DeathPruneStatement::KeepDeath => {}
            // A due check's `max` is answered by one seek of the index's
            // next column, and its count steps the broken findings alone.
            DeathPruneStatement::NewestDeath => {
                plan.assert_searches_through("tombstones", Access::Index("tombstones_change_feed"));
            }
            DeathPruneStatement::NewestBroken => {
                plan.assert_searches_through("findings", Access::Index("findings_broken_filed"));
                plan.assert_search_constraint("findings", "(vault_schema_fingerprint=?)");
            }
            // Either index over the fingerprint and the kind steps the broken
            // findings alone; the planner takes the wider one.
            DeathPruneStatement::BrokenCount => {
                plan.assert_searches_through(
                    "findings",
                    Access::Index("findings_fingerprint_kind_nocase"),
                );
                plan.assert_search_constraint(
                    "findings",
                    "(vault_schema_fingerprint=? AND kind=?)",
                );
            }
            DeathPruneStatement::LiveDocuments
            | DeathPruneStatement::RaiseFloor
            | DeathPruneStatement::ClearKept => unreachable!(),
        }
    }
}

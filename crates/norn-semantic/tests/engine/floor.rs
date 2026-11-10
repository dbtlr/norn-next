//! The death floor ADR 0052 rules for lane 2: a reader that has not read
//! through the floor reconciles against current state instead of seeking, a
//! reconcile and a census recount record the G0 their first page read and
//! never start over because of the floor, and each reader's read-through
//! generation comes only from what it recorded in the store epoch.
//!
//! Every floor here is raised by the store's own death prune, handed the
//! lower of the two readers' read-through generations as the host hands it,
//! except where a case stands for a vault whose engine was turned off while
//! the prune ran, which hands it no read-through generation at all.

use std::sync::Arc;

use crate::common::{CountingEmbedder, Scratch, delete_all, recompute, write_all};
use norn_semantic::{
    Coverage, DrainReport, DrainStep, Engine, ReadThroughGeneration, ReadThroughs,
};
use norn_store::{DEATH_PRUNE_UNIT_ROWS, DeathPrunePass, MAX_PAGE, ReadThrough, Store};

// ---- fixtures ----

/// `count` paths `{prefix}/0000.md`, `{prefix}/0001.md`, … from `from`.
fn numbered(prefix: &str, from: usize, count: usize) -> Vec<String> {
    (from..from + count)
        .map(|serial| format!("{prefix}/{serial:04}.md"))
        .collect()
}

/// Run one whole death-prune pass handed `read_through`, and answer the floor
/// it leaves.
fn prune_with(store: &mut Store, read_through: Option<ReadThrough>) -> Option<i64> {
    let mut pass = DeathPrunePass::new();
    for _ in 0..10_000 {
        let unit = store
            .begin_request()
            .death_prune_unit(&mut pass, read_through, DEATH_PRUNE_UNIT_ROWS)
            .expect("a death-prune unit");
        if unit.finished {
            return floor(store);
        }
    }
    panic!("the pass never finished");
}

/// One pass handed what the host hands it for `engine`: the lower of its two
/// readers' read-through generations in the store's epoch.
fn prune_for(store: &mut Store, engine: &Engine) -> Option<i64> {
    // The mapping the host makes: a reader that stands hands its lower
    // generation, or nothing read where it recorded none in this epoch.
    let read_through = match engine.read_through().lower_in(store.epoch()) {
        Some(generation) => ReadThrough::Through(generation),
        None => ReadThrough::Nothing,
    };
    prune_with(store, Some(read_through))
}

fn floor(store: &mut Store) -> Option<i64> {
    store
        .begin_request()
        .death_floor()
        .expect("the death floor")
}

fn generation(store: &mut Store) -> i64 {
    store
        .begin_request()
        .write_generation()
        .expect("the write generation")
}

/// The paths of every death the store holds.
fn deaths(store: &mut Store) -> Vec<String> {
    store
        .begin_request()
        .changed_tombstones_after(None, MAX_PAGE)
        .expect("the death feed")
        .into_iter()
        .map(|(_, death)| death.path.as_str().to_string())
        .collect()
}

/// One step of the engine, embedded and committed where it embeds.
fn step(engine: &mut Engine, store: &mut Store) -> DrainReport {
    match engine.step(&mut store.feed_read()).expect("a step") {
        DrainStep::Done(report) => report,
        DrainStep::Embed(pending) => engine
            .commit(pending.embed().expect("an embedding"))
            .expect("a commit"),
    }
}

/// Drain until a drain owes nothing, and answer what the drains did together.
fn settle(engine: &mut Engine, store: &mut Store) -> DrainReport {
    engine.drain(&mut store.feed_read()).expect("a drain")
}

/// A reader's read-through generation as `engine` exposes it.
fn through(store: &Store, generation: i64) -> Option<ReadThroughGeneration> {
    Some(ReadThroughGeneration {
        store_epoch: store.epoch().to_string(),
        generation,
    })
}

/// Whether the engine has not read through the floor of the store as it
/// stands.
fn below_floor(engine: &Engine, store: &mut Store) -> bool {
    let reading = store.feed_read().store_reading().expect("a store reading");
    engine.read_through().engine_below_floor(&reading)
}

/// The counts a from-zero recompute gives over `store` as it stands and the
/// rows `engine` holds.
fn recounted(store: &mut Store, engine: &Engine) -> Coverage {
    let readable = recompute(store, &norn_embed::StubEmbedder::new());
    let held = engine.projection().expect("a projection");
    let embedded = readable
        .iter()
        .filter(|row| {
            held.iter()
                .any(|kept| kept.path == row.path && kept.input_hash == row.input_hash)
        })
        .count();
    Coverage {
        embedded: embedded as u64,
        total: readable.len() as u64,
    }
}

/// A sidecar settled over 600 documents, 100 under `a/` and 500 under `z/`,
/// and then a changeset that deletes every `z/` document the engine has not
/// read, pruned as the host prunes it: 100 live documents bound what lane 2
/// keeps, so 400 deaths go unread and the floor rises to that changeset's
/// generation, which the engine's read-through stands below.
///
/// The sidecar holds 600 paths, more than two reconcile pages, so the
/// reconcile it owes spans three steps.
struct Lagging {
    scratch: Scratch,
    store: Store,
    engine: Engine,
    embedder: Arc<CountingEmbedder>,
    /// The generation the engine had drained through before the deletion.
    drained: i64,
    /// The deletion's generation, which the floor stands at.
    deleted: i64,
}

fn lagging(label: &str) -> Lagging {
    let scratch = Scratch::new(label);
    let mut store = scratch.store();
    write_all(&mut store, &numbered("a", 0, 100));
    let drained = write_all(&mut store, &numbered("z", 0, 500));
    let embedder = CountingEmbedder::new();
    let mut engine = scratch.engine(embedder.clone());
    settle(&mut engine, &mut store);
    assert_eq!(embedder.calls(), 600);
    assert_eq!(
        engine.read_through().engine_in(store.epoch()),
        Some(drained)
    );

    let deleted = delete_all(&mut store, &numbered("z", 0, 500));
    assert_eq!(prune_for(&mut store, &engine), Some(deleted));
    assert_eq!(
        deaths(&mut store).len(),
        100,
        "the bound keeps one per live document"
    );
    assert!(below_floor(&engine, &mut store));
    Lagging {
        scratch,
        store,
        engine,
        embedder,
        drained,
        deleted,
    }
}

// ---- the drain below the floor ----

/// **A drain below the floor reconciles, and re-embeds nothing whose content
/// the sidecar holds.** The vectors held for deleted paths whose deaths the
/// prune removed unread are reconciled away, as is every other path the
/// store no longer holds, and the documents that still stand are triaged
/// current by fingerprint.
#[test]
fn a_drain_below_the_floor_reconciles_away_deaths_pruned_unread() {
    let scratch = Scratch::new("floor-reconcile");
    let mut store = scratch.store();
    write_all(&mut store, &numbered("docs", 0, 4));
    let embedder = CountingEmbedder::new();
    let mut engine = scratch.engine(embedder.clone());
    settle(&mut engine, &mut store);
    assert_eq!(embedder.calls(), 4);

    let deleted = delete_all(&mut store, &numbered("docs", 0, 3));
    assert_eq!(prune_for(&mut store, &engine), Some(deleted));
    assert_eq!(
        deaths(&mut store),
        numbered("docs", 2, 1),
        "one live document keeps one death"
    );

    let report = settle(&mut engine, &mut store);
    assert!(report.rescan, "{report:?}");
    assert_eq!(report.reconciles, 1, "{report:?}");
    assert_eq!(report.reconciled_away, 3, "{report:?}");
    assert_eq!(report.skipped_current, 1, "{report:?}");
    assert_eq!(
        embedder.calls(),
        4,
        "nothing the sidecar holds is embedded again"
    );
    assert_eq!(
        engine.projection().expect("a projection"),
        recompute(&mut store, &norn_embed::StubEmbedder::new())
    );
    assert!(!below_floor(&engine, &mut store));
}

/// **A reader within the bound never falls below the floor.** Deaths the
/// engine read are removed and raise the floor, and the engine, which has
/// read through it, seeks on without reconciling.
#[test]
fn a_floor_the_engine_has_read_through_reconciles_nothing() {
    let scratch = Scratch::new("floor-read");
    let mut store = scratch.store();
    write_all(&mut store, &numbered("docs", 0, 4));
    let mut engine = scratch.engine(CountingEmbedder::new());
    settle(&mut engine, &mut store);
    let deleted = delete_all(&mut store, &numbered("docs", 0, 3));
    settle(&mut engine, &mut store);

    assert_eq!(prune_for(&mut store, &engine), Some(deleted));
    assert!(deaths(&mut store).is_empty(), "every death was read");
    assert!(!below_floor(&engine, &mut store));
    let report = settle(&mut engine, &mut store);
    assert!(report.is_settled(), "{report:?}");
}

// ---- a quiet store needs exactly one reconcile ----

/// **In a quiet store whose floor stands at exactly the current write
/// generation, the engine reconciles once and its census counts once, and
/// neither repeats.** One changeset deletes most of the vault, so the prune's
/// cutoff lands inside that changeset's generation; each reader's G0 is that
/// generation, and a floor at G0 is one it has read through.
#[test]
fn a_quiet_store_whose_floor_is_the_write_generation_reconciles_once() {
    let scratch = Scratch::new("floor-quiet");
    let mut store = scratch.store();
    write_all(&mut store, &numbered("docs", 0, 10));
    let mut engine = scratch.engine(CountingEmbedder::new());
    settle(&mut engine, &mut store);

    let deleted = delete_all(&mut store, &numbered("docs", 0, 7));
    assert_eq!(prune_for(&mut store, &engine), Some(deleted));
    assert_eq!(
        generation(&mut store),
        deleted,
        "a death-prune unit takes no generation"
    );

    let report = settle(&mut engine, &mut store);
    assert_eq!((report.reconciles, report.recounts), (1, 1), "{report:?}");
    let read_through = engine.read_through();
    assert_eq!(read_through.engine_in(store.epoch()), Some(deleted));
    assert_eq!(read_through.census_in(store.epoch()), Some(deleted));

    for _ in 0..3 {
        let report = settle(&mut engine, &mut store);
        assert!(report.is_settled(), "{report:?}");
    }
    // The prune now removes the deaths both readers read, and the floor it
    // leaves is still one they have read through.
    assert_eq!(prune_for(&mut store, &engine), Some(deleted));
    assert!(deaths(&mut store).is_empty());
    let report = settle(&mut engine, &mut store);
    assert!(report.is_settled(), "{report:?}");
    assert_eq!(
        engine.coverage(),
        Some(Coverage {
            embedded: 3,
            total: 3
        })
    );
}

/// **With no live documents, any one deletion raises the floor to the write
/// generation**, and the engine still reconciles once.
#[test]
fn a_vault_with_no_live_documents_reconciles_once_after_any_deletion() {
    let scratch = Scratch::new("floor-empty");
    let mut store = scratch.store();
    write_all(&mut store, &numbered("docs", 0, 1));
    let mut engine = scratch.engine(CountingEmbedder::new());
    settle(&mut engine, &mut store);

    let deleted = delete_all(&mut store, &numbered("docs", 0, 1));
    assert_eq!(prune_for(&mut store, &engine), Some(deleted));
    let report = settle(&mut engine, &mut store);
    assert_eq!((report.reconciles, report.recounts), (1, 1), "{report:?}");
    assert!(engine.projection().expect("a projection").is_empty());
    let report = settle(&mut engine, &mut store);
    assert!(report.is_settled(), "{report:?}");
}

// ---- a reconcile never starts over, and converges ----

/// **A held path checked live on an early reconcile page, then deleted and
/// its death removed before the reconcile completes, is removed by the next
/// reconcile.** Its death's generation is above G0, so the floor ends above
/// the G0 the completing reconcile records, and the engine reconciles again.
#[test]
fn a_path_checked_live_and_pruned_before_completion_is_removed_by_the_next_reconcile() {
    let Lagging {
        mut store,
        mut engine,
        embedder,
        drained,
        deleted,
        ..
    } = lagging("floor-round-one");
    let first = step(&mut engine, &mut store);
    assert_eq!((first.reconciles, first.recounts), (1, 1), "{first:?}");
    let checked = "a/0050.md".to_string();
    assert!(
        engine
            .projection()
            .unwrap()
            .iter()
            .any(|row| row.path == checked)
    );

    // The checked path dies, and enough later deaths pass the bound that
    // the prune removes its death unread.
    let died = delete_all(&mut store, std::slice::from_ref(&checked));
    write_all(&mut store, &numbered("m", 0, 200));
    let later = delete_all(&mut store, &numbered("m", 0, 200));
    assert_eq!(
        engine.read_through().engine_in(store.epoch()),
        Some(drained),
        "a running reconcile's death cursor raises nothing"
    );
    assert_eq!(prune_for(&mut store, &engine), Some(later));
    assert!(
        !deaths(&mut store).contains(&checked),
        "the death went unread"
    );
    assert!(died > deleted);

    // The reconcile completes without starting over and records its G0,
    // which the floor now stands above.
    let mut steps = 0;
    while engine.read_through().engine_in(store.epoch()) == Some(drained) {
        let report = step(&mut engine, &mut store);
        assert_eq!(
            report.reconciles, 0,
            "the pass is not started over: {report:?}"
        );
        steps += 1;
        assert!(steps < 10, "the reconcile never completed");
    }
    assert_eq!(
        engine.read_through().engine_in(store.epoch()),
        Some(deleted),
        "the completed reconcile records G0 itself"
    );
    assert!(
        engine
            .projection()
            .unwrap()
            .iter()
            .any(|row| row.path == checked)
    );
    assert!(below_floor(&engine, &mut store));

    let report = settle(&mut engine, &mut store);
    assert_eq!(report.reconciles, 1, "{report:?}");
    assert!(
        !engine
            .projection()
            .unwrap()
            .iter()
            .any(|row| row.path == checked)
    );
    assert_eq!(
        engine.projection().expect("a projection"),
        recompute(&mut store, &norn_embed::StubEmbedder::new())
    );
    assert!(settle(&mut engine, &mut store).is_settled());
    assert_eq!(embedder.calls(), 600, "the reconciles re-embedded nothing");
}

/// **Deletions continuing during a reconcile converge.** Each burst passes the
/// bound and raises the floor while the first reconcile runs; it is never
/// started over, it completes below the floor, and one more reconcile, run
/// once the deletions stop, leaves the engine read through the floor.
#[test]
fn deletions_during_a_reconcile_converge_without_starting_it_over() {
    let Lagging {
        mut store,
        mut engine,
        embedder,
        deleted,
        ..
    } = lagging("floor-converge");
    let mut reconciles = step(&mut engine, &mut store).reconciles;
    let mut floors = vec![floor(&mut store)];
    for burst in 0..2 {
        let burst = numbered(&format!("m{burst}"), 0, 200);
        write_all(&mut store, &burst);
        delete_all(&mut store, &burst);
        floors.push(prune_for(&mut store, &engine));
        let report = step(&mut engine, &mut store);
        assert_eq!(
            report.reconciles, 0,
            "the running pass is resumed: {report:?}"
        );
        reconciles += report.reconciles;
    }
    assert!(
        floors.windows(2).all(|pair| pair[0] < pair[1]),
        "{floors:?}"
    );
    assert_eq!(
        engine.read_through().engine_in(store.epoch()),
        Some(deleted),
        "the first reconcile completed and recorded its own G0"
    );

    let report = settle(&mut engine, &mut store);
    reconciles += report.reconciles;
    assert_eq!(reconciles, 2, "{report:?}");
    assert!(!below_floor(&engine, &mut store));
    assert!(settle(&mut engine, &mut store).is_settled());
    assert_eq!(
        engine.projection().expect("a projection"),
        recompute(&mut store, &norn_embed::StubEmbedder::new())
    );
    assert_eq!(embedder.calls(), 600);
}

/// **A reconcile stopped by a restart resumes with its stored G0 and its
/// frozen read-through generation.** An engine opened afresh part way
/// through reports the read-through the reconcile began with, resumes the
/// pass rather than beginning one, and on completing records the G0 its
/// first page read, not the write generation the store has reached since.
#[test]
fn a_reconcile_interrupted_by_a_restart_resumes_with_its_g0() {
    let Lagging {
        scratch,
        mut store,
        mut engine,
        embedder,
        drained,
        deleted,
    } = lagging("floor-restart");
    let first = step(&mut engine, &mut store);
    assert_eq!(first.reconciles, 1);
    let frozen = engine.read_through().clone();
    assert_eq!(
        frozen.engine,
        through(&store, drained),
        "frozen at what the records said"
    );

    let moved = write_all(&mut store, &numbered("late", 0, 1));
    assert!(moved > deleted);
    drop(engine);
    let mut engine = scratch.engine(embedder.clone());
    assert_eq!(
        engine.read_through(),
        &frozen,
        "the restart resumes with the stored value"
    );

    let mut steps = 0;
    while engine.read_through().engine == frozen.engine {
        let report = step(&mut engine, &mut store);
        assert_eq!(report.reconciles, 0, "the pass resumes: {report:?}");
        steps += 1;
        assert!(steps < 10, "the reconcile never completed");
    }
    assert_eq!(engine.read_through().engine, through(&store, deleted));
    let report = settle(&mut engine, &mut store);
    assert_eq!(report.reconciles, 0, "{report:?}");
    assert_eq!(engine.read_through().engine_in(store.epoch()), Some(moved));
}

// ---- the census below the floor ----

/// More documents than one census page, so a count takes several steps.
const CENSUS_SPAN: usize = MAX_PAGE + 76;

/// **A census below the floor reports its last whole counts until its
/// recount is whole, a second recount included, and a census over a new
/// store epoch reports none.** The floor is raised by a prune that ran while
/// the vault's engine was turned off, so it was handed no read-through.
#[test]
fn a_census_below_the_floor_keeps_its_last_whole_counts_until_whole() {
    let scratch = Scratch::new("floor-census-whole");
    let mut store = scratch.store();
    let paths = numbered("c", 0, CENSUS_SPAN);
    write_all(&mut store, &paths);
    let mut engine = scratch.engine(CountingEmbedder::new());
    settle(&mut engine, &mut store);
    let whole = Coverage {
        embedded: CENSUS_SPAN as u64,
        total: CENSUS_SPAN as u64,
    };
    assert_eq!(engine.coverage(), Some(whole));

    delete_all(&mut store, &paths[..5]);
    let first_floor = prune_with(&mut store, None).expect("a floor");
    let first = step(&mut engine, &mut store);
    assert_eq!(first.recounts, 1, "{first:?}");
    assert_eq!(
        engine.coverage(),
        Some(whole),
        "the last whole counts stand"
    );

    // The floor passes the recount's G0 before it completes: it counts
    // again, keeping the same last whole counts across both.
    delete_all(&mut store, &paths[CENSUS_SPAN - 5..]);
    let second_floor = prune_with(&mut store, None).expect("a floor");
    assert!(second_floor > first_floor);
    let mut recounts = 0;
    loop {
        let report = step(&mut engine, &mut store);
        recounts += report.recounts;
        if engine.coverage() != Some(whole) {
            break;
        }
        assert!(recounts <= 1, "{report:?}");
    }
    assert_eq!(
        recounts, 1,
        "the recount below a passed floor counts again once"
    );
    let report = settle(&mut engine, &mut store);
    assert_eq!(report.recounts, 0, "{report:?}");
    assert_eq!(engine.coverage(), Some(recounted(&mut store, &engine)));
    assert_eq!(
        engine.read_through().census_in(store.epoch()),
        Some(second_floor)
    );

    // A new store epoch clears them: no count is carried across databases.
    let mut store = store
        .discard_and_reopen(norn_store::StoredPathOrder::Sensitive)
        .expect("a store rebuilt from zero");
    write_all(&mut store, &paths);
    step(&mut engine, &mut store);
    assert_eq!(engine.coverage(), None);
    settle(&mut engine, &mut store);
    assert_eq!(engine.coverage(), Some(whole));
}

/// **A recount's death cursor raises no read-through generation, and a recount
/// that counts a document live and then meets its pruned death past its
/// cursor counts again.** The recount begins on a new sidecar over a store
/// that already holds a floor, with G0 = g; it counts P live on its first
/// page, and reads the one death standing; P dies at g + 1 with most of the vault, and the prune removes P's
/// death unread and raises the floor past G0 while the census's death cursor
/// reads on past where that death stood. The recount records its G0 alone,
/// counts again, and P's row does not survive.
#[test]
fn a_recount_that_meets_a_pruned_death_past_its_cursor_counts_again() {
    let scratch = Scratch::new("floor-census-sequence");
    let mut store = scratch.store();
    let counted = "a/p.md".to_string();
    let rest = numbered("c", 0, CENSUS_SPAN);
    write_all(&mut store, std::slice::from_ref(&counted));
    write_all(&mut store, &rest);
    write_all(&mut store, &numbered("x", 0, 1));
    delete_all(&mut store, &numbered("x", 0, 1));
    let floor = prune_with(&mut store, None).expect("a floor");
    // A death after the floor, which the recount's first page reads.
    write_all(&mut store, &numbered("y", 0, 1));
    let g0 = delete_all(&mut store, &numbered("y", 0, 1));
    assert!(g0 > floor);

    let mut engine = scratch.engine(CountingEmbedder::new());
    let first = step(&mut engine, &mut store);
    assert_eq!(
        first.recounts, 1,
        "a census that has read nothing is below the floor"
    );
    assert_eq!(
        engine.read_through().census,
        None,
        "the recount's death cursor read a death and raised nothing: it began with no records"
    );

    let mut doomed = vec![counted.clone()];
    doomed.extend_from_slice(&rest[..1000]);
    let died = delete_all(&mut store, &doomed);
    assert_eq!(died, g0 + 1);
    assert_eq!(prune_for(&mut store, &engine), Some(died));
    assert!(
        !deaths(&mut store).contains(&counted),
        "P's death went unread"
    );

    // The recount reaches the end of both feeds below the floor: it records
    // G0 alone and counts again.
    let mut recounts = 0;
    let mut steps = 0;
    while engine.read_through().census.is_none() {
        recounts += step(&mut engine, &mut store).recounts;
        steps += 1;
        assert!(steps < 10, "the recount never completed");
    }
    assert_eq!(engine.read_through().census, through(&store, g0));
    assert_eq!(recounts, 0);
    let report = settle(&mut engine, &mut store);
    assert_eq!(report.recounts, 1, "{report:?}");
    assert_eq!(engine.read_through().census, through(&store, died));
    let coverage = engine.coverage().expect("whole counts");
    assert_eq!(
        coverage.total,
        (CENSUS_SPAN - 1000) as u64,
        "P's row did not survive"
    );
    assert_eq!(coverage, recounted(&mut store, &engine));
}

// ---- the read-through generations exposed ----

/// **A reader that has recorded nothing has read nothing, and a completed
/// drain and a completed count each carry the read-through over the newest
/// generation's deaths.** A death cursor alone would stop one below it.
#[test]
fn the_read_through_generations_come_from_what_each_reader_recorded() {
    let scratch = Scratch::new("floor-read-through");
    let mut store = scratch.store();
    let mut engine = scratch.engine(CountingEmbedder::new());
    assert_eq!(engine.read_through(), &ReadThroughs::default());
    assert_eq!(engine.read_through().lower_in(store.epoch()), None);

    let paths = numbered("docs", 0, 300);
    let written = write_all(&mut store, &paths);
    settle(&mut engine, &mut store);
    let read_through = engine.read_through();
    assert_eq!(read_through.engine, through(&store, written));
    assert_eq!(read_through.census, through(&store, written));
    assert_eq!(read_through.lower_in("another-epoch"), None);

    // Mid-backlog: one tombstone page stops inside the second changeset's
    // deaths, so the drain has read through the generation before its
    // cursor's own, above its watermark; the census read every death in one
    // page and completed.
    let older = delete_all(&mut store, &paths[..150]);
    let newer = delete_all(&mut store, &paths[150..]);
    step(&mut engine, &mut store);
    assert_eq!(
        engine
            .watermarks()
            .tombstones
            .as_ref()
            .map(|mark| mark.generation),
        Some(written)
    );
    assert_eq!(engine.read_through().engine, through(&store, older));
    assert_eq!(engine.read_through().census, through(&store, newer));
    assert_eq!(engine.read_through().lower_in(store.epoch()), Some(older));

    settle(&mut engine, &mut store);
    assert_eq!(engine.read_through().engine, through(&store, newer));
    assert_eq!(engine.read_through().lower_in(store.epoch()), Some(newer));

    // A new store epoch is one in which neither has read anything.
    let store = store
        .discard_and_reopen(norn_store::StoredPathOrder::Sensitive)
        .expect("a store rebuilt from zero");
    assert_eq!(engine.read_through().lower_in(store.epoch()), None);
}

//! The bounded reads Layer 5C move-following consumes.

use norn_store::{
    Change, ContentModel, IncrementProvenance, Provenance, Snapshot, Store, StoredPathOrder,
};

use crate::common::{Scratch, document, path, write_documents};

/// Record deaths through the same changeset seam that maintains their keys.
fn deaths(store: &mut Store, entries: &[(&str, Provenance)]) {
    store
        .begin_request()
        .apply_increment(
            IncrementProvenance::Derived,
            entries.iter().map(|(at, provenance)| Change::Death {
                path: path(at),
                provenance: *provenance,
            }),
            &[],
            &ContentModel::none(),
        )
        .expect("recording deaths");
}

fn snapshot(store: &Store) -> Snapshot {
    std::sync::Arc::new(store.open_reader().reader.expect("a reader"))
        .try_take()
        .expect("an idle reader")
        .establish()
        .expect("a snapshot")
}

/// A suffix read carries the death's path, hash and provenance and stops at two.
#[test]
fn tombstones_by_suffix_return_one_two_or_none() {
    let scratch = Scratch::new("moved-suffix");
    let mut store = scratch.open();
    write_documents(
        &mut store.begin_request(),
        &[document("old/target.md", "hash-target", "body\n")],
    );
    deaths(&mut store, &[("old/target.md", Provenance::WatcherRemoval)]);
    let read = snapshot(&store);
    let found = read.tombstones_by_suffix("target").expect("suffix deaths");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].path, path("old/target.md"));
    assert_eq!(found[0].last_content_hash.as_deref(), Some("hash-target"));
    assert_eq!(found[0].provenance, Provenance::WatcherRemoval);
    assert!(
        read.tombstones_by_suffix("absent")
            .expect("no deaths")
            .is_empty()
    );
    drop(read);
    deaths(
        &mut store,
        &[
            ("two/target.md", Provenance::HealPrune),
            ("three/target.md", Provenance::PlanDelete),
        ],
    );
    assert_eq!(
        snapshot(&store)
            .tombstones_by_suffix("target")
            .expect("bounded deaths")
            .len(),
        2
    );
}

/// Every named shape has a slot and a test bar that judges it.
#[test]
fn the_moved_bars_cover_every_statement() {
    use norn_store::{MOVED_STATEMENTS, MovedStatement};
    let mut slots: Vec<usize> = MovedStatement::all()
        .into_iter()
        .map(MovedStatement::slot)
        .collect();
    slots.sort_unstable();
    assert_eq!(slots, (0..MOVED_STATEMENTS).collect::<Vec<_>>());
    for (slot, statement) in MovedStatement::all().into_iter().enumerate() {
        assert_eq!(statement.slot(), slot);
        let bar = match statement {
            MovedStatement::TombstonesBySuffix => {
                "tombstone_suffix_plans_seek_only_the_roots_partial_index"
            }
            MovedStatement::TombstoneAtPath => "exact_tombstone_plans_seek_the_unique_path_index",
            MovedStatement::LiveHolders => "live_holder_plans_seek_the_hash_index_without_sorting",
        };
        assert!(!bar.is_empty());
    }
}

/// The snapshot's proven root order chooses the suffix key space.
#[test]
fn tombstone_suffix_reads_use_the_roots_raw_or_folded_key() {
    for order in [
        StoredPathOrder::Sensitive,
        StoredPathOrder::AsciiCaseInsensitive,
    ] {
        let scratch = Scratch::new("moved-root-key");
        let mut store = scratch.open_under(order);
        deaths(&mut store, &[("Old/Target.md", Provenance::PlanDelete)]);
        let read = snapshot(&store);
        assert_eq!(
            read.tombstones_by_suffix("Target")
                .expect("exact case")
                .len(),
            1
        );
        assert_eq!(
            read.tombstones_by_suffix("target")
                .expect("other case")
                .len(),
            usize::from(order == StoredPathOrder::AsciiCaseInsensitive)
        );
    }
}

/// The exact path names one recorded death or none, by stored spelling.
#[test]
fn tombstone_at_path_returns_one_or_none() {
    let scratch = Scratch::new("moved-exact");
    let mut store = scratch.open();
    deaths(&mut store, &[("Old/Target.md", Provenance::HealPrune)]);
    let read = snapshot(&store);
    let found = read
        .tombstone_at_path(&path("Old/Target.md"))
        .expect("one death")
        .expect("present");
    assert_eq!(found.path, path("Old/Target.md"));
    assert_eq!(found.provenance, Provenance::HealPrune);
    assert_eq!(found.last_content_hash, None);
    assert_eq!(
        read.tombstone_at_path(&path("absent.md")).expect("absent"),
        None
    );
    assert_eq!(
        read.tombstone_at_path(&path("old/target.md"))
            .expect("other spelling"),
        None
    );
}

/// A death with no new hash keeps the first death's hash and both suffix keys.
#[test]
fn a_redeath_keeps_the_recorded_hash_and_suffix_keys() {
    for order in [
        StoredPathOrder::Sensitive,
        StoredPathOrder::AsciiCaseInsensitive,
    ] {
        let scratch = Scratch::new("moved-redeath");
        let mut store = scratch.open_under(order);
        write_documents(
            &mut store.begin_request(),
            &[document("Old/Target.md", "first-hash", "body\n")],
        );
        deaths(&mut store, &[("Old/Target.md", Provenance::WatcherRemoval)]);
        deaths(&mut store, &[("Old/Target.md", Provenance::HealPrune)]);
        let read = snapshot(&store);
        let suffix = read.tombstones_by_suffix("Target").expect("suffix death");
        let exact = read
            .tombstone_at_path(&path("Old/Target.md"))
            .expect("exact death")
            .expect("present");
        assert_eq!(suffix, vec![exact.clone()]);
        assert_eq!(exact.last_content_hash.as_deref(), Some("first-hash"));
        assert_eq!(exact.provenance, Provenance::HealPrune);
    }
}

/// Quarantine remains on disk, including where it replaces a plan-delete death.
#[test]
fn quarantine_is_unseen_by_suffix_and_exact_path_even_after_redeath() {
    let scratch = Scratch::new("moved-quarantine");
    let mut store = scratch.open();
    deaths(
        &mut store,
        &[
            ("kept/target.md", Provenance::Quarantine),
            ("old/target.md", Provenance::PlanDelete),
        ],
    );
    let read = snapshot(&store);
    assert_eq!(
        read.tombstones_by_suffix("target")
            .expect("gone files only")
            .iter()
            .map(|death| death.path.clone())
            .collect::<Vec<_>>(),
        vec![path("old/target.md")]
    );
    assert_eq!(
        read.tombstone_at_path(&path("kept/target.md"))
            .expect("quarantine"),
        None
    );
    drop(read);
    deaths(&mut store, &[("old/target.md", Provenance::Quarantine)]);
    let read = snapshot(&store);
    assert!(
        read.tombstones_by_suffix("target")
            .expect("no gone files")
            .is_empty()
    );
    assert_eq!(
        read.tombstone_at_path(&path("old/target.md"))
            .expect("reclassified"),
        None
    );
}

/// The emitted statement is the statement judged by each index bar.
fn plan_of(
    plans: &[norn_store::MovedPlan],
    statement: norn_store::MovedStatement,
) -> norn_testkit::explain::QueryPlan {
    use norn_testkit::explain::{PlanRow, QueryPlan};
    assert_eq!(plans.len(), 1);
    let emitted = &plans[0];
    assert_eq!(
        emitted.statement,
        norn_store::ReadStatement::Moved(statement)
    );
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

/// Each seek uses its declared index, without scans, automatic indexes or sorts.
fn judge_seek(plan: &norn_testkit::explain::QueryPlan, table: &str, index: &str) {
    plan.assert_no_full_scan();
    plan.assert_no_temp_btree();
    assert!(plan.rows().iter().all(|row| !row.uses_automatic_index()));
    crate::find::rows_of(plan, table)
        .assert_searches_through(table, norn_testkit::explain::Access::Index(index));
}

/// Both probe reductions search only the root's partial suffix index.
#[test]
fn tombstone_suffix_plans_seek_only_the_roots_partial_index() {
    for (order, index, other_column, other_index) in [
        (
            StoredPathOrder::Sensitive,
            "tombstones_suffix_key",
            "folded_suffix_key",
            "tombstones_folded_suffix_key",
        ),
        (
            StoredPathOrder::AsciiCaseInsensitive,
            "tombstones_folded_suffix_key",
            "suffix_key",
            "tombstones_suffix_key",
        ),
    ] {
        let scratch = Scratch::new("moved-suffix-plans");
        let mut store = scratch.open_under(order);
        deaths(
            &mut store,
            &[
                ("old/Target.md", Provenance::HealPrune),
                ("old/Target.md.md", Provenance::PlanDelete),
            ],
        );
        let read = snapshot(&store);
        for target in ["Target", "Target.md"] {
            let plans = read
                .tombstones_by_suffix_plans(target)
                .expect("suffix plans");
            let plan = plan_of(&plans, norn_store::MovedStatement::TombstonesBySuffix);
            judge_seek(&plan, "tombstones", index);
            assert!(
                plan.rows()
                    .iter()
                    .all(|row| !row.detail.contains(other_index))
            );
            assert!(
                !plans[0]
                    .plan
                    .reads
                    .iter()
                    .any(|column| column.table == "tombstones" && column.column == other_column)
            );
        }
    }
}

/// The exact-path statement seeks the unique index with no scan or sort.
#[test]
fn exact_tombstone_plans_seek_the_unique_path_index() {
    let scratch = Scratch::new("moved-exact-plans");
    let mut store = scratch.open();
    deaths(&mut store, &[("old/target.md", Provenance::PlanDelete)]);
    let plans = snapshot(&store)
        .tombstone_at_path_plans(&path("old/target.md"))
        .expect("path plans");
    let plan = plan_of(&plans, norn_store::MovedStatement::TombstoneAtPath);
    judge_seek(&plan, "tombstones", "tombstones_path");
}

/// Hash holders are bounded to two and returned in path order, independent of row ids.
#[test]
fn live_holders_return_one_two_in_path_order_or_none() {
    let scratch = Scratch::new("moved-holders");
    let mut store = scratch.open();
    write_documents(
        &mut store.begin_request(),
        &[
            document("one.md", "one-hash", "body\n"),
            document("c.md", "shared-hash", "body\n"),
            document("a.md", "shared-hash", "body\n"),
            document("b.md", "shared-hash", "body\n"),
        ],
    );
    let read = snapshot(&store);
    assert_eq!(
        read.live_holders("one-hash").expect("one holder"),
        vec![path("one.md")]
    );
    assert_eq!(
        read.live_holders("shared-hash").expect("two holders"),
        vec![path("a.md"), path("b.md")]
    );
    assert!(
        read.live_holders("absent-hash")
            .expect("no holder")
            .is_empty()
    );
}

/// The hash index hands holders back in path order without a sorter.
#[test]
fn live_holder_plans_seek_the_hash_index_without_sorting() {
    let scratch = Scratch::new("moved-holder-plans");
    let mut store = scratch.open();
    write_documents(
        &mut store.begin_request(),
        &[document("holder.md", "shared-hash", "body\n")],
    );
    let plans = snapshot(&store)
        .live_holders_plans("shared-hash")
        .expect("holder plans");
    let plan = plan_of(&plans, norn_store::MovedStatement::LiveHolders);
    judge_seek(&plan, "documents", "documents_content_hash");
}

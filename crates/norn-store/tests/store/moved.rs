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
                "tombstone_suffix_reads_seek_the_same_partial_comparison_index_on_every_root"
            }
            MovedStatement::TombstoneAtPath => {
                "tombstone_path_reads_seek_the_same_partial_comparison_index_on_every_root"
            }
            MovedStatement::LiveHolders => "live_holder_plans_seek_the_hash_index_without_sorting",
        };
        assert!(!bar.is_empty());
    }
}

/// Recorded deaths resolve through the same suffix comparison key on both roots.
#[test]
fn tombstone_suffix_reads_use_one_comparison_key_on_every_root() {
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
            1
        );
    }
}

/// Deaths sharing a comparison key are named in original path order.
/// A target ending in `.md` opens both indexed document-name ranges.
#[test]
fn deaths_sharing_a_key_are_named_in_path_order() {
    for (order, spellings, named) in [
        (
            StoredPathOrder::Sensitive,
            ["x/target.md", "x/target.MD", "x/target.Md"],
            ["x/target.MD", "x/target.Md"],
        ),
        (
            StoredPathOrder::AsciiCaseInsensitive,
            ["x/target.md", "x/TARGET.md", "x/Target.md"],
            ["x/TARGET.md", "x/Target.md"],
        ),
    ] {
        for reversed in [false, true] {
            let scratch = Scratch::new("moved-suffix-ties");
            let mut store = scratch.open_under(order);
            let mut entries: Vec<(&str, Provenance)> = spellings
                .iter()
                .map(|at| (*at, Provenance::PlanDelete))
                .collect();
            if reversed {
                entries.reverse();
            }
            deaths(&mut store, &entries);
            let found: Vec<_> = snapshot(&store)
                .tombstones_by_suffix("target")
                .expect("suffix deaths")
                .into_iter()
                .map(|death| death.path)
                .collect();
            assert_eq!(
                found,
                named.map(path).to_vec(),
                "{order:?}, reversed {reversed}"
            );
        }
    }
    for order in [
        StoredPathOrder::Sensitive,
        StoredPathOrder::AsciiCaseInsensitive,
    ] {
        for reversed in [false, true] {
            let scratch = Scratch::new("moved-two-reduction-ties");
            let mut store = scratch.open_under(order);
            let mut entries = [
                ("z/a.md.md", Provenance::PlanDelete),
                ("y/a.md.md", Provenance::PlanDelete),
                ("x/a.md", Provenance::PlanDelete),
                ("w/a.md.md", Provenance::PlanDelete),
                ("v/a.md", Provenance::PlanDelete),
            ];
            if reversed {
                entries.reverse();
            }
            deaths(&mut store, &entries);
            let found: Vec<_> = snapshot(&store)
                .tombstones_by_suffix("a.md")
                .expect("two-reduction deaths")
                .into_iter()
                .map(|death| death.path)
                .collect();
            assert_eq!(
                found,
                vec![path("v/a.md"), path("x/a.md")],
                "{order:?}, reversed {reversed}"
            );
        }
    }
}

/// The partial suffix index excludes quarantine deaths on every root.
#[test]
fn quarantine_deaths_sharing_the_key_do_not_grow_the_suffix_read() {
    let steps = |order: StoredPathOrder, quarantined: usize| {
        let scratch = Scratch::new("moved-quarantine-cost");
        let mut store = scratch.open_under(order);
        let spellings: Vec<String> = (0..quarantined)
            .map(|at| format!("q/{at:04}/target.md"))
            .collect();
        let mut entries: Vec<(&str, Provenance)> = spellings
            .iter()
            .map(|at| (at.as_str(), Provenance::Quarantine))
            .collect();
        entries.push(("zzz/target.md", Provenance::PlanDelete));
        deaths(&mut store, &entries);
        let read = snapshot(&store);
        let found = read.tombstones_by_suffix("target").expect("suffix deaths");
        assert_eq!(found.len(), 1);
        read.counters()
            .readings()
            .find(|(name, _)| *name == "vm_steps")
            .map(|(_, value)| value)
            .expect("a vm_steps reading")
    };
    for order in [
        StoredPathOrder::Sensitive,
        StoredPathOrder::AsciiCaseInsensitive,
    ] {
        assert_eq!(steps(order, 3), steps(order, 600), "{order:?}");
    }
}

/// A complete path key names a recorded death or none. A differently cased
/// target reaches the same death while preserving its original spelling.
#[test]
fn tombstone_at_path_returns_one_or_none() {
    let scratch = Scratch::new("moved-exact");
    let mut store = scratch.open();
    deaths(&mut store, &[("Old/Target.md", Provenance::HealPrune)]);
    let read = snapshot(&store);
    let found = read
        .tombstone_at_path(&path("Old/Target.md"))
        .expect("one death");
    assert_eq!(found.len(), 1);
    let found = &found[0];
    assert_eq!(found.path, path("Old/Target.md"));
    assert_eq!(found.provenance, Provenance::HealPrune);
    assert_eq!(found.last_content_hash, None);
    assert_eq!(
        read.tombstone_at_path(&path("absent.md")).expect("absent"),
        Vec::new()
    );
    assert_eq!(
        read.tombstone_at_path(&path("old/target.md"))
            .expect("other spelling")
            .into_iter()
            .map(|death| death.path)
            .collect::<Vec<_>>(),
        vec![path("Old/Target.md")]
    );
}

/// Comparison-equivalent complete paths reach the same recorded deaths
/// in original path order. Other extensions do not share that path key.
#[test]
fn comparison_equivalent_tombstone_paths_are_read_in_path_order() {
    let scratch = Scratch::new("moved-folded-exact");
    let mut store = scratch.open_under(StoredPathOrder::AsciiCaseInsensitive);
    deaths(
        &mut store,
        &[
            ("Old/Target.md", Provenance::PlanDelete),
            ("Old/Target.txt", Provenance::PlanDelete),
        ],
    );
    let read = snapshot(&store);
    for spelling in ["old/target.md", "OLD/TARGET.MD"] {
        let found = read
            .tombstone_at_path(&path(spelling))
            .expect("folded death");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, path("Old/Target.md"));
    }
    assert!(
        read.tombstone_at_path(&path("absent.md"))
            .expect("absent")
            .is_empty()
    );
    drop(read);
    for reversed in [false, true] {
        let scratch = Scratch::new("moved-folded-exact-ties");
        let mut store = scratch.open_under(StoredPathOrder::AsciiCaseInsensitive);
        deaths(&mut store, &[("x/TARGET.md", Provenance::Quarantine)]);
        let mut entries = [
            ("x/Target.md", Provenance::PlanDelete),
            ("x/target.md", Provenance::WatcherRemoval),
            ("x/target.MD", Provenance::HealPrune),
        ];
        if reversed {
            entries.reverse();
        }
        deaths(&mut store, &entries);
        let found: Vec<_> = snapshot(&store)
            .tombstone_at_path(&path("x/TARGET.MD"))
            .expect("folded deaths")
            .into_iter()
            .map(|death| death.path)
            .collect();
        assert_eq!(
            found,
            vec![path("x/Target.md"), path("x/target.MD")],
            "reversed {reversed}"
        );
    }
}

/// A complete-path comparison seek reads no sibling with another
/// extension, so those siblings do not increase the read's work.
#[test]
fn a_tombstone_path_seek_does_not_read_siblings_with_other_extensions() {
    let steps = |siblings: usize| {
        let scratch = Scratch::new("moved-folded-exact-cost");
        let mut store = scratch.open_under(StoredPathOrder::AsciiCaseInsensitive);
        let spellings: Vec<String> = (0..siblings)
            .map(|at| format!("x/target.e{at:04}"))
            .collect();
        let mut entries: Vec<(&str, Provenance)> = spellings
            .iter()
            .map(|at| (at.as_str(), Provenance::PlanDelete))
            .collect();
        entries.push(("x/Target.md", Provenance::PlanDelete));
        deaths(&mut store, &entries);
        let read = snapshot(&store);
        let found = read
            .tombstone_at_path(&path("X/TARGET.md"))
            .expect("folded death");
        assert_eq!(found.len(), 1);
        read.counters()
            .readings()
            .find(|(name, _)| *name == "vm_steps")
            .map(|(_, value)| value)
            .expect("a vm_steps reading")
    };
    assert_eq!(steps(3), steps(600));
}

/// The partial comparison-path index excludes quarantine deaths before
/// the bounded seek, so their number does not increase its work.
#[test]
fn quarantine_deaths_do_not_grow_a_tombstone_comparison_path_seek() {
    let steps = |quarantined: usize| {
        let scratch = Scratch::new("moved-folded-exact-quarantine-cost");
        let mut store = scratch.open_under(StoredPathOrder::AsciiCaseInsensitive);
        // Each nonzero mask upper-cases a distinct subset of the place's nine
        // letters, so every spelling is a separate death at one folded place.
        let spellings: Vec<String> = (1..=quarantined)
            .map(|mask| {
                let mut letter = 0;
                "x/target.md"
                    .chars()
                    .map(|c| {
                        if !c.is_ascii_alphabetic() {
                            return c;
                        }
                        letter += 1;
                        if mask & (1 << (letter - 1)) == 0 {
                            c
                        } else {
                            c.to_ascii_uppercase()
                        }
                    })
                    .collect()
            })
            .collect();
        let mut entries: Vec<(&str, Provenance)> = spellings
            .iter()
            .map(|at| (at.as_str(), Provenance::Quarantine))
            .collect();
        entries.push(("x/target.md", Provenance::PlanDelete));
        deaths(&mut store, &entries);
        let read = snapshot(&store);
        let found = read
            .tombstone_at_path(&path("X/TARGET.MD"))
            .expect("folded death");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, path("x/target.md"));
        read.counters()
            .readings()
            .find(|(name, _)| *name == "vm_steps")
            .map(|(_, value)| value)
            .expect("a vm_steps reading")
    };
    assert_eq!(steps(3), steps(255));
}

/// A case-variant Markdown link finds its recorded death through the
/// same complete-path comparison key as the indexed document.
#[test]
fn a_case_variant_markdown_link_finds_its_tombstone_by_comparison_key() {
    use crate::health::{body_of, derived};
    use norn_wire::FindingKind;

    let scratch = Scratch::new("moved-folding-markdown");
    let mut store = scratch.open_under(StoredPathOrder::AsciiCaseInsensitive);
    let target = derived("notes/a.md", "moved bytes\n");
    let hash = target.content_hash.clone();
    write_documents(
        &mut store.begin_request(),
        &[
            target,
            derived("src/h.md", &body_of(&["[x](../Notes/A.md)", "[[Notes/A]]"])),
        ],
    );
    let judged = |store: &mut Store| -> Vec<Option<FindingKind>> {
        let findings = store
            .begin_request()
            .judge_link_health(&[path("src/h.md")], &ContentModel::none())
            .expect("judging links");
        let mut kinds = vec![None, None];
        for finding in findings {
            kinds[finding.ordinal.expect("a link finding") as usize] = Some(finding.kind);
        }
        kinds
    };
    assert_eq!(
        judged(&mut store),
        vec![None, None],
        "both links resolve live"
    );
    deaths(&mut store, &[("notes/a.md", Provenance::WatcherRemoval)]);
    write_documents(
        &mut store.begin_request(),
        &[document("archive/a.md", &hash, "moved bytes\n")],
    );
    assert_eq!(
        judged(&mut store),
        vec![Some(FindingKind::Broken), Some(FindingKind::Broken)],
        "both links break"
    );
    let read = snapshot(&store);
    let by_suffix = read.tombstones_by_suffix("Notes/A").expect("suffix death");
    assert_eq!(by_suffix.len(), 1);
    let exact = read
        .tombstone_at_path(&path("Notes/A.md"))
        .expect("exact death");
    assert_eq!(exact, by_suffix);
    assert_eq!(exact[0].path, path("notes/a.md"));
    assert_eq!(exact[0].last_content_hash.as_deref(), Some(hash.as_str()));
    assert_eq!(
        read.live_holders(
            exact[0]
                .last_content_hash
                .as_deref()
                .expect("recorded hash")
        )
        .expect("holders"),
        vec![path("archive/a.md")]
    );
}

/// A repeated death retains its last known hash and normalized suffix and path keys.
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
            .expect("exact death");
        assert_eq!(suffix, exact);
        assert_eq!(exact[0].last_content_hash.as_deref(), Some("first-hash"));
        assert_eq!(exact[0].provenance, Provenance::HealPrune);
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
        Vec::new()
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
        Vec::new()
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

/// Suffix reads seek the partial suffix index, independent of filesystem order.
#[test]
fn tombstone_suffix_reads_seek_the_same_partial_comparison_index_on_every_root() {
    for (order, index, other_column, other_index) in [
        (
            StoredPathOrder::Sensitive,
            "tombstones_suffix_key",
            "path_key",
            "tombstones_path_key",
        ),
        (
            StoredPathOrder::AsciiCaseInsensitive,
            "tombstones_suffix_key",
            "path_key",
            "tombstones_path_key",
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

/// Exact-path reads seek the partial normalized path index on both roots.
#[test]
fn tombstone_path_reads_seek_the_same_partial_comparison_index_on_every_root() {
    for (order, index) in [
        (StoredPathOrder::Sensitive, "tombstones_path_key"),
        (StoredPathOrder::AsciiCaseInsensitive, "tombstones_path_key"),
    ] {
        let scratch = Scratch::new("moved-exact-plans");
        let mut store = scratch.open_under(order);
        deaths(&mut store, &[("old/target.md", Provenance::PlanDelete)]);
        let plans = snapshot(&store)
            .tombstone_at_path_plans(&path("old/target.md"))
            .expect("path plans");
        let plan = plan_of(&plans, norn_store::MovedStatement::TombstoneAtPath);
        judge_seek(&plan, "tombstones", index);
        assert!(!plans[0].plan.reads.iter().any(|column| {
            column.table == "tombstones" && column.column.ends_with("suffix_key")
        }));
    }
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
